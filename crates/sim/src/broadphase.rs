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
use bevy_ecs::prelude::{Or, Query, ResMut, Resource, With, Without};
use glam::Vec2;
use std::collections::BTreeMap;

use crate::components::{Position, Projectile, Ship, Target};

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

/// Coarse interest-tier grid cell size in world units (E011 / 00008-ship-ai, AD-002). Much
/// larger than [`BROAD_CELL`] (4×): this tier answers *interest/proximity* questions (AOI/LOD
/// classification, far sensor scans), not collision, so the default `AiTuning` AOI radii
/// (60 / 240) resolve to small bounded neighborhoods (~3×3 up to ~9×9 cells).
pub const COARSE_CELL_SIZE: f32 = 64.0;

/// Integer coarse-grid coordinate of a world value along one axis.
#[inline]
fn coarse_axis_cell(v: f32) -> i32 {
    (v / COARSE_CELL_SIZE).floor() as i32
}

/// The **coarse interest tier** beside the fine [`SpatialHash`] (E011 AD-002): a second flat
/// sparse `BTreeMap` grid with much larger cells, rebuilt once per tick and read many times by
/// the AOI/LOD classifier, far perception scans, and dormant-glide promotion triggers.
///
/// Unlike the fine hash, bodies are inserted as **points** (their centre cell only) — the coarse
/// tier asks "what is *near* this position", not "what does this segment *hit*", so no radius
/// inflation is needed; [`Self::near`] is conservative by covering the whole `pos ± radius` cell
/// AABB instead.
///
/// **Determinism.** Same doctrine as [`SpatialHash`]: integer-keyed `BTreeMap`, per-cell `Vec`s
/// sorted by `Entity` bits at build (independent of the caller's archetype iteration order), and
/// query results sorted + de-duplicated — stable across runs/platforms.
#[derive(Default, Debug)]
pub struct CoarseGrid {
    cells: BTreeMap<(i32, i32), Vec<Entity>>,
}

impl CoarseGrid {
    /// The coarse cell containing world position `pos`. `pub` because the LOD classifier and
    /// far-scan cadences key off coarse cell coordinates directly.
    #[inline]
    pub fn cell_of(pos: Vec2) -> (i32, i32) {
        (coarse_axis_cell(pos.x), coarse_axis_cell(pos.y))
    }

    /// Build the grid from `(entity, position)` points — once per tick, then read-only. Each
    /// per-cell bucket is sorted by `Entity` bits so the built structure (and every query over
    /// it) is independent of the caller's iteration order.
    pub fn build(items: impl Iterator<Item = (Entity, Vec2)>) -> Self {
        let mut cells: BTreeMap<(i32, i32), Vec<Entity>> = BTreeMap::new();
        for (entity, pos) in items {
            cells.entry(Self::cell_of(pos)).or_default().push(entity);
        }
        for bucket in cells.values_mut() {
            bucket.sort_unstable_by_key(|e| e.to_bits());
        }
        Self { cells }
    }

