//! Connectivity flood-fill + chunk severing (US3, Phase 5, FR-015/016).
//!
//! When a section is destroyed and its cells are removed from the ship's
//! [`FitLayout`], the **remaining** cell-grid may no longer be one connected piece.
//! This module finds the disconnected regions ([`connected_region`], T026) and
//! splits each into a drifting physics body ([`sever_chunk`], T027) that inherits
//! the parent's center-of-mass momentum (INV-D07) — so a blown-off wing drifts away,
//! never zero-velocity-pops.
//!
//! **Connectivity is a flood-fill ONLY at a destruction event** (INV-D08): nothing
//! here runs per frame. The destruction worker ([`on_section_destroyed`](super::destruction::on_section_destroyed),
//! T028) is the sole caller; a tick where no section reached `0` does no flood-fill.
//!
//! The severed chunk is a **new authoritative ECS entity** (server-authoritative,
//! INV-D16) carrying the body components ([`Position`]/[`Velocity`]/[`Heading`]/
//! [`AngularVelocity`]) + a residual [`FitLayout`] of just its cells. It introduces
//! **no new physics engine**: the existing E001/E002 flight/motion systems advance
//! it (this fn does not step physics itself), and Phase 6 salvages it.
//!
//! Derive discipline matches the rest of `sim`: [`Component`] on [`Wreck`]; serde as
//! the replication (E003) / persistence (E004) seam — present, not exercised this
//! epic; value semantics.

use std::collections::{BTreeSet, HashSet, VecDeque};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::Vec2;
use serde::{Deserialize, Serialize};

use super::salvage::SalvageOutcome;
use crate::components::{
    AngularVelocity, CollisionRadius, Destructible, Heading, MeshAnchor, Position, Velocity,
};
use crate::fitting::{Cell, Fit, FitLayout, HullCatalog, CELL_WORLD_SIZE};
use crate::motion::BodyState;

/// Why a [`Wreck`] exists (data-model.md `WreckOrigin`, FR-020).
///
/// A whole ship that died ([`DestroyedShip`](WreckOrigin::DestroyedShip)) vs a
/// disconnected region severed off a still-living ship
/// ([`SeveredChunk`](WreckOrigin::SeveredChunk)). The persistent lootable spawn that
/// consumes this is Phase 6 (T032); Phase 5 only needs the tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WreckOrigin {
    /// The whole ship was destroyed (core gone / structure collapsed, INV-D15).
    DestroyedShip,
    /// A disconnected hull region severed off a still-living ship (FR-015/016).
    SeveredChunk,
}

/// A severed-but-still-physical hull region split off a living ship
/// (contracts/damage-api.md `WreckChunk`, FR-015/016).
///
/// The product of [`sever_chunk`]: a drifting [`BodyState`] (inherited COM momentum,
/// INV-D07), the (sorted, deterministic) [`Cell`]s it carried away, and its salvage.
/// The `salvage` is populated in Phase 6 (T031); Phase 5 leaves it an **empty**
/// `Vec` (no salvage is decided until the chunk is salvaged).
#[derive(Clone, Debug, PartialEq)]
pub struct WreckChunk {
    /// The chunk's drift kinematics — position + the inherited COM velocity
    /// (INV-D07). Reuses [`BodyState`]; no new physics engine.
    pub body: BodyState,
    /// The cells severed into this chunk, sorted for determinism (Principle II).
    pub cells: Vec<Cell>,
    /// Per-module salvage; **empty** this epic (decided in Phase 6, T031).
    pub salvage: Vec<SalvageOutcome>,
}

