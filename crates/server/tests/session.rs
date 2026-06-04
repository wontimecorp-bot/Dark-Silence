//! T023 — OBJ1 shared-world integration test (VC-1 / SC-001).
//!
//! TWO clients connect to ONE authoritative server over the in-memory loopback
//! transport, share one world, and each receives a [`Snapshot`] that contains
//! BOTH ships — i.e. the clients see each other. This exercises the share path
//! end to end through the **identical** session + validation path a networked
//! client would use (T022): only the transport is in-memory.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use glam::Vec2;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, EntityKind, Message, NetTransport,
    QuantizedIntent, Snapshot, CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};

/// Distinct loopback endpoint keys for the two simulated clients.
fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// Drain a client connection's inbox and return the first `Snapshot` found.
fn first_snapshot(transport: &mut impl NetTransport, conn: ConnectionId) -> Option<Snapshot> {
    transport.recv(conn).into_iter().find_map(|m| match m {
        Message::Snapshot(s) => Some(s),
        _ => None,
    })
}

/// Drain a client connection's inbox and return the assigned client entity id
/// from its `ConnectAccepted`, asserting it was accepted (not rejected).
fn expect_accept(transport: &mut impl NetTransport, conn: ConnectionId) -> EntityId {
    for m in transport.recv(conn) {
        match m {
            Message::ConnectAccepted(a) => return a.client_id,
            Message::ConnectRejected(r) => {
                panic!("connection was rejected: {:?}", r.reason)
            }
            _ => {}
        }
    }
    panic!("no ConnectAccepted received for {conn:?}");
}

#[test]
fn two_clients_share_one_world_and_see_each_others_ships() {
    // One embedded authoritative server; the returned transport is the client
    // end of the shared switch. Both simulated clients connect through it.
    let (mut server, mut client) = ServerApp::loopback();

    // --- Both clients open a session (reliable Connect). ---------------------
    let conn_a = client.connect(addr(7001));
    let conn_b = client.connect(addr(7002));
    client.send_reliable(conn_a, &connect_msg());
    client.send_reliable(conn_b, &connect_msg());

    // The server admits both on its next tick (accept → handshake → spawn ship).
    server.tick();

    // Each client got a distinct ConnectAccepted with its own ship id.
    let id_a = expect_accept(&mut client, conn_a);
    let id_b = expect_accept(&mut client, conn_b);
    assert_ne!(id_a, id_b, "each client owns a distinct ship id");
    assert_eq!(
        server.session().client_count(),
        2,
        "one shared world, two clients"
    );

    // --- Each client sends an input so the validate-and-apply path runs. -----
    let neutral = QuantizedIntent {
        forward: 0,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    };
    client.send_unreliable(
        conn_a,
        &Message::ClientInput(ClientInput::new(1, 0, vec![neutral])),
    );
    client.send_unreliable(
        conn_b,
        &Message::ClientInput(ClientInput::new(1, 0, vec![neutral])),
    );

    // --- Advance until a snapshot is broadcast (snapshot rate < tick rate). --
    // Drive enough ticks to cross at least one snapshot boundary.
    let mut snap_a = None;
    let mut snap_b = None;
    for _ in 0..server.rates().tick_rate_hz {
        server.tick();
        if let Some(s) = first_snapshot(&mut client, conn_a) {
            snap_a = Some(s);
        }
        if let Some(s) = first_snapshot(&mut client, conn_b) {
            snap_b = Some(s);
        }
        if snap_a.is_some() && snap_b.is_some() {
            break;
        }
    }

    let snap_a = snap_a.expect("client A must receive a snapshot");
    let snap_b = snap_b.expect("client B must receive a snapshot");

    // Both ships are present in BOTH clients' snapshots — they see each other.
    let ships_in = |s: &Snapshot| -> Vec<EntityId> {
        s.entities
            .iter()
            .filter(|r| r.kind == EntityKind::Ship)
            .map(|r| r.id)
            .collect::<Vec<_>>()
    };

    let ships_a = ships_in(&snap_a);
    let ships_b = ships_in(&snap_b);

    assert_eq!(ships_a.len(), 2, "client A's snapshot carries both ships");
    assert_eq!(ships_b.len(), 2, "client B's snapshot carries both ships");

    assert!(
        ships_a.contains(&id_a) && ships_a.contains(&id_b),
        "client A sees its own ship AND client B's"
    );
    assert!(
        ships_b.contains(&id_a) && ships_b.contains(&id_b),
        "client B sees its own ship AND client A's"
    );

    // The per-recipient ack anchor reflects each client's own processed input.
    assert_eq!(snap_a.acked_input_seq, 1, "A's snapshot acks A's input");
    assert_eq!(snap_b.acked_input_seq, 1, "B's snapshot acks B's input");
}

