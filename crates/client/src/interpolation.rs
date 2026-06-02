//! Remote-entity interpolation (OBJ4, AD-005, ADR-0013).
//!
//! Remote entities — every ship/projectile/target that is **not** the local
//! predicted ship — are NOT simulated on the client (that is the server's
//! authority, Principle I). They are **interpolated ~100 ms in the past** from a
//! buffer of received [`Snapshot`]s (TR-010): the client renders each remote at a
//! render time `now − interp_delay`, lerping its dequantized pose between the two
//! buffered snapshots that bracket that time. The fixed delay is what lets the
//! buffer ride out a single dropped snapshot (TR-036/SC-004) — there is always a
//! newer snapshot already buffered to interpolate toward.
//!
//! This reuses the same render-interpolation idea as the E002 fixed-step
//! [`crate::render_sync`] seam (blend between two known poses by a fraction), but
//! the two poses here come from **network snapshots** rather than adjacent fixed
//! steps, and the blend fraction is driven by the interpolation clock rather than
//! the fixed-timestep overstep.
//!
//! The pieces, per the tasks:
//! - [`SnapshotBuffer`] (T041): a bounded ring of received snapshots, capped at
//!   [`SnapshotBuffer::CAP`] (TR-027) — oldest dropped on overflow.
//! - [`SnapshotBuffer::push`] (T043, TR-037): discards stale/out-of-order/duplicate
//!   snapshots so the applied sequence advances **monotonically** (never regresses).
//! - [`SnapshotBuffer::interpolate_remotes`] (T042, TR-010): the per-remote
//!   bracket-and-lerp, shortest-arc on heading, clean appear/disappear.

use std::collections::HashMap;

use glam::Vec2;
use protocol::{apply_delta, EntityId, EntityKind, FullState, Snapshot};

/// One remote entity's interpolated render state for the current frame
/// (T042). Position/heading are dequantized sim units; the renderer mirrors
/// these into a Bevy `Transform`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InterpolatedEntity {
    /// Stable network id (so the renderer can match it to a spawned visual).
    pub id: EntityId,
    /// What kind of entity it is (picks the prefab/visual).
    pub kind: EntityKind,
    /// Interpolated position in sim units.
    pub pos: Vec2,
    /// Interpolated heading in radians (shortest-arc blended).
    pub heading: f32,
}

/// A bounded buffer of received authoritative [`Snapshot`]s, the source the
/// remote-entity interpolation reads (T041, TR-010/027).
///
/// Capped at [`SnapshotBuffer::CAP`] snapshots (TR-027): on overflow the **oldest**
/// is dropped, never grown unboundedly. The buffer's applied sequence advances
/// **monotonically** — [`SnapshotBuffer::push`] discards any snapshot whose
/// `server_tick` is not strictly newer than the newest already applied
/// (stale/out-of-order/duplicate, TR-037), so the interpolation timeline can never
/// jump backward.
#[derive(Debug, Clone)]
pub struct SnapshotBuffer {
    /// Buffered snapshots in ascending `server_tick` order (oldest first). Length
    /// `0..=CAP`; `push` keeps it sorted by construction (it only appends a
    /// strictly-newer snapshot).
    snapshots: Vec<Snapshot>,
    /// The newest `server_tick` ever applied. The monotonic high-water mark: a
    /// snapshot must beat this to be accepted (TR-037). `None` until the first
    /// snapshot is applied.
    newest_applied_tick: Option<u32>,
    /// Server tick rate (Hz), used to convert a `server_tick` to a millisecond
    /// timestamp on the interpolation timeline. Server-announced (TR-044).
    tick_rate_hz: u16,
}

impl SnapshotBuffer {
    /// Maximum buffered snapshots (TR-027 baseline 32). The oldest is dropped on
    /// overflow so memory is bounded regardless of snapshot rate / session length.
    pub const CAP: usize = 32;

