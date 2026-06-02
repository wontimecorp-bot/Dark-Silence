//! The renet UDP adapter (Phase 4, gated behind the `udp` feature) — the one
//! and only place a renet / renet_netcode / renetcode type may appear (SC-006,
//! HINT-002, ADR-0014).
//!
//! [`RenetTransport`] implements the library-agnostic [`crate::NetTransport`]
//! trait over a real `UdpSocket`, driving `renet`'s `RenetClient`/`RenetServer`
//! plus the `renet_netcode` transport pump. The `impl NetTransport` surface
//! exposes only `protocol` / `glam` / `sim` / `std` types — every renet symbol
//! is confined to this module body. A consumer written against the loopback
//! transport (Phase 2/3) swaps to this one with no gameplay change (proven by
//! `tests/isolation.rs`).
//!
//! ## Channel mapping (AD-006 / HINT-004, T026)
//! Two channels carry the encoded [`Message`] payloads:
//! - [`CHANNEL_RELIABLE`] — reliable-ordered. Handshake (`Connect` /
//!   `ConnectAccepted` / `ConnectRejected`) and `Disconnect`. Fed by
//!   [`NetTransport::send_reliable`].
//! - [`CHANNEL_UNRELIABLE`] — unreliable. `ClientInput` (carrying the redundant
//!   recent tail, TR-006/007), `Snapshot`, and `SnapshotAck`. Fed by
//!   [`NetTransport::send_unreliable`].
//!
//! ## Secure mode (TR-048, T027/T028)
//! [`RenetTransport::secure_server`] configures `ServerAuthentication::Secure`
//! and [`RenetTransport::secure_client`] connects with a `ConnectToken` minted
//! by a [`TokenIssuer`] (the [`StubTokenIssuer`] holds a local signing key). An
//! `Unsecure` client cannot establish against a `Secure` server, so an
//! unauthenticated connect is rejected by netcode before any payload flows.
//!
//! ## Stats (TR-005, T029)
//! [`NetStats`] counts **application-payload bytes** only — the summed encoded
//! `Message` lengths sent / received per connection. renet/UDP transport
//! headers, acks, and netcode framing are explicitly excluded (renet's own
//! per-client byte counters are available via `network_info` but are not what
//! `stats()` returns).

use crate::messages::{ConnectionId, Message, NetStats};
use crate::transport::{DisconnectReason, NetTransport};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use renet::{ChannelConfig, ConnectionConfig, RenetClient, RenetServer, SendType, ServerEvent};
use renet_netcode::{
    ClientAuthentication, ConnectToken, NetcodeClientTransport, NetcodeError,
    NetcodeServerTransport, ServerAuthentication, ServerConfig, NETCODE_KEY_BYTES,
    NETCODE_USER_DATA_BYTES,
};

// --- Channel ids (T026, AD-006 / HINT-004) -----------------------------------

/// Path-MTU baseline (bytes): an inbound payload larger than this is treated as
/// malformed and dropped at the receive boundary (TR-029/030), mirroring the
/// server's `decode_inbound` oversize guard so the two receive paths agree.
const MAX_INBOUND_PAYLOAD_BYTES: usize = 1200;

/// Reliable-ordered channel: handshake messages and `Disconnect`.
const CHANNEL_RELIABLE: u8 = 0;
/// Unreliable channel: `ClientInput`, `Snapshot`, `SnapshotAck`.
const CHANNEL_UNRELIABLE: u8 = 1;

/// How long renet waits before resending an un-acked reliable message.
const RELIABLE_RESEND: Duration = Duration::from_millis(300);

/// Per-channel memory ceiling (renet default magnitude). Reliable channels
/// disconnect when this fills; unreliable ones drop. Generous for our small
/// bit-packed messages.
const CHANNEL_MEMORY_BYTES: usize = 5 * 1024 * 1024;

/// Application protocol id — distinguishes this game/version from any other
/// netcode app on the same wire. Both ends MUST agree (it is baked into the
/// connect token). E004 may bump this per protocol revision.
pub const RENET_PROTOCOL_ID: u64 = 0x00DA_8551_1E4E_0003;

