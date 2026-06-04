//! The binary wire message catalog (TR-004) and its bit-packed codec (TR-045).
//!
//! Every type here is library-agnostic: it derives `serde` (so it composes with
//! the rest of the codebase) and `bitcode`'s `Encode`/`Decode` (so it bit-packs
//! to a compact, deterministic-width form). No renet type appears anywhere in
//! this file or its public surface (SC-006).
//!
//! Quantized fields ([`crate::quantize`]) keep snapshots small; the
//! `Message::{encode, decode}` pair (T011) round-trips any variant to an equal
//! value (asserted in `tests/roundtrip.rs`).

use crate::quantize::{QAngle, QVec2};
use crate::transport::DisconnectReason;
use bitcode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use sim::ShipIntent;

/// Size of the opaque connect token, in bytes. A fixed-width stub here; E004
/// fills it with real account/auth material. Fixed width keeps `Connect`'s
/// encoded size deterministic.
pub const CLIENT_TOKEN_BYTES: usize = 32;

/// Maximum number of redundant inputs carried in a single [`ClientInput`]
/// (TR-027). Capping the tail bounds the worst-case packet size; the newest
/// input is first so a receiver can self-heal one lost packet by replaying the
/// tail (TR-006/007).
pub const MAX_INPUT_TAIL: usize = 8;

// --- Newtypes (T007) ----------------------------------------------------------

/// Opaque per-connection handle minted by a [`crate::transport::NetTransport`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Encode, Decode)]
pub struct ConnectionId(pub u32);

/// Stable network id of a replicated entity. Distinct from a `bevy_ecs::Entity`,
/// whose generational id is runtime-local and must not cross the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Encode, Decode)]
pub struct EntityId(pub u32);

/// Which kind of replicated entity an [`EntityRecord`] describes — picks the
/// client-side prefab/interpolation behavior.
///
/// `Debris` is **additive**: appended last so the bitcode variant indices of the
/// existing variants are unchanged — the wire form of `Ship`/`Projectile`/`Target`
/// stays byte-identical. It is currently produced **only** on the client-only
/// in-process render path ([`server::ServerApp::render_state`]) for severed
/// ship-fragment chunks and a destroyed hulk (FIX 0b); the networked snapshot path
/// (`server::ServerApp::full_records`) never emits it. The roundtrip test in
/// `tests/roundtrip.rs` covers it so the wire codec stays green.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub enum EntityKind {
    /// A player/AI ship.
    Ship,
    /// A fired projectile.
    Projectile,
    /// A destructible target.
    Target,
    /// A ship-fragment debris chunk (severed wreck piece or a destroyed hulk) — the
    /// client renders it as a tinted, tumbling ship-fragment box rather than a grey
    /// asteroid sphere (FIX 0b). The chunk's residual cell-count rides in
    /// [`EntityRecord::flags`] as a size hint (clamped to `u8`).
    Debris,
}

/// Bytes sent/received on a connection so far — the bandwidth baseline (TR-014).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct NetStats {
    /// Total encoded message bytes sent on this connection.
    pub bytes_out: u64,
    /// Total encoded message bytes received on this connection.
    pub bytes_in: u64,
}

// --- Handshake / teardown messages (T008) -------------------------------------

/// c→s, reliable-ordered. Opens a session and declares the client's protocol
/// version for the server's compatibility check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct Connect {
    /// Wire protocol version; mismatches are rejected with [`RejectReason::Version`].
    pub protocol_version: u16,
    /// Opaque auth/identity token stub (E004 fills it).
    pub client_token: [u8; CLIENT_TOKEN_BYTES],
}

/// s→c, reliable-ordered. Session parameters the client uses to align its fixed
/// step and interpolation buffer (TR-024/025/026/044).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct ConnectAccepted {
    /// The id the server assigned this client's owned entity.
    pub client_id: EntityId,
    /// Server simulation tick rate (Hz) — the client matches this fixed step.
    pub tick_rate_hz: u16,
    /// Snapshot send rate (Hz).
    pub snapshot_rate_hz: u16,
    /// Interpolation delay (ms) for remote entities (TR-010).
    pub interp_delay_ms: u16,
    /// The server's current tick at accept time, so the client can phase-align.
    pub server_tick: u32,
}

