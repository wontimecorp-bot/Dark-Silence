//! The fit layout — the fit IS the hit/armor map (FR-018/019/020/021, ADR-0008,
//! AD-002/AD-005).
//!
//! ADR-0008's keystone consequence: a ship's positional [`Hull`] cell-grid, once
//! a [`Fit`] populates its slots, **is** the hitbox/armor map E007 resolves damage
//! against. This module realizes that contract:
//!
//! - [`build_layout`] turns a [`Fit`] (+ its [`Hull`] and the [`ModuleCatalog`])
//!   into a [`FitLayout`]: a per-cell [`CellOccupant`] map covering **every**
//!   authored hull cell (INV-F11), each carrying the slot it belongs to, the
//!   module installed there (if any), that module's live `health`, and an
//!   occlusion `depth` (outer cells = lower depth — encountered first along a hit
//!   line, INV-F10).
//! - [`module_at`] / [`cell_map`] are the E007 per-cell lookups (FR-019): what
//!   occupies a cell and the live occupant + health map.
//! - [`resolve_hit`] traces a segment across the grid with the **existing** swept
//!   point-vs-circle primitive ([`Physics::swept_cast`], reused — not reinvented)
//!   and returns the FIRST module struck **outer-before-inner by depth** (INV-F10,
//!   FR-018/021): a centrally-placed module is shielded by the cells covering it.
//! - [`hardpoint_arc`] derives a weapon mount's position/facing firing arc
//!   ([`FiringArc`], FR-020), bounded `(0, π]` (INV-F12); `None` for a non-weapon
//!   slot.
//!
//! E006 **produces** this map + the first-hit geometry + the arcs; E007 **consumes**
//! them (penetration, defense channels, destruction). E006 does not mutate health
//! — it exposes it (contracts/fitting-api.md §3).
//!
//! Derive discipline matches the rest of the fitting domain: serde as a
//! replication/persistence seam (not exercised this epic), value semantics; a
//! `BTreeMap` keys the cells so iteration/order is deterministic (Principle II).

use std::collections::BTreeMap;

use bevy_ecs::component::Component;
use glam::Vec2;
use serde::{Deserialize, Serialize};

use super::content::{ModuleCatalog, STRUCT_CELL_MASS};
use super::fit::{Fit, ModuleRef};
use super::hull::{FiringArc, Hull, HullId, SlotId, CELL_WORLD_SIZE};
use crate::physics::{Physics, RapierPhysics};

/// A grid cell coordinate `(col, row)` on the hull — the shared fitting / hit-map
/// unit (contracts/fitting-api.md `Cell`). Mirrors [`super::hull::GridCell::coord`].
pub type Cell = (u16, u16);

/// What occupies one cell of the hull grid, its live health, and its occlusion
/// depth (data-model.md `CellOccupant`).
///
/// A cell is one of two **kinds** (mirroring [`GridCell::structural`](crate::fitting::GridCell::structural)):
/// - a **module cell** (`structural == false`) on a slot coord: when a module is
///   installed it carries that module's id + live `health` (seeded from `health_max`);
///   an empty module-slot cell carries `module: None`, `health: 0.0` (no installed
///   device to damage).
/// - a **structural cell** (`structural == true`): filler hull plating with no slot,
///   seeded with [`STRUCT_CELL_HP`](crate::fitting::content::STRUCT_CELL_HP) so the
///   dense hull body has hit points Phase 2 can carve away (Phase 1A).
///
/// A cell's `depth` orders it along a hit line: **smaller depth = outer**, encountered
/// first (INV-F10).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellOccupant {
    /// The slot this cell belongs to. A structural cell carries the sentinel
    /// `SlotId(u32::MAX)` (no slot identity).
    pub slot: SlotId,
    /// The module installed in that slot, or `None` for an empty / structural cell
    /// (FR-019).
    pub module: Option<super::module::ModuleId>,
    /// Live hit points (`>= 0`). A module cell is seeded from the installed module's
    /// `health_max` (`0.0` when empty); a **structural** cell from `STRUCT_CELL_HP`.
    /// E007 mutates this as it applies damage; E006 only exposes it.
    pub health: f32,
    /// Occlusion depth: **smaller = outer** (encountered first along a hit line).
    /// A fully-interior cell has a larger depth and is reached only after the
    /// covering cells (INV-F10).
    pub depth: u16,
    /// `true` for a **structural** filler cell, `false` for a **module** (slot) cell —
    /// mirrors [`GridCell::structural`](crate::fitting::GridCell::structural). Lets
    /// downstream code (Phase 1B voxel rendering, Phase 2 carving) tell hull plating
    /// from hardpoints without re-deriving the slot-coord match.
    pub structural: bool,
}