    /// A fresh, empty buffer for a server running at `tick_rate_hz` (the rate the
    /// server announced in [`protocol::ConnectAccepted`], TR-044). The tick rate
    /// maps a snapshot's `server_tick` onto the interpolation timeline in ms.
    pub fn new(tick_rate_hz: u16) -> Self {
        Self {
            snapshots: Vec::new(),
            newest_applied_tick: None,
            tick_rate_hz: tick_rate_hz.max(1),
        }
    }

    /// Convert a `server_tick` to a millisecond timestamp on the interpolation
    /// timeline. Linear in the tick rate (TR-044), so adjacent snapshots are a
    /// fixed interval apart — the buffer window the interp delay rides on.
    fn tick_to_ms(&self, tick: u32) -> f64 {
        (tick as f64) * 1000.0 / (self.tick_rate_hz as f64)
    }

    /// Buffer a received `snapshot` (T043, TR-037).
    ///
    /// **Monotonic gate:** a snapshot whose `server_tick` is **≤** the newest
    /// already-applied tick is discarded — that covers a *stale/out-of-order*
    /// snapshot (an older tick arriving late) and a *duplicate* (the same tick
    /// seen twice). Only a strictly-newer snapshot is appended, so the applied
    /// sequence (and the interpolation timeline) advances monotonically and never
    /// regresses (no backward jump, SC-012).
    ///
    /// On overflow past [`SnapshotBuffer::CAP`] the **oldest** snapshot is dropped
    /// (TR-027) — never grow unboundedly.
    ///
    /// Returns `true` if the snapshot was applied, `false` if it was discarded as
    /// stale/duplicate.
    pub fn push(&mut self, snapshot: Snapshot) -> bool {
        if let Some(newest) = self.newest_applied_tick {
            // Stale/out-of-order (older tick) or duplicate (same tick): discard.
            // `<=` covers both — a duplicate is `==`, an out-of-order late arrival
            // is `<`. The buffer never regresses.
            if snapshot.server_tick <= newest {
                return false;
            }
        }
        self.newest_applied_tick = Some(snapshot.server_tick);
        self.snapshots.push(snapshot);
        // Bound the buffer: drop the oldest on overflow (TR-027).
        if self.snapshots.len() > Self::CAP {
            let overflow = self.snapshots.len() - Self::CAP;
            self.snapshots.drain(0..overflow);
        }
        true
    }

    /// Number of buffered snapshots.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether the buffer holds no snapshots.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// The newest `server_tick` applied (the monotonic high-water mark), or `None`
    /// if nothing has been applied yet.
    pub fn newest_applied_tick(&self) -> Option<u32> {
        self.newest_applied_tick
    }

    /// The newest buffered snapshot, if any.
    pub fn newest(&self) -> Option<&Snapshot> {
        self.snapshots.last()
    }