/// Default maximum concurrent clients a secure/unsecure server accepts.
const DEFAULT_MAX_CLIENTS: usize = 64;

/// Connect-token lifetime (seconds) handed to a client by the issuer.
const TOKEN_EXPIRE_SECONDS: u64 = 300;
/// Netcode per-connection idle timeout (seconds) baked into the token.
const TOKEN_TIMEOUT_SECONDS: i32 = 15;

/// The two channels both ends use. Symmetric: server and client declare the
/// same channel set so each can send and receive on both.
fn channels() -> Vec<ChannelConfig> {
    vec![
        ChannelConfig {
            channel_id: CHANNEL_RELIABLE,
            max_memory_usage_bytes: CHANNEL_MEMORY_BYTES,
            send_type: SendType::ReliableOrdered {
                resend_time: RELIABLE_RESEND,
            },
        },
        ChannelConfig {
            channel_id: CHANNEL_UNRELIABLE,
            max_memory_usage_bytes: CHANNEL_MEMORY_BYTES,
            send_type: SendType::Unreliable,
        },
    ]
}

/// Build the `ConnectionConfig` shared by both roles (same channel set in both
/// directions).
fn connection_config() -> ConnectionConfig {
    ConnectionConfig {
        available_bytes_per_tick: 60_000,
        server_channels_config: channels(),
        client_channels_config: channels(),
    }
}

/// Wall-clock duration since the Unix epoch — netcode timestamps connect tokens
/// and drives expiry. Falls back to `ZERO` only if the system clock predates the
/// epoch (never in practice), keeping this infallible for the transport pump.
fn now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

// --- Token issuance seam (T027, TR-048) ---------------------------------------

/// Mints `renet_netcode` connect tokens for secure client connections. Kept
/// behind a trait so E004 can drop in account-backed issuance (real auth, user
/// data, ban checks) **without touching the secure-connection path** in
/// [`RenetTransport`]. The returned `ConnectToken` is a renet_netcode type, so
/// implementors of this trait necessarily live alongside the adapter (the type
/// never escapes the `udp`-gated module).
pub trait TokenIssuer {
    /// Issue a connect token authorizing `client_id` to reach `server_addr`.
    /// `Err` means issuance failed (bad inputs / crypto error); the caller
    /// surfaces it as a connect failure rather than panicking.
    fn issue(&self, client_id: u64, server_addr: SocketAddr) -> Result<ConnectToken, NetcodeError>;
}

/// A local, self-contained [`TokenIssuer`] holding a signing **private key** in
/// process — the development/stub issuer (T027). It signs tokens with the same
/// key the secure server is configured with, so tokens it mints are accepted by
/// that server and nothing else. E004 replaces this with an account service
/// that validates identity before issuing; the secure-connection code below is
/// unchanged by that swap because it only ever sees a `ConnectToken`.
pub struct StubTokenIssuer {
    /// The shared secret. The matching secure server is built from the same key
    /// (see [`StubTokenIssuer::private_key`] / [`RenetTransport::secure_server`]).
    private_key: [u8; NETCODE_KEY_BYTES],
    /// Application protocol id baked into every issued token.
    protocol_id: u64,
}

impl StubTokenIssuer {
    /// Create an issuer with a freshly generated random signing key and the
    /// default [`RENET_PROTOCOL_ID`]. Pair it with a server built from
    /// [`StubTokenIssuer::private_key`].
    pub fn new() -> Self {
        Self {
            private_key: renet_netcode::generate_random_bytes(),
            protocol_id: RENET_PROTOCOL_ID,
        }
    }

    /// Create an issuer from an explicit signing key and protocol id — used when
    /// the caller wants the server and issuer to share a known key.
    pub fn with_key(private_key: [u8; NETCODE_KEY_BYTES], protocol_id: u64) -> Self {
        Self {
            private_key,
            protocol_id,
        }
    }

    /// The signing key. A secure server MUST be constructed with this exact key
    /// (`RenetTransport::secure_server`) for this issuer's tokens to be accepted.
    pub fn private_key(&self) -> [u8; NETCODE_KEY_BYTES] {
        self.private_key
    }