/// The full per-cell occupant + health map E007 consumes (contracts/fitting-api.md
/// `CellMap`). Keyed by [`Cell`] for deterministic iteration; mirrors the
/// occupied-and-empty completeness of [`FitLayout`] but exposes only the occupant
/// value (the E007 read surface, FR-019).
pub type CellMap = BTreeMap<Cell, CellOccupant>;

/// The fit-layout hit/armor map — the queryable hitbox E007 reads (data-model.md
/// `FitLayout`; ADR-0008). Built from a [`Fit`] by [`build_layout`] and rebuilt on
/// every fit change (INV-F08); attached to the ship entity as a `bevy_ecs`
/// [`Component`].
///
/// Every authored hull cell is present (INV-F11): the map covers the whole chassis
/// so a hit anywhere on the grid resolves to a defined occupant (or an empty
/// structural cell). Per-cell `health` is the live module health (mutated by E007).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitLayout {
    /// The hull this layout was built for (resolved in `HullCatalog`).
    pub hull: HullId,
    /// Per-cell occupant; covers **every** authored hull cell (INV-F11).
    pub cells: CellMap,
}

impl FitLayout {
    /// The occupant of `cell`, if that cell is authored on the hull (null-safe).
    pub fn occupant(&self, cell: Cell) -> Option<&CellOccupant> {
        self.cells.get(&cell)
    }
}

/// The cell-space **center** that both the carve entry ray and the armor-angle radial
/// anchor on for `layout` — chosen to MATCH the client's `hull_mesh_center` so the carve
/// enters, and the impact angle is measured, where the cells actually are:
///
/// - a **`Wreck`** target (`is_wreck == true`) → the **cell-COM** `mean(col+0.5, row+0.5)`
///   over its CURRENT [`cells`](FitLayout::cells). A severed chunk's `Position` is its
///   cell-COM and its cells render around it, so an off-centre piece is referenced where
///   it sits — not at the (often empty) original grid centre.
/// - a **live ship** (`is_wreck == false`) → the **grid centre** `(cols·0.5, rows·0.5)`:
///   its `Position` sits at the grid centre (byte-identical to the prior behaviour).
///
/// Single-sources the centre formula for both `fitted_damage_system` (the entry point) and
/// `apply_damage` (the armor angle) so the two references can never drift apart — that
/// drift was the wreckage-ricochet bug. Deterministic: the `BTreeMap` cells iterate in
/// sorted [`Cell`] order.
pub fn layout_center(layout: &FitLayout, grid_dims: (u16, u16), is_wreck: bool) -> Vec2 {
    if is_wreck {
        let n = layout.cells.len().max(1) as f32;
        let sum = layout.cells.keys().fold(Vec2::ZERO, |acc, &(col, row)| {
            acc + Vec2::new(col as f32 + 0.5, row as f32 + 0.5)
        });
        sum / n
    } else {
        Vec2::new(grid_dims.0 as f32 * 0.5, grid_dims.1 as f32 * 0.5)
    }
}

/// Resolve the cell-space centre the carve + armor angle anchor on: the **frozen**
/// [`MeshAnchor`](crate::components::MeshAnchor) when the entity has one (a wreck captured
/// its reference at creation), else the live [`layout_center`] fallback (cell-COM for a
/// wreck, grid centre for a ship). Single-sources the choice for both
/// [`fitted_damage_system`](crate::collision::fitted_damage_system) (the entry point) and
/// [`apply_damage`](crate::damage::apply_damage) (the armor angle) so they never diverge,
/// and freezes a wreck's reference so carving a cell does not shift the piece (it no longer
/// chases the moving COM).
pub fn center_or_anchor(
    anchor: Option<Vec2>,
    layout: &FitLayout,
    grid_dims: (u16, u16),
    is_wreck: bool,
) -> Vec2 {
    anchor.unwrap_or_else(|| layout_center(layout, grid_dims, is_wreck))
}