/// Why a [`Connect`] was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub enum RejectReason {
    /// `protocol_version` did not match the server.
    Version,
    /// The server is at capacity.
    Full,
    /// The client/token is banned.
    Banned,
}

/// s→c, reliable-ordered. Refuses a connection with a reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct ConnectRejected {
    /// Why the connection was refused.
    pub reason: RejectReason,
}

/// Both directions, reliable-ordered. Clean session teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct Disconnect {
    /// Why the connection is closing.
    pub reason: DisconnectReason,
}

// --- Client input (T009) ------------------------------------------------------

/// A single step's pilot intent, quantized for the wire. The networked form of
/// [`sim::ShipIntent`]: analog axes are quantized to `-1..=1` (`i8` of −1/0/+1),
/// the two flags stay boolean. Convert with [`From`]/[`Into`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct QuantizedIntent {
    /// Forward (+1) / reverse (−1) thrust, quantized to −1/0/+1.
    pub forward: i8,
    /// Strafe left (+1) / right (−1), quantized to −1/0/+1.
    pub strafe: i8,
    /// Turn left (+1) / right (−1), quantized to −1/0/+1.
    pub turn: i8,
    /// Fire this step.
    pub fire: bool,
    /// Toggle flight-assist this step.
    pub toggle_assist: bool,
    /// Phase F — hold the afterburner (boost) this step.
    pub afterburner: bool,
}

/// Quantize an analog axis in `-1.0..=1.0` to the nearest of −1/0/+1.
fn quantize_axis(value: f32) -> i8 {
    if value > 0.5 {
        1
    } else if value < -0.5 {
        -1
    } else {
        0
    }
}

impl From<ShipIntent> for QuantizedIntent {
    fn from(intent: ShipIntent) -> Self {
        Self {
            forward: quantize_axis(intent.forward),
            strafe: quantize_axis(intent.strafe),
            turn: quantize_axis(intent.turn),
            fire: intent.fire,
            toggle_assist: intent.toggle_assist,
            afterburner: intent.afterburner,
        }
    }
}

impl From<QuantizedIntent> for ShipIntent {
    fn from(q: QuantizedIntent) -> Self {
        Self {
            forward: q.forward as f32,
            strafe: q.strafe as f32,
            turn: q.turn as f32,
            fire: q.fire,
            toggle_assist: q.toggle_assist,
            afterburner: q.afterburner,
        }
    }
}

/// c→s, unreliable. The latest input plus a redundant tail of recent inputs so
/// one lost packet self-heals (TR-006/007/027). `inputs[0]` is the newest; the
/// tail is capped at [`MAX_INPUT_TAIL`] — use [`ClientInput::new`], which
/// truncates, rather than constructing the field directly with an oversized vec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct ClientInput {
    /// Monotonic per-client sequence number of the newest input.
    pub seq: u32,
    /// Simulation tick the newest input applies to.
    pub tick: u32,
    /// Recent inputs, newest first, length `1..=MAX_INPUT_TAIL`.
    pub inputs: Vec<QuantizedIntent>,
}

impl ClientInput {
    /// Build a `ClientInput`, truncating the tail to [`MAX_INPUT_TAIL`]
    /// (newest-first) so the wire bound (TR-027) always holds.
    pub fn new(seq: u32, tick: u32, mut inputs: Vec<QuantizedIntent>) -> Self {
        inputs.truncate(MAX_INPUT_TAIL);
        Self { seq, tick, inputs }
    }
}

/// c→s, unreliable. Acks the newest snapshot id the client has applied, so the
/// server can delta against a known baseline (may piggyback on [`ClientInput`]
/// in a later optimization).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct SnapshotAck {
    /// Newest snapshot id the client has applied.
    pub last_snapshot_id: u16,
}

