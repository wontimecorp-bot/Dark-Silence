//! T030 {TR-005} — seam isolation proof.
//!
//! Asserts the [`protocol::NetTransport`] surface is library-agnostic: a single
//! generic `use_transport<T: NetTransport>` exercises BOTH the in-memory
//! [`protocol::LoopbackTransport`] and the renet-backed
//! [`protocol::RenetTransport`] through the *same* trait methods, with no renet
//! type in any signature. That it compiles for both is the compile-time proof
//! that swapping transports requires no `sim`/gameplay change (SC-006).
//!
//! Gated behind `udp` because it references `RenetTransport`; the trait-surface
//! guarantee also holds in the default build (the loopback half compiles there
//! via `roundtrip`/`loopback` tests).
#![cfg(feature = "udp")]

use protocol::{
    ConnectionId, DisconnectReason, LoopbackTransport, Message, NetStats, NetTransport,
    RenetTransport, SnapshotAck,
};
use std::net::{Ipv4Addr, UdpSocket};

/// The library-agnostic exercise. Touches every method of the trait using only
/// `protocol`/`std` types. If a renet type had leaked into the trait, this
/// generic function would fail to type-check against `LoopbackTransport`.
///
/// Driven against the **server** role of each transport (where `accept()` is
/// meaningful for both). It does NOT assert delivery (the renet transport needs
/// a driven pump and a peer for that — covered by `renet_udp.rs`); its job is to
/// prove the surface compiles and runs identically for both implementors.
fn use_transport<T: NetTransport>(t: &mut T) {
    // No connection has been established, so `accept()` drains nothing; this is
    // a pure surface exercise. A sentinel id stands in for the per-connection
    // calls — both transports treat an unknown handle as a silent no-op.
    let pending: Vec<ConnectionId> = t.accept();
    let conn = pending.first().copied().unwrap_or(ConnectionId(0));

    let msg = Message::SnapshotAck(SnapshotAck {
        last_snapshot_id: 7,
    });
    t.send_reliable(conn, &msg);
    t.send_unreliable(conn, &msg);
    let _received: Vec<Message> = t.recv(conn);
    let _stats: NetStats = t.stats(conn);
    t.disconnect(conn, DisconnectReason::ClientClosed);
}

#[test]
fn loopback_satisfies_the_library_agnostic_surface() {
    let (_client, mut server) = LoopbackTransport::pair();
    use_transport(&mut server);
}

#[test]
fn renet_satisfies_the_same_library_agnostic_surface() {
    // Bind an ephemeral local socket; a free port is assigned by the OS. This
    // does not require a peer — we only drive the trait methods, which never
    // block on a real connection.
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral udp socket");
    let server_addr = socket.local_addr().expect("socket local addr");
    let mut server =
        RenetTransport::unsecure_server(socket, server_addr).expect("build renet server");

    // Compile-time proof: the SAME generic fn accepts the renet transport.
    use_transport(&mut server);
}
