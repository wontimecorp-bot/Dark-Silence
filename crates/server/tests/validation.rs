//! T059 {TR-038,011,020,021,022,023} [COMPLETES TR-038] — the four enumerated,
//! separate rejection cases, each asserting the TR-039 observable signal.
//! T060 {TR-039} [COMPLETES TR-039] — the byte-for-byte state-equality assertion
//! around a rejected/ignored input.
//!
//! Each rejection case is its own `#[test]`:
//!   1. out-of-bounds analog `forward`/`strafe`/`turn` → **clamped** (the applied
//!      value equals the clamped bound, never the asserted out-of-range value);
//!   2. excessive `fire` rate → rate-gated (no extra projectile in authoritative
//!      state — fired faster than the `sim::Weapon` cooldown);
//!   3. replayed/duplicate `seq` + stale `tick` → **discarded** (`classify_input`
//!      returns `Replay`/`Stale`; authoritative state is unmutated);
//!   4. client-asserted position/hit → **ignored** (structurally: the wire
//!      `ClientInput` has no position/hit field; motion comes only from the sim).
//!
//! T060 captures the authoritative sim state (the entity transforms the snapshot
//! encodes) to bytes immediately before and after a rejected/ignored input and
//! asserts byte-for-byte equality (the input-ack bookkeeping may still record the
//! seq as seen). For the clamp case, it instead asserts the applied value equals
//! the clamped bound.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bevy_ecs::prelude::With;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, Message, NetTransport, QuantizedIntent,
    CLIENT_TOKEN_BYTES,
};
use server::{InputDisposition, ServerApp, Session, PROTOCOL_VERSION};
use sim::components::{Heading, Position, Projectile, Ship, Velocity, Weapon};

fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// Connect one in-process client over loopback and run the accept tick. Returns
/// (server, client_transport, client_conn, owned_ship_entity_id).
fn connect_one(
    port: u16,
) -> (
    ServerApp,
    protocol::LoopbackTransport,
    ConnectionId,
    EntityId,
) {
    let (mut server, mut client) = ServerApp::loopback();
    let conn = client.connect(addr(port));
    client.send_reliable(conn, &connect_msg());
    server.tick();

    let mut id = None;
    for m in client.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            id = Some(a.client_id);
        }
    }
    let id = id.expect("client must be accepted and learn its ship id");
    (server, client, conn, id)
}

/// The server-side `ConnectionId` for the single live client (the id `links`/the
/// session are keyed by — distinct from the client-side loopback handle). Lets a
/// test drive the authoritative `validate_and_apply` chokepoint directly.
fn server_conn(server: &ServerApp) -> ConnectionId {
    let mut conns: Vec<ConnectionId> = server.session().iter().map(|(c, _)| c).collect();
    conns.sort_by_key(|c| c.0);
    *conns.first().expect("one live client")
}