// --- Snapshot (T010) ----------------------------------------------------------

/// One entity's quantized state in a [`Snapshot`] (TR-013). Positions/velocities
/// are quantized to sector-relative bounds and heading to a fixed-bit angle
/// (see [`crate::quantize`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct EntityRecord {
    /// Network id of the entity.
    pub id: EntityId,
    /// What the entity is.
    pub kind: EntityKind,
    /// Quantized position (sector-relative).
    pub pos: QVec2,
    /// Quantized velocity.
    pub vel: QVec2,
    /// Quantized heading.
    pub heading: QAngle,
    /// Per-entity boolean flags (e.g. firing, flight-assist) — bit meanings are
    /// assigned by the gameplay layer.
    pub flags: u8,
}

/// s→c, unreliable. The authoritative world state, delta-coded against the
/// client's last-acked snapshot, carrying the per-client input ack (TR-008/013).
///
/// `baseline_id` names the snapshot id this delta was computed against, or
/// [`Snapshot::KEYFRAME_BASELINE`] for a **full keyframe** (delta-from-nothing):
/// the server emits a keyframe when the client's acked baseline is unknown or
/// unavailable, so `entities` then carries the complete world and `removed` is
/// empty. A lost ack therefore re-baselines gracefully (see
/// [`crate::delta::apply_delta`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct Snapshot {
    /// Server tick this snapshot represents.
    pub server_tick: u32,
    /// Highest [`ClientInput::seq`] the server has applied — the reconciliation
    /// anchor: the client replays inputs with `seq > acked_input_seq`.
    pub acked_input_seq: u32,
    /// Snapshot id this delta was computed against (the client's last ack).
    pub baseline_id: u16,
    /// Entities present/changed since the baseline.
    pub entities: Vec<EntityRecord>,
    /// Entities removed since the baseline.
    pub removed: Vec<EntityId>,
}

impl Snapshot {
    /// Sentinel `baseline_id` marking a **full keyframe** (delta-from-nothing):
    /// `entities` carries the complete world, `removed` is empty, and the snapshot
    /// reconstructs correctly from any baseline. Snapshot ids are minted from `1`
    /// upward (`0` is "nothing acked yet"); `u16::MAX` is reserved as this
    /// sentinel so a real baseline id can never collide with it.
    pub const KEYFRAME_BASELINE: u16 = u16::MAX;
}

// --- Message union + codec (T011) ---------------------------------------------

/// The tagged union of every wire message. Encoded bit-packed via
/// [`Message::encode`] / [`Message::decode`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub enum Message {
    /// See [`Connect`].
    Connect(Connect),
    /// See [`ConnectAccepted`].
    ConnectAccepted(ConnectAccepted),
    /// See [`ConnectRejected`].
    ConnectRejected(ConnectRejected),
    /// See [`ClientInput`].
    ClientInput(ClientInput),
    /// See [`Snapshot`].
    Snapshot(Snapshot),
    /// See [`SnapshotAck`].
    SnapshotAck(SnapshotAck),
    /// See [`Disconnect`].
    Disconnect(Disconnect),
}

/// Error decoding a [`Message`] from bytes (malformed/truncated input). Wraps
/// the underlying bitcode failure as a stable, library-agnostic error so no
/// codec type leaks into the public surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Human-readable description of why decoding failed.
    pub detail: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to decode message: {}", self.detail)
    }
}

impl std::error::Error for DecodeError {}

impl Message {
    /// Bit-pack this message to bytes (TR-045). Unchanged-entity / quantized
    /// fields stay small because bitcode packs at the bit level.
    pub fn encode(&self) -> Vec<u8> {
        bitcode::encode(self)
    }

    /// Decode a message from bytes, returning [`DecodeError`] on malformed or
    /// truncated input (never panics on bad input — fail-fast, TR validation).
    pub fn decode(bytes: &[u8]) -> Result<Message, DecodeError> {
        bitcode::decode::<Message>(bytes).map_err(|e| DecodeError {
            detail: e.to_string(),
        })
    }
}