    /// Interpolate every remote entity at render time `now_ms − interp_delay_ms`
    /// (T042, TR-010).
    ///
    /// The render time is `now_ms − interp_delay_ms`: the client renders remotes a
    /// fixed delay in the past so there is (almost) always a newer snapshot
    /// buffered to interpolate *toward*. For each entity present in the buffer:
    /// - find the two buffered snapshots whose tick-derived timestamps **bracket**
    ///   the render time and that both contain the entity, and **linearly
    ///   interpolate** its dequantized position and (shortest-arc) heading by the
    ///   fraction between them;
    /// - if the render time is before the oldest buffered snapshot, hold the
    ///   entity's **oldest** known pose (no extrapolation backward);
    /// - if the render time is at or past the newest buffered snapshot, hold the
    ///   entity's **newest** known pose (no forward extrapolation — the freeze
    ///   that rides out a dropped snapshot, TR-036);
    /// - an entity present in only one of the two bracketing snapshots appears /
    ///   disappears cleanly (it is interpolated only while both ends carry it; it
    ///   is rendered at the single pose it does have when only one end carries it,
    ///   and absent entirely once no buffered snapshot near the render time has it).
    ///
    /// The `local_id` ship is **excluded** — the local ship is predicted, not
    /// interpolated (AD-005). Returns one [`InterpolatedEntity`] per visible
    /// remote.
    pub fn interpolate_remotes(
        &self,
        now_ms: f64,
        interp_delay_ms: f64,
        local_id: EntityId,
    ) -> Vec<InterpolatedEntity> {
        if self.snapshots.is_empty() {
            return Vec::new();
        }
        let render_ms = now_ms - interp_delay_ms;

        // Locate the bracketing pair (lo, hi) of snapshot indices around the
        // render time: lo is the newest snapshot at or before render_ms, hi is the
        // oldest after it. Either may be missing at the timeline ends.
        let (lo_idx, hi_idx) = self.bracket(render_ms);

        // Collect the distinct remote ids the renderer might show, drawn from the
        // bracketing snapshots only — entities far in the past/future are not the
        // current frame's concern.
        let mut out: Vec<InterpolatedEntity> = Vec::new();
        let mut seen: HashMap<u32, ()> = HashMap::new();

        // Helper closure inputs: the lo/hi snapshots (if present) and their ms.
        let lo = lo_idx.map(|i| &self.snapshots[i]);
        let hi = hi_idx.map(|i| &self.snapshots[i]);

        // Iterate the union of entity ids in the bracketing snapshots.
        for snap in [lo, hi].into_iter().flatten() {
            for record in &snap.entities {
                if record.id == local_id {
                    // The local ship is predicted, never interpolated (AD-005).
                    continue;
                }
                if seen.insert(record.id.0, ()).is_some() {
                    continue; // already produced this id
                }
                if let Some(e) = self.interp_one(record.id, record.kind, lo, hi, render_ms) {
                    out.push(e);
                }
            }
        }

        // Stable output order (by id) so the renderer's frame-to-frame matching is
        // deterministic regardless of snapshot record order.
        out.sort_by_key(|e| e.id.0);
        out
    }

