//! T040 {TR-007} — prediction responsiveness (SC-001 prediction path).
//!
//! Applying a predicted input moves the **local** ship *immediately* — the same
//! tick, before any server round-trip — so the local player feels no input delay
//! (AD-005: the client predicts only its own ship by running the shared `sim`).
//!
//! Driven over the in-memory [`LoopbackTransport`] (no renet): the client
//! connects to an embedded `ServerApp`, learns its own ship id, then predicts
//! locally. The assertion is that the predicted ship has already moved BEFORE the
//! server has even ticked the input — i.e. the response is local and immediate,
//! not gated on the authoritative loop.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use client::input::{build_client_input, InputSequencer};
use client::prediction::{InputBuffer, Predictor, ShipInit};
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, Message, NetTransport, CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::ShipIntent;

fn addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7777)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// Connect the loopback client and return its assigned ship [`EntityId`].
fn handshake(
    server: &mut ServerApp,
    client: &mut impl NetTransport,
    conn: ConnectionId,
) -> EntityId {
    client.send_reliable(conn, &connect_msg());
    server.tick();
    for m in client.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            return a.client_id;
        }
    }
    panic!("no ConnectAccepted received");
}

#[test]
fn predicted_input_moves_local_ship_immediately_no_round_trip() {
    let (mut server, mut client) = ServerApp::loopback();
    let conn = client.connect(addr());
    let _local_id = handshake(&mut server, &mut client, conn);

    // The client predicts its own ship by running the SHARED sim. Seed it to the
    // same pose the server spawned its ship at (origin, at rest, assist On).
    let mut predictor = Predictor::new(ShipInit::default(), 1.0 / 60.0);
    let mut buffer = InputBuffer::new();
    let mut sequencer = InputSequencer::new();

    let before = predictor.ship_state();

    // Full-forward thrust this tick.
    let intent = ShipIntent {
        forward: 1.0,
        ..Default::default()
    };

    // Build the numbered wire input (T033) and send it to the server — but do NOT
    // tick the server. The local prediction must not depend on the server having
    // processed anything.
    let wire: ClientInput = build_client_input(&mut sequencer, /*tick*/ 0, intent);
    client.send_unreliable(conn, &Message::ClientInput(wire.clone()));

    // Predict locally: apply the input to the local ship and step the shared sim
    // ONCE. This is the immediate-response path.
    predictor.predict(
        &mut buffer,
        client::prediction::NumberedInput {
            seq: wire.seq,
            intent: wire.inputs[0],
        },
    );

    let after = predictor.ship_state();

    // The local ship has moved this very tick, with the server not yet ticked on
    // this input — the response is immediate (SC-001).
    assert!(
        after.pos.x > before.pos.x,
        "predicted local ship must move forward immediately (before any server \
         round-trip): before={before:?} after={after:?}"
    );
    assert_eq!(
        buffer.len(),
        1,
        "the predicted input is buffered for reconcile"
    );
    assert_eq!(buffer.newest().unwrap().seq, wire.seq);

    // Sanity: the server has genuinely NOT advanced this ship yet — its tick is
    // still at the post-handshake value (1: the accept tick), proving the local
    // motion did not come from the authoritative loop.
    assert_eq!(
        server.server_tick(),
        1,
        "server must not have ticked the input — prediction is purely local"
    );
}

#[test]
fn build_client_input_numbers_monotonically_with_redundant_tail() {
    // T033: each produced input carries the next monotonic seq and a newest-first
    // redundant tail capped at MAX_INPUT_TAIL.
    let mut sequencer = InputSequencer::new();
    let neutral = ShipIntent::default();

    let first = build_client_input(&mut sequencer, 0, neutral);
    assert_eq!(first.seq, 1, "first input is seq 1");
    assert_eq!(first.inputs.len(), 1, "tail holds just the first input");

    let second = build_client_input(&mut sequencer, 1, neutral);
    assert_eq!(second.seq, 2, "seq is monotonic");
    assert_eq!(second.inputs.len(), 2, "tail accumulates, newest-first");

    // Drive past the tail cap; it must never exceed MAX_INPUT_TAIL.
    for tick in 2..20 {
        let ci = build_client_input(&mut sequencer, tick, neutral);
        assert!(
            ci.inputs.len() <= protocol::MAX_INPUT_TAIL,
            "redundant tail is bounded at MAX_INPUT_TAIL (TR-027)"
        );
    }
    let last = build_client_input(&mut sequencer, 20, neutral);
    assert_eq!(last.seq, 21, "seq keeps counting every produced input");
    assert_eq!(last.inputs.len(), protocol::MAX_INPUT_TAIL);
}
