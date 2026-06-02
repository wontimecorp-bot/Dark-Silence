//! E003 protocol crate — the library-agnostic netcode seam.
//!
//! Holds the binary wire messages ([`messages`]), the [`NetTransport`] adapter
//! trait (the swap seam, [`transport`]), bit-packed quantization ([`quantize`]),
//! and an in-memory [`loopback`] transport. No renet type appears in the public
//! surface (SC-006); the renet UDP adapter is gated behind the `udp` feature so
//! loopback can be proven first (HINT-001/002, ADR-0014). Phase 4 adds that
//! adapter behind `#[cfg(feature = "udp")]`.

pub mod delta;
pub mod loopback;
pub mod messages;
pub mod quantize;
pub mod transport;

// The renet UDP adapter is the ONLY module that may reference a renet /
// renet_netcode type, and it exists only when the `udp` feature is on (HINT-002,
// SC-006, ADR-0014). Default builds compile with no renet present.
#[cfg(feature = "udp")]
mod renet_adapter;

// --- Public surface re-exports ------------------------------------------------
// Downstream phases consume these directly from the crate root. Only
// `protocol`/`glam`/`sim`/`std` types appear here — never a renet type.

pub use delta::{apply_delta, snapshot_wire_id, FullState};
pub use loopback::{LoopbackTransport, LossJitterConfig};
pub use messages::{
    ClientInput, Connect, ConnectAccepted, ConnectRejected, ConnectionId, DecodeError, Disconnect,
    EntityId, EntityKind, EntityRecord, Message, NetStats, QuantizedIntent, RejectReason, Snapshot,
    SnapshotAck, CLIENT_TOKEN_BYTES, MAX_INPUT_TAIL,
};
pub use quantize::{
    QAngle, QVec2, ANGLE_BITS, ANGLE_TOLERANCE, POS_BITS, POS_RANGE, POS_TOLERANCE, VEL_BITS,
    VEL_RANGE, VEL_TOLERANCE,
};
pub use transport::{DisconnectReason, NetTransport};

// The renet adapter exports (Phase 4) — gated so the default surface never
// mentions renet. The exported types' public method signatures use only
// `protocol`/`glam`/`sim`/`std` types (SC-006); the `TokenIssuer::issue`
// return type is a renet_netcode `ConnectToken`, which is why that seam lives
// entirely behind the `udp` feature.
#[cfg(feature = "udp")]
pub use renet_adapter::{RenetTransport, StubTokenIssuer, TokenIssuer, RENET_PROTOCOL_ID};