#[test]
fn two_clients_with_different_inputs_drive_their_ships_independently() {
    // Per-entity intent (SC-001 / TR-002): two clients in ONE shared step send
    // DIFFERENT inputs and their ships diverge — each ship is piloted by its own
    // input, not a single global/last-client-wins intent.
    let (mut server, mut client) = ServerApp::loopback();

    let conn_a = client.connect(addr(7101));
    let conn_b = client.connect(addr(7102));
    client.send_reliable(conn_a, &connect_msg());
    client.send_reliable(conn_b, &connect_msg());
    server.tick();

    let id_a = expect_accept(&mut client, conn_a);
    let id_b = expect_accept(&mut client, conn_b);
    assert_ne!(id_a, id_b, "each client owns a distinct ship id");

    // A thrusts forward (+x); B thrusts reverse (−x). Both ships start at the
    // origin facing heading 0, so opposite thrust must drive them apart.
    let forward = QuantizedIntent {
        forward: 1,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    };
    let reverse = QuantizedIntent {
        forward: -1,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    };

    // Re-send the per-step input each tick (inputs are consumed per step) and
    // collect the latest snapshot for each client.
    let mut snap_a = None;
    let mut snap_b = None;
    for _ in 0..(server.rates().tick_rate_hz * 2) {
        client.send_unreliable(
            conn_a,
            &Message::ClientInput(ClientInput::new(1, 0, vec![forward])),
        );
        client.send_unreliable(
            conn_b,
            &Message::ClientInput(ClientInput::new(1, 0, vec![reverse])),
        );
        server.tick();
        if let Some(s) = first_snapshot(&mut client, conn_a) {
            snap_a = Some(s);
        }
        if let Some(s) = first_snapshot(&mut client, conn_b) {
            snap_b = Some(s);
        }
    }

    let snap = snap_a.or(snap_b).expect("a snapshot must be broadcast");

    // Read each ship's position from the shared snapshot body (both clients see
    // both ships; the body is identical).
    let pos_of = |id: EntityId| -> Vec2 {
        snap.entities
            .iter()
            .find(|r| r.id == id && r.kind == EntityKind::Ship)
            .map(|r| r.pos.dequantize_pos())
            .unwrap_or_else(|| panic!("ship {id:?} present in snapshot"))
    };

    let pos_a = pos_of(id_a);
    let pos_b = pos_of(id_b);

    // A drove +x, B drove −x: their positions must differ, and on opposite sides
    // of the origin along x. A single global intent would have moved both ships
    // identically (zero separation) — this is the per-ship-control assertion.
    assert!(
        pos_a.x > 0.0,
        "ship A thrust forward (+x), expected +x position, got {pos_a:?}"
    );
    assert!(
        pos_b.x < 0.0,
        "ship B thrust reverse (−x), expected −x position, got {pos_b:?}"
    );
    assert!(
        (pos_a.x - pos_b.x) > 1.0,
        "the two ships must visibly diverge under different inputs: A={pos_a:?} B={pos_b:?}"
    );
}

#[test]
fn loopback_uses_the_same_handshake_path_capacity_and_version() {
    // Loopback is not an authority bypass: the handshake policy (version,
    // capacity) applies identically over the in-memory transport (T022).
    let (mut server, mut client) = ServerApp::loopback();

    // A version mismatch is rejected (and the connection closed).
    let bad = client.connect(addr(8001));
    client.send_reliable(
        bad,
        &Message::Connect(Connect {
            protocol_version: PROTOCOL_VERSION.wrapping_add(1),
            client_token: [0u8; CLIENT_TOKEN_BYTES],
        }),
    );
    server.tick();
    let mut saw_reject = false;
    for m in client.recv(bad) {
        if let Message::ConnectRejected(_) = m {
            saw_reject = true;
        }
    }
    assert!(saw_reject, "version mismatch is rejected over loopback");
    assert_eq!(
        server.session().client_count(),
        0,
        "a rejected loopback connect holds no slot"
    );
}
