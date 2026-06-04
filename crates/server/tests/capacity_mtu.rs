//! T062 {TR-024,025,029} — capacity ceiling + MTU payload bound (SC-011).
//!
//! - A `Connect` at the capacity ceiling (the 9th client, `MAX_CLIENTS = 8`) is
//!   refused with `ConnectRejected { Full }` and leaks NO slot: `client_count`
//!   stays 8.
//! - Every emitted `Snapshot` for the baseline world encodes to ≤ 1200 B (the MTU
//!   payload bound, so no IP fragmentation).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use protocol::{
    ClientInput, Connect, ConnectionId, Message, NetTransport, QuantizedIntent, RejectReason,
    CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, MAX_CLIENTS, MAX_PAYLOAD_BYTES, PROTOCOL_VERSION};

fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

#[test]
fn ninth_connect_is_refused_full_with_no_leaked_slot() {
    // TR-025 / SC-011: MAX_CLIENTS connect successfully; the (MAX_CLIENTS + 1)th is
    // rejected Full and consumes NO slot.
    let (mut server, mut client) = ServerApp::loopback();
    assert_eq!(MAX_CLIENTS, 8, "the capacity ceiling under test is 8");

    // Connect exactly MAX_CLIENTS clients.
    let mut conns: Vec<ConnectionId> = Vec::new();
    for i in 0..MAX_CLIENTS {
        let c = client.connect(addr(7600 + i as u16));
        client.send_reliable(c, &connect_msg());
        conns.push(c);
    }
    server.tick(); // accept all 8

    // All 8 were accepted.
    let mut accepted = 0;
    for &c in &conns {
        for m in client.recv(c) {
            match m {
                Message::ConnectAccepted(_) => accepted += 1,
                Message::ConnectRejected(r) => panic!("client {c:?} rejected: {:?}", r.reason),
                _ => {}
            }
        }
    }
    assert_eq!(
        accepted, MAX_CLIENTS,
        "all MAX_CLIENTS clients are admitted"
    );
    assert_eq!(server.session().client_count(), MAX_CLIENTS);

    // The 9th client connects at the ceiling.
    let ninth = client.connect(addr(7600 + MAX_CLIENTS as u16));
    client.send_reliable(ninth, &connect_msg());
    server.tick();

    let mut saw_full = false;
    for m in client.recv(ninth) {
        match m {
            Message::ConnectRejected(r) => {
                assert_eq!(r.reason, RejectReason::Full, "the 9th is refused as Full");
                saw_full = true;
            }
            Message::ConnectAccepted(_) => panic!("the 9th client must NOT be admitted"),
            _ => {}
        }
    }
    assert!(saw_full, "the 9th client receives ConnectRejected{{Full}}");
    assert_eq!(
        server.session().client_count(),
        MAX_CLIENTS,
        "a Full reject leaks NO slot — client_count stays {MAX_CLIENTS}"
    );
}

#[test]
fn every_snapshot_for_the_baseline_world_fits_the_mtu() {
    // TR-029 / SC-011: with the full baseline world (MAX_CLIENTS ships), every
    // broadcast Snapshot encodes to ≤ 1200 B — no IP fragmentation.
    let (mut server, mut client) = ServerApp::loopback();

    let mut conns: Vec<ConnectionId> = Vec::new();
    for i in 0..MAX_CLIENTS {
        let c = client.connect(addr(7700 + i as u16));
        client.send_reliable(c, &connect_msg());
        conns.push(c);
    }
    server.tick(); // accept all
    for &c in &conns {
        // Drain the ConnectAccepted so the inbox holds only snapshots afterward.
        let _ = client.recv(c);
    }
    assert_eq!(server.session().client_count(), MAX_CLIENTS);

    // Have every ship fire so the world also carries projectiles (a fuller
    // baseline than ships alone), then drive several snapshot broadcasts.
    let firing = QuantizedIntent {
        forward: 1,
        strafe: 0,
        turn: 0,
        fire: true,
        toggle_assist: false,
        afterburner: false,
    };

    let mut largest = 0usize;
    let mut snapshots_seen = 0usize;
    for seq in 1..=u32::from(server.rates().tick_rate_hz) {
        for &c in &conns {
            client.send_unreliable(
                c,
                &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![firing])),
            );
        }
        server.tick();
        // Inspect every snapshot each client received this tick.
        for &c in &conns {
            for m in client.recv(c) {
                if let Message::Snapshot(_) = &m {
                    let encoded = m.encode().len();
                    largest = largest.max(encoded);
                    snapshots_seen += 1;
                    assert!(
                        encoded <= MAX_PAYLOAD_BYTES,
                        "a baseline-world Snapshot encoded to {encoded} B > {MAX_PAYLOAD_BYTES} \
                         B MTU bound (would fragment)"
                    );
                }
            }
        }
    }

    assert!(
        snapshots_seen > 0,
        "the server must broadcast at least one snapshot"
    );
    // Report the high-water mark (visible under `-- --nocapture`).
    eprintln!(
        "[capacity_mtu] {snapshots_seen} snapshots inspected; largest={largest} B (bound {MAX_PAYLOAD_BYTES} B)"
    );
    assert!(
        largest <= MAX_PAYLOAD_BYTES,
        "largest observed snapshot {largest} B within the {MAX_PAYLOAD_BYTES} B MTU bound"
    );
}
