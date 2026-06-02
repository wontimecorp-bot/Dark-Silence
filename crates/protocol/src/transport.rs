//! The `NetTransport` adapter trait — the swap seam (TR-005, HINT-002).
//!
//! This trait is the *only* contract the rest of the netcode talks to. Its
//! signatures use exclusively `protocol`, `glam`, `sim`, and `std` types —
//! **never** a renet (or any other netcode-library) type. That is what lets the
//! in-memory [`crate::loopback::LoopbackTransport`] and a future renet-backed
//! adapter (Phase 4, behind the `udp` feature) be swapped without touching a
//! single consumer (SC-006).
//!
//! The trait is object-safe so a server can hold `Box<dyn NetTransport>`.

use crate::messages::{ConnectionId, Message, NetStats};
use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Why a connection was (or is being) closed. Small, stable enum; appears on the
/// wire inside [`Message::Disconnect`], so it derives serde + bitcode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub enum DisconnectReason {
    /// No traffic within the keep-alive window.
    Timeout,
    /// A malformed or unexpected message was received.
    ProtocolError,
    /// The client requested a clean disconnect.
    ClientClosed,
    /// The server is shutting the connection down (shutdown, kick, etc.).
    ServerClosed,
}

/// The transport-adapter seam. One transport instance plays either the client
/// or server role; loopback links a matched pair (see
/// [`crate::loopback::LoopbackTransport::pair`]).
///
/// Reliability is the *caller's* declared intent, mapped to the underlying
/// transport's channels: handshake and teardown go [`NetTransport::send_reliable`];
/// per-tick input and snapshots go [`NetTransport::send_unreliable`]. The
/// loopback transport delivers both losslessly and in order (a clean baseline
/// before real loss/jitter knobs arrive in a later task).
pub trait NetTransport {
    /// Client side: open a session to `endpoint` and return its local handle.
    /// For loopback the address is just a registry key into the shared switch.
    fn connect(&mut self, endpoint: SocketAddr) -> ConnectionId;

    /// Server side: return the handles of clients that connected since the last
    /// call (drains the pending-accept queue).
    fn accept(&mut self) -> Vec<ConnectionId>;

    /// Send `msg` on the reliable-ordered channel (handshake, disconnect).
    fn send_reliable(&mut self, conn: ConnectionId, msg: &Message);

    /// Send `msg` on the unreliable channel (`ClientInput`, `Snapshot`).
    fn send_unreliable(&mut self, conn: ConnectionId, msg: &Message);

    /// Drain and return all messages received on `conn` since the last call.
    fn recv(&mut self, conn: ConnectionId) -> Vec<Message>;

    /// Close `conn` with the given reason.
    fn disconnect(&mut self, conn: ConnectionId, reason: DisconnectReason);

    /// Bytes in/out for `conn` so far — the bandwidth baseline (TR-014).
    fn stats(&self, conn: ConnectionId) -> NetStats;
}