    /// Interpolate a single entity `id` between the bracketing snapshots, or hold
    /// the single pose it has if only one end carries it. Returns `None` if
    /// neither bracketing snapshot carries the entity (clean disappear).
    fn interp_one(
        &self,
        id: EntityId,
        kind: EntityKind,
        lo: Option<&Snapshot>,
        hi: Option<&Snapshot>,
        render_ms: f64,
    ) -> Option<InterpolatedEntity> {
        let lo_rec = lo.and_then(|s| s.entities.iter().find(|r| r.id == id));
        let hi_rec = hi.and_then(|s| s.entities.iter().find(|r| r.id == id));

        match (lo, hi, lo_rec, hi_rec) {
            // Both bracket snapshots carry the entity: lerp between them.
            (Some(lo_s), Some(hi_s), Some(lr), Some(hr)) => {
                let lo_ms = self.tick_to_ms(lo_s.server_tick);
                let hi_ms = self.tick_to_ms(hi_s.server_tick);
                let span = hi_ms - lo_ms;
                // Degenerate equal-time bracket (shouldn't happen given the
                // monotonic gate): snap to the newer end.
                let t = if span > 0.0 {
                    ((render_ms - lo_ms) / span).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                let pos = lr.pos.dequantize_pos().lerp(hr.pos.dequantize_pos(), t);
                let heading = lerp_angle(lr.heading.dequantize(), hr.heading.dequantize(), t);
                Some(InterpolatedEntity {
                    id,
                    kind,
                    pos,
                    heading,
                })
            }
            // Only the older end carries it (it has since disappeared, or we are at
            // the newest end with no forward snapshot): hold its last known pose
            // (freeze — no forward extrapolation; this is what rides out a dropped
            // snapshot, TR-036).
            (_, _, Some(lr), None) => Some(InterpolatedEntity {
                id,
                kind,
                pos: lr.pos.dequantize_pos(),
                heading: lr.heading.dequantize(),
            }),
            // Only the newer end carries it (it just appeared, or the render time
            // is before the oldest buffered snapshot): show it at that single pose
            // (clean appear — no backward extrapolation).
            (_, _, None, Some(hr)) => Some(InterpolatedEntity {
                id,
                kind,
                pos: hr.pos.dequantize_pos(),
                heading: hr.heading.dequantize(),
            }),
            // Neither bracket carries it: it is not visible this frame.
            _ => None,
        }
    }

    /// Find the bracketing snapshot indices `(lo, hi)` around `render_ms` on the
    /// tick-derived timeline: `lo` is the newest snapshot at or before `render_ms`,
    /// `hi` is the oldest after it. At the timeline ends one side is `None`:
    /// - before the oldest snapshot: `(None, Some(0))`;
    /// - at/after the newest snapshot: `(Some(last), None)` — the **freeze** that
    ///   holds the last known pose rather than extrapolating forward (TR-036).
    fn bracket(&self, render_ms: f64) -> (Option<usize>, Option<usize>) {
        let n = self.snapshots.len();
        if n == 0 {
            return (None, None);
        }
        // Snapshots are sorted ascending by tick (push only appends newer), so
        // their timestamps are ascending too.
        let mut lo: Option<usize> = None;
        let mut hi: Option<usize> = None;
        for i in 0..n {
            let ms = self.tick_to_ms(self.snapshots[i].server_tick);
            if ms <= render_ms {
                lo = Some(i);
            } else {
                hi = Some(i);
                break;
            }
        }
        (lo, hi)
    }
}

/// Client-side delta reconstruction (E003, OBJ6, T063 client half).
///
/// The server delta-codes each [`Snapshot`] against the snapshot the client last
/// acked (Principle VI). Before the client can interpolate or reconcile it must
/// rebuild the **full** authoritative state for the tick from baseline + delta
/// ([`protocol::apply_delta`]). This holds that running baseline and the
/// last-acked id so it deltas against exactly the state the server deltas against:
///
/// - a **keyframe** (`baseline_id == KEYFRAME_BASELINE`) reconstructs from nothing
///   — it carries the whole world — and re-anchors the baseline (so a lost ack
///   re-baselines gracefully, T064);
/// - a **delta** whose `baseline_id` matches the client's last-acked id is folded
///   onto the acked baseline;
/// - a **delta** whose `baseline_id` does NOT match (the server deltaed against a
///   baseline the client never acked — e.g. the very first deltas before the
///   client's first ack lands) is dropped as unreconstructable; the server keeps
///   deltaing against the last-acked / keyframing until an ack catches up.
///
/// On a successful reconstruction the client should ack the snapshot id (so the
/// server advances its baseline) and the reconstructor adopts the reconstructed
/// full state as its new acked baseline. The reconstructed full [`Snapshot`]
/// (every entity in `entities`, empty `removed`) is what feeds the
/// [`SnapshotBuffer`] and reconciliation — both consume full state unchanged.
#[derive(Debug, Clone, Default)]
pub struct DeltaReconstructor {
    /// The snapshot id of the currently-acked baseline (`None` until the first
    /// keyframe/reconstruction is adopted).
    acked_id: Option<u16>,
    /// The full reconstructed state of the acked baseline — what the client holds
    /// and the server deltas against.
    baseline: FullState,
}

impl DeltaReconstructor {
    /// A fresh reconstructor with no baseline (the first snapshot must be a
    /// keyframe, which the server sends when the client has acked nothing).
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of the currently-acked baseline (`None` ⇒ nothing reconstructed yet).
    pub fn acked_id(&self) -> Option<u16> {
        self.acked_id
    }

    /// The full reconstructed state of the acked baseline.
    pub fn baseline(&self) -> &FullState {
        &self.baseline
    }

