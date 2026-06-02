//! T031 {TR-041} — real-UDP round-trip over the renet adapter.
//!
//! Stands up a [`protocol::RenetTransport`] server and client on real
//! `127.0.0.1` UDP sockets, drives the netcode pump in a bounded loop until the
//! client connects, then asserts a [`protocol::Message`] round-trips each way
//! over the wire. This is distinct from the in-memory loopback (Phase 2): bytes
//! traverse the OS socket stack.
//!
//! Robustness: ephemeral OS-assigned ports (no fixed-port collisions), a
//! generous iteration cap, and a tiny sleep per step so the OS can move
//! datagrams between the two sockets. Bind failure is surfaced as a clear
//! `expect`, not a silent skip.
#![cfg(feature = "udp")]

use protocol::{
    Connect, ConnectAccepted, EntityId, Message, NetTransport, RenetTransport, CLIENT_TOKEN_BYTES,
};
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

/// Fixed step fed to both transports each iteration.
const DT: Duration = Duration::from_millis(16);
/// Upper bound on drive iterations before declaring failure (generous: ~16s of
/// simulated time, well past netcode's connect handshake).
const MAX_ITERS: usize = 1000;
/// Small real sleep so the loopback OS socket can ferry datagrams between the
/// two bound sockets within an iteration.
const STEP_SLEEP: Duration = Duration::from_millis(2);

/// Bind a server transport on an ephemeral 127.0.0.1 port and a client aimed at
/// it. Returns `(client, server)`. Panics with a clear message on bind failure.
fn make_pair() -> (RenetTransport, RenetTransport) {
    let server_socket =
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind server udp socket on 127.0.0.1");
    let server_addr = server_socket
        .local_addr()
        .expect("server socket has a local addr");

    let server = RenetTransport::unsecure_server(server_socket, server_addr)
        .expect("build unsecure renet server");

    let client_socket =
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind client udp socket on 127.0.0.1");
    let client = RenetTransport::unsecure_client(client_socket, server_addr, 42)
        .expect("build unsecure renet client");

    (client, server)
}

/// A representative client→server handshake message.
fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: 1,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// A representative server→client accept message.
fn accept_msg() -> Message {
    Message::ConnectAccepted(ConnectAccepted {
        client_id: EntityId(99),
        tick_rate_hz: 60,
        snapshot_rate_hz: 20,
        interp_delay_ms: 100,
        server_tick: 12345,
    })
}

#[test]
fn message_round_trips_over_real_udp() {
    let (mut client, mut server) = make_pair();

    // Mint the client handle (advisory address; the target is fixed at build).
    let client_conn = client.connect((Ipv4Addr::LOCALHOST, 0).into());

    // --- Drive until the client connects and the server accepts it. ----------
    let mut server_conn = None;
    let mut connected = false;
    for _ in 0..MAX_ITERS {
        client.update(DT).ok();
        server.update(DT).ok();

        if server_conn.is_none() {
            if let Some(c) = server.accept().into_iter().next() {
                server_conn = Some(c);
            }
        }
        if client.is_connected() && server_conn.is_some() {
            connected = true;
            break;
        }
        std::thread::sleep(STEP_SLEEP);
    }
    assert!(
        connected,
        "client failed to connect to server over UDP within {MAX_ITERS} iterations"
    );
    let server_conn = server_conn.expect("server accepted the client");

    // --- Client → server round-trip. -----------------------------------------
    let sent = connect_msg();
    client.send_reliable(client_conn, &sent);

    let mut got_on_server = Vec::new();
    for _ in 0..MAX_ITERS {
        client.update(DT).ok();
        server.update(DT).ok();
        got_on_server.extend(server.recv(server_conn));
        if !got_on_server.is_empty() {
            break;
        }
        std::thread::sleep(STEP_SLEEP);
    }
    assert!(
        got_on_server.contains(&sent),
        "server did not receive the client message over UDP; got {got_on_server:?}"
    );

    // --- Server → client round-trip. ------------------------------------------
    let reply = accept_msg();
    server.send_reliable(server_conn, &reply);

    let mut got_on_client = Vec::new();
    for _ in 0..MAX_ITERS {
        server.update(DT).ok();
        client.update(DT).ok();
        got_on_client.extend(client.recv(client_conn));
        if !got_on_client.is_empty() {
            break;
        }
        std::thread::sleep(STEP_SLEEP);
    }
    assert!(
        got_on_client.contains(&reply),
        "client did not receive the server reply over UDP; got {got_on_client:?}"
    );

    // --- Stats count application-payload bytes (T029). ------------------------
    let client_stats = client.stats(client_conn);
    let server_stats = server.stats(server_conn);
    assert_eq!(
        client_stats.bytes_out,
        sent.encode().len() as u64,
        "client bytes_out must equal the encoded payload length (no transport headers)"
    );
    assert_eq!(
        server_stats.bytes_in,
        sent.encode().len() as u64,
        "server bytes_in must equal the encoded payload length (no transport headers)"
    );
}
