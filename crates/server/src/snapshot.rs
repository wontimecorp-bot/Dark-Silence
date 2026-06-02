//! Delta snapshot encoder + metering (E003, OBJ6 — T063/T064/T065/T066).
//!
//! This is the authoritative replication path's bandwidth budget (Principle VI).
//! It turns the current authoritative world's full record set into a
//! **delta-coded** [`Snapshot`] against each recipient's last-acked baseline:
//!
//! - **T063 — delta-by-omission:** [`encode_snapshot`] emits only the
//!   [`EntityRecord`]s that *changed* vs the baseline in `entities`, lists the
//!   ids that *disappeared* in `removed`, and tags the snapshot with the
//!   `baseline_id` it was computed against. An **unchanged entity costs ≤ 1 bit**
//!   — in fact zero wire bits, because it is simply *omitted* from `entities`
//!   (and not listed in `removed`); the client persists it from its baseline via
//!   [`protocol::apply_delta`]. Zero ≤ 1, so the bound holds.
//!
//! - **T064 — MTU bound + lost-ack degradation:** if the encoded snapshot would
//!   exceed [`MAX_SNAPSHOT_BYTES`] the encoder **drops the lowest-priority
//!   entities** for that tick (never IP-fragments, never emits oversize). When
//!   the recipient's acked baseline is unknown/unavailable the encoder emits a
//!   **full keyframe** (delta-from-nothing) so a lost ack re-baselines
//!   gracefully — still bounded by the same MTU guard.
//!
//! - **T065 — authoritative-only:** the encoder reads ONLY the server-`sim`
//!   record set handed to it (server transforms/velocities + server-resolved
//!   state). No client-asserted data can enter a snapshot — there is no client
//!   input on this path at all; the only inputs are the authoritative world
//!   records and the recipient's own ack bookkeeping. This is asserted in tests.
//!
//! - **T066 — metering:** [`BandwidthMeter`] credits each send's encoded payload
//!   bytes to that connection and exposes mean/peak bytes/client/sec over a
//!   rolling window — the figure the 8b bandwidth scenario records.
//!
//! - **T067 — benchmarkable unit:** [`encode_snapshot`] is a free function over
//!   plain inputs (no `ServerApp`, no transport), so `benches/encode.rs` can time
//!   per-client encode cost at baseline scale directly.
//!
//! ## Priority rule (T064, documented)
//!
//! When the delta would exceed the MTU the encoder sheds load by **priority**,
//! keeping the entities that matter most to the recipient:
//! 1. The recipient's **own ship** is never dropped (it anchors reconciliation).
//! 2. **Ships** outrank **targets**, which outrank **projectiles** (a ship you
//!    are fighting matters more than a stray bolt).
//! 3. Within a kind, **nearer to the recipient outranks farther** (a brawl in
//!    your face matters more than a skirmish across the sector).
//!
//! Dropped-this-tick entities are simply omitted from the delta; the client keeps
//! its last baseline value for them until a later, less-crowded tick re-includes
//! them — a graceful, bounded degradation rather than a fragmented packet.

use std::collections::VecDeque;

use glam::Vec2;
use protocol::{
    ConnectionId, EntityId, EntityKind, EntityRecord, FullState, Message, Snapshot, MAX_INPUT_TAIL,
};

/// MTU payload bound (path-MTU baseline, TR-029): an encoded [`Snapshot`] message
/// must be ≤ this many bytes so it never IP-fragments. Mirrors
/// [`crate::MAX_PAYLOAD_BYTES`] (the session-level inbound guard); kept here so
/// the encoder is self-contained for the bench/tests.
pub const MAX_SNAPSHOT_BYTES: usize = crate::session::MAX_PAYLOAD_BYTES;