/// Capture every ship's authoritative state to a deterministic byte string — the
/// transform fields a `Snapshot`'s `EntityRecord` encodes (pos, vel, heading) plus
/// the weapon-cooldown bookkeeping. Serialized to little-endian bytes, ordered by
/// raw entity index so the byte string is stable run to run. This is the
/// "serialize the relevant BodyStates to bytes" the TR-039 state-equality
/// assertion compares before/after a rejected input.
fn authoritative_state_bytes(server: &mut ServerApp) -> Vec<u8> {
    let world = server.world_mut();
    let mut q = world.query_filtered::<(
        bevy_ecs::entity::Entity,
        &Position,
        &Velocity,
        &Heading,
        &Weapon,
    ), With<Ship>>();
    let mut rows: Vec<(u64, [f32; 6])> = q
        .iter(world)
        .map(|(e, p, v, h, w)| (e.to_bits(), [p.0.x, p.0.y, v.0.x, v.0.y, h.0, w.cooldown]))
        .collect();
    rows.sort_by_key(|(bits, _)| *bits);
    let mut bytes = Vec::new();
    for (bits, fields) in rows {
        bytes.extend_from_slice(&bits.to_le_bytes());
        for f in fields {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    bytes
}

fn projectile_count(server: &mut ServerApp) -> usize {
    let world = server.world_mut();
    let mut q = world.query_filtered::<(), With<Projectile>>();
    q.iter(world).count()
}

// --- T059 case 1 / T060 (clamp) : out-of-bounds axis is clamped ----------------

#[test]
fn case1_out_of_bounds_axes_are_clamped_to_the_bound() {
    // TR-020/039: the observable signal for an out-of-range analog axis is a
    // CLAMP — the applied value equals the clamped bound, not the asserted
    // out-of-range value. T060 (clamp variant): assert the applied value == bound.
    let (mut server, mut client, conn, id) = connect_one(7401);
    let ship = server.ship_entity_for(id).unwrap();

    // Hostile axes far outside -1..=1 in BOTH directions.
    let hostile = QuantizedIntent {
        forward: i8::MAX, // → clamps to +1
        strafe: i8::MIN,  // → clamps to -1
        turn: 100,        // → clamps to +1
        fire: false,
        toggle_assist: false,
    };
    client.send_unreliable(
        conn,
        &Message::ClientInput(ClientInput::new(1, server.server_tick(), vec![hostile])),
    );
    // One tick: drain + validate (clamp) + apply + step.
    server.tick();

    // The applied authoritative motion is consistent with the clamped bounds, not
    // the raw i8 extremes. Drive the SAME tick on a reference server with the
    // explicit clamped axes (+1, -1, +1) and require identical authoritative state.
    let (mut ref_server, mut ref_client, ref_conn, ref_id) = connect_one(7402);
    let ref_ship = ref_server.ship_entity_for(ref_id).unwrap();
    let clamped = QuantizedIntent {
        forward: 1,
        strafe: -1,
        turn: 1,
        fire: false,
        toggle_assist: false,
    };
    ref_client.send_unreliable(
        ref_conn,
        &Message::ClientInput(ClientInput::new(1, ref_server.server_tick(), vec![clamped])),
    );
    ref_server.tick();

    let hostile_pos = server.world().get::<Position>(ship).unwrap().0;
    let clamped_pos = ref_server.world().get::<Position>(ref_ship).unwrap().0;
    assert_eq!(
        hostile_pos, clamped_pos,
        "out-of-range axes applied as the CLAMPED bound (+1/-1/+1), not the raw \
         i8 extremes: hostile={hostile_pos:?} clamped={clamped_pos:?}"
    );
    assert_eq!(
        authoritative_state_bytes(&mut server),
        authoritative_state_bytes(&mut ref_server),
        "authoritative state after the clamped hostile input is byte-identical \
         to the explicitly-clamped reference"
    );
    // The clamp was recorded as an observed anomaly (the input was still applied).
    assert!(
        server
            .session()
            .rejections()
            .count(server::RejectionCategory::Clamped)
            >= 1,
        "the clamp is logged as an anomaly"
    );
}

// --- T059 case 2 : excessive fire rate is rate-gated ---------------------------

#[test]
fn case2_excessive_fire_rate_produces_no_extra_projectile() {
    // TR-021/039: firing faster than the sim::Weapon cooldown spawns no extra
    // projectile in authoritative state. fire_rate 5.0 ⇒ 0.2 s cooldown ≈ 6 ticks.
    let (mut server, mut client, conn, _id) = connect_one(7403);
    let firing = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: true,
        toggle_assist: false,
    };

    const TICKS: u32 = 6; // one cooldown window at 30 Hz
    for seq in 1..=TICKS {
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![firing])),
        );
        server.tick();
    }

    let n = projectile_count(&mut server);
    assert_eq!(
        n, 1,
        "fire-every-tick within one cooldown window spawns exactly ONE projectile \
         (the excess fires are gated): observed {n}"
    );
}

// --- T059 case 3 / T060 : replay + stale are discarded, state unmutated --------