/// A persistent, lootable wreck — a destroyed ship **or** a severed region
/// (data-model.md `Wreck`, contracts/damage-api.md `Wreck`, FR-020).
///
/// A `bevy_ecs` [`Component`] on a persistent physical world entity (it co-occurs
/// with [`Position`]/[`Velocity`]/[`Heading`]/[`AngularVelocity`] + a residual
/// [`FitLayout`]). `claimed` flips exactly once (single-resolution, INV-D10). The
/// persistent-wreck **spawn** + the `contents` salvage population are Phase 6
/// (T032/T031); Phase 5 only fixes the shape (and uses it as a minimal
/// whole-ship-destroyed marker, INV-D15) — so `contents` is left **empty** here.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wreck {
    /// Why this wreck exists (whole-ship vs severed chunk).
    pub origin: WreckOrigin,
    /// The salvageable contents; **empty** this epic (populated in Phase 6, T031).
    pub contents: Vec<SalvageOutcome>,
    /// Whether the wreck has been looted; flips once (single-resolution, INV-D10).
    pub claimed: bool,
}

impl Wreck {
    /// A fresh, unclaimed wreck of the given origin with no contents yet. The salvage
    /// walk that populates `contents` runs at the spawn site
    /// ([`salvage_layout`](super::salvage::salvage_layout), T031/T032); a wreck spawned
    /// without resolvable content resources stays empty until claimed.
    pub fn new(origin: WreckOrigin) -> Self {
        Self {
            origin,
            contents: Vec::new(),
            claimed: false,
        }
    }

    /// Loot this wreck **exactly once** (T031, INV-D10): the first call returns the
    /// precomputed [`contents`](Wreck::contents) and flips `claimed`; every later call
    /// returns an **empty** `Vec` (no double-claim).
    ///
    /// Damage applies regardless of who dealt it (friendly/hostile is irrelevant to
    /// the outcome), but the **loot** is single-resolution — claimed once. Reading the
    /// (unclaimed) contents without consuming them is
    /// [`salvage`](super::salvage::salvage); this is the consuming claim E013 calls.
    pub fn claim(&mut self) -> Vec<SalvageOutcome> {
        if self.claimed {
            return Vec::new();
        }
        self.claimed = true;
        self.contents.clone()
    }
}

/// The ship's core/command cell — the most-interior occupied cell (MVP convention).
///
/// There is **no `core` field** on the hull, so E007 adopts this convention: the
/// core is the cell with the **maximum** [`CellOccupant.depth`](crate::fitting::CellOccupant::depth)
/// (smaller depth = outer; larger = more interior), ties broken by [`Cell`]
/// `BTreeMap` order for determinism (Principle II). Rationale: central placement is
/// the hardest to sever — "cores are graph-deep", matching the depth/occlusion model
/// (INV-F10). Returns `None` when no cells remain (the whole ship is gone →
/// whole-ship-destroyed, INV-D15).
///
/// The flood-fill itself is **core-agnostic**: [`connected_region`] takes the core
/// as a parameter, so this convention is isolated here.
pub fn core_cell(layout: &FitLayout) -> Option<Cell> {
    // `layout.cells` is a `BTreeMap<Cell, _>`, so iterating yields cells in `Cell`
    // sorted order; `max_by_key` keeps the LAST max on ties, so to break ties by the
    // smallest `Cell` deterministically we fold manually preferring the earlier cell.
    let mut best: Option<(u16, Cell)> = None;
    for (&cell, occ) in &layout.cells {
        match best {
            // Strictly deeper wins; an equal depth keeps the earlier (smaller) cell
            // because the BTreeMap iteration already visits the smaller cell first.
            Some((best_depth, _)) if occ.depth <= best_depth => {}
            _ => best = Some((occ.depth, cell)),
        }
    }
    best.map(|(_, cell)| cell)
}

/// The 4-neighbour (von Neumann) cells of `cell` — `±1` in column OR row.
///
/// Saturating arithmetic guards the grid edge (a `0` coordinate yields no negative
/// neighbour); the caller filters to cells that actually remain in the layout.
fn neighbours(cell: Cell) -> [Cell; 4] {
    let (c, r) = cell;
    [
        (c.saturating_sub(1), r),
        (c.saturating_add(1), r),
        (c, r.saturating_sub(1)),
        (c, r.saturating_add(1)),
    ]
}

