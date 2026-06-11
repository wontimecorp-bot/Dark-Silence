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
use bevy_ecs::prelude::{Or, Query, Res, ResMut, Resource, With, Without};
use glam::Vec2;
use std::collections::BTreeMap;

use crate::ai::tuning::AiTuning;
use crate::components::{CollisionRadius, Position, Projectile, Ship, Target};

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

/// The per-tick **obstacle field** the ship-AI steers around (R96 Part D): a
/// flat, deterministically-ordered list of `(centre, radius)` circles for the
/// LARGE neutral bodies a ship should avoid colliding with — asteroids,
/// outposts, transports — NOT other ships. A build-once-read-many `Resource`
/// in the [`CoarseIndex`] mould: inserted (inert, empty) at world construction
/// so it exists in every world, rebuilt each tick ONLY by the
/// `ScenarioActive`-gated [`build_obstacle_field_system`], and consumed by the
/// AI execute arm (move + combat) through `add_obstacle_danger`.
///
/// **Why a flat `Vec`, not a grid**: the obstacle set is tiny (a handful of
/// neutral bodies per sector) and every consumer scans the WHOLE list against
/// its own ship position, so a sparse grid would only add bookkeeping. The
/// determinism doctrine is the same as [`CoarseGrid`] though — the `Vec` is
/// sorted by POSITION bits at build (`(pos.x.to_bits(), pos.y.to_bits())`), so
/// the list is identical across runs/platforms regardless of the caller's
/// archetype iteration order.
#[derive(Resource, Default, Debug)]
pub struct ObstacleField {
    /// `(centre, radius)` of each avoid-worthy body, sorted by position bits.
    pub obstacles: Vec<(Vec2, f32)>,
}

impl ObstacleField {
    /// Build the field from `(position, radius)` items, keeping only bodies with
    /// `radius >= min_radius` (the large neutral bodies — small debris/ships are
    /// not avoided here), then sort by `(pos.x bits, pos.y bits)` for cross-run
    /// stability (mirrors [`CoarseGrid::build`]'s sort-at-build doctrine). Pure +
    /// deterministic — no RNG, no HashMap iteration.
    pub fn build(items: impl Iterator<Item = (Vec2, f32)>, min_radius: f32) -> Self {
        let mut obstacles: Vec<(Vec2, f32)> =
            items.filter(|&(_, radius)| radius >= min_radius).collect();
        // Sort by position bits — stable across runs/platforms, independent of
        // the source query's archetype iteration order (the CoarseGrid doctrine).
        obstacles.sort_unstable_by_key(|&(pos, _)| (pos.x.to_bits(), pos.y.to_bits()));
        Self { obstacles }
    }
}

/// Rebuild the [`ObstacleField`] from every large neutral [`Target`] body
/// (asteroid/outpost/transport — the things to AVOID, never ships or
/// projectiles) once per tick, before the AI execute arm consumes it. Filtered
/// to `radius >= AiTuning::obstacle_min_radius` so only sizeable bodies enter
/// the field (small debris is not steered around).
///
/// Registered gated on `ScenarioActive` AND on the resource existing (the
/// `build_coarse_index_system` double-gate), right after the coarse-index
/// build. It mutates ONLY this resource — no gameplay state — so golden worlds
/// that DO run it (`demo_enemies_smoke` spawns `Target`s with
/// [`CollisionRadius`], so the field populates there) stay bit-identical: no
/// `AiBrain` consumes the field in those worlds, so no intent changes.
pub fn build_obstacle_field_system(
    mut field: ResMut<ObstacleField>,
    tuning: Res<AiTuning>,
    bodies: Query<(&Position, &CollisionRadius), (With<Target>, Without<Projectile>)>,
) {
    *field = ObstacleField::build(
        bodies.iter().map(|(p, r)| (p.0, r.0)),
        tuning.obstacle_min_radius,
    );
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

    /// R96 Part D — the [`ObstacleField`] build is deterministic: the output
    /// `Vec` is sorted by position bits (so cross-run/platform stable), drops
    /// sub-`min_radius` bodies (small debris is not an obstacle), and is
    /// independent of the source iteration order (mirrors the coarse-grid
    /// order-independence tests). A duplicate-position pair (two bodies stacked
    /// on the same point) stays present but ADJACENT — the build does not dedup
    /// distinct bodies, only orders them stably.
    #[test]
    fn obstacle_field_build_is_sorted_deduped_and_order_independent() {
        let min_radius = 20.0;
        // Three qualifying bodies in deliberately UNSORTED position order, plus
        // one sub-min body that must be filtered out.
        let big = [
            (Vec2::new(70.0, 10.0), 30.0),
            (Vec2::new(-40.0, 5.0), 25.0),
            (Vec2::new(70.0, -10.0), 40.0), // same x as the first → y breaks the tie
        ];
        let small = (Vec2::new(0.0, 0.0), 5.0); // below min_radius → dropped

        let forward: Vec<(Vec2, f32)> = big.iter().copied().chain(std::iter::once(small)).collect();
        let mut reversed = forward.clone();
        reversed.reverse();

        let f1 = ObstacleField::build(forward.into_iter(), min_radius);
        let f2 = ObstacleField::build(reversed.into_iter(), min_radius);

        // (a) Build-order independence: same input set → identical output.
        assert_eq!(
            f1.obstacles, f2.obstacles,
            "obstacle field is independent of source iteration order"
        );
        // (b) The sub-min body is filtered out; the three big ones survive.
        assert_eq!(f1.obstacles.len(), 3, "small debris is not an obstacle");
        assert!(
            f1.obstacles.iter().all(|&(_, r)| r >= min_radius),
            "every retained obstacle clears the min radius"
        );
        // (c) Sorted by (x bits, y bits): the canonical key the build uses — the
        // determinism property is cross-run STABILITY, not numeric monotonicity
        // (raw f32 bits order negatives after positives; that is fine — every run
        // agrees).
        let mut sorted = f1.obstacles.clone();
        sorted.sort_by_key(|&(p, _)| (p.x.to_bits(), p.y.to_bits()));
        assert_eq!(
            f1.obstacles, sorted,
            "obstacles are sorted by position bits"
        );
        // (d) The two same-x bodies are ordered by y BITS — for f32, +10 sorts
        // before −10 (the sign bit lands negatives last). The exact order is
        // immaterial; that it is DETERMINISTIC (matches the to_bits key) is.
        let xs70: Vec<f32> = f1
            .obstacles
            .iter()
            .filter(|&&(p, _)| p.x == 70.0)
            .map(|&(p, _)| p.y)
            .collect();
        assert_eq!(
            xs70,
            vec![10.0, -10.0],
            "the position-bits tiebreak orders equal-x bodies deterministically by y bits"
        );
    }
}