    /// The protocol id this issuer stamps into tokens (and the server must use).
    pub fn protocol_id(&self) -> u64 {
        self.protocol_id
    }
}

impl Default for StubTokenIssuer {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenIssuer for StubTokenIssuer {
    fn issue(&self, client_id: u64, server_addr: SocketAddr) -> Result<ConnectToken, NetcodeError> {
        // No per-client user data in the stub (E004 will fill it from the
        // account record). Empty user data keeps the token deterministic-width.
        let user_data = [0u8; NETCODE_USER_DATA_BYTES];
        ConnectToken::generate(
            now(),
            self.protocol_id,
            TOKEN_EXPIRE_SECONDS,
            client_id,
            TOKEN_TIMEOUT_SECONDS,
            vec![server_addr],
            Some(&user_data),
            &self.private_key,
        )
        .map_err(NetcodeError::from)
    }
}

// --- Per-connection payload-byte bookkeeping (T029, TR-005) -------------------

/// Application-payload byte counters for one connection. Counts only encoded
/// `Message` bytes; renet/UDP/netcode framing is excluded by construction
/// because we credit these at the `send_*` / `recv` boundary, never from
/// renet's packet counters.
#[derive(Clone, Copy, Debug, Default)]
struct PayloadStats {
    bytes_out: u64,
    bytes_in: u64,
}

// --- Role state ---------------------------------------------------------------

/// Server-role internals: the renet server plus the netcode UDP transport and
/// the connection-id ↔ renet `ClientId` mapping.
struct ServerState {
    server: RenetServer,
    transport: NetcodeServerTransport,
    /// Maps our [`ConnectionId`] (the public handle) to renet's `ClientId`.
    conn_to_client: HashMap<u32, u64>,
    /// Reverse map so a netcode connect event resolves to a stable handle.
    client_to_conn: HashMap<u64, u32>,
    /// Per-connection payload byte counters (keyed by our connection id).
    stats: HashMap<u32, PayloadStats>,
    /// Connections established since the last `accept()` drain.
    pending_accept: Vec<ConnectionId>,
    /// Monotonic source of [`ConnectionId`] values.
    next_conn: u32,
}

/// Client-role internals: the renet client plus the netcode UDP transport. A
/// client has exactly one connection (to its server).
struct ClientState {
    client: RenetClient,
    transport: NetcodeClientTransport,
    /// The single connection handle minted for this client.
    conn: ConnectionId,
    /// Payload byte counters for the one connection.
    stats: PayloadStats,
    /// `true` once `connect()` has been called and the handle is live.
    connected_handle: bool,
}

/// Either role of the renet adapter. Both states embed a fixed-size netcode
/// packet buffer; the client state is the larger of the two, so it is boxed to
/// keep the enum compact (and to silence `clippy::large_enum_variant`).
enum Role {
    Server(Box<ServerState>),
    Client(Box<ClientState>),
}

/// A renet + renet_netcode UDP transport implementing [`NetTransport`]. One
/// instance is either a server or a client. Construct with
/// [`RenetTransport::unsecure_server`] / [`RenetTransport::unsecure_client`]
/// (prototyping) or the secure variants ([`RenetTransport::secure_server`] /
/// [`RenetTransport::secure_client`], TR-048).
///
/// Every method below speaks only `protocol`/`glam`/`sim`/`std` types; the
/// renet machinery lives entirely in [`Role`].
pub struct RenetTransport {
    role: Role,
}

impl RenetTransport {
    /// Server role, **unsecure** netcode (prototyping/testing only — no
    /// encryption or token auth). Binds `socket`; `public_addr` is the address
    /// advertised to clients (usually `socket.local_addr()`).
    pub fn unsecure_server(
        socket: UdpSocket,
        public_addr: SocketAddr,
    ) -> Result<Self, NetcodeError> {
        Self::build_server(
            socket,
            public_addr,
            ServerAuthentication::Unsecure,
            RENET_PROTOCOL_ID,
        )
    }