/// The local-space center of a unit cell `(col, row)` — its mid-point on the grid
/// (`coord + 0.5`). The hit-line `p0`→`p1` and the cell circles in [`resolve_hit`]
/// share this local cell-space.
fn cell_center(coord: Cell) -> Vec2 {
    Vec2::new(coord.0 as f32 + 0.5, coord.1 as f32 + 0.5)
}

/// The inertial **mass of one cell** (Phase M5) — the single source of truth for a body's mass.
/// A **module cell** (an installed, catalog-resolvable module) weighs that module's authored
/// `mass`; a **structural / empty / dangling** cell weighs [`STRUCT_CELL_MASS`]. Summed over a
/// [`FitLayout`]'s cells ([`layout_mass`]) it gives the body's mass for flight acceleration,
/// projectile knockback, wreck drift, and inertia alike — so mass is continuous as a ship erodes
/// into a wreck and reflects what the body is actually made of (a reactor cell outweighs plating).
///
/// Uses the compile-time [`STRUCT_CELL_MASS`]; for the live-tunable path (the dev panel's
/// `SimTuning.struct_cell_mass`) call [`cell_mass_with`].
pub fn cell_mass(occupant: &CellOccupant, modules: &ModuleCatalog) -> f32 {
    cell_mass_with(occupant, modules, STRUCT_CELL_MASS)
}

/// [`cell_mass`] with an explicit structural-cell mass (Phase M6 live tuning): a module cell
/// weighs its module's mass; a structural / empty / dangling cell weighs `struct_cell_mass`.
pub fn cell_mass_with(
    occupant: &CellOccupant,
    modules: &ModuleCatalog,
    struct_cell_mass: f32,
) -> f32 {
    occupant
        .module
        .and_then(|m| modules.get(m))
        .map(|m| m.mass)
        .unwrap_or(struct_cell_mass)
}

/// A body's total inertial **mass** = Σ [`cell_mass`] over its current cells (Phase M5). The one
/// mass `derive_ship_stats` gives flight AND `fitted_damage_system` gives the projectile-impulse +
/// wreck drift, so a live ship and the wreck it becomes share the same mass basis (no jump on
/// death). Deterministic; empty layout → `0`. Live-tunable variant: [`layout_mass_with`].
pub fn layout_mass(layout: &FitLayout, modules: &ModuleCatalog) -> f32 {
    layout_mass_with(layout, modules, STRUCT_CELL_MASS)
}

/// [`layout_mass`] with an explicit structural-cell mass (Phase M6 live tuning).
pub fn layout_mass_with(layout: &FitLayout, modules: &ModuleCatalog, struct_cell_mass: f32) -> f32 {
    layout
        .cells
        .values()
        .map(|o| cell_mass_with(o, modules, struct_cell_mass))
        .sum()
}

/// A body's **moment of inertia** about its mass-weighted centre of mass, in world units²·mass
/// (Phase M5): `Σ cell_mass·|cell_center − COM|² · CELL_WORLD_SIZE²`, using the REAL per-cell mass
/// so an off-centre hit spins a reactor/armor-heavy body less than light plating. Floored
/// `> 0` (a single-cell or massless body still divides safely). Deterministic. Live-tunable
/// variant: [`layout_inertia_with`].
pub fn layout_inertia(layout: &FitLayout, modules: &ModuleCatalog) -> f32 {
    layout_inertia_with(layout, modules, STRUCT_CELL_MASS)
}