/// The set of cells still attached to `core` over the **remaining** hull grid
/// (`layout.cells`), by a 4-neighbour flood-fill (T026, FR-015).
///
/// BFS over the remaining cell keyset using von Neumann ([`neighbours`]) adjacency,
/// starting from `core`. The returned [`HashSet`] is what stays connected to the
/// core; any cell in `layout.cells` **not** in this set is part of a disconnected
/// region the caller severs. Visitation uses a sorted frontier (the `BTreeSet`
/// pending queue) for deterministic order (Principle II), though the result is a set.
///
/// **Core-severed edge (INV-D15)**: if `core` is not present in `layout.cells` (the
/// core cell was itself destroyed) the flood-fill returns an **empty** set, which
/// signals whole-ship-destroyed to the caller (every remaining cell is "disconnected
/// from a missing core"). The caller ([`on_section_destroyed`](super::destruction::on_section_destroyed))
/// handles that as the whole-ship path rather than severing the entire ship into
/// chunks.
pub fn connected_region(layout: &FitLayout, core: Cell) -> HashSet<Cell> {
    let mut attached: HashSet<Cell> = HashSet::new();
    // The core itself must remain on the grid; otherwise there is nothing to anchor
    // connectivity to → empty set (whole-ship-destroyed signal, INV-D15).
    if !layout.cells.contains_key(&core) {
        return attached;
    }

    // A FIFO queue gives a clean BFS; we seed deterministically and push neighbours
    // in the sorted `neighbours` order so traversal is reproducible.
    let mut frontier: VecDeque<Cell> = VecDeque::new();
    frontier.push_back(core);
    attached.insert(core);

    while let Some(cell) = frontier.pop_front() {
        for n in neighbours(cell) {
            if n == cell {
                // A saturating-edge self-loop (e.g. col 0's "left" is itself); skip.
                continue;
            }
            if layout.cells.contains_key(&n) && attached.insert(n) {
                frontier.push_back(n);
            }
        }
    }

    attached
}

/// The local-space center of a unit cell `(col, row)` — its mid-point on the grid
/// (`coord + 0.5`), matching the E006 layout cell-space.
fn cell_center(cell: Cell) -> Vec2 {
    Vec2::new(cell.0 as f32 + 0.5, cell.1 as f32 + 0.5)
}

/// The mean cell-center (local-space center of mass) of a non-empty cell set.
///
/// `None` for an empty set (defensive — the caller never severs an empty region).
fn local_com<'a>(cells: impl IntoIterator<Item = &'a Cell>) -> Option<Vec2> {
    let mut sum = Vec2::ZERO;
    let mut count = 0u32;
    for &cell in cells {
        sum += cell_center(cell);
        count += 1;
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f32)
    }
}

/// The collision-circle radius (world units) for a severed chunk, sized to the
/// **chunk's own cell footprint** so a drifting piece is shootable with a collider that
/// matches what is rendered (destructible wreckage).
///
/// It is the half-extent of the chunk's cell bounding-box **longest** axis in world
/// units: `max(width_cells, height_cells) · `[`CELL_WORLD_SIZE`]` · 0.5`, mirroring
/// [`hull_collision_radius`](crate::fitting::hull_collision_radius) but over the chunk's
/// extent rather than the whole hull. A **one-cell** chunk has a `1×1` bbox → a small but
/// non-zero radius (`1 · CELL_WORLD_SIZE · 0.5`); a `CHUNK_MIN_CELLS`-cell floor on the
/// span guarantees even a single-cell sliver still has a finite radius (never `0`, so the
/// swept-cast can hit it). `0.0` only for the degenerate empty set (never severed).
fn chunk_collision_radius(cells: &[Cell]) -> f32 {
    /// The minimum bbox span (in cells) used to floor the chunk radius — a one-cell chunk
    /// still gets a `1`-cell footprint rather than a zero radius.
    const CHUNK_MIN_CELLS: u16 = 1;
    let mut min_col = u16::MAX;
    let mut max_col = u16::MIN;
    let mut min_row = u16::MAX;
    let mut max_row = u16::MIN;
    for &(col, row) in cells {
        min_col = min_col.min(col);
        max_col = max_col.max(col);
        min_row = min_row.min(row);
        max_row = max_row.max(row);
    }
    if min_col > max_col {
        return 0.0; // empty set (defensive — the caller never severs an empty region)
    }
    // Bounding-box span in cells (+1 because both endpoints are inclusive), floored so a
    // 1-cell chunk keeps a finite footprint.
    let width = (max_col - min_col + 1).max(CHUNK_MIN_CELLS);
    let height = (max_row - min_row + 1).max(CHUNK_MIN_CELLS);
    width.max(height) as f32 * CELL_WORLD_SIZE * 0.5
}