    /// Reconstruct the full state for `snapshot` (T063 client half).
    ///
    /// Returns a full [`Snapshot`] — every reconstructed entity in `entities`,
    /// `removed` empty — that the buffer and reconciliation consume directly, plus
    /// the id to ack so the server advances its baseline. Returns `None` for an
    /// unreconstructable delta (one whose `baseline_id` the client does not hold);
    /// the caller drops it and waits for a keyframe / a matching baseline.
    ///
    /// On success the reconstructor adopts the reconstructed full state as its new
    /// acked baseline and records the new acked id.
    pub fn reconstruct(&mut self, snapshot: &Snapshot) -> Option<Reconstructed> {
        let full = if snapshot.is_keyframe() {
            // A keyframe carries the whole world — reconstruct from nothing.
            apply_delta(&FullState::new(), snapshot)
        } else if self.acked_id == Some(snapshot.baseline_id) {
            // A delta against the baseline we hold — fold it on.
            apply_delta(&self.baseline, snapshot)
        } else if self.acked_id.is_none() && snapshot.baseline_id == 0 {
            // The server's first deltas (before any ack) baseline against `0`
            // ("nothing acked yet"); treat that as delta-from-empty so the client
            // can bootstrap before its first ack round-trips.
            apply_delta(&FullState::new(), snapshot)
        } else {
            // Unreconstructable: the server deltaed against a baseline we never
            // acked. Drop it; the server keeps deltaing against the last-acked /
            // keyframing until an ack catches up.
            return None;
        };

        // Adopt the reconstructed state as the new acked baseline, identified by
        // this snapshot's wire id (its server tick → u16, the SAME mapping the
        // server records its sent state under, so the two agree on identity).
        let ack_id = snapshot.wire_id();
        self.acked_id = Some(ack_id);
        self.baseline = full.clone();

        // The full snapshot the buffer + reconcile consume: same header, every
        // reconstructed record, no removals.
        let full_snapshot = Snapshot {
            server_tick: snapshot.server_tick,
            acked_input_seq: snapshot.acked_input_seq,
            baseline_id: snapshot.baseline_id,
            entities: full.to_records(),
            removed: Vec::new(),
        };
        Some(Reconstructed {
            full: full_snapshot,
            ack_id,
        })
    }
}

/// The result of reconstructing one delta snapshot (T063): the full snapshot to
/// feed the buffer/reconcile, plus the id to ack so the server advances baseline.
#[derive(Debug, Clone)]
pub struct Reconstructed {
    /// The fully-reconstructed snapshot (all entities, no removals).
    pub full: Snapshot,
    /// The id the client should ack so the server promotes its delta baseline.
    pub ack_id: u16,
}

/// Shortest-path angular interpolation in radians (no seam jump at ±π). Mirrors
/// the E002 [`crate::render_sync`] `lerp_angle` so interpolated remotes turn the
/// short way exactly as the local fixed-step interpolation does.
pub fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let diff = (b - a + PI).rem_euclid(TAU) - PI;
    a + diff * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{EntityRecord, QAngle, QVec2};