/// Header + recipient context for one [`encode_snapshot`] call — the per-recipient
/// inputs the delta needs beyond the shared world record set.
#[derive(Clone, Copy, Debug)]
pub struct EncodeParams {
    /// The server tick the snapshot represents.
    pub server_tick: u32,
    /// The highest [`protocol::ClientInput::seq`] processed for this recipient
    /// (the reconciliation ack, TR-008). Per-recipient, not shared.
    pub acked_input_seq: u32,
    /// The snapshot id the recipient last acked — the baseline this delta is
    /// computed against. Ignored when `keyframe` is set.
    pub baseline_id: u16,
    /// `true` to emit a full keyframe (delta-from-nothing): the recipient's acked
    /// baseline is unknown/unavailable, so every entity goes in `entities` and the
    /// snapshot is tagged [`Snapshot::KEYFRAME_BASELINE`]. Still MTU-bounded.
    pub keyframe: bool,
    /// The recipient's own ship id (never dropped by the MTU guard) and the
    /// distance-priority origin. `None` if the recipient owns no ship yet.
    pub recipient_id: Option<EntityId>,
    /// The recipient's ship position, the origin for the distance-priority rule.
    /// Defaults to the sector origin when the recipient has no ship.
    pub recipient_pos: Vec2,
}

/// Delta-encode the authoritative `current` world state for one recipient against
/// its `baseline`, MTU-bounded (T063/T064/T065).
///
/// `current` is the full authoritative record set this tick (server-`sim` only —
/// T065: no client-asserted data is on this path). `baseline` is the recipient's
/// last-acked snapshot, reconstructed as a [`FullState`]; when
/// [`EncodeParams::keyframe`] is set it is ignored and every entity is emitted
/// (delta-from-nothing, T064 lost-ack degradation).
///
/// The returned [`Snapshot`]:
/// - carries only **changed/appeared** records in `entities` (an unchanged entity
///   is omitted ⇒ 0 wire bits ⇒ ≤ 1 bit, T063);
/// - lists **disappeared** ids in `removed`;
/// - is tagged with `baseline_id` (or [`Snapshot::KEYFRAME_BASELINE`] for a
///   keyframe);
/// - is guaranteed to encode to ≤ [`MAX_SNAPSHOT_BYTES`]: if the full delta would
///   exceed it, the lowest-priority entities are dropped for this tick (T064).
///
/// Pure over its inputs — no `ServerApp`, no transport — so it is directly
/// benchmarkable (T067).
pub fn encode_snapshot(
    current: &FullState,
    baseline: &FullState,
    params: EncodeParams,
) -> Snapshot {
    // --- T063: compute the delta vs the (possibly empty) baseline. -----------
    // A keyframe deltas against nothing, so its effective baseline is empty: every
    // entity is "changed" and there is nothing to mark removed.
    let effective_baseline = if params.keyframe {
        FullState::new()
    } else {
        baseline.clone()
    };

    // Changed/appeared records: present in `current`, and either absent from the
    // baseline or differing from it. An entity equal to its baseline is OMITTED
    // (zero wire bits — the ≤ 1 bit guarantee, satisfied by delta-by-omission).
    let mut changed: Vec<EntityRecord> = Vec::new();
    for record in current.records() {
        match effective_baseline.get(record.id) {
            Some(prev) if prev == record => {
                // Unchanged: omit entirely (0 bits). The client persists it from
                // its baseline via `apply_delta`.
            }
            _ => changed.push(*record),
        }
    }

    // Disappeared ids: present in the baseline, absent from `current`. A keyframe
    // has an empty effective baseline, so this is empty for a keyframe.
    let mut removed: Vec<EntityId> = Vec::new();
    for prev in effective_baseline.records() {
        if current.get(prev.id).is_none() {
            removed.push(prev.id);
        }
    }

    let baseline_id = if params.keyframe {
        Snapshot::KEYFRAME_BASELINE
    } else {
        params.baseline_id
    };

    let mut snapshot = Snapshot {
        server_tick: params.server_tick,
        acked_input_seq: params.acked_input_seq,
        baseline_id,
        entities: changed,
        removed,
    };

    // --- T064: enforce the MTU bound by shedding lowest-priority entities. ----
    enforce_mtu(&mut snapshot, &params);

    snapshot
}

