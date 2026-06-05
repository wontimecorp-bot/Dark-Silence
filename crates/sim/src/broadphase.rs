//! Refinement 5 Phase 1 — a **deterministic sparse spatial broad-phase hash**.
//!
//! The collision/carve systems sweep projectiles against targets. The naive form is `O(projectiles ×
//! targets)` and (worse) clones every target's full `FitLayout` each tick. This hash makes the
//! broad-phase `O(occupied cells)`: each body's circle is bucketed into the integer grid cells it
//! overlaps; a swept-segment query returns the entities in the cells the segment's bounding box
//! covers — a **conservative superset** of the bodies the segment could actually hit (no false
//! negatives), so a consumer that selects by an order-independent metric (e.g. lowest time-of-impact)
//! gets a byte-identical result while only touching nearby candidates.
//!
//! **Determinism.** Pure integer-keyed `BTreeMap` (no `HashMap`, no float-ordered iteration); the
//! query result is sorted by `Entity` bits and de-duplicated, so candidate order is stable across
//! runs/platforms. This is the project's own broad-phase (NOT a physics-engine internal one, whose
//! cross-platform determinism isn't guaranteed). It is the shared foundation projectiles, ramming,
//! and explosions all query.

use bevy_ecs::entity::Entity;
use glam::Vec2;
use std::collections::BTreeMap;

/// Broad-phase grid cell size in world units. Independent of the carve `CELL_WORLD_SIZE`; sized so a
/// typical body spans ~one cell and a per-tick projectile segment covers ~one or two. Tunable for
/// occupancy vs query breadth.
pub const BROAD_CELL: f32 = 16.0;

/// Integer grid coordinate of a world value along one axis.
#[inline]
fn axis_cell(v: f32) -> i32 {
    (v / BROAD_CELL).floor() as i32
}

/// Inclusive `(min, max)` cell range covering the world-space AABB `[lo, hi]`.
#[inline]
fn aabb_cells(lo: Vec2, hi: Vec2) -> ((i32, i32), (i32, i32)) {
    (
        (axis_cell(lo.x), axis_cell(lo.y)),
        (axis_cell(hi.x), axis_cell(hi.y)),
    )
}

/// A deterministic sparse spatial hash over circle-bodies (rebuilt per tick).
#[derive(Default, Debug)]
pub struct SpatialHash {
    cells: BTreeMap<(i32, i32), Vec<Entity>>,
}

impl SpatialHash {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bucket `entity`'s circle `(center, radius)` into every grid cell its AABB overlaps. Inserting
    /// by the full circle-AABB (not just the centre cell) is what makes [`Self::query_segment`]
    /// conservative without inflating queries by a global max radius: the cell containing the
    /// segment's closest point to a hit body is guaranteed to be one of that body's inserted cells.
    pub fn insert(&mut self, entity: Entity, center: Vec2, radius: f32) {
        let r = Vec2::splat(radius.max(0.0));
        let (lo, hi) = aabb_cells(center - r, center + r);
        for cy in lo.1..=hi.1 {
            for cx in lo.0..=hi.0 {
                self.cells.entry((cx, cy)).or_default().push(entity);
            }
        }
    }

    /// Candidate entities for the swept segment `p0 → p1`: the bodies in every cell the segment's
    /// AABB covers, **sorted by entity + de-duplicated** (deterministic). A conservative superset of
    /// the bodies the segment actually intersects — never a false negative — so selecting the hit by
    /// an order-independent metric over these candidates equals selecting it over all bodies.
    pub fn query_segment(&self, p0: Vec2, p1: Vec2) -> Vec<Entity> {
        let (lo, hi) = aabb_cells(p0.min(p1), p0.max(p1));
        let mut out: Vec<Entity> = Vec::new();
        for cy in lo.1..=hi.1 {
            for cx in lo.0..=hi.0 {
                if let Some(bucket) = self.cells.get(&(cx, cy)) {
                    out.extend_from_slice(bucket);
                }
            }
        }
        out.sort_unstable_by_key(|e| e.to_bits());
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::segment_circle_toi;
    use bevy_ecs::world::World;

    /// The broad-phase is **conservative**: every body the segment truly intersects (per the exact
    /// `segment_circle_toi`) is in the candidate set. Bodies are laid out deterministically (no RNG).
    #[test]
    fn query_never_misses_a_real_hit() {
        let mut world = World::new();
        // A deterministic spread of bodies of varied radius across several broad cells.
        let bodies: Vec<(Entity, Vec2, f32)> = (0..40)
            .map(|i| {
                let e = world.spawn_empty().id();
                let x = (i as f32 * 7.0) - 140.0;
                let y = ((i * 13) % 50) as f32 - 25.0;
                let r = 0.5 + (i % 5) as f32 * 6.0; // up to ~24.5, spanning multiple cells
                (e, Vec2::new(x, y), r)
            })
            .collect();

        let mut hash = SpatialHash::new();
        for &(e, c, r) in &bodies {
            hash.insert(e, c, r);
        }

        // A spread of swept segments; for each, the candidate set must include every truly-hit body.
        let segments = [
            (Vec2::new(-200.0, 0.0), Vec2::new(200.0, 0.0)),
            (Vec2::new(0.0, -60.0), Vec2::new(0.0, 60.0)),
            (Vec2::new(-150.0, -40.0), Vec2::new(150.0, 40.0)),
            (Vec2::new(33.0, 5.0), Vec2::new(41.0, -3.0)),
            (Vec2::new(100.0, 100.0), Vec2::new(101.0, 101.0)),
        ];
        for (p0, p1) in segments {
            let candidates = hash.query_segment(p0, p1);
            for &(e, c, r) in &bodies {
                if segment_circle_toi(p0, p1, c, r).is_some() {
                    assert!(
                        candidates.contains(&e),
                        "broad-phase missed a real hit (false negative): seg {p0:?}->{p1:?} vs body {c:?} r{r}"
                    );
                }
            }
        }
    }

    /// Results are deterministic + sorted + de-duplicated regardless of insert order.
    #[test]
    fn query_is_sorted_deduped_and_insert_order_independent() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();

        let mut h1 = SpatialHash::new();
        h1.insert(a, Vec2::new(0.0, 0.0), 20.0); // large → many cells (tests dedup)
        h1.insert(b, Vec2::new(5.0, 0.0), 1.0);
        h1.insert(c, Vec2::new(2.0, 2.0), 1.0);

        let mut h2 = SpatialHash::new();
        h2.insert(c, Vec2::new(2.0, 2.0), 1.0);
        h2.insert(a, Vec2::new(0.0, 0.0), 20.0);
        h2.insert(b, Vec2::new(5.0, 0.0), 1.0);

        let q1 = h1.query_segment(Vec2::new(-1.0, 0.0), Vec2::new(6.0, 0.0));
        let q2 = h2.query_segment(Vec2::new(-1.0, 0.0), Vec2::new(6.0, 0.0));
        assert_eq!(q1, q2, "candidate set must be insert-order independent");
        // Sorted by entity bits + no duplicates (the large body spans many cells).
        let mut sorted = q1.clone();
        sorted.sort_unstable_by_key(|e| e.to_bits());
        sorted.dedup();
        assert_eq!(q1, sorted, "candidates must be sorted + de-duplicated");
    }
}