    /// Server role, **secure** netcode (TR-048): clients must present a valid
    /// `ConnectToken` signed with `private_key`. An `Unsecure` client cannot
    /// establish. `private_key` MUST match the [`StubTokenIssuer`] (or E004
    /// issuer) that mints client tokens, and `protocol_id` MUST match too.
    pub fn secure_server(
        socket: UdpSocket,
        public_addr: SocketAddr,
        private_key: [u8; NETCODE_KEY_BYTES],
        protocol_id: u64,
    ) -> Result<Self, NetcodeError> {
        Self::build_server(
            socket,
            public_addr,
            ServerAuthentication::Secure { private_key },
            protocol_id,
        )
    }

    /// Shared server construction for both auth modes.
    fn build_server(
        socket: UdpSocket,
        public_addr: SocketAddr,
        authentication: ServerAuthentication,
        protocol_id: u64,
    ) -> Result<Self, NetcodeError> {
        let server_config = ServerConfig {
            current_time: now(),
            max_clients: DEFAULT_MAX_CLIENTS,
            protocol_id,
            public_addresses: vec![public_addr],
            authentication,
        };
        let transport = NetcodeServerTransport::new(server_config, socket)?;
        let server = RenetServer::new(connection_config());
        Ok(Self {
            role: Role::Server(Box::new(ServerState {
                server,
                transport,
                conn_to_client: HashMap::new(),
                client_to_conn: HashMap::new(),
                stats: HashMap::new(),
                pending_accept: Vec::new(),
                next_conn: 0,
            })),
        })
    }

    /// Client role, **unsecure** netcode (prototyping/testing only). Binds
    /// `socket` and targets `server_addr` with the given `client_id`.
    pub fn unsecure_client(
        socket: UdpSocket,
        server_addr: SocketAddr,
        client_id: u64,
    ) -> Result<Self, NetcodeError> {
        let authentication = ClientAuthentication::Unsecure {
            protocol_id: RENET_PROTOCOL_ID,
            client_id,
            server_addr,
            user_data: None,
        };
        Self::build_client(socket, authentication)
    }

    /// Client role, **secure** netcode (TR-048): connect with a `ConnectToken`
    /// minted by a [`TokenIssuer`] (e.g. [`StubTokenIssuer`]) for `client_id`
    /// and `server_addr`. The token's signing key/protocol id must match the
    /// secure server's. This is the only secure-connect entry point, so E004
    /// can swap the issuer behind [`TokenIssuer`] without changing it.
    pub fn secure_client(
        socket: UdpSocket,
        server_addr: SocketAddr,
        client_id: u64,
        issuer: &dyn TokenIssuer,
    ) -> Result<Self, NetcodeError> {
        let connect_token = issuer.issue(client_id, server_addr)?;
        let authentication = ClientAuthentication::Secure { connect_token };
        Self::build_client(socket, authentication)
    }

    /// Shared client construction for both auth modes.
    fn build_client(
        socket: UdpSocket,
        authentication: ClientAuthentication,
    ) -> Result<Self, NetcodeError> {
        let transport = NetcodeClientTransport::new(now(), authentication, socket)?;
        let client = RenetClient::new(connection_config());
        Ok(Self {
            role: Role::Client(Box::new(ClientState {
                client,
                transport,
                // Placeholder; replaced on `connect()`.
                conn: ConnectionId(0),
                stats: PayloadStats::default(),
                connected_handle: false,
            })),
        })
    }

    /// Whether the (client-role) underlying renet connection is established.
    /// Server role always returns `false` (it tracks per-client state instead).
    pub fn is_connected(&self) -> bool {
        match &self.role {
            Role::Client(c) => c.client.is_connected(),
            Role::Server(_) => false,
        }
    }