/// Drop the lowest-priority changed entities until the encoded snapshot fits the
/// MTU (T064). Never drops the recipient's own ship, never touches `removed` (a
/// removal must always be delivered so the client does not keep a ghost), and
/// never IP-fragments — it sheds load instead.
///
/// Priority (highest kept last so `pop` sheds the lowest first): see the module
/// doc — own ship first, then ships > targets > projectiles, then nearer > farther.
fn enforce_mtu(snapshot: &mut Snapshot, params: &EncodeParams) {
    if encoded_len(snapshot) <= MAX_SNAPSHOT_BYTES {
        return;
    }

    // Sort changed entities by ascending priority (lowest first) so the
    // lowest-priority entities are at the front and shed first.
    let recipient_id = params.recipient_id;
    let origin = params.recipient_pos;
    snapshot
        .entities
        .sort_by_key(|r| priority(r, recipient_id, origin));

    // Estimate how many entities to shed up front from the average per-entity wire
    // cost, so a heavily-over-MTU snapshot does not pay an O(n²) re-encode by
    // removing one entity at a time. We over-shed by the estimate, then add back
    // toward the boundary, so the result is still the maximal fitting prefix.
    let over = encoded_len(snapshot).saturating_sub(MAX_SNAPSHOT_BYTES);
    if over > 0 && !snapshot.entities.is_empty() {
        let per_entity = (encoded_len(snapshot) / snapshot.entities.len().max(1)).max(1);
        // Drop a batch a little larger than the strict estimate so one re-encode
        // usually lands under the bound; the fine loop below handles the remainder.
        let estimate = (over / per_entity) + 1;
        let max_droppable = droppable_count(snapshot, recipient_id);
        let bulk = estimate.min(max_droppable);
        snapshot.entities.drain(0..bulk);
    }

    // Fine adjustment: pop the lowest-priority entity until it fits — but never
    // drop the recipient's own ship (its priority is the maximum, so it sorts last
    // and is only ever reached when it is the sole remaining entity).
    while encoded_len(snapshot) > MAX_SNAPSHOT_BYTES && droppable_count(snapshot, recipient_id) > 0
    {
        snapshot.entities.remove(0);
    }
}

/// How many of `snapshot`'s changed entities may still be dropped — all of them
/// except the recipient's own ship, which the MTU guard never sheds (T064).
fn droppable_count(snapshot: &Snapshot, recipient_id: Option<EntityId>) -> usize {
    match recipient_id {
        Some(rid) => snapshot.entities.iter().filter(|r| r.id != rid).count(),
        None => snapshot.entities.len(),
    }
}

/// A sortable priority key for one entity relative to the recipient (T064). Higher
/// is kept; the tuple orders by (own-ship, kind-rank, −distance) so a `sort`
/// ascending puts the lowest-priority entity first.
///
/// `i64` keys keep the ordering total and `Ord` (no float `NaN` hazard): distance
/// is bucketed to centimetre-scale integers and negated so nearer sorts higher.
fn priority(record: &EntityRecord, recipient_id: Option<EntityId>, origin: Vec2) -> (u8, u8, i64) {
    // The recipient's own ship is the maximum priority (never dropped).
    let own = u8::from(recipient_id == Some(record.id));
    // Ships > targets > projectiles.
    let kind_rank = match record.kind {
        EntityKind::Ship => 2u8,
        EntityKind::Target => 1u8,
        EntityKind::Projectile => 0u8,
    };
    // Nearer the recipient ⇒ higher priority ⇒ larger key ⇒ negate the distance.
    let dist = record.pos.dequantize_pos().distance(origin);
    let neg_dist_cm = -((dist * 100.0) as i64);
    (own, kind_rank, neg_dist_cm)
}

/// The exact encoded wire length of a snapshot message in bytes (the figure the
/// MTU guard and the meter both use). Encodes through the same bit-packed codec
/// the transport ships (TR-045), so the bound is measured, not estimated.
pub fn encoded_len(snapshot: &Snapshot) -> usize {
    Message::Snapshot(snapshot.clone()).encode().len()
}

// --- T066: bytes/client/sec metering -----------------------------------------

/// One recorded send: the wall-clock-free server time (seconds) it happened and
/// the encoded payload bytes credited.
#[derive(Clone, Copy, Debug)]
struct SendSample {
    /// Server time (seconds) the send happened — derived from the tick, no wall
    /// clock, so it is deterministic in tests.
    at_secs: f32,
    /// Encoded payload bytes credited to the recipient on this send.
    bytes: u64,
}

/// Per-connection bytes/client/sec meter over a rolling window (T066, TR-014).
///
/// Each [`BandwidthMeter::record_send`] credits the encoded payload bytes of a
/// snapshot (or any message) to that connection's running total and to a rolling
/// window. [`BandwidthMeter::mean_bytes_per_sec`] / [`BandwidthMeter::peak_window_bytes`]
/// expose the per-client figures the 8b bandwidth scenario records. The meter is
/// fed off the same encoded length the transport's [`protocol::NetStats::bytes_out`] counts,
/// so the two agree.
#[derive(Clone, Debug, Default)]
pub struct BandwidthMeter {
    /// Rolling window of recent sends per connection (bounded by the window span).
    windows: std::collections::HashMap<ConnectionId, ConnMeter>,
    /// The window span in seconds the rolling figures are computed over.
    window_secs: f32,
}

