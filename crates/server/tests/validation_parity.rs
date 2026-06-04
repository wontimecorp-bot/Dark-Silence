//! T058 {TR-018} [COMPLETES TR-018] — loopback is NOT a validation/authority
//! bypass (SC-009).
//!
//! The validate-and-apply chokepoint is **transport-agnostic** (Principle I): an
//! input driven over the in-memory loopback transport hits the *identical*
//! `ServerApp::validate_and_apply` path a networked client would. This test drives
//! two hostile inputs through the **public loopback wire** (`send_unreliable` →
//! `server.tick()` → the server drains and validates) and asserts:
//!
//! 1. an out-of-bounds analog axis (`forward` far beyond `±1`) is **clamped** to
//!    the bound by the same validation path (the ship's authoritative intent /
//!    motion reflects the clamped `+1`, never the asserted out-of-range value);
//! 2. firing faster than the authoritative `sim::Weapon` cooldown is **rate-gated**
//!    — no extra projectile appears in authoritative state.
//!
//! If loopback were a bypass, either the raw out-of-range value would take effect
//! or the excess fire would spawn extra projectiles. Neither does.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, Message, NetTransport, QuantizedIntent,
    CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::components::{Position, Projectile, Velocity};

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
    server.tick(); // accept → handshake → spawn ship

    let mut id = None;
    for m in client.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            id = Some(a.client_id);
        }
    }
    let id = id.expect("client must be accepted and learn its ship id");
    (server, client, conn, id)
}

/// Count authoritative projectiles currently in the world.
fn projectile_count(server: &mut ServerApp) -> usize {
    let world = server.world_mut();
    let mut q = world.query_filtered::<(), bevy_ecs::prelude::With<Projectile>>();
    q.iter(world).count()
}

#[test]
fn out_of_bounds_axis_is_clamped_when_driven_over_loopback() {
    // SC-009 / TR-018: a hostile out-of-range axis sent over the in-memory
    // loopback wire is clamped by the SAME validate_and_apply path a networked
    // client hits — loopback is not a bypass.
    let (mut server, mut client, conn, id) = connect_one(7301);
    let ship = server
        .ship_entity_for(id)
        .expect("connected client owns an authoritative ship");

    // forward = i8::MAX (far beyond +1), neutral turn/strafe, no fire. A bypass
    // would let this drive the ship at 127× thrust; the clamp bounds it to +1.
    let hostile = QuantizedIntent {
        forward: i8::MAX,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    };

    // Drive the same hostile input each tick over the wire (increasing seq so the
    // intake classifier accepts each as fresh, not a replay), advancing the sim.
    for seq in 1..=u32::from(server.rates().tick_rate_hz) {
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![hostile])),
        );
        server.tick();
    }

    // The ship moved forward (+x) under thrust — the input was applied, not
    // rejected — but the motion is consistent with the CLAMPED +1 axis, never the
    // raw i8::MAX. Compare against the same clamped input driven through a fresh
    // server: identical motion ⇒ the out-of-range value never escaped the clamp.
    let pos_hostile = server.world().get::<Position>(ship).unwrap().0;
    let vel_hostile = server.world().get::<Velocity>(ship).unwrap().0;

    let (mut ref_server, mut ref_client, ref_conn, ref_id) = connect_one(7302);
    let ref_ship = ref_server.ship_entity_for(ref_id).unwrap();
    let clamped = QuantizedIntent {
        forward: 1, // the clamped bound a well-behaved client would send
        ..hostile
    };
    for rseq in 1..=u32::from(ref_server.rates().tick_rate_hz) {
        ref_client.send_unreliable(
            ref_conn,
            &Message::ClientInput(ClientInput::new(
                rseq,
                ref_server.server_tick(),
                vec![clamped],
            )),
        );
        ref_server.tick();
    }
    let pos_clamped = ref_server.world().get::<Position>(ref_ship).unwrap().0;
    let vel_clamped = ref_server.world().get::<Velocity>(ref_ship).unwrap().0;

    assert!(
        pos_hostile.x > 0.0,
        "the clamped forward thrust still moves the ship (+x): {pos_hostile:?}"
    );
    assert_eq!(
        pos_hostile, pos_clamped,
        "out-of-range axis driven over loopback produces the SAME motion as the \
         clamped +1 bound — loopback ran full validation (no bypass). \
         hostile={pos_hostile:?} clamped={pos_clamped:?}"
    );
    assert_eq!(
        vel_hostile, vel_clamped,
        "velocity matches the clamped bound, not a 127× thrust"
    );
}

#[test]
fn excessive_fire_rate_is_gated_when_driven_over_loopback() {
    // SC-009 / TR-018: firing every tick over loopback cannot beat the
    // authoritative sim::Weapon cooldown — the same gate a networked firer hits.
    let (mut server, mut client, conn, _id) = connect_one(7303);

    // fire_rate = 5.0 ⇒ cooldown 0.2 s after a shot; at 30 Hz a tick is ~33.3 ms,
    // so within the first ~6 ticks at most ONE projectile may spawn. Fire on every
    // one of 6 consecutive ticks: a bypass would spawn 6 projectiles.
    let firing = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: true,
        toggle_assist: false,
        afterburner: false,
    };

    const TICKS_WITHIN_ONE_COOLDOWN: u32 = 6; // 6 * 33.3 ms ≈ 200 ms = one cooldown
    for seq in 1..=TICKS_WITHIN_ONE_COOLDOWN {
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![firing])),
        );
        server.tick();
    }

    let n = projectile_count(&mut server);
    assert_eq!(
        n, 1,
        "firing every tick within one cooldown window spawns exactly ONE \
         projectile over loopback — the authoritative cooldown gate held (no \
         bypass); observed {n}"
    );
}