/// [`layout_inertia`] with an explicit structural-cell mass (Phase M6 live tuning).
pub fn layout_inertia_with(
    layout: &FitLayout,
    modules: &ModuleCatalog,
    struct_cell_mass: f32,
) -> f32 {
    let total = layout_mass_with(layout, modules, struct_cell_mass);
    if total <= f32::MIN_POSITIVE {
        return f32::MIN_POSITIVE;
    }
    // Mass-weighted centre of mass in cell-space.
    let com = layout.cells.iter().fold(Vec2::ZERO, |acc, (&coord, occ)| {
        acc + cell_center(coord) * cell_mass_with(occ, modules, struct_cell_mass)
    }) / total;
    let i_cellspace: f32 = layout
        .cells
        .iter()
        .map(|(&coord, occ)| {
            cell_mass_with(occ, modules, struct_cell_mass)
                * (cell_center(coord) - com).length_squared()
        })
        .sum();
    (i_cellspace * CELL_WORLD_SIZE * CELL_WORLD_SIZE).max(f32::MIN_POSITIVE)
}

/// The occlusion depth of a cell (INV-F10): how far **inward** it sits, measured as
/// the inward offset from the grid edge toward the center. An edge cell yields `0`
/// (outermost); a cell one ring in yields `1`; the central cell yields the largest
/// depth. Computed from the per-axis distance to the nearest grid boundary so the
/// metric is a clean integer ring index independent of hull size.
///
/// Concretely: `depth = min(col, cols-1-col, row, rows-1-row)` — the cell's ring
/// from the perimeter. A hit line entering from outside crosses lower-depth (outer)
/// rings before higher-depth (inner) ones, so ordering occupants by ascending
/// `depth` gives outer-before-inner (the shield-the-interior property, FR-021).
fn cell_depth(hull: &Hull, coord: Cell) -> u16 {
    let (cols, rows) = hull.grid_dims;
    // Saturating arithmetic: an out-of-bounds coord (never authored, but defensive)
    // collapses to a 0 ring rather than underflowing.
    let from_left = coord.0;
    let from_right = cols.saturating_sub(1).saturating_sub(coord.0);
    let from_bottom = coord.1;
    let from_top = rows.saturating_sub(1).saturating_sub(coord.1);
    from_left.min(from_right).min(from_bottom).min(from_top)
}

/// Build the [`FitLayout`] hit/armor map for `fit` on `hull`, resolving installed
/// modules through `catalog` (FR-019, INV-F10/F11). **Pure** — reads only its
/// arguments, mutates nothing — so the client preview, the running sim, and a
/// future authoritative server build identical maps (Principle II).
///
/// Completeness (INV-F11): **every** authored hull cell becomes a [`CellOccupant`].
/// Health is seeded by cell **kind** (Phase 1A):
/// - a **module cell** whose owning slot holds a module carries that module's id + its
///   `health` (seeded from `health_max`); an empty module-slot cell carries
///   `module: None`, `health: 0.0` (no installed device to damage — unchanged);
/// - a **structural cell** (filler plating, no slot) carries `module: None` and
///   `health: STRUCT_CELL_HP` so the dense hull body has hit points Phase 2 can carve
///   (it is `structural == true`).
///
/// Each occupant's `depth` is its perimeter ring ([`cell_depth`]) — outer cells lower,
/// central cells higher (INV-F10).
///
/// A dangling [`ModuleId`](super::module::ModuleId) (not in `catalog`) yields a module
/// occupant with `module: None` / `health: 0.0` — the dangling-ref *rejection* is
/// `validate_fit`'s concern (INV-F13); the map stays total regardless.
///
/// **Combat-invariant (Phase 1A)**: structural HP is purely additive data. `resolve_hit`
/// iterates `hull.slots` (module cells only), so structural cells are never an entry/
/// behind target; `derive_ship_stats`/salvage key off `module.is_some()`, so neither
/// reads structural cells. The dense grid therefore changes **no** combat outcome this
/// phase — it only populates the per-cell health store for Phase 2 carving.
pub fn build_layout(hull: &Hull, fit: &Fit, catalog: &ModuleCatalog) -> FitLayout {
    build_layout_with(hull, fit, catalog, super::content::STRUCT_CELL_HP)
}