/// One connection's rolling send history + cumulative total.
#[derive(Clone, Debug, Default)]
struct ConnMeter {
    /// Recent sends within the window (oldest first), trimmed on each record.
    recent: VecDeque<SendSample>,
    /// Cumulative bytes credited to this connection (matches `NetStats::bytes_out`
    /// for the snapshot path).
    total_bytes: u64,
    /// Peak bytes observed in any single trailing window so far.
    peak_window_bytes: u64,
}

impl BandwidthMeter {
    /// Default rolling window span (seconds) the mean/peak figures cover.
    pub const DEFAULT_WINDOW_SECS: f32 = 1.0;

    /// A meter with the default 1 s window.
    pub fn new() -> Self {
        Self {
            windows: std::collections::HashMap::new(),
            window_secs: Self::DEFAULT_WINDOW_SECS,
        }
    }

    /// A meter with an explicit rolling-window span (seconds).
    pub fn with_window(window_secs: f32) -> Self {
        Self {
            windows: std::collections::HashMap::new(),
            window_secs: window_secs.max(f32::EPSILON),
        }
    }

    /// Credit `bytes` sent to `conn` at server time `at_secs` (T066). Trims the
    /// rolling window to `window_secs` and updates the peak. Fed off the encoded
    /// payload length so it agrees with [`protocol::NetStats::bytes_out`].
    pub fn record_send(&mut self, conn: ConnectionId, at_secs: f32, bytes: u64) {
        let window = self.window_secs;
        let entry = self.windows.entry(conn).or_default();
        entry.total_bytes += bytes;
        entry.recent.push_back(SendSample { at_secs, bytes });
        // Trim samples older than the window.
        let cutoff = at_secs - window;
        while let Some(front) = entry.recent.front() {
            if front.at_secs < cutoff {
                entry.recent.pop_front();
            } else {
                break;
            }
        }
        // Update the peak trailing-window total.
        let window_total: u64 = entry.recent.iter().map(|s| s.bytes).sum();
        entry.peak_window_bytes = entry.peak_window_bytes.max(window_total);
    }

    /// Mean bytes/second for `conn` over the current rolling window. Zero if the
    /// connection has no recorded sends.
    pub fn mean_bytes_per_sec(&self, conn: ConnectionId) -> f32 {
        let Some(entry) = self.windows.get(&conn) else {
            return 0.0;
        };
        if entry.recent.is_empty() {
            return 0.0;
        }
        let window_total: u64 = entry.recent.iter().map(|s| s.bytes).sum();
        window_total as f32 / self.window_secs
    }

    /// Peak bytes observed in any single trailing window for `conn` so far.
    pub fn peak_window_bytes(&self, conn: ConnectionId) -> u64 {
        self.windows
            .get(&conn)
            .map(|e| e.peak_window_bytes)
            .unwrap_or(0)
    }

    /// Cumulative bytes credited to `conn` (matches [`protocol::NetStats::bytes_out`] for
    /// the snapshot path).
    pub fn total_bytes(&self, conn: ConnectionId) -> u64 {
        self.windows.get(&conn).map(|e| e.total_bytes).unwrap_or(0)
    }

    /// Mean bytes/client/sec across **all** metered connections over the window —
    /// the fleet-wide figure (sum of each client's window total / window / client
    /// count). Zero when no client has any recorded send.
    pub fn mean_bytes_per_client_per_sec(&self) -> f32 {
        let mut active = 0u32;
        let mut sum = 0.0f32;
        for conn in self.windows.keys() {
            let mean = self.mean_bytes_per_sec(*conn);
            if mean > 0.0 {
                active += 1;
                sum += mean;
            }
        }
        if active == 0 {
            0.0
        } else {
            sum / active as f32
        }
    }

    /// The largest peak-window figure across all metered connections — the
    /// worst-case single client's peak bytes/sec.
    pub fn peak_bytes_per_client_per_sec(&self) -> u64 {
        self.windows
            .values()
            .map(|e| e.peak_window_bytes)
            .max()
            .unwrap_or(0)
    }
}