    /// Entities in every coarse cell the AABB `pos ± radius` covers, **sorted by entity bits +
    /// de-duplicated**. A conservative superset of the entities truly within `radius` of `pos`
    /// (never a false negative — a point body in range lies in a covered cell); consumers apply
    /// their own exact distance/tier test over these candidates.
    pub fn near(&self, pos: Vec2, radius: f32) -> Vec<Entity> {
        let r = Vec2::splat(radius.max(0.0));
        let lo = Self::cell_of(pos - r);
        let hi = Self::cell_of(pos + r);
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

/// Build-once-read-many holder for the per-tick [`CoarseGrid`] (E011 TR-007). Inserted at
/// server-world construction so it exists (inert, empty) in every world; only the
/// `ScenarioActive`-gated [`build_coarse_index_system`] ever rebuilds it.
#[derive(Resource, Default, Debug)]
pub struct CoarseIndex(pub CoarseGrid);

/// Rebuild the [`CoarseIndex`] from every interest-relevant body — ships and targets (the
/// entities AI cares about being near), **projectiles excluded** — once per tick, before the
/// AOI/LOD classifier consumes it (HINT-001).
///
/// Registered gated on `ScenarioActive` AND on the resource existing (belt-and-suspenders, the
/// `recompute_ship_stats_system` double-gate pattern): a world that inserts `ScenarioActive`
/// without the index (hand-rolled test worlds) skips it instead of panicking. It mutates ONLY
/// this resource — no gameplay state — so golden worlds that do run it (`demo_enemies_smoke`
/// spawns a scenario) remain bit-identical.
pub fn build_coarse_index_system(
    mut index: ResMut<CoarseIndex>,
    bodies: Query<(Entity, &Position), (Or<(With<Ship>, With<Target>)>, Without<Projectile>)>,
) {
    index.0 = CoarseGrid::build(bodies.iter().map(|(e, p)| (e, p.0)));
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

    /// An inserted entity is findable via `near` at its own position (even with a tiny radius),
    /// and an entity far outside the queried range is absent.
    #[test]
    fn coarse_near_finds_resident_and_excludes_far() {
        let mut world = World::new();
        let here = world.spawn_empty().id();
        let far = world.spawn_empty().id();
        let pos = Vec2::new(37.0, -91.0);
        let grid = CoarseGrid::build(
            [
                (here, pos),
                (far, pos + Vec2::splat(10.0 * COARSE_CELL_SIZE)),
            ]
            .into_iter(),
        );

        let q = grid.near(pos, 1.0);
        assert!(
            q.contains(&here),
            "entity must be findable at its own position"
        );
        assert!(
            !q.contains(&far),
            "entity ~10 cells away must not appear in a tiny-radius query"
        );
    }

    /// A `near` query whose radius spans multiple coarse cells returns every in-range entity,
    /// sorted by entity bits + de-duplicated, independent of build iteration order.
    #[test]
    fn coarse_near_spanning_cells_is_sorted_deduped_and_order_independent() {
        let mut world = World::new();
        // Three bodies in three different coarse cells around the origin, all within radius 100.
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let bodies = [
            (a, Vec2::new(-70.0, 0.0)),
            (b, Vec2::new(0.0, 0.0)),
            (c, Vec2::new(70.0, 70.0)),
        ];

        let g1 = CoarseGrid::build(bodies.into_iter());
        let mut rev = bodies;
        rev.reverse();
        let g2 = CoarseGrid::build(rev.into_iter());

        let q1 = g1.near(Vec2::ZERO, 100.0);
        let q2 = g2.near(Vec2::ZERO, 100.0);
        assert_eq!(q1, q2, "results must be build-order independent");
        for (e, p) in bodies {
            assert!(
                q1.contains(&e),
                "in-range body at {p:?} missing from multi-cell query"
            );
        }
        let mut sorted = q1.clone();
        sorted.sort_unstable_by_key(|e| e.to_bits());
        sorted.dedup();
        assert_eq!(q1, sorted, "results must be sorted + de-duplicated");
    }

    /// `cell_of` floors (does not truncate toward zero): negative coordinates land in negative
    /// cells, and exact cell-edge values belong to the cell they open.
    #[test]
    fn coarse_cell_of_boundary_and_negative_coords() {
        assert_eq!(CoarseGrid::cell_of(Vec2::ZERO), (0, 0));
        // Just inside cell (0, 0) on both axes.
        assert_eq!(
            CoarseGrid::cell_of(Vec2::new(
                COARSE_CELL_SIZE - 0.001,
                COARSE_CELL_SIZE - 0.001
            )),
            (0, 0)
        );
        // The exact edge opens the NEXT cell.
        assert_eq!(
            CoarseGrid::cell_of(Vec2::new(COARSE_CELL_SIZE, COARSE_CELL_SIZE)),
            (1, 1)
        );
        // Negative values floor into cell -1 (truncation toward zero would give 0).
        assert_eq!(CoarseGrid::cell_of(Vec2::new(-0.001, -0.001)), (-1, -1));
        // A full negative cell width lands exactly on cell -1's opening edge.
        assert_eq!(
            CoarseGrid::cell_of(Vec2::new(-COARSE_CELL_SIZE, -COARSE_CELL_SIZE)),
            (-1, -1)
        );
        // One step further opens cell -2.
        assert_eq!(
            CoarseGrid::cell_of(Vec2::new(
                -COARSE_CELL_SIZE - 0.001,
                -COARSE_CELL_SIZE - 0.001
            )),
            (-2, -2)
        );
    }
}