/// [`build_layout`] with an explicit structural-cell HP (Phase M6 live tuning): structural filler
/// cells are seeded with `struct_cell_hp` instead of the compile-time [`STRUCT_CELL_HP`]
/// (`crate::fitting::content::STRUCT_CELL_HP`), so the dev panel can retune hull erosion + a
/// re-derive rebuilds layouts at the new value.
pub fn build_layout_with(
    hull: &Hull,
    fit: &Fit,
    catalog: &ModuleCatalog,
    struct_cell_hp: f32,
) -> FitLayout {
    let mut cells: CellMap = BTreeMap::new();

    for grid_cell in &hull.cells {
        let coord = grid_cell.coord;

        let (slot_id, module_id, health) = if grid_cell.structural {
            // A structural filler cell: no slot identity, seeded with the tunable
            // structural HP so the dense body is carvable in Phase 2.
            (SlotId(u32::MAX), None, struct_cell_hp)
        } else {
            // A module cell: the slot sitting on it drives occupancy. Found positionally
            // so the map stays correct under any authoring.
            match hull.slots.iter().find(|s| s.coord == coord) {
                Some(slot) => {
                    let module_id = fit.module_in(slot.id);
                    let health = module_id
                        .and_then(|m| catalog.get(m))
                        .map(|m| m.health_max)
                        .unwrap_or(0.0);
                    (slot.id, module_id, health)
                }
                // Defensive: a non-structural cell with no slot (never authored this
                // way) is treated as an empty module-slot cell (health 0).
                None => (SlotId(u32::MAX), None, 0.0),
            }
        };

        cells.insert(
            coord,
            CellOccupant {
                slot: slot_id,
                module: module_id,
                health,
                depth: cell_depth(hull, coord),
                structural: grid_cell.structural,
            },
        );
    }

    FitLayout {
        hull: hull.id,
        cells,
    }
}

/// The module installed at `cell` on `fit`'s `hull`, if any (FR-019; contracts/
/// fitting-api.md §3 `module_at`). Returns a [`ModuleRef`] (slot + module id) so
/// E007 has the installed-module identity; `None` for an empty or slot-less cell.
/// **Pure** — reads only its arguments.
pub fn module_at(fit: &Fit, cell: Cell, hull: &Hull, catalog: &ModuleCatalog) -> Option<ModuleRef> {
    let slot = hull.slots.iter().find(|s| s.coord == cell)?;
    let module = fit.module_in(slot.id)?;
    // Only report an occupant whose module actually resolves (a dangling ref is no
    // occupant for E007's purposes; validate_fit rejects the fit, INV-F13).
    catalog.get(module)?;
    Some(ModuleRef::new(slot.id, module))
}

/// The full per-cell occupant + live-health map for `fit` (FR-019; contracts/
/// fitting-api.md §3 `cell_map`). Covers every authored hull cell (INV-F11) with
/// its occupant + module health, the value E007 mutates as it applies damage.
/// **Pure** — a thin projection of [`build_layout`]'s cell map.
pub fn cell_map(fit: &Fit, hull: &Hull, catalog: &ModuleCatalog) -> CellMap {
    build_layout(hull, fit, catalog).cells
}

/// The resolution of a hit traced across the hull grid (contracts/fitting-api.md §3
/// `HitResolution`). Names the first module struck, the time-of-impact along the
/// query segment, and the cell entered. `toi ∈ [0, 1]` along `p0`→`p1`, consistent
/// with [`SweptHit`](crate::physics::SweptHit).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitResolution {
    /// The installed module struck (slot + module id).
    pub module: ModuleRef,
    /// Time-of-impact fraction along `p0`→`p1` (`∈ [0, 1]`).
    pub toi: f32,
    /// The grid cell the line entered to strike the module.
    pub cell: Cell,
}

/// The per-cell circle radius used to sweep the hit line across the grid. A unit
/// cell spans `1.0` in local space; a radius of `0.5` inscribes the cell so a line
/// passing through the cell center registers, while a clean pass between cells does
/// not over-trigger. (The grid is coarse section-granularity, HINT-004; a finer
/// authoring would shrink this with the cell size.)
const CELL_RADIUS: f32 = 0.5;