/// The carrier hull's `(cols, rows)` grid dimensions for `ship`, resolved through its
/// [`Fit`] + the [`HullCatalog`] resource — the same lookup
/// [`on_section_destroyed`](super::destruction::on_section_destroyed) and
/// [`hull_collision_radius`](crate::fitting::hull_collision_radius) use.
///
/// `None` when the ship has no `Fit`, the `HullCatalog` resource is absent, or the
/// hull id does not resolve (a minimal test world) — the caller then falls back to a
/// zero offset rather than panicking (INV-D16). The grid centre derived from these
/// dims is the point the render centres the ship on, so a severed chunk's spawn offset
/// matches the visible hull.
fn grid_dims_of(world: &World, ship: Entity) -> Option<(u16, u16)> {
    let fit = world.get::<Fit>(ship)?;
    let hulls = world.get_resource::<HullCatalog>()?;
    hulls.get(fit.hull).map(|h| h.grid_dims)
}

/// Split the disconnected region `cells` off `ship` into a new drifting physics
/// body, inheriting the parent's COM momentum (T027, FR-016, INV-D07).
///
/// Steps (data-model.md "Resolution Order" §263, contracts/damage-api.md §3):
///
/// 1. Read the parent's [`Position`]/[`Velocity`]/[`Heading`]/[`AngularVelocity`]
///    (any missing component defaults to zero so a minimal test ship works).
/// 2. The chunk's **local** COM = mean of its cells' centers (in cell-space). The
///    offset reference is the hull's **grid centre** `(cols·0.5, rows·0.5)` — the SAME
///    point the render centres the ship's cells on (`build_hull_mesh`'s `center`), and
///    the point the ship's `Position` sits at — NOT the mean of the remaining cells. So
///    a chunk appears EXACTLY where its cells were drawn on the live hull, then drifts.
///    The cell-space offset `r_local = (Δcol, Δrow) = chunk_com − grid_centre` is then
///    mapped into the ship's LOCAL WORLD frame to match the render convention
///    (`build_hull_mesh`): forward(`+X`) ← `row`, lateral(`+Y`) ← `col`, scaled by
///    [`CELL_WORLD_SIZE`] — i.e. `r_local_world = (Δrow, Δcol)·CELL_WORLD_SIZE`. That is
///    finally rotated into world by the parent `Heading`
///    (`r = Vec2::from_angle(heading).rotate(r_local_world)`), giving the chunk's world
///    position `parent.pos + r`. (The old code rotated the raw `(Δcol, Δrow)` cell
///    vector directly — unscaled and axis-swapped — so the chunk teleported ~3× off and
///    onto the wrong axis; this is the teleport fix.)
/// 3. **Inherit COM momentum (INV-D07)**: `chunk_vel = parent.vel + angvel ×ᵣ` where
///    the 2D rigid-rotation term at the chunk COM is `angvel * (-r.y, r.x)` — the
///    linear velocity plus the rotation-induced velocity at the chunk's COM, so it
///    drifts (never a zero-velocity pop). The chunk also inherits the parent's
///    `Heading` and `AngularVelocity`.
/// 4. **Move** the cells out of the parent's [`FitLayout`] into the chunk (removed
///    from the parent, collected + **sorted** for determinism), and spawn a NEW
///    world entity carrying the body components + a residual `FitLayout` of just
///    those cells (same `hull` id) — so the existing motion systems advance it and
///    Phase 6 can salvage it.
///
/// A single orphan cell severs cleanly as a (small) chunk — no dangling
/// un-targetable fragment (INV-D09). Server-authoritative spawn (INV-D16); this fn
/// does **not** step physics (the flight/motion systems do).
pub fn sever_chunk(world: &mut World, ship: Entity, cells: &HashSet<Cell>) -> WreckChunk {
    // --- 1. Parent body state (default any missing component to zero) -----------
    let parent_pos = world
        .get::<Position>(ship)
        .map(|p| p.0)
        .unwrap_or(Vec2::ZERO);
    let parent_vel = world
        .get::<Velocity>(ship)
        .map(|v| v.0)
        .unwrap_or(Vec2::ZERO);
    let heading = world.get::<Heading>(ship).map(|h| h.0).unwrap_or(0.0);
    let angvel = world
        .get::<AngularVelocity>(ship)
        .map(|a| a.0)
        .unwrap_or(0.0);

    // --- 2. Offset from the hull grid centre to the chunk COM, rotated into world ---
    // Collect the chunk cells in deterministic (sorted) order; reused for the body
    // and the residual layout.
    let mut chunk_cells: Vec<Cell> = cells.iter().copied().collect();
    chunk_cells.sort_unstable();

    let chunk_com_local = local_com(&chunk_cells).unwrap_or(Vec2::ZERO);
    let parent_layout = world.get::<FitLayout>(ship).cloned();
    // The offset reference is the parent's RENDER/CARVE anchor — the cell-space point whose
    // world location IS the parent's `Position` (the point the render centres its cells on).
    // For a LIVE ship that is the hull GRID CENTRE (resolved from its `Fit` + `HullCatalog`).
    // For a `Wreck` (which has NO `Fit`) it is the parent's frozen [`MeshAnchor`] (Fix #6/#7) —
    // WITHOUT this, severing a piece off a wreck (splitting it) fell back to the chunk COM (a
    // ZERO offset), so the sub-chunk spawned ON TOP of the parent and the halves overlapped
    // ("fell together"). With the parent anchor, `r_local = chunk_com − parent_anchor` is the
    // real cell-space offset of the sub-region within the parent → it spawns where its cells
    // were → the halves SEPARATE. Falls back to the chunk COM only in a minimal test world
    // with neither a `MeshAnchor` nor a resolvable hull.
    let grid_centre_local = world
        .get::<MeshAnchor>(ship)
        .map(|a| a.0)
        .or_else(|| {
            grid_dims_of(world, ship)
                .map(|(cols, rows)| Vec2::new(cols as f32 * 0.5, rows as f32 * 0.5))
        })
        .unwrap_or(chunk_com_local);

    // Cell-space offset `(Δcol, Δrow)`. Map it into the ship's LOCAL WORLD frame the
    // SAME way the render does (`build_hull_mesh`): forward(+X) ← row, lateral(+Y) ← col,
    // scaled by `CELL_WORLD_SIZE`. So `X = Δrow`, `Y = Δcol`, ×CELL_WORLD_SIZE.
    let r_local = chunk_com_local - grid_centre_local;
    let r_local_world = Vec2::new(r_local.y, r_local.x) * CELL_WORLD_SIZE;
    // Rotate the local-world offset into world by the parent's heading.
    let r = Vec2::from_angle(heading).rotate(r_local_world);
    let chunk_pos = parent_pos + r;

    // --- 3. Inherit COM momentum: linear + rigid-rotation term at the chunk COM --
    // 2D cross of scalar angvel × vector r is the perpendicular `angvel*(-r.y, r.x)`.
    let chunk_vel = parent_vel + angvel * Vec2::new(-r.y, r.x);

    // --- 4. Move the cells out of the parent layout into the chunk --------------
    let mut residual_cells = crate::fitting::CellMap::new();
    let hull_id = parent_layout.as_ref().map(|l| l.hull);
    if let Some(mut parent) = world.get_mut::<FitLayout>(ship) {
        for cell in &chunk_cells {
            if let Some(occ) = parent.cells.remove(cell) {
                residual_cells.insert(*cell, occ);
            }
        }
    }

    // --- 5. Decide the chunk's salvage contents ONCE, at spawn (T032, INV-D09) ---
    // The chunk's residual cells (with their *live* preserved health) are walked into
    // intact-vs-scrap loot now (the compute step; `salvage`/`claim` only read it). A
    // minimal test world without the `ModuleCatalog`/`SalvageConfig` resources falls
    // back to empty contents (no panic) — the chunk still spawns and drifts.
    let chunk_layout = hull_id.map(|hull| FitLayout {
        hull,
        cells: residual_cells.clone(),
    });
    let contents = match (
        chunk_layout.as_ref(),
        world.get_resource::<crate::fitting::ModuleCatalog>(),
        world.get_resource::<super::content::SalvageConfig>(),
    ) {
        (Some(layout), Some(catalog), Some(cfg)) => {
            super::salvage::salvage_layout(layout, catalog, cfg)
        }
        _ => Vec::new(),
    };

    // Spawn the NEW chunk entity: the body components + a residual cell-grid (same
    // hull id) so the existing flight/motion systems advance it and Phase 6 salvages
    // it. If the parent had no layout (a minimal test ship), the chunk still spawns
    // with its body so momentum inheritance is observable. The persistent `Wreck`
    // carries the precomputed lootable `contents`.
    //
    // **Destructible wreckage**: the chunk also spawns with a [`CollisionRadius`] sized to
    // its OWN cell footprint ([`chunk_collision_radius`]) + a [`Destructible`] marker, so
    // it is shootable — a hit erodes it further (and despawns it when fully carved) via the
    // `fitted_damage_system` carve query (`With<FitLayout>, With<Destructible>`) and the
    // wreck branch of [`on_cells_carved`]. The radius matches the chunk's footprint (not
    // the whole hull), so the collider tracks what is rendered.
    let chunk_radius = chunk_collision_radius(&chunk_cells);
    let mut entity = world.spawn((
        Position(chunk_pos),
        Velocity(chunk_vel),
        Heading(heading),
        AngularVelocity(angvel),
        CollisionRadius(chunk_radius),
        Destructible,
        // FROZEN render/carve anchor (Fix #6): the chunk's cell-COM AT SEVER — the cell-space
        // point whose world location is `chunk_pos`. Render + carve resolve to this fixed
        // reference, so carving a cell off the chunk later does not re-centre (shift) it.
        MeshAnchor(chunk_com_local),
        Wreck {
            origin: WreckOrigin::SeveredChunk,
            contents: contents.clone(),
            claimed: false,
        },
    ));
    if let Some(hull) = hull_id {
        entity.insert(FitLayout {
            hull,
            cells: residual_cells,
        });
    }

    WreckChunk {
        body: BodyState::new(chunk_pos, chunk_vel),
        cells: chunk_cells,
        salvage: contents,
    }
}

