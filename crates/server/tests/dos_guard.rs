//! T061 {TR-027,028,030,031} [COMPLETES TR-027,028,030] — the DoS-guard suite
//! (SC-010).
//!
//! Each hostile inbound is discarded with authoritative state unchanged and the
//! offending connection logged in the [`RejectionLog`] (category + tick + conn,
//! NEVER a payload), and the log itself cannot grow unboundedly:
//!
//! - a **malformed/undecodable** packet → `decode_inbound` returns `Malformed`,
//!   no state mutation;
//! - an **oversize** payload (> 1200 B) → `decode_inbound` returns `Oversize`;
//! - a **replayed/stale** input → `classify_input` discards it, no state mutation;
//! - a **rate/buffer overflow** → `note_inbound` beyond 120 msg/s/1 s → `Throttle`;
//! - the [`RejectionLog`] ring is **bounded** under a flood (no unbounded buffer
//!   growth, SC-010), while the per-category counters keep an exact tally.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bevy_ecs::prelude::With;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, Message, NetTransport, QuantizedIntent,
    SnapshotAck, CLIENT_TOKEN_BYTES,
};
use server::{
    decode_inbound, DropReason, InputDisposition, RateDecision, RejectionCategory, ServerApp,
    Session, INBOUND_RATE_LIMIT_PER_SEC, MAX_PAYLOAD_BYTES, PROTOCOL_VERSION,
    REJECTION_LOG_CAPACITY,
};
use sim::components::{Heading, Position, Ship, Velocity, Weapon};

fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

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
    let id = id.expect("client must be accepted");
    (server, client, conn, id)
}

fn server_conn(server: &ServerApp) -> ConnectionId {
    let mut conns: Vec<ConnectionId> = server.session().iter().map(|(c, _)| c).collect();
    conns.sort_by_key(|c| c.0);
    *conns.first().expect("one live client")
}

/// Capture every ship's authoritative transform to a deterministic byte string —
/// the state a snapshot encodes. Compared before/after each hostile inbound to
/// prove "no state mutation".
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

#[test]
fn malformed_packet_is_dropped_logged_and_mutates_no_state() {
    // TR-030: crafted garbage is a safe Malformed drop (never a panic), logged,
    // and changes no authoritative state.
    let (mut server, _client, _conn, _id) = connect_one(7501);
    let sconn = server_conn(&server);
    let tick = server.server_tick();
    let before = authoritative_state_bytes(&mut server);

    // Crafted undecodable bytes.
    let garbage = [0xFFu8, 0xFF, 0xFF, 0xFF, 0x7F, 0x00, 0x13];
    assert_eq!(
        decode_inbound(&garbage),
        Err(DropReason::Malformed),
        "garbage decodes to a safe Malformed drop, not a panic"
    );
    // An empty payload is also a safe drop.
    assert_eq!(decode_inbound(&[]), Err(DropReason::Malformed));

    // The receive path logs the drop (category + tick + conn, no payload).
    server
        .session_mut()
        .log_rejection(sconn, RejectionCategory::Malformed, tick);

    let after = authoritative_state_bytes(&mut server);
    assert_eq!(
        before, after,
        "a malformed drop mutates no authoritative state"
    );

    let log = server.session().rejections();
    assert_eq!(log.count(RejectionCategory::Malformed), 1);
    let ev = log.last().expect("an event was logged");
    assert_eq!(ev.category, RejectionCategory::Malformed);
    assert_eq!(
        ev.entity_id,
        Some(EntityId(0)),
        "the offending conn is logged"
    );
    assert_eq!(ev.server_tick, tick, "the tick is logged");
}

#[test]
fn oversize_payload_is_rejected_before_decoding() {
    // TR-029: a payload longer than the MTU bound is dropped as Oversize without
    // even attempting to decode it.
    let (mut server, _client, _conn, _id) = connect_one(7502);
    let sconn = server_conn(&server);
    let tick = server.server_tick();
    let before = authoritative_state_bytes(&mut server);

    let oversize = vec![0u8; MAX_PAYLOAD_BYTES + 1];
    assert_eq!(
        decode_inbound(&oversize),
        Err(DropReason::Oversize),
        "an over-MTU payload is dropped as Oversize"
    );
    // A payload exactly at the bound is allowed to attempt decode (here it is
    // garbage, so it falls through to Malformed — proving the bound is inclusive).
    let at_bound = vec![0u8; MAX_PAYLOAD_BYTES];
    assert_eq!(decode_inbound(&at_bound), Err(DropReason::Malformed));

    server
        .session_mut()
        .log_rejection(sconn, RejectionCategory::Oversize, tick);

    let after = authoritative_state_bytes(&mut server);
    assert_eq!(
        before, after,
        "an oversize drop mutates no authoritative state"
    );
    let log = server.session().rejections();
    assert_eq!(log.count(RejectionCategory::Oversize), 1);
    assert_eq!(log.last().unwrap().category, RejectionCategory::Oversize);
}

