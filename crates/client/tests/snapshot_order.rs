//! T049 {TR-037} — late/duplicate/out-of-order snapshots advance the
//! interpolation buffer monotonically with **no backward jump** (SC-012).
//!
//! TR-037 is distinct from packet loss (T048/TR-036) and from input replay
//! (TR-022/023): it governs *snapshot* sequencing. A snapshot whose `server_tick`
//! is older than the newest already-applied snapshot MUST be discarded as stale
//! (and must not regress the buffer); a duplicate MUST be ignored. This test
//! drives a deliberately **reordered and duplicated** snapshot stream into the
//! [`SnapshotBuffer`] and asserts:
//!
//! 1. the buffer's applied tick advances monotonically (never regresses) — stale
//!    and duplicate snapshots are discarded;
//! 2. the interpolated remote position never jumps backward across the reordered
//!    delivery (the timeline cannot rewind, SC-012).
//!
//! Driven directly against the buffer (no transport) so the reordering is exact
//! and deterministic, independent of the loss/jitter medium.

use client::interpolation::SnapshotBuffer;
use glam::Vec2;
use protocol::{EntityId, EntityKind, EntityRecord, QAngle, QVec2, Snapshot};

fn record(id: u32, pos: Vec2) -> EntityRecord {
    EntityRecord {
        id: EntityId(id),
        kind: EntityKind::Ship,
        pos: QVec2::quantize_pos(pos),
        vel: QVec2::quantize_vel(Vec2::ZERO),
        heading: QAngle::quantize(0.0),
        flags: 0,
    }
}

/// A snapshot at `server_tick` with the remote at `x` along +x.
fn snap(server_tick: u32, remote_x: f32) -> Snapshot {
    Snapshot {
        server_tick,
        acked_input_seq: 0,
        baseline_id: 0,
        entities: vec![record(7, Vec2::new(remote_x, 0.0))],
        removed: Vec::new(),
    }
}

#[test]
fn reordered_and_duplicated_snapshots_advance_monotonically() {
    let mut buf = SnapshotBuffer::new(30);

    // A deliberately reordered + duplicated delivery sequence. The "true" timeline
    // is ticks 10,20,30,40,50,60 (remote at x = tick/10). We deliver them late,
    // out of order, and with duplicates mixed in.
    let delivery: &[(u32, f32, bool)] = &[
        (10, 1.0, true),  // applied
        (30, 3.0, true),  // applied (jumps ahead)
        (20, 2.0, false), // STALE (older than 30) → discarded
        (30, 3.0, false), // DUPLICATE of newest → discarded
        (50, 5.0, true),  // applied
        (40, 4.0, false), // STALE (older than 50) → discarded
        (50, 5.0, false), // DUPLICATE → discarded
        (60, 6.0, true),  // applied
        (10, 1.0, false), // very stale → discarded
    ];

    let mut newest_seen: Option<u32> = None;
    for &(tick, x, expect_applied) in delivery {
        let applied = buf.push(snap(tick, x));
        assert_eq!(
            applied, expect_applied,
            "tick {tick} apply decision mismatch (expected applied={expect_applied})"
        );

        // The applied high-water mark is monotonic: it never regresses.
        let now_newest = buf.newest_applied_tick();
        if let (Some(prev), Some(cur)) = (newest_seen, now_newest) {
            assert!(
                cur >= prev,
                "applied tick regressed: {cur} < {prev} after delivering tick {tick}"
            );
        }
        newest_seen = now_newest;
    }

    // Only the strictly-increasing subsequence was applied: 10,30,50,60.
    assert_eq!(buf.len(), 4, "stale + duplicate snapshots were discarded");
    assert_eq!(buf.newest_applied_tick(), Some(60));
}

#[test]
fn interpolated_remote_never_jumps_backward_under_reordering() {
    let mut buf = SnapshotBuffer::new(30);
    let local_id = EntityId(999); // not present; nothing excluded
    let remote_id = EntityId(7);
    let interp_delay_ms = 100.0;

    // Same reordered/duplicated stream as above.
    let delivery: &[(u32, f32)] = &[
        (10, 1.0),
        (30, 3.0),
        (20, 2.0), // stale
        (30, 3.0), // dup
        (50, 5.0),
        (40, 4.0), // stale
        (60, 6.0),
        (10, 1.0), // very stale
    ];

    // 30 Hz → 33.33 ms/tick. Walk a render clock forward as snapshots arrive and
    // assert the interpolated remote-x is non-decreasing (the timeline never
    // rewinds, even though delivery did, SC-012).
    let tick_ms = 1000.0 / 30.0;
    let mut now_ms = 0.0_f64;
    let mut last_x: Option<f32> = None;

    for &(tick, x) in delivery {
        buf.push(snap(tick, x));
        // Advance render time by one tick each delivery and sample the remote.
        now_ms += tick_ms;
        let out = buf.interpolate_remotes(now_ms, interp_delay_ms, local_id);
        if let Some(e) = out.into_iter().find(|e| e.id == remote_id) {
            if let Some(prev) = last_x {
                assert!(
                    e.pos.x >= prev - 1e-3,
                    "interpolated remote jumped backward under reordered delivery: \
                     {} < {} (after tick {tick})",
                    e.pos.x,
                    prev
                );
            }
            last_x = Some(e.pos.x);
        }
    }

    // It did advance (the remote really moved forward overall).
    assert!(
        last_x.unwrap_or(0.0) > 1.0,
        "the remote should have advanced forward overall: {last_x:?}"
    );
}