    /// Drive the transport one step: advance renet + the netcode pump by `dt`,
    /// pull inbound UDP packets, and flush outbound ones. MUST be called every
    /// tick by the owning loop (the loopback transport needs no equivalent
    /// because it is synchronous). Returns the netcode transport error if the
    /// pump fails this step (e.g. the client got disconnected); the caller may
    /// log or react. Server-side per-packet errors are handled internally by
    /// the netcode transport and do not abort the step.
    pub fn update(&mut self, dt: Duration) -> Result<(), NetcodeError> {
        match &mut self.role {
            Role::Server(s) => {
                s.server.update(dt);
                // The netcode transport pumps the socket (recv) and applies
                // packets to the renet server. Per-client send/IO errors are
                // logged inside the transport, not returned; only a fatal
                // transport error bubbles up.
                s.transport
                    .update(dt, &mut s.server)
                    .map_err(transport_to_netcode)?;

                // Surface newly connected/disconnected clients as accept/teardown.
                while let Some(event) = s.server.get_event() {
                    match event {
                        ServerEvent::ClientConnected { client_id } => {
                            s.register_client(client_id);
                        }
                        ServerEvent::ClientDisconnected { client_id, .. } => {
                            s.unregister_client(client_id);
                        }
                    }
                }

                s.transport.send_packets(&mut s.server);
                Ok(())
            }
            Role::Client(c) => {
                c.client.update(dt);
                let update_res = c
                    .transport
                    .update(dt, &mut c.client)
                    .map_err(transport_to_netcode);
                // Always attempt to flush; send_packets is a no-op if discon-
                // nected and returns a benign error we fold into update_res.
                if c.client.is_connected() {
                    let _ = c.transport.send_packets(&mut c.client);
                }
                update_res
            }
        }
    }
}

/// Collapse a `renet_netcode::NetcodeTransportError` into a `NetcodeError`. The
/// transport error wraps netcode/renet/io variants; we map them to the
/// netcode-level error so no renet_netcode type escapes a public signature.
fn transport_to_netcode(err: renet_netcode::NetcodeTransportError) -> NetcodeError {
    match err {
        renet_netcode::NetcodeTransportError::Netcode(e) => e,
        renet_netcode::NetcodeTransportError::IO(e) => NetcodeError::IoError(e),
        // A renet-level disconnect is surfaced as a generic netcode error.
        renet_netcode::NetcodeTransportError::Renet(_) => NetcodeError::ClientNotConnected,
    }
}

impl ServerState {
    /// Mint (or fetch) a [`ConnectionId`] for a renet `ClientId`, recording the
    /// mapping and enqueueing it for `accept()`.
    fn register_client(&mut self, client_id: u64) {
        if self.client_to_conn.contains_key(&client_id) {
            return;
        }
        let conn = self.next_conn;
        self.next_conn += 1;
        self.conn_to_client.insert(conn, client_id);
        self.client_to_conn.insert(client_id, conn);
        self.stats.insert(conn, PayloadStats::default());
        self.pending_accept.push(ConnectionId(conn));
    }

    /// Drop the mapping for a disconnected client. Stats are retained so a
    /// final `stats()` query still works until the handle is forgotten.
    fn unregister_client(&mut self, client_id: u64) {
        if let Some(conn) = self.client_to_conn.remove(&client_id) {
            self.conn_to_client.remove(&conn);
            // Keep `stats[conn]` for post-mortem queries; it is harmless.
        }
    }
}

impl NetTransport for RenetTransport {
    fn connect(&mut self, _endpoint: SocketAddr) -> ConnectionId {
        // The netcode target was already fixed at construction (the connect
        // token / unsecure auth carries the server address), so `endpoint` is
        // advisory here — unlike loopback, a renet client cannot retarget after
        // construction. We mint and return the single client handle.
        match &mut self.role {
            Role::Client(c) => {
                if !c.connected_handle {
                    c.conn = ConnectionId(0);
                    c.connected_handle = true;
                }
                c.conn
            }
            Role::Server(_) => {
                // A server does not initiate connections; return a sentinel.
                // (Calling `connect` on the server role is a misuse; the
                // loopback transport has the same server-vs-client split.)
                ConnectionId(u32::MAX)
            }
        }
    }

    fn accept(&mut self) -> Vec<ConnectionId> {
        match &mut self.role {
            Role::Server(s) => std::mem::take(&mut s.pending_accept),
            Role::Client(_) => Vec::new(),
        }
    }

    fn send_reliable(&mut self, conn: ConnectionId, msg: &Message) {
        self.send_on(conn, msg, CHANNEL_RELIABLE);
    }

    fn send_unreliable(&mut self, conn: ConnectionId, msg: &Message) {
        self.send_on(conn, msg, CHANNEL_UNRELIABLE);
    }