/// Resolve the FIRST module struck by the segment `p0`→`p1` traced across `fit`'s
/// hull grid, **outer-before-inner by depth** (FR-018/021, INV-F10; contracts/
/// fitting-api.md §3 `resolve_hit`). Returns `None` if the line strikes no
/// installed module.
///
/// `p0`/`p1` are in the hull's **local cell-space** (the same space
/// [`cell_center`] maps a coord into; world↔local transform is the caller's
/// concern). The trace reuses the **existing** swept point-vs-circle primitive
/// ([`Physics::swept_cast`] → [`crate::collision::segment_circle_toi`]) against
/// each occupied cell's inscribed circle — it is **not** reinvented here.
///
/// Ordering (INV-F10, the survivability property FR-021): every occupied cell the
/// line enters is collected with its `toi` and occlusion `depth`, then the winner
/// is the one struck **first along the line** — primarily the lowest `toi`, with
/// `depth` (outer = lower) as the deterministic tie-break when two cells are
/// entered at the same time-of-impact. A module placed behind others (higher depth)
/// is therefore reached only after the covering cells: central placement shields,
/// edge placement exposes. Two fits differing only in a module's placement resolve
/// the same shot to different depths (SC-004).
pub fn resolve_hit(
    fit: &Fit,
    p0: Vec2,
    p1: Vec2,
    hull: &Hull,
    catalog: &ModuleCatalog,
) -> Option<HitResolution> {
    let physics = RapierPhysics::new();

    let mut best: Option<(f32, u16, ModuleRef, Cell)> = None;

    for slot in &hull.slots {
        // Only occupied slots are strikeable modules (empty/structural cells carry
        // no installed device for the first-module-struck query).
        let Some(module_id) = fit.module_in(slot.id) else {
            continue;
        };
        // A dangling module ref is not a strikeable occupant (INV-F13).
        if catalog.get(module_id).is_none() {
            continue;
        }

        let center = cell_center(slot.coord);
        let Some(hit) = physics.swept_cast(p0, p1, center, CELL_RADIUS) else {
            continue;
        };
        // `swept_cast`/`segment_circle_toi` already guarantees toi ∈ [0, 1].
        let depth = cell_depth(hull, slot.coord);
        let candidate = (
            hit.toi,
            depth,
            ModuleRef::new(slot.id, module_id),
            slot.coord,
        );

        // First-along-the-line wins: lowest toi, then outer (lower depth) on a tie.
        let take = match best {
            None => true,
            Some((best_toi, best_depth, _, _)) => {
                hit.toi < best_toi - f32::EPSILON
                    || ((hit.toi - best_toi).abs() <= f32::EPSILON && depth < best_depth)
            }
        };
        if take {
            best = Some(candidate);
        }
    }

    best.map(|(toi, _, module, cell)| HitResolution { module, toi, cell })
}

/// Smallest weapon firing-arc half-angle (INV-F12 lower bound): a strictly positive
/// floor so the arc is never zero-width even for a perfectly centered mount.
const MIN_HALF_ANGLE: f32 = std::f32::consts::FRAC_PI_8;
/// Widest weapon firing-arc half-angle (INV-F12 upper bound `π`): a perimeter mount
/// covers a full half-plane, never a wrap-around (`> π`) arc.
const MAX_HALF_ANGLE: f32 = std::f32::consts::PI;