/// Enumerate the disconnected regions of `layout` (the cells NOT in `attached`),
/// each a maximal 4-connected component, in a **deterministic** order (sorted by the
/// component's smallest-cell representative) — the order
/// [`on_section_destroyed`](super::destruction::on_section_destroyed) severs them in
/// (Principle II).
///
/// `attached` is the [`connected_region`] result (the cells still attached to the
/// core). This groups every remaining cell **outside** it into its connected
/// component so each severs as exactly one chunk.
pub(crate) fn disconnected_regions(
    layout: &FitLayout,
    attached: &HashSet<Cell>,
) -> Vec<HashSet<Cell>> {
    // The disconnected cells, in sorted order so component discovery is reproducible.
    let pending: BTreeSet<Cell> = layout
        .cells
        .keys()
        .copied()
        .filter(|c| !attached.contains(c))
        .collect();

    let mut visited: HashSet<Cell> = HashSet::new();
    let mut regions: Vec<HashSet<Cell>> = Vec::new();

    for &seed in &pending {
        if visited.contains(&seed) {
            continue;
        }
        // Flood the component containing `seed` over the disconnected cells.
        let mut region: HashSet<Cell> = HashSet::new();
        let mut frontier: VecDeque<Cell> = VecDeque::new();
        frontier.push_back(seed);
        visited.insert(seed);
        region.insert(seed);
        while let Some(cell) = frontier.pop_front() {
            for n in neighbours(cell) {
                if n != cell && pending.contains(&n) && visited.insert(n) {
                    region.insert(n);
                    frontier.push_back(n);
                }
            }
        }
        regions.push(region);
    }

    // Deterministic order: by each region's smallest-cell representative.
    regions.sort_by_key(|r| r.iter().copied().min().unwrap_or((u16::MAX, u16::MAX)));
    regions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::{CellOccupant, HullId, SlotId};

    /// Build a `FitLayout` over the given cells, each at the given depth, with a
    /// dummy occupant — enough to drive connectivity/core/sever in isolation.
    fn layout_with(cells: &[(Cell, u16)]) -> FitLayout {
        let mut map = crate::fitting::CellMap::new();
        for &(cell, depth) in cells {
            map.insert(
                cell,
                CellOccupant {
                    slot: SlotId(0),
                    module: None,
                    health: 1.0,
                    depth,
                    structural: false,
                },
            );
        }
        FitLayout {
            hull: HullId(1),
            cells: map,
        }
    }

    #[test]
    fn core_cell_is_the_deepest_occupied_cell() {
        // (1,1) is deepest (depth 2); ties would break to the smaller cell.
        let layout = layout_with(&[((0, 0), 0), ((1, 0), 0), ((1, 1), 2), ((0, 1), 1)]);
        assert_eq!(core_cell(&layout), Some((1, 1)));
        // No cells → whole ship gone.
        let empty = layout_with(&[]);
        assert_eq!(core_cell(&empty), None);
    }

    #[test]
    fn core_cell_breaks_depth_ties_by_smallest_cell() {
        // Two equally-deep cells: the smaller `Cell` (BTreeMap order) wins.
        let layout = layout_with(&[((2, 2), 5), ((0, 1), 5), ((3, 3), 1)]);
        assert_eq!(core_cell(&layout), Some((0, 1)));
    }

    #[test]
    fn connected_region_floods_a_contiguous_line() {
        // A 4-cell horizontal line, all attached to the core at one end.
        let layout = layout_with(&[((0, 0), 0), ((1, 0), 0), ((2, 0), 0), ((3, 0), 0)]);
        let region = connected_region(&layout, (0, 0));
        assert_eq!(region.len(), 4);
        for c in [(0, 0), (1, 0), (2, 0), (3, 0)] {
            assert!(region.contains(&c));
        }
    }

    #[test]
    fn connected_region_excludes_a_gap_separated_island() {
        // Core half: (0,0)-(1,0). A gap at (2,0) is missing → (3,0)-(4,0) is an
        // island disconnected from the core.
        let layout = layout_with(&[((0, 0), 0), ((1, 0), 0), ((3, 0), 0), ((4, 0), 0)]);
        let region = connected_region(&layout, (0, 0));
        assert_eq!(region.len(), 2);
        assert!(region.contains(&(0, 0)) && region.contains(&(1, 0)));
        assert!(!region.contains(&(3, 0)) && !region.contains(&(4, 0)));

        // The island is the one disconnected region.
        let regions = disconnected_regions(&layout, &region);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].len(), 2);
        assert!(regions[0].contains(&(3, 0)) && regions[0].contains(&(4, 0)));
    }

    #[test]
    fn missing_core_returns_empty_set() {
        // The core (2,2) is not on the grid → whole-ship-destroyed signal (INV-D15).
        let layout = layout_with(&[((0, 0), 0), ((1, 0), 0)]);
        assert!(connected_region(&layout, (2, 2)).is_empty());
    }
}