    fn recv(&mut self, conn: ConnectionId) -> Vec<Message> {
        let mut out = Vec::new();
        match &mut self.role {
            Role::Server(s) => {
                let Some(&client_id) = s.conn_to_client.get(&conn.0) else {
                    return out;
                };
                let mut bytes_in: u64 = 0;
                // Drain both channels; order across channels is not guaranteed
                // by a real transport, which is fine — reliability is per
                // channel, and the gameplay layer tags messages by variant.
                for channel in [CHANNEL_RELIABLE, CHANNEL_UNRELIABLE] {
                    while let Some(payload) = s.server.receive_message(client_id, channel) {
                        bytes_in += payload.len() as u64;
                        // Route through the same malformed/oversize guard the
                        // server's `decode_inbound` applies (TR-029/030, T056):
                        // an over-MTU or undecodable payload is dropped here so
                        // malformed bytes never reach the gameplay layer and never
                        // panic. We still counted its wire bytes above (matching
                        // loopback's "bytes that arrived" accounting).
                        if payload.len() <= MAX_INBOUND_PAYLOAD_BYTES {
                            if let Ok(message) = Message::decode(&payload) {
                                out.push(message);
                            }
                        }
                    }
                }
                if let Some(stat) = s.stats.get_mut(&conn.0) {
                    stat.bytes_in += bytes_in;
                }
            }
            Role::Client(c) => {
                if conn != c.conn {
                    return out;
                }
                let mut bytes_in: u64 = 0;
                for channel in [CHANNEL_RELIABLE, CHANNEL_UNRELIABLE] {
                    while let Some(payload) = c.client.receive_message(channel) {
                        bytes_in += payload.len() as u64;
                        // Same malformed/oversize guard as the server path
                        // (TR-029/030, T056): oversize or undecodable is dropped.
                        if payload.len() <= MAX_INBOUND_PAYLOAD_BYTES {
                            if let Ok(message) = Message::decode(&payload) {
                                out.push(message);
                            }
                        }
                    }
                }
                c.stats.bytes_in += bytes_in;
            }
        }
        out
    }

    fn disconnect(&mut self, conn: ConnectionId, _reason: DisconnectReason) {
        match &mut self.role {
            Role::Server(s) => {
                if let Some(&client_id) = s.conn_to_client.get(&conn.0) {
                    s.server.disconnect(client_id);
                    // The netcode transport flushes the disconnect packet on
                    // the next `update()`; we drop our mapping eagerly so no
                    // further sends route to this handle.
                    s.unregister_client(client_id);
                }
            }
            Role::Client(c) => {
                if conn == c.conn {
                    c.client.disconnect();
                    c.transport.disconnect();
                }
            }
        }
    }

    fn stats(&self, conn: ConnectionId) -> NetStats {
        match &self.role {
            Role::Server(s) => s
                .stats
                .get(&conn.0)
                .map(|st| NetStats {
                    bytes_out: st.bytes_out,
                    bytes_in: st.bytes_in,
                })
                .unwrap_or_default(),
            Role::Client(c) => {
                if conn == c.conn {
                    NetStats {
                        bytes_out: c.stats.bytes_out,
                        bytes_in: c.stats.bytes_in,
                    }
                } else {
                    NetStats::default()
                }
            }
        }
    }
}

impl RenetTransport {
    /// Shared send path: encode `msg`, push it onto `channel`, and credit the
    /// connection's outbound payload-byte counter (T029 — payload bytes only).
    fn send_on(&mut self, conn: ConnectionId, msg: &Message, channel: u8) {
        let payload = msg.encode();
        let bytes = payload.len() as u64;
        match &mut self.role {
            Role::Server(s) => {
                let Some(&client_id) = s.conn_to_client.get(&conn.0) else {
                    // Unknown / torn-down handle: drop silently (matches loopback).
                    return;
                };
                s.server.send_message(client_id, channel, payload);
                if let Some(stat) = s.stats.get_mut(&conn.0) {
                    stat.bytes_out += bytes;
                }
            }
            Role::Client(c) => {
                if conn != c.conn {
                    return;
                }
                c.client.send_message(channel, payload);
                c.stats.bytes_out += bytes;
            }
        }
    }
}
