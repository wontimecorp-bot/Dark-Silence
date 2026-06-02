//! Per-client snapshot encode benchmark (E003, OBJ6, T067, TR-047).
//!
//! Recorded-only — **no pass/fail gate** (a benchmark observes, it does not
//! assert). Measures the cost of one [`server::encode_snapshot`] call at baseline
//! scale (≈ 6 ships + their projectiles, ≤ 8 clients) so the per-client encode
//! cost on the replication hot path is observable, per the performance QC
//! category (project-instructions: benchmarks on the replication hot path).
//!
//! Stable-Rust approach: a timed [`std::time::Instant`] loop printing per-encode
//! microseconds — no nightly `#[bench]`, no criterion dependency. Declared
//! `harness = false` in `Cargo.toml`, so this `main` is the entry point. The
//! encoder is a free function over plain inputs ([`server::encode_snapshot`]), so
//! the bench builds the baseline-scale world records once and times the encode in
//! isolation (no `ServerApp`, no transport).

use std::time::Instant;

use glam::Vec2;
use protocol::{EntityId, EntityKind, EntityRecord, FullState, QAngle, QVec2};
use server::{encode_snapshot, encoded_len, EncodeParams};

/// Build a baseline-scale authoritative world: `ships` ships plus
/// `projectiles_per_ship` projectiles each, spread across the sector so the
/// distance-priority key and quantization see realistic values.
fn baseline_world(ships: u32, projectiles_per_ship: u32) -> FullState {
    let mut records: Vec<EntityRecord> = Vec::new();
    let mut next_id = 0u32;
    for s in 0..ships {
        let angle = (s as f32) * 0.9; // spread the ships around the sector
        let pos = Vec2::new(angle.cos() * 30.0, angle.sin() * 30.0);
        records.push(EntityRecord {
            id: EntityId(next_id),
            kind: EntityKind::Ship,
            pos: QVec2::quantize_pos(pos),
            vel: QVec2::quantize_vel(Vec2::new(5.0, -3.0)),
            heading: QAngle::quantize(angle),
            flags: 0,
        });
        next_id += 1;
        for p in 0..projectiles_per_ship {
            let ppos = pos + Vec2::new(p as f32 * 2.0, p as f32);
            records.push(EntityRecord {
                id: EntityId(next_id),
                kind: EntityKind::Projectile,
                pos: QVec2::quantize_pos(ppos),
                vel: QVec2::quantize_vel(Vec2::new(60.0, 0.0)),
                heading: QAngle::quantize(0.0),
                flags: 0,
            });
            next_id += 1;
        }
    }
    FullState::from_records(records)
}

/// Time `iters` encodes of `current` against `baseline` and report per-encode µs.
fn time_encode(label: &str, current: &FullState, baseline: &FullState, keyframe: bool, iters: u32) {
    let params = EncodeParams {
        server_tick: 1,
        acked_input_seq: 0,
        baseline_id: if keyframe { u16::MAX } else { 1 },
        keyframe,
        recipient_id: Some(EntityId(0)),
        recipient_pos: Vec2::ZERO,
    };

    // Warm up (let the allocator/branch predictor settle) and capture a sample
    // encoded size so the report shows the bytes the per-encode cost produced.
    let warm = encode_snapshot(current, baseline, params);
    let bytes = encoded_len(&warm);

    let start = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iters {
        let snap = encode_snapshot(current, baseline, params);
        // Consume the result so the optimizer cannot elide the encode.
        sink = sink.wrapping_add(snap.entities.len());
        std::hint::black_box(&snap);
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_secs_f64() * 1e6 / iters as f64;
    println!(
        "[encode-bench] {label:<28} entities_in={:<5} encoded={bytes:<5}B  {per:>8.3} µs/encode  ({iters} iters, sink={sink})",
        current.len()
    );
}

fn main() {
    const ITERS: u32 = 100_000;
    println!("[encode-bench] per-client snapshot encode at baseline scale (T067, recorded-only)");

    // Baseline scale: ~6 ships + a handful of projectiles each.
    let world = baseline_world(6, 3); // 6 ships + 18 projectiles = 24 entities

    // 1) Full keyframe (delta-from-nothing) — the lost-ack / first-snapshot cost.
    let empty = FullState::new();
    time_encode("keyframe (24 entities)", &world, &empty, true, ITERS);

    // 2) Steady-state delta vs an identical baseline — the common case where most
    //    entities are unchanged (delta-by-omission ⇒ a near-empty delta).
    time_encode("delta vs identical", &world, &world, false, ITERS);

    // 3) Delta where every ship moved (a busy tick): ships changed, projectiles
    //    unchanged — a partial delta.
    let mut moved = world.clone();
    let moved_records: Vec<EntityRecord> = moved
        .records()
        .iter()
        .map(|r| {
            if r.kind == EntityKind::Ship {
                let p = r.pos.dequantize_pos() + Vec2::new(1.0, 1.0);
                EntityRecord {
                    pos: QVec2::quantize_pos(p),
                    ..*r
                }
            } else {
                *r
            }
        })
        .collect();
    moved = FullState::from_records(moved_records);
    time_encode("delta (6 ships moved)", &moved, &world, false, ITERS);

    // 4) A crowded battle that exceeds the MTU, so the priority-drop path runs:
    //    8 ships + 200 projectiles. Exercises the sort + shed cost (T064).
    let crowded = baseline_world(8, 25); // 8 ships + 200 projectiles = 208 entities
    time_encode(
        "delta MTU-shed (208 in)",
        &crowded,
        &empty,
        true,
        ITERS / 10,
    );

    println!("[encode-bench] done (no pass/fail gate — figures are recorded for reference)");
}
