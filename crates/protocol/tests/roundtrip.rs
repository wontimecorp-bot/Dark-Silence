//! T016 {TR-004} — every `Message` variant round-trips `encode → decode` to an
//! EQUAL value. Independent of any harness: constructs one of each variant
//! (including a multi-entity `Snapshot` with a removed list and a full 8-entry
//! `ClientInput` tail) and asserts `decode(encode(m)) == m`.

use protocol::messages::QuantizedIntent;
use protocol::quantize::{QAngle, QVec2};
use protocol::{
    ClientInput, Connect, ConnectAccepted, ConnectRejected, ConnectionId, Disconnect,
    DisconnectReason, EntityId, EntityKind, EntityRecord, Message, RejectReason, Snapshot,
    SnapshotAck, CLIENT_TOKEN_BYTES, MAX_INPUT_TAIL,
};

/// Round-trip a message and assert the decoded value equals the original.
fn assert_roundtrip(msg: Message) {
    let bytes = msg.encode();
    let decoded = Message::decode(&bytes).expect("decode of self-encoded message must succeed");
    assert_eq!(decoded, msg, "round-trip changed the message");
}

#[test]
fn connect_roundtrips() {
    let mut token = [0u8; CLIENT_TOKEN_BYTES];
    for (i, b) in token.iter_mut().enumerate() {
        *b = i as u8;
    }
    assert_roundtrip(Message::Connect(Connect {
        protocol_version: 7,
        client_token: token,
    }));
}

#[test]
fn connect_accepted_roundtrips() {
    assert_roundtrip(Message::ConnectAccepted(ConnectAccepted {
        client_id: EntityId(42),
        tick_rate_hz: 60,
        snapshot_rate_hz: 20,
        interp_delay_ms: 100,
        server_tick: 123_456,
    }));
}

#[test]
fn connect_rejected_roundtrips() {
    for reason in [
        RejectReason::Version,
        RejectReason::Full,
        RejectReason::Banned,
    ] {
        assert_roundtrip(Message::ConnectRejected(ConnectRejected { reason }));
    }
}

#[test]
fn disconnect_roundtrips() {
    for reason in [
        DisconnectReason::Timeout,
        DisconnectReason::ProtocolError,
        DisconnectReason::ClientClosed,
        DisconnectReason::ServerClosed,
    ] {
        assert_roundtrip(Message::Disconnect(Disconnect { reason }));
    }
}

#[test]
fn client_input_full_tail_roundtrips() {
    // A full MAX_INPUT_TAIL (8) entry tail, newest first, with varied axes/flags.
    let inputs: Vec<QuantizedIntent> = (0..MAX_INPUT_TAIL)
        .map(|i| QuantizedIntent {
            forward: [(-1i8), 0, 1][i % 3],
            strafe: [1i8, -1, 0][i % 3],
            turn: [0i8, 1, -1][i % 3],
            fire: i % 2 == 0,
            toggle_assist: i % 3 == 0,
        })
        .collect();
    assert_eq!(inputs.len(), MAX_INPUT_TAIL);
    assert_roundtrip(Message::ClientInput(ClientInput::new(900, 901, inputs)));
}

#[test]
fn client_input_tail_is_capped() {
    // Constructing with more than MAX_INPUT_TAIL truncates (TR-027).
    let inputs: Vec<QuantizedIntent> = (0..MAX_INPUT_TAIL + 5)
        .map(|_| QuantizedIntent {
            forward: 1,
            strafe: 0,
            turn: -1,
            fire: false,
            toggle_assist: false,
        })
        .collect();
    let ci = ClientInput::new(1, 1, inputs);
    assert_eq!(ci.inputs.len(), MAX_INPUT_TAIL);
    assert_roundtrip(Message::ClientInput(ci));
}

#[test]
fn snapshot_ack_roundtrips() {
    assert_roundtrip(Message::SnapshotAck(SnapshotAck {
        last_snapshot_id: 65_000,
    }));
}

#[test]
fn snapshot_with_entities_and_removed_roundtrips() {
    let entities = vec![
        EntityRecord {
            id: EntityId(1),
            kind: EntityKind::Ship,
            pos: QVec2::quantize_pos(glam::Vec2::new(10.0, -20.0)),
            vel: QVec2::quantize_vel(glam::Vec2::new(1.5, -2.5)),
            heading: QAngle::quantize(0.5),
            flags: 0b0000_0001,
        },
        EntityRecord {
            id: EntityId(2),
            kind: EntityKind::Projectile,
            pos: QVec2::quantize_pos(glam::Vec2::new(-300.0, 400.0)),
            vel: QVec2::quantize_vel(glam::Vec2::new(50.0, 0.0)),
            heading: QAngle::quantize(-1.25),
            flags: 0,
        },
        EntityRecord {
            id: EntityId(3),
            kind: EntityKind::Target,
            pos: QVec2::quantize_pos(glam::Vec2::new(0.0, 0.0)),
            vel: QVec2::quantize_vel(glam::Vec2::new(0.0, 0.0)),
            heading: QAngle::quantize(3.0),
            flags: 0b1000_0000,
        },
    ];
    assert_roundtrip(Message::Snapshot(Snapshot {
        server_tick: 7777,
        acked_input_seq: 555,
        baseline_id: 12,
        entities,
        removed: vec![EntityId(9), EntityId(10), EntityId(11)],
    }));
}

#[test]
fn empty_snapshot_roundtrips() {
    assert_roundtrip(Message::Snapshot(Snapshot {
        server_tick: 0,
        acked_input_seq: 0,
        baseline_id: 0,
        entities: vec![],
        removed: vec![],
    }));
}

#[test]
fn decode_rejects_garbage() {
    // Truncated/garbage bytes must fail gracefully, never panic.
    let err = Message::decode(&[0xFF, 0xFF, 0xFF, 0xFF]);
    assert!(err.is_err(), "garbage bytes must not decode to a Message");
}

#[test]
fn intent_roundtrips_through_quantized_form() {
    use sim::ShipIntent;
    let intent = ShipIntent {
        forward: 1.0,
        strafe: -1.0,
        turn: 0.0,
        fire: true,
        toggle_assist: false,
    };
    let q: QuantizedIntent = intent.into();
    let back: ShipIntent = q.into();
    assert_eq!(back, intent);
    // The connection-id newtype is part of the surface used by transports.
    let _ = ConnectionId(0);
}