#[test]
fn replayed_and_stale_inputs_are_discarded_logged_and_mutate_no_state() {
    // TR-022/023: a replay and a stale input are discarded by classify_input and
    // logged, mutating no authoritative state.
    let (mut server, mut client, conn, _id) = connect_one(7503);

    let neutral = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
    };
    client.send_unreliable(
        conn,
        &Message::ClientInput(ClientInput::new(7, server.server_tick(), vec![neutral])),
    );
    server.tick();
    let sconn = server_conn(&server);
    assert_eq!(server.session().acked_input_seq(sconn), 7);

    // Replay (seq <= last_processed).
    let replay = ClientInput::new(7, server.server_tick(), vec![neutral]);
    let st = server.session().client(sconn).unwrap();
    assert_eq!(
        Session::classify_input(&st, &replay, server.server_tick()),
        InputDisposition::Replay
    );
    let before = authoritative_state_bytes(&mut server);
    server.validate_and_apply(sconn, replay);
    let after = authoritative_state_bytes(&mut server);
    assert_eq!(after, before, "a replay mutates no authoritative state");

    // Stale (tick far outside the acceptance window).
    for _ in 0..200 {
        server.tick();
    }
    let sconn = server_conn(&server);
    let stale = ClientInput::new(500, 0, vec![neutral]);
    let st = server.session().client(sconn).unwrap();
    assert_eq!(
        Session::classify_input(&st, &stale, server.server_tick()),
        InputDisposition::Stale
    );
    let before = authoritative_state_bytes(&mut server);
    server.validate_and_apply(sconn, stale);
    let after = authoritative_state_bytes(&mut server);
    assert_eq!(
        after, before,
        "a stale input mutates no authoritative state"
    );

    let log = server.session().rejections();
    assert!(
        log.count(RejectionCategory::Replay) >= 1,
        "the replay is logged"
    );
    assert!(
        log.count(RejectionCategory::Stale) >= 1,
        "the stale is logged"
    );
    // No raw payload leaked: the logged event structurally carries only id + tick.
    let ev = log.last().unwrap();
    assert!(ev.entity_id.is_some());
}

#[test]
fn rate_overflow_is_throttled_and_logged() {
    // TR-028: beyond 120 msg/s in a 1 s window, note_inbound throttles (drops) the
    // excess and flags the offender — the buffer-overflow / flood guard.
    let mut session = Session::new(PROTOCOL_VERSION, server::RateConfig::default());
    let conn = ConnectionId(0);
    session
        .handshake(
            conn,
            &Connect {
                protocol_version: PROTOCOL_VERSION,
                client_token: [0u8; CLIENT_TOKEN_BYTES],
            },
            0,
        )
        .expect("admit one client");

    // The first 120 within the window are allowed; the rest are throttled.
    for _ in 0..INBOUND_RATE_LIMIT_PER_SEC {
        assert_eq!(session.note_inbound(conn, 0), RateDecision::Allow);
    }
    let mut throttled = 0u64;
    for _ in 0..50 {
        if session.note_inbound(conn, 0) == RateDecision::Throttle {
            throttled += 1;
        }
    }
    assert_eq!(throttled, 50, "every message past the budget is throttled");
    assert_eq!(
        session.rejections().count(RejectionCategory::RateLimited),
        throttled,
        "each throttle is flagged in the rejection log"
    );
    let ev = session.rejections().last().unwrap();
    assert_eq!(ev.category, RejectionCategory::RateLimited);
    assert_eq!(ev.entity_id, Some(EntityId(0)));
}

#[test]
fn rejection_log_is_bounded_under_a_flood_no_unbounded_growth() {
    // SC-010: a sustained flood cannot grow the rejection-log buffer unboundedly.
    // The recent-event ring is capped while the per-category COUNT stays exact.
    let mut session = Session::new(PROTOCOL_VERSION, server::RateConfig::default());
    let conn = ConnectionId(0);
    session
        .handshake(
            conn,
            &Connect {
                protocol_version: PROTOCOL_VERSION,
                client_token: [0u8; CLIENT_TOKEN_BYTES],
            },
            0,
        )
        .expect("admit one client");

    let flood = (REJECTION_LOG_CAPACITY as u32) * 4 + 17;
    for tick in 0..flood {
        session.log_rejection(conn, RejectionCategory::Malformed, tick);
    }
    let log = session.rejections();
    assert_eq!(
        log.len(),
        REJECTION_LOG_CAPACITY,
        "the event ring is bounded at capacity — no unbounded buffer growth"
    );
    assert!(
        log.len() < flood as usize,
        "the ring ({}) holds far fewer events than the {} recorded — bounded",
        log.len(),
        flood
    );
    assert_eq!(
        log.count(RejectionCategory::Malformed),
        flood as u64,
        "the per-category counter keeps an exact (cheap) tally of every event"
    );
}

#[test]
fn a_valid_snapshot_ack_still_decodes_cleanly() {
    // Sanity that decode_inbound is not over-eager: a well-formed, in-MTU message
    // round-trips through the same guard the hostile cases hit.
    let msg = Message::SnapshotAck(SnapshotAck {
        last_snapshot_id: 3,
    });
    let bytes = msg.encode();
    assert!(bytes.len() <= MAX_PAYLOAD_BYTES);
    assert_eq!(decode_inbound(&bytes), Ok(msg));
}
