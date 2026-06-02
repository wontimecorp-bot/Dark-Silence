//! T032 {TR-048} [COMPLETES TR-048] — secure (authenticated + encrypted)
//! connect, and rejection of an unauthenticated connect.
//!
//! - A client holding a valid [`protocol::StubTokenIssuer`] connect-token
//!   establishes a **secure** session against a `Secure` server and a message
//!   flows.
//! - An `Unsecure` client (no token, no shared key) attempting the same
//!   `Secure` server **never establishes** — netcode rejects it before any
//!   payload can flow.
//!
//! Gated behind `udp` (references `RenetTransport`/`StubTokenIssuer`).
#![cfg(feature = "udp")]

use protocol::{
    Connect, Message, NetTransport, RenetTransport, StubTokenIssuer, CLIENT_TOKEN_BYTES,
    RENET_PROTOCOL_ID,
};
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

const DT: Duration = Duration::from_millis(16);
const MAX_ITERS: usize = 1000;
const STEP_SLEEP: Duration = Duration::from_millis(2);

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: 1,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// Build a secure server bound to an ephemeral 127.0.0.1 port, keyed by the
/// issuer's signing key. Returns `(server, server_addr, issuer)`.
fn secure_server() -> (RenetTransport, std::net::SocketAddr, StubTokenIssuer) {
    let issuer = StubTokenIssuer::new();
    let socket =
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind secure server socket on 127.0.0.1");
    let addr = socket.local_addr().expect("server socket local addr");
    let server =
        RenetTransport::secure_server(socket, addr, issuer.private_key(), issuer.protocol_id())
            .expect("build secure server");
    (server, addr, issuer)
}

#[test]
fn tokened_client_establishes_secure_session_and_message_flows() {
    let (mut server, server_addr, issuer) = secure_server();

    let client_socket =
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind secure client socket");
    let mut client = RenetTransport::secure_client(client_socket, server_addr, 7, &issuer)
        .expect("build secure client with issuer token");
    let client_conn = client.connect(server_addr);

    // Drive until the secure handshake completes.
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
        "secure client failed to establish within {MAX_ITERS} iterations"
    );
    let server_conn = server_conn.expect("server accepted the secure client");

    // A message flows over the encrypted session.
    let sent = connect_msg();
    client.send_reliable(client_conn, &sent);
    let mut got = Vec::new();
    for _ in 0..MAX_ITERS {
        client.update(DT).ok();
        server.update(DT).ok();
        got.extend(server.recv(server_conn));
        if !got.is_empty() {
            break;
        }
        std::thread::sleep(STEP_SLEEP);
    }
    assert!(
        got.contains(&sent),
        "secure session did not deliver the message; got {got:?}"
    );
}

#[test]
fn unsecure_client_is_rejected_by_secure_server() {
    let (mut server, server_addr, _issuer) = secure_server();

    // An UNSECURE client: no connect token, no shared signing key. Against a
    // Secure server it must never establish (netcode denies it). It also uses
    // the default protocol id, but the lack of a valid token is the decisive
    // rejection.
    let client_socket =
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind unsecure client socket");
    let mut client = RenetTransport::unsecure_client(client_socket, server_addr, 13)
        .expect("build unsecure client");
    let _client_conn = client.connect(server_addr);

    let _ = RENET_PROTOCOL_ID; // documents the shared protocol id is irrelevant to the rejection.

    // Drive a full window. The client must NOT become connected and the server
    // must NOT accept any connection.
    let mut server_accepted = false;
    for _ in 0..MAX_ITERS {
        client.update(DT).ok();
        server.update(DT).ok();
        if !server.accept().is_empty() {
            server_accepted = true;
            break;
        }
        if client.is_connected() {
            break;
        }
        std::thread::sleep(STEP_SLEEP);
    }

    assert!(
        !client.is_connected(),
        "an unsecure client must NOT establish a session with a secure server"
    );
    assert!(
        !server_accepted,
        "a secure server must NOT accept an unauthenticated/unsecure connect"
    );
}