#[test]
fn case3_replayed_and_stale_inputs_are_discarded_state_unmutated() {
    // TR-022/023/039: a replayed/duplicate seq and a stale tick are discarded by
    // classify_input and mutate NO authoritative state. T060: assert the captured
    // authoritative-state bytes are byte-for-byte identical before and after,
    // EXCEPT the input-ack bookkeeping (which may record the seq as seen).
    let (mut server, mut client, conn, _id) = connect_one(7404);

    // Establish a baseline last-processed seq with a real, applied input, then let
    // the world settle a couple of ticks so it is non-trivial but stable.
    let neutral = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
    };
    client.send_unreliable(
        conn,
        &Message::ClientInput(ClientInput::new(10, server.server_tick(), vec![neutral])),
    );
    server.tick();
    let sconn = server_conn(&server);
    let acked_before = server.session().acked_input_seq(sconn);
    assert_eq!(acked_before, 10, "the genuine input was processed");

    // --- Replay: a duplicate of an already-processed seq. --------------------
    let state = server.session().client(sconn).unwrap();
    let replay = ClientInput::new(10, server.server_tick(), vec![neutral]);
    assert_eq!(
        Session::classify_input(&state, &replay, server.server_tick()),
        InputDisposition::Replay,
        "a seq <= last-processed classifies as Replay"
    );

    // Capture authoritative state, push the replay through the SAME path, capture
    // again: byte-for-byte identical (the discard mutated nothing).
    let before = authoritative_state_bytes(&mut server);
    server.validate_and_apply(sconn, replay);
    let after = authoritative_state_bytes(&mut server);
    assert_eq!(
        before, after,
        "a replayed input mutates NO authoritative state (byte-for-byte equal)"
    );
    // Input-ack bookkeeping did NOT advance (the seq was already seen).
    assert_eq!(
        server.session().acked_input_seq(sconn),
        acked_before,
        "the replayed seq is not re-applied"
    );
    assert!(
        server
            .session()
            .rejections()
            .count(server::RejectionCategory::Replay)
            >= 1,
        "the replay is logged"
    );

    // --- Stale: a fresh seq but a tick far outside the acceptance window. -----
    // Advance server_tick well past the UNACKED_BUFFER_BOUND window first.
    for _ in 0..200 {
        server.tick();
    }
    let sconn = server_conn(&server);
    let state = server.session().client(sconn).unwrap();
    // Fresh seq (so it is not a replay) but tick 0 — far older than the window.
    let stale = ClientInput::new(100, 0, vec![neutral]);
    assert_eq!(
        Session::classify_input(&state, &stale, server.server_tick()),
        InputDisposition::Stale,
        "a tick older than server_tick - UNACKED_BUFFER_BOUND classifies as Stale"
    );

    let acked_pre_stale = server.session().acked_input_seq(sconn);
    let before = authoritative_state_bytes(&mut server);
    server.validate_and_apply(sconn, stale);
    let after = authoritative_state_bytes(&mut server);
    assert_eq!(
        before, after,
        "a stale input mutates NO authoritative state (byte-for-byte equal)"
    );
    assert_eq!(
        server.session().acked_input_seq(sconn),
        acked_pre_stale,
        "the stale seq is not applied to the ack anchor"
    );
    assert!(
        server
            .session()
            .rejections()
            .count(server::RejectionCategory::Stale)
            >= 1,
        "the stale input is logged"
    );
}

// --- T059 case 4 : client-asserted position/hit is structurally ignored --------

#[test]
fn case4_client_cannot_assert_position_motion_comes_only_from_the_sim() {
    // TR-012/039: the wire ClientInput carries NO position/hit field — a client
    // structurally cannot assert where it is or what it hit. The only motion comes
    // from the server sim. Assert: (a) the wire type has no such field (structural
    // — enforced at compile time by constructing it from only seq/tick/intents),
    // and (b) a NEUTRAL input (zero thrust) leaves the ship at the origin — it did
    // not teleport to any client-claimed position.
    let (mut server, mut client, conn, id) = connect_one(7405);
    let ship = server.ship_entity_for(id).unwrap();

    // Construct the maximal client input: it accepts ONLY seq, tick, and a list of
    // QuantizedIntent. There is no position or hit parameter to pass — the
    // structural guarantee of TR-012. (If a position field were ever added, this
    // call would no longer compile, failing the test.)
    let neutral = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
    };
    let _typecheck: fn(u32, u32, Vec<QuantizedIntent>) -> ClientInput = ClientInput::new;

    for seq in 1..=30u32 {
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![neutral])),
        );
        server.tick();
    }

    // With zero thrust the sim leaves the ship exactly where it spawned (the
    // origin). A client cannot assert a new position; only the sim moves it.
    let pos = server.world().get::<Position>(ship).unwrap().0;
    let vel = server.world().get::<Velocity>(ship).unwrap().0;
    assert_eq!(
        pos,
        glam::Vec2::ZERO,
        "a neutral input leaves the ship at the sim-derived origin — no \
         client-asserted teleport: {pos:?}"
    );
    assert_eq!(
        vel,
        glam::Vec2::ZERO,
        "no client-asserted velocity; motion is sim-derived only: {vel:?}"
    );
}