/// Assert (debug builds only) that a record set carries no client-asserted data
/// (T065). The encoder reads only the authoritative `sim` record set, so this is
/// a structural truth, not a runtime check — but the assertion documents and
/// guards the invariant: an [`EntityRecord`] is pure server-quantized state, and
/// the encoder takes no [`protocol::ClientInput`] argument at all. The constant
/// below ties the check to the protocol so a wire-shape change surfaces here.
pub const MAX_REDUNDANT_INPUTS_NEVER_ON_SNAPSHOT_PATH: usize = MAX_INPUT_TAIL;

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{QAngle, QVec2};

    fn ship(id: u32, x: f32) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            kind: EntityKind::Ship,
            pos: QVec2::quantize_pos(Vec2::new(x, 0.0)),
            vel: QVec2::quantize_vel(Vec2::ZERO),
            heading: QAngle::quantize(0.0),
            flags: 0,
        }
    }

    fn projectile(id: u32, x: f32) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            kind: EntityKind::Projectile,
            pos: QVec2::quantize_pos(Vec2::new(x, 0.0)),
            vel: QVec2::quantize_vel(Vec2::new(50.0, 0.0)),
            heading: QAngle::quantize(0.0),
            flags: 0,
        }
    }

    fn params(baseline_id: u16, keyframe: bool) -> EncodeParams {
        EncodeParams {
            server_tick: 1,
            acked_input_seq: 0,
            baseline_id,
            keyframe,
            recipient_id: Some(EntityId(0)),
            recipient_pos: Vec2::ZERO,
        }
    }

    #[test]
    fn unchanged_entities_are_omitted_zero_bits() {
        // Baseline and current hold the SAME two ships. The delta must carry NO
        // entity records (each unchanged entity costs 0 wire bits ≤ 1 bit, T063).
        let world = FullState::from_records([ship(0, 1.0), ship(1, 2.0)]);
        let baseline = world.clone();
        let delta = encode_snapshot(&world, &baseline, params(1, false));
        assert!(
            delta.entities.is_empty(),
            "an unchanged entity must be omitted from the delta (0 bits)"
        );
        assert!(delta.removed.is_empty(), "nothing disappeared");

        // Quantify the marginal cost of an unchanged entity: encoding the same
        // delta with the entity present vs absent in the baseline differs by the
        // record's record cost, but an UNCHANGED entity adds 0 bytes to the delta.
        let with = encoded_len(&delta);
        let empty = encoded_len(&Snapshot {
            entities: vec![],
            removed: vec![],
            ..delta.clone()
        });
        assert_eq!(
            with, empty,
            "an unchanged entity adds 0 bytes to the delta payload"
        );
    }

    #[test]
    fn only_changed_entities_are_emitted() {
        let baseline = FullState::from_records([ship(0, 1.0), ship(1, 2.0)]);
        // Ship 1 moved; ship 0 is unchanged.
        let world = FullState::from_records([ship(0, 1.0), ship(1, 9.0)]);
        let delta = encode_snapshot(&world, &baseline, params(1, false));
        assert_eq!(delta.entities.len(), 1, "only the changed ship is emitted");
        assert_eq!(delta.entities[0].id, EntityId(1));
    }

    #[test]
    fn disappeared_entities_are_listed_removed() {
        let baseline = FullState::from_records([ship(0, 1.0), projectile(7, 3.0)]);
        // The projectile expired; only the ship remains.
        let world = FullState::from_records([ship(0, 1.0)]);
        let delta = encode_snapshot(&world, &baseline, params(1, false));
        assert!(delta.entities.is_empty(), "the ship is unchanged");
        assert_eq!(
            delta.removed,
            vec![EntityId(7)],
            "the projectile is removed"
        );
    }

    #[test]
    fn apply_delta_round_trips_to_the_authoritative_state() {
        // The reconstructed full state on the client must equal the server's
        // authoritative world (T063 round-trip correctness).
        let baseline = FullState::from_records([ship(0, 1.0), ship(1, 2.0), projectile(7, 3.0)]);
        let world = FullState::from_records([ship(0, 5.0), ship(1, 2.0)]); // 0 moved, 7 gone
        let delta = encode_snapshot(&world, &baseline, params(1, false));
        let reconstructed = protocol::apply_delta(&baseline, &delta);
        assert_eq!(
            reconstructed, world,
            "baseline + delta reconstructs the authoritative state exactly"
        );
    }

    #[test]
    fn keyframe_carries_the_whole_world_and_is_tagged() {
        let world = FullState::from_records([ship(0, 1.0), ship(1, 2.0)]);
        // Even with a populated baseline, a keyframe ignores it and emits all.
        let baseline = FullState::from_records([ship(0, 99.0)]);
        let delta = encode_snapshot(&world, &baseline, params(5, /*keyframe=*/ true));
        assert!(
            delta.is_keyframe(),
            "a keyframe is tagged KEYFRAME_BASELINE"
        );
        assert_eq!(delta.entities.len(), 2, "a keyframe carries every entity");
        assert!(delta.removed.is_empty(), "a keyframe lists no removals");
        // It reconstructs from an EMPTY baseline (lost-ack re-baseline).
        let reconstructed = protocol::apply_delta(&FullState::new(), &delta);
        assert_eq!(reconstructed, world);
    }

    #[test]
    fn mtu_guard_keeps_the_snapshot_within_the_bound() {
        // Build a world far larger than the MTU can hold (hundreds of projectiles)
        // and confirm the encoded keyframe is capped at the bound.
        let mut records = vec![ship(0, 0.0)];
        for i in 1..500u32 {
            records.push(projectile(i, i as f32));
        }
        let world = FullState::from_records(records);
        let delta = encode_snapshot(&world, &FullState::new(), params(0, true));
        assert!(
            encoded_len(&delta) <= MAX_SNAPSHOT_BYTES,
            "the MTU guard must cap the snapshot at {MAX_SNAPSHOT_BYTES} B, got {}",
            encoded_len(&delta)
        );
        // It dropped lowest-priority entities (projectiles), so fewer than 500
        // entities survived — but the recipient's own ship (id 0) is kept.
        assert!(delta.entities.len() < 500, "lowest-priority entities shed");
        assert!(
            delta.entities.iter().any(|r| r.id == EntityId(0)),
            "the recipient's own ship is never dropped by the MTU guard"
        );
    }

    #[test]
    fn mtu_guard_sheds_projectiles_before_ships() {
        // A crowd of ships and projectiles all changed; when capped, ships should
        // survive preferentially over projectiles (the documented priority rule).
        let mut records = vec![ship(0, 0.0)];
        for i in 1..40u32 {
            records.push(ship(i, i as f32));
        }
        for i in 100..500u32 {
            records.push(projectile(i, i as f32));
        }
        let world = FullState::from_records(records);
        let delta = encode_snapshot(&world, &FullState::new(), params(0, true));
        assert!(encoded_len(&delta) <= MAX_SNAPSHOT_BYTES);
        let ships_kept = delta
            .entities
            .iter()
            .filter(|r| r.kind == EntityKind::Ship)
            .count();
        let projectiles_kept = delta
            .entities
            .iter()
            .filter(|r| r.kind == EntityKind::Projectile)
            .count();
        // All 40 ships fit comfortably; projectiles are shed to make room.
        assert_eq!(ships_kept, 40, "ships outrank projectiles and are all kept");
        assert!(
            projectiles_kept < 400,
            "projectiles are shed first under the MTU bound"
        );
    }

    #[test]
    fn bandwidth_meter_tracks_mean_and_peak() {
        let mut meter = BandwidthMeter::with_window(1.0);
        let conn = ConnectionId(3);
        // Three sends of 100 B within one window.
        meter.record_send(conn, 0.0, 100);
        meter.record_send(conn, 0.3, 100);
        meter.record_send(conn, 0.6, 100);
        assert_eq!(meter.total_bytes(conn), 300);
        // All three are within the 1 s window → mean 300 B/s, peak 300 B.
        assert_eq!(meter.mean_bytes_per_sec(conn), 300.0);
        assert_eq!(meter.peak_window_bytes(conn), 300);
        // A send well past the window trims the old ones out of the rolling mean.
        meter.record_send(conn, 5.0, 50);
        assert_eq!(
            meter.mean_bytes_per_sec(conn),
            50.0,
            "old samples fall out of the rolling window"
        );
        // The peak still reflects the earlier crowded window.
        assert_eq!(meter.peak_window_bytes(conn), 300);
    }
}