    fn record(id: u32, kind: EntityKind, pos: Vec2, heading: f32) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            kind,
            pos: QVec2::quantize_pos(pos),
            vel: QVec2::quantize_vel(Vec2::ZERO),
            heading: QAngle::quantize(heading),
            flags: 0,
        }
    }

    fn snapshot(server_tick: u32, entities: Vec<EntityRecord>) -> Snapshot {
        Snapshot {
            server_tick,
            acked_input_seq: 0,
            baseline_id: 0,
            entities,
            removed: Vec::new(),
        }
    }

    #[test]
    fn buffer_caps_at_32_dropping_oldest() {
        let mut buf = SnapshotBuffer::new(30);
        for tick in 1..=100u32 {
            buf.push(snapshot(tick, vec![]));
        }
        assert_eq!(buf.len(), SnapshotBuffer::CAP, "buffer bounded at CAP");
        // Oldest dropped: the surviving snapshots are the newest 32.
        assert_eq!(buf.snapshots.first().unwrap().server_tick, 100 - 32 + 1);
        assert_eq!(buf.snapshots.last().unwrap().server_tick, 100);
    }

    #[test]
    fn push_discards_stale_and_duplicate_monotonically() {
        let mut buf = SnapshotBuffer::new(30);
        assert!(buf.push(snapshot(10, vec![])));
        assert!(buf.push(snapshot(20, vec![])));
        // Out-of-order (older) discarded.
        assert!(!buf.push(snapshot(15, vec![])), "stale tick discarded");
        // Duplicate discarded.
        assert!(!buf.push(snapshot(20, vec![])), "duplicate tick discarded");
        // Newer accepted.
        assert!(buf.push(snapshot(21, vec![])));
        assert_eq!(buf.newest_applied_tick(), Some(21));
        assert_eq!(buf.len(), 3, "only 10, 20, 21 applied");
    }

    #[test]
    fn interpolates_linearly_between_two_snapshots() {
        // 30 Hz → 1000/30 ≈ 33.33 ms/tick. Two snapshots a tick apart.
        let mut buf = SnapshotBuffer::new(30);
        let remote = EntityId(7);
        buf.push(snapshot(
            0,
            vec![record(remote.0, EntityKind::Ship, Vec2::new(0.0, 0.0), 0.0)],
        ));
        buf.push(snapshot(
            3,
            vec![record(remote.0, EntityKind::Ship, Vec2::new(3.0, 0.0), 0.0)],
        ));
        // tick 0 → 0 ms, tick 3 → 100 ms. Render time exactly halfway (50 ms): with
        // interp_delay 0 and now 50 → render 50 → halfway → x ≈ 1.5 (within one
        // position quantization step, since the records are quantized on the wire).
        let out = buf.interpolate_remotes(50.0, 0.0, EntityId(999));
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].pos.x - 1.5).abs() < protocol::POS_TOLERANCE,
            "midpoint interpolation, got {}",
            out[0].pos.x
        );
    }

    #[test]
    fn excludes_the_local_ship() {
        let mut buf = SnapshotBuffer::new(30);
        let local = EntityId(1);
        buf.push(snapshot(
            0,
            vec![
                record(local.0, EntityKind::Ship, Vec2::ZERO, 0.0),
                record(2, EntityKind::Ship, Vec2::new(5.0, 0.0), 0.0),
            ],
        ));
        buf.push(snapshot(
            3,
            vec![
                record(local.0, EntityKind::Ship, Vec2::new(1.0, 0.0), 0.0),
                record(2, EntityKind::Ship, Vec2::new(6.0, 0.0), 0.0),
            ],
        ));
        let out = buf.interpolate_remotes(50.0, 0.0, local);
        assert_eq!(out.len(), 1, "local ship excluded, only the remote remains");
        assert_eq!(out[0].id, EntityId(2));
    }

    #[test]
    fn heading_takes_the_shortest_arc() {
        // From +170° to −170° the short way is +20° across the seam, not −340°.
        use std::f32::consts::PI;
        let a = 170.0_f32.to_radians();
        let b = (-170.0_f32).to_radians();
        let mid = lerp_angle(a, b, 0.5);
        // Halfway the short way lands at ±180° (the seam), magnitude near π.
        assert!(mid.abs() > 175.0_f32.to_radians() && mid.abs() <= PI + 1e-3);
    }

    #[test]
    fn holds_newest_pose_past_the_end_no_extrapolation() {
        // Render time well past the newest snapshot → freeze at the newest pose
        // (no forward extrapolation). This is what rides out a dropped snapshot.
        let mut buf = SnapshotBuffer::new(30);
        let remote = EntityId(7);
        buf.push(snapshot(
            0,
            vec![record(remote.0, EntityKind::Ship, Vec2::ZERO, 0.0)],
        ));
        buf.push(snapshot(
            3,
            vec![record(remote.0, EntityKind::Ship, Vec2::new(3.0, 0.0), 0.0)],
        ));
        // Render time 500 ms — far past tick 3 (100 ms). Holds the newest pose
        // (within one position quantization step of the un-quantized 3.0).
        let out = buf.interpolate_remotes(500.0, 0.0, EntityId(999));
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].pos.x - 3.0).abs() < protocol::POS_TOLERANCE,
            "frozen at newest pose, got {}",
            out[0].pos.x
        );
    }

    // --- DeltaReconstructor (T063 client half) -------------------------------

    fn keyframe(server_tick: u32, entities: Vec<EntityRecord>) -> Snapshot {
        Snapshot {
            server_tick,
            acked_input_seq: 0,
            baseline_id: Snapshot::KEYFRAME_BASELINE,
            entities,
            removed: Vec::new(),
        }
    }

    fn delta(
        server_tick: u32,
        baseline_id: u16,
        entities: Vec<EntityRecord>,
        removed: Vec<EntityId>,
    ) -> Snapshot {
        Snapshot {
            server_tick,
            acked_input_seq: 0,
            baseline_id,
            entities,
            removed,
        }
    }

    #[test]
    fn reconstructor_bootstraps_from_a_keyframe() {
        let mut r = DeltaReconstructor::new();
        let kf = keyframe(
            5,
            vec![
                record(1, EntityKind::Ship, Vec2::new(1.0, 0.0), 0.0),
                record(2, EntityKind::Ship, Vec2::new(2.0, 0.0), 0.0),
            ],
        );
        let out = r.reconstruct(&kf).expect("a keyframe always reconstructs");
        assert_eq!(out.full.entities.len(), 2, "keyframe carries every entity");
        assert!(out.full.removed.is_empty());
        assert_eq!(out.ack_id, kf.wire_id(), "ack the snapshot's wire id");
        assert_eq!(r.acked_id(), Some(kf.wire_id()));
    }

    #[test]
    fn reconstructor_folds_a_delta_onto_the_acked_baseline() {
        let mut r = DeltaReconstructor::new();
        // Bootstrap with a keyframe of two ships.
        let kf = keyframe(
            5,
            vec![
                record(1, EntityKind::Ship, Vec2::new(1.0, 0.0), 0.0),
                record(2, EntityKind::Ship, Vec2::new(2.0, 0.0), 0.0),
            ],
        );
        let bootstrap = r.reconstruct(&kf).unwrap();
        let baseline_id = bootstrap.ack_id;

        // A delta against that baseline: ship 1 moved, ship 2 omitted (unchanged).
        let d = delta(
            6,
            baseline_id,
            vec![record(1, EntityKind::Ship, Vec2::new(9.0, 0.0), 0.0)],
            vec![],
        );
        let out = r
            .reconstruct(&d)
            .expect("delta against held baseline applies");
        // The reconstructed full state carries BOTH ships — ship 2 persisted from
        // the baseline (delta-by-omission), ship 1 updated.
        assert_eq!(out.full.entities.len(), 2, "unchanged ship 2 persists");
        let ship1 = out
            .full
            .entities
            .iter()
            .find(|e| e.id == EntityId(1))
            .unwrap();
        assert!(
            (ship1.pos.dequantize_pos().x - 9.0).abs() < protocol::POS_TOLERANCE,
            "ship 1 updated to the delta value"
        );
        assert!(
            out.full.entities.iter().any(|e| e.id == EntityId(2)),
            "ship 2 still present"
        );
    }

    #[test]
    fn reconstructor_applies_removals() {
        let mut r = DeltaReconstructor::new();
        let kf = keyframe(
            5,
            vec![
                record(1, EntityKind::Ship, Vec2::ZERO, 0.0),
                record(7, EntityKind::Projectile, Vec2::new(3.0, 0.0), 0.0),
            ],
        );
        let baseline_id = r.reconstruct(&kf).unwrap().ack_id;
        // The projectile expired.
        let d = delta(6, baseline_id, vec![], vec![EntityId(7)]);
        let out = r.reconstruct(&d).unwrap();
        assert_eq!(out.full.entities.len(), 1, "the removed projectile is gone");
        assert_eq!(out.full.entities[0].id, EntityId(1));
    }

    #[test]
    fn reconstructor_drops_unreconstructable_delta() {
        let mut r = DeltaReconstructor::new();
        // Bootstrap a baseline at id A.
        let kf = keyframe(5, vec![record(1, EntityKind::Ship, Vec2::ZERO, 0.0)]);
        r.reconstruct(&kf).unwrap();
        // A delta against a baseline id we never acked → unreconstructable → None.
        let d = delta(
            6,
            12345, // not our acked baseline id
            vec![record(1, EntityKind::Ship, Vec2::new(1.0, 0.0), 0.0)],
            vec![],
        );
        assert!(
            r.reconstruct(&d).is_none(),
            "a delta against an unknown baseline is dropped"
        );
    }
}