/// Derive a weapon hardpoint's firing arc from its position + facing on `hull`
/// (FR-020, INV-F12; contracts/fitting-api.md §3 `hardpoint_arc`). Returns `None`
/// for a non-weapon slot (or an unknown slot id). **Pure** — reads only its args.
///
/// The arc is **defined here as fit data**; its *enforcement* (turret track /
/// can-this-weapon-hit) is E007 (the firing-arc note in data-model.md / the
/// contract). The returned [`FiringArc`]:
///
/// - `center = slot.facing` — the mount's facing on the hull. At runtime a consumer
///   adds the hull `heading` (`center = heading + facing`, data-model.md); E006
///   exposes the hull-local center (the position/facing-derived datum), and the
///   heading offset is applied by the combat consumer that knows the live heading.
/// - `half_angle ∈ (0, π]` (INV-F12) — a function of the mount's position: an
///   edge/perimeter mount gets a **wider** arc (more open field of fire), a central
///   mount a **narrower** one. Derived from the cell's perimeter ring
///   ([`cell_depth`]) normalized over the hull's max ring, mapped onto
///   `[MIN_HALF_ANGLE, π]` and floored strictly above `0` so it is never a
///   zero-width or wrap-around arc.
pub fn hardpoint_arc(hull: &Hull, slot: SlotId) -> Option<FiringArc> {
    let slot = hull.slot(slot)?;
    if !slot.is_weapon_mount {
        return None;
    }

    // Position factor: outer mounts (depth 0) → wide arc; central mounts → narrow.
    // Normalize the cell's perimeter ring over the hull's maximum possible ring so
    // the factor is a clean 0..=1 independent of hull size.
    let (cols, rows) = hull.grid_dims;
    let max_ring = ((cols.min(rows)).saturating_sub(1) / 2).max(1) as f32;
    let ring = cell_depth(hull, slot.coord) as f32;
    let centrality = (ring / max_ring).clamp(0.0, 1.0); // 0 = edge, 1 = center

    // Edge (centrality 0) → MAX_HALF_ANGLE; center (centrality 1) → MIN_HALF_ANGLE.
    let half_angle = MAX_HALF_ANGLE - centrality * (MAX_HALF_ANGLE - MIN_HALF_ANGLE);
    // Belt-and-suspenders: clamp strictly into (0, π] (INV-F12).
    let half_angle = half_angle.clamp(MIN_HALF_ANGLE, MAX_HALF_ANGLE);

    Some(FiringArc {
        center: slot.facing,
        half_angle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::content::{
        seed_catalogs, HULL_FIGHTER, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC,
    };

    #[test]
    fn build_layout_covers_every_authored_cell() {
        // INV-F11: the layout map has exactly one occupant per authored hull cell.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let fit = Fit::new(HULL_FIGHTER);
        let layout = build_layout(hull, &fit, &modules);
        assert_eq!(layout.cells.len(), hull.cells.len());
        for cell in &hull.cells {
            let occ = layout
                .occupant(cell.coord)
                .expect("every authored cell present");
            // Phase 1A: the occupant's kind mirrors the authored GridCell's kind.
            assert_eq!(
                occ.structural, cell.structural,
                "occupant kind matches the authored cell kind"
            );
        }
    }

    #[test]
    fn dense_hull_seeds_structural_cells_with_struct_cell_hp() {
        // Phase 1A: the fighter is now a DENSE filled silhouette — far more cells than
        // its 7 slots. A structural (filler) cell carries no module and is seeded with
        // STRUCT_CELL_HP so Phase 2 can carve it; an empty module-slot cell stays at 0.
        use crate::fitting::content::STRUCT_CELL_HP;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let fit = Fit::new(HULL_FIGHTER); // empty fit: no module cell is occupied
        let layout = build_layout(hull, &fit, &modules);

        // The dense grid has many more cells than the 7 hardpoints (a real ship body).
        assert!(
            layout.cells.len() > hull.slots.len(),
            "dense silhouette has more cells ({}) than slots ({})",
            layout.cells.len(),
            hull.slots.len()
        );

        let mut structural = 0;
        let mut module_cells = 0;
        for occ in layout.cells.values() {
            if occ.structural {
                structural += 1;
                assert_eq!(occ.module, None, "a structural cell holds no module");
                assert_eq!(
                    occ.health, STRUCT_CELL_HP,
                    "a structural cell is seeded with STRUCT_CELL_HP"
                );
            } else {
                module_cells += 1;
                // An empty (unfitted) module-slot cell keeps the historical 0 health.
                assert_eq!(occ.health, 0.0, "an empty module-slot cell stays at 0 hp");
            }
        }
        // One module cell per slot, the rest structural filler.
        assert_eq!(module_cells, hull.slots.len(), "one module cell per slot");
        assert!(structural > 0, "the dense body has structural filler cells");
    }

    #[test]
    fn occupied_cell_reports_module_health() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        // Slot 0 is the central reactor slot on the fighter.
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        let reactor_coord = hull.slot(SlotId(0)).unwrap().coord;
        let layout = build_layout(hull, &fit, &modules);
        let occ = layout.occupant(reactor_coord).unwrap();
        assert_eq!(occ.module, Some(MODULE_REACTOR_BASIC));
        assert_eq!(
            occ.health,
            modules.get(MODULE_REACTOR_BASIC).unwrap().health_max
        );
    }

    #[test]
    fn central_cell_has_greater_depth_than_an_edge_cell() {
        // INV-F10: the central reactor cell sits deeper (higher depth) than a
        // forward weapon mount on the perimeter.
        let (_, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let center = hull.slot(SlotId(0)).unwrap().coord; // reactor, central
        let edge = hull.slot(SlotId(3)).unwrap().coord; // weapon, forward edge
        assert!(cell_depth(hull, center) > cell_depth(hull, edge));
    }

    #[test]
    fn hardpoint_arc_is_bounded_and_none_for_non_weapon() {
        let (_, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        // A weapon mount (slot 3) exposes a bounded arc.
        let arc = hardpoint_arc(hull, SlotId(3)).expect("weapon mount has an arc");
        assert!(arc.half_angle > 0.0 && arc.half_angle <= std::f32::consts::PI);
        // The reactor slot (slot 0) is not a weapon mount.
        assert!(hardpoint_arc(hull, SlotId(0)).is_none());
    }

    #[test]
    fn resolve_hit_picks_the_module_on_the_line() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        // Put the reactor in its central slot, fire a line straight through it.
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        let coord = hull.slot(SlotId(0)).unwrap().coord;
        let center = cell_center(coord);
        let hit = resolve_hit(
            &fit,
            center - Vec2::new(3.0, 0.0),
            center + Vec2::new(3.0, 0.0),
            hull,
            &modules,
        )
        .expect("line through the reactor cell strikes it");
        assert_eq!(hit.module.module, MODULE_REACTOR_BASIC);
        assert_eq!(hit.cell, coord);
        assert!((0.0..=1.0).contains(&hit.toi));
        // An autocannon id exists in the catalog (referenced for the import).
        assert!(modules.get(MODULE_AUTOCANNON).is_some());
    }

    // --- Phase M5: per-cell mass helpers ----------------------------------------

    #[test]
    fn cell_mass_is_module_mass_or_the_structural_constant() {
        use crate::fitting::content::STRUCT_CELL_MASS;
        let (modules, _) = seed_catalogs();
        let reactor_mass = modules.get(MODULE_REACTOR_BASIC).unwrap().mass;
        // A module cell weighs its installed module's mass.
        let module_cell = CellOccupant {
            slot: SlotId(0),
            module: Some(MODULE_REACTOR_BASIC),
            health: 1.0,
            depth: 0,
            structural: false,
        };
        assert_eq!(cell_mass(&module_cell, &modules), reactor_mass);
        // A structural / empty cell weighs the structural constant.
        let struct_cell = CellOccupant {
            slot: SlotId(u32::MAX),
            module: None,
            health: 0.0,
            depth: 0,
            structural: true,
        };
        assert_eq!(cell_mass(&struct_cell, &modules), STRUCT_CELL_MASS);
    }

    #[test]
    fn layout_mass_sums_real_cell_masses() {
        use crate::fitting::content::STRUCT_CELL_MASS;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        let layout = build_layout(hull, &fit, &modules);

        // The total equals the explicit per-cell sum.
        let expected: f32 = layout.cells.values().map(|o| cell_mass(o, &modules)).sum();
        assert!((layout_mass(&layout, &modules) - expected).abs() < 1e-6);
        // A fitted (heavy) reactor cell makes the body outweigh all-structural plating.
        let all_structural = layout.cells.len() as f32 * STRUCT_CELL_MASS;
        assert!(
            layout_mass(&layout, &modules) > all_structural,
            "a fitted reactor cell outweighs plain plating"
        );
    }

    #[test]
    fn layout_inertia_is_positive_and_tracks_footprint() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let fit = Fit::new(HULL_FIGHTER);
        let layout = build_layout(hull, &fit, &modules);
        let full = layout_inertia(&layout, &modules);
        assert!(full > 0.0 && full.is_finite());

        // A single-cell body has (near-)zero inertia — all its mass sits at its own COM —
        // so it is strictly less than the whole footprint's inertia.
        let first = *layout.cells.keys().next().unwrap();
        let mut one = layout.clone();
        one.cells.retain(|&c, _| c == first);
        assert!(
            layout_inertia(&one, &modules) < full,
            "a single cell has less rotational inertia than the full body"
        );
    }
}
