//! Per-ship defense-layer state components + the US1 damage traversal (FR-002/003/
//! 004/009/011, data-model.md).
//!
//! The ordered [`DefenseLayer`](crate::damage::DefenseLayer) stack (Shields →
//! Armor → Hull/Structure → Systems) keeps per-target state on the ship entity.
//! This module defines those `bevy_ecs` components **and** the live traversal that
//! reads/mutates them — [`apply_damage`] (the full pipeline), plus the entry-point
//! geometry helpers [`resolve_entry_point`]/[`route_behind`] (which reuse the E006
//! [`resolve_hit`] outer-before-inner sweep, no new geometry). The state shapes:
//!
//! - [`Shields`] — the regenerating, power-linked outer pool (one per ship).
//! - [`SectionArmor`] — the per-section plate map (`SectionId` → [`ArmorFacet`]),
//!   the angle-math input.
//! - [`HullStructure`] — the aggregate structural-HP backstop (one per ship).
//! - [`SectionHealth`] — a section's structural integrity (`0` → section
//!   destroyed → connectivity check).
//! - [`DamageContext`] — the per-ship handle bundling the layer state for one
//!   resolution.
//!
//! The per-module live health lives in the E006 `FitLayout`/`CellOccupant.health`
//! (the damage **target**); E007 does **not** invent a parallel module-health
//! store (data-model.md). `SectionHealth` is the only *new* health datum — the
//! per-section structural HP.
//!
//! Derive discipline matches the rest of `sim`: `Component` so they live on the
//! ship entity; serde as the replication (E003) / persistence (E004) seam — present,
//! not exercised this epic; value semantics. `Shields`/`HullStructure`/
//! `SectionHealth`/`ArmorFacet` are `Copy`; `SectionArmor`/`DamageContext` hold a
//! map (`Clone`, not `Copy`).

use std::collections::{BTreeMap, BTreeSet};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::Vec2;
use serde::{Deserialize, Serialize};

use super::content::{ArmorMaterial, PenetrationConfig};
use super::event::DamageEvent;
use super::penetration::{resolve_penetration, PenetrationResult};
use super::resist::{layer_resist, DefenseLayer, ResistanceMatrix};
use super::sever::Wreck;
use super::shields::shield_absorb;
use crate::components::ArmorHp;
use crate::fitting::{
    resolve_hit, Cell, CellOccupant, Fit, FitLayout, HitResolution, Hull, HullCatalog,
    ModuleCatalog, ModuleRef, SectionId,
};
use crate::physics::{Physics, RapierPhysics};

/// The regenerating, power-linked outer shield pool — one per ship (FR-010,
/// data-model.md `Shields`).
///
/// Absorbs first in the traversal (strong vs `ThermalEnergy`). `current` is
/// clamped `0..=max` (INV-D01); `max`/`regen_rate` are seeded from the fitted
/// Shield module(s). `power_linked` shields regenerate only while powered (the
/// reactor supplies power) and decay when unpowered, exposing Armor at `0`
/// (INV-D14). Mutation/regen is US1; this is the shape.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shields {
    /// Live shield HP, clamped `0.0..=max` (INV-D01).
    pub current: f32,
    /// Maximum shield HP (`>= 0`); seeded from fitted Shield module(s).
    pub max: f32,
    /// Regen per second while powered (`>= 0`); a fitted shield's `regen` overrides
    /// the [`ShieldConfig`](crate::damage::ShieldConfig) default.
    pub regen_rate: f32,
    /// Whether this shield draws from the reactor budget; if so it regenerates only
    /// while powered and decays when the reactor is lost (FR-013/INV-D14).
    pub power_linked: bool,
}

impl Shields {
    /// A depleted shield pool (`current == 0`) with the given capacity/regen.
    pub fn depleted(max: f32, regen_rate: f32, power_linked: bool) -> Self {
        Self {
            current: 0.0,
            max,
            regen_rate,
            power_linked,
        }
    }

    /// A full shield pool (`current == max`) with the given capacity/regen.
    pub fn full(max: f32, regen_rate: f32, power_linked: bool) -> Self {
        Self {
            current: max,
            max,
            regen_rate,
            power_linked,
        }
    }
}

/// One section's nominal plate: thickness, material, and outward face normal
/// (data-model.md `ArmorFacet`).
///
/// The armor gate reads `thickness` + `material` for the effective armor
/// (`thickness * material.multiplier() / cos(angle)`). The impact `angle` itself is
/// now derived from the **real hit geometry** (the entry cell's radial position on
/// the hull), NOT from this `normal` — see [`apply_damage`]'s armor gate. The
/// `normal` field is therefore effectively unused for the angle (kept for the
/// data-model shape; harmless). Seeded from fitted Armor module(s) + the hull
/// section authoring. `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArmorFacet {
    /// Nominal plate thickness (`> 0`).
    pub thickness: f32,
    /// Plate material; multiplier on thickness (content seam).
    pub material: ArmorMaterial,
    /// Outward face normal (unit). Historically the impact-angle source; the angle is
    /// now derived from the entry cell's hull-radial geometry in [`apply_damage`], so
    /// this field no longer drives the angle (kept for the data-model shape).
    pub normal: Vec2,
}

/// The per-section plate map — one per ship (FR-005, data-model.md `SectionArmor`).
///
/// Keyed by the hull's [`SectionId`]s; the angle math reads the entry section's
/// [`ArmorFacet`]. A section's facet is removed when its section is destroyed
/// (severing). Holds a `BTreeMap` for deterministic iteration (Principle II), so it
/// is `Clone`, not `Copy`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct SectionArmor {
    /// Per-section plate, keyed by hull `SectionId`.
    pub sections: BTreeMap<SectionId, ArmorFacet>,
}

impl SectionArmor {
    /// An empty armor map (no sections plated yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// The plate facet of `section`, if plated (null-safe; no panic on an unknown
    /// section).
    pub fn facet(&self, section: SectionId) -> Option<&ArmorFacet> {
        self.sections.get(&section)
    }
}

/// The aggregate structural-HP backstop — one per ship (FR-003, data-model.md
/// `HullStructure`).
///
/// Reduced by Hull-routed `Blast`/spillover; `current` is clamped `0..=max`
/// (INV-D01). `current == 0` contributes to ship-destroy (with core-sever). `Copy`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HullStructure {
    /// Live structural HP, clamped `0.0..=max` (INV-D01).
    pub current: f32,
    /// Maximum structural HP (`> 0`).
    pub max: f32,
}

impl HullStructure {
    /// A full structural backstop (`current == max`).
    pub fn full(max: f32) -> Self {
        Self { current: max, max }
    }
}

/// One section's structural integrity — the new per-section structural HP datum
/// (data-model.md `SectionHealth`).
///
/// This is the **only** new health store E007 introduces (the per-module health is
/// the reused E006 `CellOccupant.health`). It aggregates a section's structural
/// integrity (empty/structural cells); `current` clamped `0..=max` (INV-D01).
/// `0.0` → section destroyed → removed from the layout → triggers the connectivity
/// check (FR-014/015/017). `Copy`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionHealth {
    /// Which hull section this integrity belongs to.
    pub section: SectionId,
    /// Live structural HP, clamped `0.0..=max` (INV-D01).
    pub current: f32,
    /// Maximum structural HP (`>= 0`).
    pub max: f32,
}

impl SectionHealth {
    /// A full-integrity section (`current == max`).
    pub fn full(section: SectionId, max: f32) -> Self {
        Self {
            section,
            current: max,
            max,
        }
    }

    /// Whether this section has been destroyed (`current <= 0`), the
    /// connectivity-check trigger (FR-017).
    pub fn is_destroyed(&self) -> bool {
        self.current <= 0.0
    }
}

/// The per-ship handle bundling the defense-layer state for one resolution
/// (data-model.md `DamageContext`).
///
/// A convenience aggregate the US1 damage system reads to traverse the stack for a
/// single hit; rebuilt/attached when the ship gains a `Fit` (alongside E006's
/// `FitLayout`). It bundles the layer state owned by the ship — `Shields` +
/// `SectionArmor` + `HullStructure` (the `FitLayout`/per-cell health is the E006
/// component, looked up separately). Shape only this phase; the traversal that
/// reads it is US1. Holds a non-`Copy` map, so `Clone`.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DamageContext {
    /// The outer shield pool.
    pub shields: Shields,
    /// The per-section plate map.
    pub armor: SectionArmor,
    /// The structural-HP backstop.
    pub hull: HullStructure,
}

// --- The US1 traversal — entry point, route-behind, and `apply_damage` ----------

/// How far the entry ray is extended across the hull grid from the impact point
/// along the event direction, so [`resolve_entry_point`] sweeps the whole chassis
/// (the largest seed hull is the 13×15 corvette in cell-space, so this comfortably
/// spans it). Public so the narrow-phase hit selection in
/// [`fitted_damage_system`](crate::collision::fitted_damage_system) builds the SAME
/// cell-space segment [`apply_damage`]/[`carve_path`] carve along (single-sourced reach
/// → the selected target is exactly the one that carves).
pub const REACH: f32 = 64.0;

// --- Phase 2 carving (fine destruction) tuning ----------------------------------
//
// After the upstream Shields + Armor gate, a penetrating shot carries a `damage`
// budget (the post-armor surviving magnitude) and a `penetration` budget down the
// shot ray, **carving a channel** of cells from the entry inward (the "eaten-away"
// model, ADR-0008/GDD §5). At each cell along the path:
//   - subtract the cell's *current* `health` worth of work from the budgets;
//   - if the cell's health reaches `<= 0`, it is destroyed (removed) and the shot
//     continues to the next cell with reduced budgets (it spent energy punching
//     through — see [`CARVE_FALLOFF`]);
//   - if the cell survives (its health is only chipped), the shot **lodges** there
//     and stops (a weak shot chips one cell; a strong/high-pen shot tunnels deep).
// The walk stops when a cell survives, the damage OR penetration budget is spent, or
// the ray exits the grid. Deterministic: the cells are walked in ascending time-of-
// impact along the ray (ties broken by outer-before-inner depth, then `Cell` order).
//
// All consts are grounded-but-scaled tunables (Phase 3 refines feel). They are sized
// (with `STRUCT_CELL_HP = 10`, the autocannon's `damage 12 → penetration 36`) so a
// sustained square-on burst visibly erodes a channel and kills the demo fighter in
// ~5–15 s, while a single weak shot only chips a cell or two.

/// Penetration cost to punch *through* one destroyed cell and keep tunnelling: each
/// cell carved subtracts this fixed amount from the shot's `penetration` budget (on
/// top of the per-cell damage cost). Tunnelling deeper costs penetration, so a finite
/// shot carves a finite-depth channel and a low-pen shot stops shallow even when its
/// damage budget is large. `> 0`.
pub(crate) const CARVE_PEN_COST: f32 = 8.0;

/// Multiplicative damage falloff applied to the surviving `damage` budget after a
/// cell is punched through (the shot loses energy crossing each cell). `∈ (0, 1)`: a
/// value near `1` carves a long channel from one strong shot, near `0` chips ~one
/// cell. With the per-cell `health` subtraction this gives a tunable, decaying
/// channel depth.
pub(crate) const CARVE_FALLOFF: f32 = 0.75;

/// The per-cell `health` floor used as the carve *work cost* for an empty module-slot
/// cell (`health == 0`, no installed device): such a cell still costs a little to
/// punch through so the channel does not carve infinitely free through hollow slots.
pub(crate) const CARVE_MIN_CELL_COST: f32 = 1.0;

/// The fallback armor facet for an **unplated** entry section (no [`ArmorFacet`] in
/// [`SectionArmor`]): a thin steel plate normal to the incoming shot. So a hit on a
/// bare section still runs the penetration gate (it almost always penetrates), the
/// pipeline stays total, and an unplated ship is not damage-immune.
fn default_facet(ev: &DamageEvent) -> ArmorFacet {
    // Face the shot head-on (angle ≈ 0): a bare section offers minimal deflection.
    let normal = if ev.dir.length_squared() > f32::EPSILON {
        -ev.dir.normalize()
    } else {
        Vec2::X
    };
    ArmorFacet {
        thickness: 1.0,
        material: ArmorMaterial::Steel,
        normal,
    }
}

/// Map a struck [`Cell`] to the hull [`SectionId`] it belongs to — the key into
/// [`SectionArmor`] for the entry section's plate (data-model.md). Looks up the
/// authored [`GridCell`](crate::fitting::GridCell) at `cell` on `hull`; `None` for a
/// coord the hull never authored (defensive — `resolve_hit` only returns authored
/// cells).
fn section_of_cell(hull: &Hull, cell: Cell) -> Option<SectionId> {
    hull.cells
        .iter()
        .find(|gc| gc.coord == cell)
        .map(|gc| gc.section)
}

/// Resolve the **entry point** module a [`DamageEvent`] strikes (FR-002), reusing
/// the E006 [`resolve_hit`] outer-before-inner sweep (no new geometry, INV-D16).
///
/// Builds the hull-local segment from the event geometry — `p0 = ev.point`, `p1 =
/// ev.point + ev.dir * REACH` (a reach spanning the grid) — and returns the first
/// installed module struck. `None` (empty / structural / off-grid) ⇒ the caller
/// yields `NoModule` (the "hit on nothing" edge case, never a panic — the pipeline
/// is total). Pure; reads only its arguments.
pub fn resolve_entry_point(
    fit: &Fit,
    hull: &Hull,
    catalog: &ModuleCatalog,
    ev: &DamageEvent,
) -> Option<HitResolution> {
    let p0 = ev.point;
    let p1 = ev.point + ev.dir * REACH;
    resolve_hit(fit, p0, p1, hull, catalog)
}

/// Resolve the module **one ring deeper** behind the entry cell along the shot ray
/// (FR-009, INV-D06 — outer-before-inner), to route surviving post-penetration
/// damage to (the cell behind).
///
/// Re-runs [`resolve_hit`] on the sub-segment starting just **past** the entry
/// cell's center along `ev.dir` (nudged beyond the cell's inscribed circle so the
/// entry module is excluded), returning the next module struck. `None` if nothing
/// is behind the entry point — the caller spills the surviving damage to
/// [`HullStructure`]. Pure.
pub fn route_behind(
    fit: &Fit,
    hull: &Hull,
    catalog: &ModuleCatalog,
    entry: &HitResolution,
    ev: &DamageEvent,
) -> Option<HitResolution> {
    let dir = if ev.dir.length_squared() > f32::EPSILON {
        ev.dir.normalize()
    } else {
        return None;
    };
    // The entry cell center in local cell-space (coord + 0.5), nudged just past the
    // cell's inscribed circle (radius 0.5) so `resolve_hit` excludes the entry
    // module and returns the next-deeper one.
    let center = Vec2::new(entry.cell.0 as f32 + 0.5, entry.cell.1 as f32 + 0.5);
    let p0 = center + dir * (0.5 + 1.0e-3);
    let p1 = p0 + dir * REACH;
    resolve_hit(fit, p0, p1, hull, catalog)
}

/// The cells the carve ray passes through, in deterministic entry-inward order, with
/// the radius the per-cell inscribed circle uses. Mirrors the E006
/// [`CELL_RADIUS`](crate::fitting) inscribe so a ray through a cell centre registers.
pub const CARVE_CELL_RADIUS: f32 = 0.5;

/// The deterministic sort key of one crossed cell: ascending time-of-impact along the
/// ray (first crossed first), ties broken outer-before-inner by occlusion `depth`, then
/// by [`Cell`] order (Principle II — no `HashMap` iteration; the source map is a
/// `BTreeMap`). Shared by [`first_cell_hit`] and [`carve_path`] so the "which cell is
/// reached first" ordering is single-sourced.
fn carve_order(a: &(f32, u16, Cell), b: &(f32, u16, Cell)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.1.cmp(&b.1))
        .then(a.2.cmp(&b.2))
}

/// R58 — is `p` inside the CCW convex polygon `poly` (on-edge counts as inside)?
fn point_in_convex(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        // Cross of edge a→b with a→p; <0 = p is to the RIGHT of a CCW edge = outside.
        if (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) < 0.0 {
            return false;
        }
    }
    true
}

/// R58 — the entry time-of-impact (fraction `0..1` along `p0→p1`) where the segment first reaches the
/// CCW convex polygon `poly`: `0` if `p0` is already inside, else the smallest edge-crossing `t`, or
/// `None` for a miss. Pure 2-D f32 (deterministic). The polygon analogue of the legacy swept-circle toi.
fn swept_point_convex_toi(p0: Vec2, p1: Vec2, poly: &[Vec2]) -> Option<f32> {
    if poly.len() < 3 {
        return None;
    }
    if point_in_convex(p0, poly) {
        return Some(0.0);
    }
    let d = p1 - p0;
    let n = poly.len();
    let mut best: Option<f32> = None;
    for i in 0..n {
        let a = poly[i];
        let e = poly[(i + 1) % n] - a;
        let denom = d.x * e.y - d.y * e.x;
        if denom.abs() < 1.0e-9 {
            continue; // parallel
        }
        let diff = a - p0;
        let t = (diff.x * e.y - diff.y * e.x) / denom;
        let u = (diff.x * d.y - diff.y * d.x) / denom;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            best = Some(best.map_or(t, |bt| bt.min(t)));
        }
    }
    best
}

/// R58 — the cell-crossing toi for one cell, honouring its [`CellShape`]: a `Full` cell uses the EXACT
/// legacy inscribed-circle swept test (byte-identical); a sub-shape cell uses the segment-vs-its-convex-
/// polygon entry test, so the HITBOX matches the visual. `None` = the ray misses this cell.
fn cell_toi(
    physics: &RapierPhysics,
    p0: Vec2,
    p1: Vec2,
    cell: Cell,
    occ: &CellOccupant,
) -> Option<f32> {
    if occ.shape.is_full() {
        let center = Vec2::new(cell.0 as f32 + 0.5, cell.1 as f32 + 0.5);
        physics
            .swept_cast(p0, p1, center, CARVE_CELL_RADIUS)
            .map(|h| h.toi)
    } else {
        swept_point_convex_toi(p0, p1, &occ.shape.corners(cell.0, cell.1))
    }
}

/// The **first present cell** of `layout` the ray `p0 → p1` crosses, paired with its
/// cell-crossing time-of-impact — the single-sourced "which cell does the ray reach
/// first" test (Principle II). `None` when the ray crosses no present cell (a genuine
/// miss against this layout's cells — NOT a hit, no fallback).
///
/// Reuses the **existing** swept point-vs-circle primitive ([`Physics::swept_cast`])
/// against each present cell's inscribed circle ([`CARVE_CELL_RADIUS`]) — the same CCD
/// `resolve_hit`/`carve_path` use, no new geometry. Used BOTH by the narrow-phase target
/// selection in [`fitted_damage_system`](crate::collision::fitted_damage_system) (the
/// lowest cell-toi across candidate targets is the one hit) AND by [`carve_path`] (the
/// entry cell of the carve), so the chosen target is guaranteed to carve a cell. Pure;
/// reads only its arguments.
pub fn first_cell_hit(layout: &FitLayout, p0: Vec2, p1: Vec2) -> Option<(Cell, f32)> {
    let physics = RapierPhysics::new();
    let mut best: Option<(f32, u16, Cell)> = None;
    for (&cell, occ) in &layout.cells {
        if let Some(toi) = cell_toi(&physics, p0, p1, cell, occ) {
            let candidate = (toi, occ.depth, cell);
            if best.is_none_or(|b| carve_order(&candidate, &b) == std::cmp::Ordering::Less) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(toi, _, cell)| (cell, toi))
}

/// Clamp the cell-space carve entry `point` to the hull's OUTER surface along the shot line
/// `dir`. The common case — a shot meeting the silhouette from OUTSIDE — returns `point`
/// UNCHANGED (its forward entry cell already IS the outer surface, so the carve is
/// byte-identical to a direct entry). When `point` landed INSIDE the silhouette — an
/// off-axis shot whose broad-phase contact crossed the (inscribed) `CollisionRadius` circle
/// at a point already within the solid hull — it returns the position where the shot LINE
/// first enters the hull from outside, so the carve channels IN from the surface instead of
/// starting at an interior cell.
///
/// `fwd_cell` is the forward-scan entry the caller already computed
/// (`first_cell_hit(layout, point, point + dir·REACH)`) — today's entry; reused to tell the
/// surface case (the back-extended scan finds the SAME first cell → unchanged) from the
/// buried case (it finds an OUTER cell → enter there). `back` must exceed the hull's interior
/// depth so the back-extended scan starts outside the cells (e.g. `grid.max_dim · 1.5`).
/// Pure; reads only its arguments.
pub fn surface_entry(
    layout: &FitLayout,
    point: Vec2,
    dir: Vec2,
    back: f32,
    fwd_cell: Cell,
) -> Vec2 {
    let p0 = point - dir * back;
    let p1 = point + dir * REACH;
    match first_cell_hit(layout, p0, p1) {
        // The outer-surface cell differs from today's forward entry → the contact was buried
        // inside the hull; enter at the surface crossing along the same shot line.
        Some((surf_cell, toi)) if surf_cell != fwd_cell => p0 + (p1 - p0) * toi,
        // Already meeting the surface from outside (or no present cell) → unchanged.
        _ => point,
    }
}

/// Walk the present cells of `layout` the ray `p0 → p1` passes through, in the
/// deterministic order the shot would carve them ([`carve_order`]: ascending
/// time-of-impact, ties outer-before-inner by occlusion `depth`, then by [`Cell`]
/// order).
///
/// Reuses the **same** per-cell swept point-vs-circle crossing test [`first_cell_hit`]
/// single-sources — so `carve_path`'s entry cell is exactly the target the narrow-phase
/// selection picked. Returns each crossed cell paired with its occupant snapshot (its
/// live `health`/`module`), so the caller can carve it without re-borrowing the layout
/// per step. Pure; reads only its arguments.
fn carve_path(layout: &FitLayout, p0: Vec2, p1: Vec2) -> Vec<(Cell, CellOccupant)> {
    let physics = RapierPhysics::new();
    let mut hits: Vec<(f32, u16, Cell, CellOccupant)> = Vec::new();
    for (&cell, occ) in &layout.cells {
        if let Some(toi) = cell_toi(&physics, p0, p1, cell, occ) {
            hits.push((toi, occ.depth, cell, *occ));
        }
    }
    hits.sort_by(|a, b| carve_order(&(a.0, a.1, a.2), &(b.0, b.1, b.2)));
    hits.into_iter()
        .map(|(_, _, cell, occ)| (cell, occ))
        .collect()
}

/// Whether the **geometry cell set** has any cell **in front of** `entry_cell` along the
/// actual shot line (toward the shooter, `-dir_n`) — i.e. the shot bored through that material
/// to reach the entry (a **tunnel** → head-on), as opposed to the entry being the first cell
/// the shot meets (a **fresh outer surface** → its obliquity, and a genuine glancing
/// ricochet, applies).
///
/// `cells` is the **reference shape**: the authored hull for a live ship (a bored channel's
/// carved cells are still authored → buried → head-on, no re-ricochet stall, Fix #5), OR a
/// `Wreck`'s CURRENT cells for a detached chunk (its REAL shape, not the original ship it came
/// from — Fix #9). Replaces the old single dominant-axis `front` cell, which misfired for an
/// off-axis bore on a shaped hull (Fix #5). Walks the set along the REAL ray: a cell `c` is "in
/// front" iff it lies toward the shooter (`d·(−dir_n) > 0`) AND on the shot line (perpendicular
/// offset `< CARVE_CELL_RADIUS`). Length-independent, direction-accurate, any shape. Pure +
/// deterministic (a `BTreeSet` scan).
fn cell_in_front(cells: &BTreeSet<Cell>, entry_cell: Cell, dir_n: Vec2) -> bool {
    let entry_center = Vec2::new(entry_cell.0 as f32 + 0.5, entry_cell.1 as f32 + 0.5);
    let back = -dir_n; // unit vector toward the shooter (dir_n is normalized)
    cells.iter().any(|&coord| {
        if coord == entry_cell {
            return false;
        }
        let d = Vec2::new(coord.0 as f32 + 0.5, coord.1 as f32 + 0.5) - entry_center;
        let along = d.dot(back);
        // Toward the shooter (`along > 0`) AND on the shot line (perpendicular within the
        // per-cell carve inscribe) → material the shot bored through to get here.
        along > 0.0 && (d - back * along).length() < CARVE_CELL_RADIUS
    })
}

/// A surface hit may only RICOCHET if the entry cell is backed by a real (≥2-cell-thick)
/// surface — i.e. it has at least this many PRESENT neighbours. A 1-cell-wide line / tip /
/// 1–2-cell shard has ≤2 present neighbours: its [`local_surface_normal`] is degenerate (it
/// points ALONG the line, not perpendicular to the broad face the shot meets), which made
/// thin scrap spuriously ricochet broad-side hits at ~90° (Fix #10). Below this count → treat
/// the hit as head-on (carve). Solid 2-D chunk edges have ≥3 present neighbours, so they keep
/// deflecting genuine grazes; live-ship authored cells are part of the solid silhouette
/// (always ≥3), so the gate never trips for them. Tunable: raise it to also let 2-wide strips
/// carve.
pub(crate) const RICOCHET_MIN_NEIGHBORS: u8 = 3;

/// Half-width of the **smoothing kernel** for [`local_surface_normal`]. `2` → a 5×5 window.
/// A wider window than the immediate 3×3 averages the outward direction over more of the local
/// surface, so the normal varies SMOOTHLY between adjacent cells (consistent ricochet — no
/// "same angle, neighbouring cell flips") and ROUNDS convex corners (the gradient blends the
/// two faces, more so with a bigger radius — Fix #11 M1). This is the gradient of a smoothed
/// occupancy field = the normal of a smoothed marching-squares contour, computed cheaply
/// without building the polyline. Tunable: larger = rounder/smoother (the client contour's
/// smoothing should be tuned to match this).
pub(crate) const SMOOTH_NORMAL_RADIUS: i32 = 2;

/// The **smoothed local outward surface normal** at `cell` within the **geometry cell set**,
/// plus the COUNT of `cell`'s immediate (8-)neighbours that are present. The normal points
/// toward the local void (= outward): each ABSENT cell within the [`SMOOTH_NORMAL_RADIUS`]
/// window contributes `offset / |offset|²` (an inverse-distance-weighted unit vector toward
/// it), so nearer void dominates and the result is a smooth gradient that rounds corners and
/// agrees between adjacent cells. `Vec2::ZERO` for a fully-surrounded cell → the caller treats
/// that as head-on. The immediate-8 `present_count` gates ricochet eligibility
/// ([`RICOCHET_MIN_NEIGHBORS`]) — a thin shard (few neighbours) always carves regardless of the
/// (then-unreliable) normal.
///
/// `cells` is the authored hull for a live ship, or a `Wreck`'s CURRENT cells for a chunk — so
/// a detached piece faces the way its ACTUAL shape faces, not the original ship's (Fix #9; a
/// 1-cell chunk has no present neighbours → normal `0` + count `0` → head-on, always carves).
/// Pure + deterministic (a fixed-order `BTreeSet` membership scan; no transcendentals).
fn local_surface_normal(cells: &BTreeSet<Cell>, cell: Cell, radius: i32) -> (Vec2, u8) {
    let (c, r) = (cell.0 as i32, cell.1 as i32);
    let mut normal = Vec2::ZERO;
    let mut present_count: u8 = 0; // immediate 8-neighbours only — the thin-shard gate input
    for dc in -radius..=radius {
        for dr in -radius..=radius {
            if dc == 0 && dr == 0 {
                continue;
            }
            let nc = c + dc;
            let nr = r + dr;
            // An out-of-bounds neighbour is empty (void) too — it still pulls the normal out.
            let present = nc >= 0 && nr >= 0 && cells.contains(&(nc as u16, nr as u16));
            if present {
                if dc.abs() <= 1 && dr.abs() <= 1 {
                    present_count += 1;
                }
            } else {
                // Inverse-distance-weighted direction toward the void cell:
                // `(dc,dr)/|(dc,dr)|² = unit_dir / distance` → nearer void weighs more (smooth).
                let d2 = (dc * dc + dr * dr) as f32;
                normal += Vec2::new(dc as f32, dr as f32) / d2;
            }
        }
    }
    (normal, present_count)
}

/// The legible "what happened" tag the HUD reads (FR-024, SC-005) — never numeric
/// spam, advisory only (the server owns the authoritative mutation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// The shield pool absorbed the whole event (strong vs `ThermalEnergy`).
    ShieldAbsorbed,
    /// The shot bounced off the plate (steep, non-overmatching — FR-006).
    Ricochet,
    /// The shot punched into the plate and routed damage to the cell behind.
    Penetrated,
    /// The shot punched clean through (reduced pass-through tier).
    OverPenetrated,
    /// The hit landed on no installed module (empty/structural cell) — the
    /// "hit on nothing" edge case (the pipeline is total, never a panic).
    NoModule,
}

/// The result of [`apply_damage`] (contracts/damage-api.md §1) — the legible
/// outcome the HUD reads + the carving result the destruction/sever worker consumes
/// (Phase 2 fine destruction).
///
/// **Phase 2 carving shape**: `apply_damage` no longer routes a single
/// route-behind-or-spill magnitude — it carves a **channel** of cells along the shot
/// ray (see [`apply_damage`]). [`destroyed_cells`](DamageOutcome::destroyed_cells)
/// lists every cell the carve removed from the target's [`FitLayout`] (deterministic
/// entry-inward order); [`struck`](DamageOutcome::struck) is the entry module (or the
/// first module cell carved) for the HUD; [`destroyed`](DamageOutcome::destroyed) is
/// `true` iff at least one cell was removed (a destruction event the caller flood-
/// fills on). `DamageOutcome` holds a `Vec`, so it is `Clone`, not `Copy`.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageOutcome {
    /// The module the hit is attributed to for the HUD: the entry module (the first
    /// module cell on the ray), or `None` on a shield-absorb / structural-only / no-
    /// module hit.
    pub struck: Option<ModuleRef>,
    /// The magnitude that actually landed (the shield-absorbed amount for a
    /// `ShieldAbsorbed`, else the post-armor damage budget spent carving).
    pub applied: f32,
    /// The deepest [`DefenseLayer`] the surviving damage reached.
    pub layer_reached: DefenseLayer,
    /// The legibility tag (FR-024).
    pub result: HitKind,
    /// Whether this hit removed at least one cell (a destruction event the caller
    /// connectivity-checks / core-death-checks on). Equivalent to
    /// `!destroyed_cells.is_empty()`.
    pub destroyed: bool,
    /// The cells the carve removed from the target's [`FitLayout`], in the
    /// deterministic entry-inward order they were carved (Phase 2). A module cell
    /// among them means that module was destroyed. Empty on a ricochet / shield-
    /// absorb / no-module hit (nothing carved).
    pub destroyed_cells: Vec<Cell>,
}

/// Apply a [`DamageEvent`] to `target` through the upstream layers **Shields →
/// Armor gate** and then **carve a channel of cells** along the shot ray (Phase 2
/// fine destruction, ADR-0008/GDD §5), mutating the live `FitLayout`/`Shields`
/// components and returning the carving [`DamageOutcome`].
///
/// **Upstream (unchanged)**: the shot is absorbed by the [`Shields`] pool first; a
/// full absorb returns `ShieldAbsorbed`. The surviving magnitude is mitigated by the
/// Armor matrix cell, then the **armor gate** ([`resolve_penetration`], reading the
/// entry cell's hull-radial impact angle + the section's [`ArmorFacet`]) decides the
/// tier: a `Ricochet` deposits nothing and stops (no cell carved); otherwise the gate
/// yields a surviving-damage fraction (the `pen_tier`) applied to the post-armor
/// magnitude. The running magnitude stays **monotonically non-increasing** across the
/// upstream layers (T018 guards this).
///
/// **Carving (Phase 2)**: the post-armor surviving magnitude becomes a **damage
/// budget** and the event's `penetration` a **penetration budget**, walked down the
/// ray ([`carve_path`], entry-inward, deterministic). At each cell the shot subtracts
/// the cell's `health`-worth of work; a cell driven to `health <= 0` is **destroyed**
/// (removed from the layout, recorded in `destroyed_cells`) and the shot continues
/// with reduced budgets ([`CARVE_FALLOFF`]/[`CARVE_PEN_COST`]); a cell that only
/// survives chipped **lodges** the shot (stop). The walk also stops when either budget
/// is exhausted or the ray exits the grid. So a strong/high-pen shot carves a deeper
/// channel; a weak shot chips one cell; a `NoModule`/structural-only entry still
/// carves structural cells (the hull erodes where there is no module).
///
/// Total + server-authoritative (INV-D16): a hit with no resolvable fit/hull/resource
/// yields `DamageOutcome { result: NoModule, .. }` (never a panic). Health is clamped
/// `>= 0` (INV-D01). The cell removal flows to the client via the existing client-only
/// `render_state` cell payload (the hull mesh rebuilds on the cell-set change), so the
/// erosion renders for free. The borrow checker is managed by reading the small,
/// `Copy`/`Clone` component snapshots up front, then writing the mutated layout/
/// shields back at the end.
pub fn apply_damage(world: &mut World, target: Entity, ev: DamageEvent) -> DamageOutcome {
    // --- 1. Read the target's components + the content resources --------------
    // Clone the small reads so the resource/component borrows do not overlap the
    // later mutable writes (World access is sequenced).
    //
    // The carve is `Fit`-independent: the hull (and thus `grid_dims`) is resolved from
    // the target's `FitLayout.hull`, NOT from a `Fit`. So **wreckage** (a severed chunk
    // / destroyed-ship hulk — which carries a residual `FitLayout` but NO `Fit`) carves
    // through this SAME code path as a live ship; `Fit` stays on live ships but is no
    // longer required here.
    let Some(layout) = world.get::<FitLayout>(target).cloned() else {
        return no_module();
    };
    let mut shields = world.get::<Shields>(target).copied();
    // HullStructure is no longer mutated by carving (Phase 2 retired the structural-
    // spill model in favour of cell carving + core-death). It is still read so the
    // write-back leaves the (now-unchanged) component intact; kept for the unfitted/
    // backstop seam and the seeded-but-unused INV-D14 power-link decay path.
    let hull_structure = world.get::<HullStructure>(target).copied();
    let section_armor = world
        .get::<SectionArmor>(target)
        .cloned()
        .unwrap_or_default();
    // Severed wreckage uses its OWN CURRENT shape for the armor geometry (Fix #9), not the
    // original ship it came from; a live ship uses the authored hull (Fix #5/#8). Read with the
    // other per-target components, before the resource borrow.
    let is_wreck = world.get::<Wreck>(target).is_some();

    let Some(hulls) = world.get_resource::<HullCatalog>() else {
        return no_module();
    };
    // Resolve the hull / `grid_dims` from the layout's `hull` id (not a `Fit`) — the
    // `Fit`-independent lookup that lets wreckage carve through this same path.
    let Some(hull) = hulls.get(layout.hull).cloned() else {
        return no_module();
    };
    // The carve reads only the live `FitLayout` (cell health/occupancy) + the armor/
    // matrix content — it no longer needs the module catalog to resolve a behind-cell
    // (the route-behind model is retired). Keep a presence guard so a world without the
    // E007 content resources degrades to `NoModule` rather than half-resolving.
    if world.get_resource::<ModuleCatalog>().is_none() {
        return no_module();
    }
    let matrix = match world.get_resource::<ResistanceMatrix>() {
        Some(m) => *m,
        None => return no_module(),
    };
    let pen_cfg = world
        .get_resource::<PenetrationConfig>()
        .copied()
        .unwrap_or_default();
    // Phase M6: the promoted carve / ricochet feel-consts, live-tunable via the dev panel.
    // Absent (a minimal/determinism world) → the const defaults (byte-identical to pre-M6).
    let sim = world
        .get_resource::<crate::tuning::SimTuning>()
        .copied()
        .unwrap_or_default();

    let channel = ev.channel;

    // --- 2. Geometry: the carve path + its entry (surface) cell --------------
    // The carve walks the cells the ray crosses, entry-inward (deterministic). The
    // **entry cell** is the FIRST cell on this path — the genuine hull-surface cell
    // the shot meets, where the armor angle is read (NOT a deep module cell).
    let p0 = ev.point;
    let p1 = ev.point + ev.dir * REACH;
    let path = carve_path(&layout, p0, p1);

    // With **cell-precise** hit selection (see
    // [`fitted_damage_system`](crate::collision::fitted_damage_system)) `apply_damage` is
    // only ever called on a target whose cells the ray actually crosses — the narrow-phase
    // picks the target with the lowest cell-crossing toi via the SAME
    // [`first_cell_hit`]/`carve_path` crossing test — so `carve_path` here is non-empty.
    // An empty `path` therefore means only a degenerate case (an emptied layout mid-
    // despawn, or a direct non-selection call with no crossing): the total-pipeline
    // `NoModule` edge (correct, never a panic). There is NO nearest-cell fallback: a shot
    // that crosses no cell is a clean miss, not a force-carve on the wrong/nearest cell.
    let Some(&(entry_cell, _)) = path.first() else {
        return no_module();
    };

    // --- 3. Shields: absorb first --------------------------------------------
    let mut m = ev.magnitude;
    if let Some(s) = shields.as_mut() {
        let before = s.current;
        let (surviving, _depleted) = shield_absorb(s, &ev, &matrix);
        if surviving <= 0.0 {
            // Fully absorbed: write the drained shield back and stop.
            let absorbed = before - s.current;
            write_back(world, target, layout, shields, hull_structure);
            return DamageOutcome {
                struck: None,
                applied: absorbed,
                layer_reached: DefenseLayer::Shields,
                result: HitKind::ShieldAbsorbed,
                destroyed: false,
                destroyed_cells: Vec::new(),
            };
        }
        m = surviving;
    }

    // --- 4. Armor gate (approach obliquity vs the hull, erosion-independent) -
    let facet = section_of_cell(&hull, entry_cell)
        .and_then(|section| section_armor.facet(section).copied())
        .unwrap_or_else(|| default_facet(&ev));

    // Impact angle = the shot's obliquity against the entry cell's outer surface — measured
    // from the cell's **local surface normal** ([`local_surface_normal`]: the direction its
    // plate actually faces, from which neighbours are solid vs void), with a tunnel guard so a
    // bored channel does not re-ricochet. A shot meeting that surface square-on
    // (`cos_impact ≈ 1`) penetrates; a glancing edge hit (`cos_impact ≈ 0`) ricochets.
    //
    // Tunnel guard (erosion-aware, using the AUTHORED `hull`, [`authored_cell_in_front`]): if
    // the **original** silhouette had a cell *in front of* the entry cell along the actual
    // shot line (i.e. the shot bored through authored material to reach it), the shot is down a
    // tunnel → head-on (`angle 0`) — the first surviving cell recedes toward the core as a
    // channel is carved, and a shot down an already-bored tunnel meets it head-on along the
    // tunnel axis (no plate obliquity). Only when the entry cell is the first authored cell the
    // shot meets (a fresh outer surface) does its local-normal obliquity apply — so fresh
    // glancing hits ricochet, but a bored channel never re-ricochets. (The local normal also
    // replaced the old grid-centre radial, which mislabeled square-on flank hits on the
    // elongated hull as glancing — Fix #8.)
    let dir_n = if ev.dir.length_squared() > f32::EPSILON {
        ev.dir.normalize()
    } else {
        Vec2::X
    };
    // The reference shape for the armor geometry: a `Wreck` uses its OWN CURRENT cells (its
    // real, detached shape — Fix #9, so a chunk faces the way it ACTUALLY faces, not the
    // original ship it came from); a live ship uses the authored hull (Fix #5/#8). For a live
    // ship this set IS the authored hull cells, so the result is byte-identical to before.
    let geom_cells: BTreeSet<Cell> = if is_wreck {
        layout.cells.keys().copied().collect()
    } else {
        hull.cells.iter().map(|gc| gc.coord).collect()
    };
    // Tunnel vs fresh surface, tested **direction-accurately along the real ray**: the entry is
    // buried (a bored tunnel → head-on) iff `geom_cells` had any cell in front of it toward the
    // shooter on the shot line ([`cell_in_front`]). The previous single-dominant-axis check
    // misfired for off-axis bores on a shaped hull (Fix #5).
    let buried = cell_in_front(&geom_cells, entry_cell, dir_n);

    // The obliquity uses the entry cell's LOCAL surface normal (the direction its plate
    // actually faces, from which of its neighbours are solid vs void), NOT a far-away centre
    // radial. The old `entry_cell − grid_centre` proxy was wrong for an elongated/winged hull
    // — a flank cell's true normal faces sideways while the radial points along the long axis,
    // so square-on flank hits spuriously ricocheted (Fix #8). The local normal is exact for
    // any shape and any carve state.
    let (surface_normal, present_neighbors) =
        local_surface_normal(&geom_cells, entry_cell, sim.smooth_normal_radius);
    let cos_impact = if surface_normal.length_squared() <= f32::EPSILON {
        1.0 // no outward direction → treat as head-on
    } else {
        (-dir_n).dot(surface_normal.normalize())
    };
    let angle = if buried || present_neighbors < sim.ricochet_min_neighbors || cos_impact < 0.0 {
        // Down a bored tunnel (material in front — Fix #5's guard), a thin shard (too few
        // neighbours for a reliable surface normal → no spurious ricochet, Fix #10), or a
        // BACK-FACING entry whose local surface points away from the shooter (a concave/inside
        // corner — Fix #11 M1: previously `clamp(0,1)` forced this to 90° → a spurious ricochet;
        // the entry should be a front face, so treat it as head-on) → never a ricochet.
        0.0
    } else {
        // `cos_impact ∈ [0,1]`: 1 = square-on (carve), 0 = grazing-parallel (→ 90°, ricochet).
        cos_impact.min(1.0).acos()
    };

    // Mitigate at the Armor layer before the penetration tier applies.
    m *= 1.0 - layer_resist(&matrix, DefenseLayer::Armor, channel);

    let pen = resolve_penetration(
        facet.thickness,
        angle,
        ev.penetration,
        ev.pen_size,
        facet.material,
        &pen_cfg,
    );

    // The kind tag + the post-armor surviving fraction that becomes the carve damage
    // budget. A `Ricochet` deposits nothing and stops (no cell carved) via an early
    // return; the other tiers scale the running magnitude `m` and yield their tag.
    let result = match pen {
        PenetrationResult::Ricochet { .. } => {
            // Bounced: nothing carves. Write back the (untouched) layout/shields. The
            // HUD attribution is the entry cell's module if it is a module cell.
            let struck = layout
                .cells
                .get(&entry_cell)
                .and_then(|occ| occ.module.map(|m| ModuleRef::new(occ.slot, m)));
            write_back(world, target, layout, shields, hull_structure);
            return DamageOutcome {
                struck,
                applied: 0.0,
                layer_reached: DefenseLayer::Armor,
                result: HitKind::Ricochet,
                destroyed: false,
                destroyed_cells: Vec::new(),
            };
        }
        PenetrationResult::NonPenetration { surviving, .. } => {
            // Stopped-ish by the plate: only the small non-pen tier survives to carve.
            m *= surviving;
            HitKind::Penetrated
        }
        PenetrationResult::Penetration { surviving, .. } => {
            m *= surviving;
            HitKind::Penetrated
        }
        PenetrationResult::OverPenetration { surviving, .. } => {
            m *= surviving;
            HitKind::OverPenetrated
        }
    };

    // --- 4b. Armor-HP layer (Phase F + Refinement 13): a depleting HP buffer between the shield
    // and the hull carve. A PENETRATING hit (a Ricochet already returned above, so armor is never
    // touched on a bounce) is soaked by the armor while it holds (`current > 0`): the post-armor
    // magnitude `m` (the would-be carve budget) drains the pool. A hit `<= current` is fully
    // soaked → the hull does NOT carve (a normal shot is « remaining armor, so it always takes this
    // path — armor is a buffer you strip over many shots). Refinement 13: a hit LARGER than the
    // remaining armor drains it to 0 and **spills the excess** to the carve, so a single
    // over-the-top hit (a hard ram) punches through. When the component is ABSENT (every
    // determinism/test ship carries none) the block is skipped → the headless path stays
    // byte-identical, and a test ship that does carry it is only hit by sub-armor shots (fully
    // soaked) → still byte-identical. Armor does not regenerate.
    if let Some(mut armor) = world.get::<ArmorHp>(target).copied() {
        if armor.current > 0.0 {
            let absorbed = armor.current.min(m.max(0.0));
            armor.current = (armor.current - m.max(0.0)).max(0.0);
            if let Some(mut comp) = world.get_mut::<ArmorHp>(target) {
                *comp = armor;
            }
            // Refinement 13: armor SPILLS. Subtract what the plate soaked; only the EXCESS — a
            // single hit bigger than the remaining armor (a hard RAM, or a future heavy weapon) —
            // spills past to carve the hull. A normal autocannon shot is « the remaining armor, so
            // it is still fully soaked (the `m <= 0` branch below returns with no carve), preserving
            // the 2-stage armor feel for shots AND the byte-identical headless path (test ships
            // carry no `ArmorHp`; any that do are only ever hit by sub-armor shots).
            m = (m - absorbed).max(0.0);
            if m <= 0.0 {
                // Fully soaked: hull protected — write back the (uncarved) layout/shields and stop.
                // HUD attribution is the entry cell's module if it is one (else `None`).
                let struck = layout
                    .cells
                    .get(&entry_cell)
                    .and_then(|occ| occ.module.map(|mid| ModuleRef::new(occ.slot, mid)));
                write_back(world, target, layout, shields, hull_structure);
                return DamageOutcome {
                    struck,
                    applied: absorbed,
                    layer_reached: DefenseLayer::Armor,
                    result,
                    destroyed: false,
                    destroyed_cells: Vec::new(),
                };
            }
            // else: the over-the-top excess `m` continues to the carve below.
        }
    }

    // --- 5. Carve budget = the post-armor surviving magnitude ----------------
    // The Hull/Systems matrix passes from the old route-behind model are **not**
    // applied to the carve budget: the cells the channel carves ARE the hull
    // structure / the systems behind it, so applying a Hull/Systems resist here would
    // double-count the mitigation against the very cells being removed. The shot has
    // already paid Shields + the Armor matrix + the penetration tier; what survives is
    // the work it does carving cells. (The matrix's Hull/Systems columns still matter
    // upstream via the channel's strong-vs-layer pen behavior; they are just not a
    // second multiplier on the carve work.)
    //
    // --- 6. Carve a channel of cells along the shot ray (Phase 2) ------------
    // The post-armor magnitude is the damage budget; the event penetration is the
    // penetration budget. Walk the present cells the ray crosses (the `path` computed
    // in step 2, entry-inward) and carve each in turn until a cell survives (lodge), a
    // budget is spent, or the ray exits the grid. Removed cells are recorded
    // (deterministic order) + dropped from the live FitLayout → the client erodes the
    // hull for free via the cell payload.
    let mut layout = layout;

    let mut damage_budget = m.max(0.0);
    let mut pen_budget = ev.penetration.max(0.0);
    let mut destroyed_cells: Vec<Cell> = Vec::new();
    let mut applied = 0.0_f32;
    // The HUD attribution: the first MODULE cell the channel touches (entry module if
    // it is a module cell, else the first module cell carved through), else `None`.
    let mut struck_ref: Option<ModuleRef> = None;

    for (cell, occ) in &path {
        if damage_budget <= 0.0 || pen_budget <= 0.0 {
            break;
        }
        // The cell's live remaining health — the work needed to DESTROY it. An already-
        // dead cell (`health <= 0`, e.g. chipped to 0 by a prior shot) needs no work
        // and is removed immediately.
        let remaining = occ.health.max(0.0);
        // Record the module attribution from the first module cell on the channel.
        if struck_ref.is_none() {
            if let Some(module_id) = occ.module {
                struck_ref = Some(ModuleRef::new(occ.slot, module_id));
            }
        }

        if damage_budget >= remaining {
            // Punched through: the shot finishes this cell. Remove it, spend the budgets
            // (a hollow/already-dead cell still costs a small CARVE_MIN_CELL_COST so the
            // channel is not infinitely free), apply the falloff, and continue carving
            // the next cell deeper with the reduced budget.
            let cost = remaining.max(sim.carve_min_cell_cost);
            applied += remaining;
            layout.cells.remove(cell);
            destroyed_cells.push(*cell);
            damage_budget = (damage_budget - cost).max(0.0) * sim.carve_falloff;
            pen_budget -= sim.carve_pen_cost;
        } else {
            // The shot lodges in this cell: chip its health (clamped >= 0) and stop.
            applied += damage_budget;
            if let Some(live) = layout.cells.get_mut(cell) {
                live.health = (live.health - damage_budget).max(0.0);
            }
            break;
        }
    }

    let destroyed = !destroyed_cells.is_empty();
    // The deepest layer the carve reached: Systems if a module cell was on the channel,
    // else HullStructure (the structural body).
    let layer_reached = if struck_ref.is_some() {
        DefenseLayer::Systems
    } else {
        DefenseLayer::HullStructure
    };

    write_back(world, target, layout, shields, hull_structure);

    DamageOutcome {
        struck: struck_ref,
        applied,
        layer_reached,
        result,
        destroyed,
        destroyed_cells,
    }
}

/// The "hit on nothing" total-pipeline outcome (no geometry / no fit / missing
/// resource): no module struck, nothing applied (INV total, never a panic).
fn no_module() -> DamageOutcome {
    DamageOutcome {
        struck: None,
        applied: 0.0,
        layer_reached: DefenseLayer::Shields,
        result: HitKind::NoModule,
        destroyed: false,
        destroyed_cells: Vec::new(),
    }
}

/// Write the mutated layer state back onto the target entity (the live
/// `FitLayout`/`Shields`/`HullStructure` components), server-authoritative.
fn write_back(
    world: &mut World,
    target: Entity,
    layout: FitLayout,
    shields: Option<Shields>,
    hull_structure: Option<HullStructure>,
) {
    if let Some(mut comp) = world.get_mut::<FitLayout>(target) {
        *comp = layout;
    }
    if let Some(s) = shields {
        if let Some(mut comp) = world.get_mut::<Shields>(target) {
            *comp = s;
        }
    }
    if let Some(hs) = hull_structure {
        if let Some(mut comp) = world.get_mut::<HullStructure>(target) {
            *comp = hs;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shields_full_and_depleted_constructors() {
        let full = Shields::full(100.0, 5.0, true);
        assert_eq!(full.current, 100.0);
        assert_eq!(full.max, 100.0);
        assert!(full.power_linked);

        let dep = Shields::depleted(100.0, 5.0, false);
        assert_eq!(dep.current, 0.0);
        assert!(!dep.power_linked);
    }

    #[test]
    fn section_armor_facet_lookup_is_null_safe() {
        let mut armor = SectionArmor::new();
        armor.sections.insert(
            SectionId(0),
            ArmorFacet {
                thickness: 10.0,
                material: ArmorMaterial::Steel,
                normal: Vec2::new(1.0, 0.0),
            },
        );
        assert!(armor.facet(SectionId(0)).is_some());
        assert!(armor.facet(SectionId(99)).is_none());
    }

    #[test]
    fn section_health_destroyed_at_zero() {
        let mut sh = SectionHealth::full(SectionId(1), 50.0);
        assert!(!sh.is_destroyed());
        sh.current = 0.0;
        assert!(sh.is_destroyed());
    }

    #[test]
    fn damage_context_bundles_layer_state() {
        let ctx = DamageContext {
            shields: Shields::full(50.0, 2.0, true),
            armor: SectionArmor::new(),
            hull: HullStructure::full(200.0),
        };
        assert_eq!(ctx.shields.max, 50.0);
        assert_eq!(ctx.hull.max, 200.0);
    }

    // R58 — the sub-cell HITBOX matches the visual: a shot through a corner triangle's solid side hits,
    // a shot that only crosses its cut-away (empty) corner misses. `HalfNE` at cell (0,0) keeps the NE
    // right-angle (corners (1,1)/(0,1)/(1,0)), so the solid region is x+y >= 1 and the SW corner is gone.
    #[test]
    fn sub_shape_hitbox_matches_the_triangle() {
        use crate::fitting::CellShape;
        let tri = CellShape::HalfNE.corners(0, 0);

        // Inside the solid NE half (x+y > 1) vs the cut-away SW corner (x+y < 1).
        assert!(point_in_convex(Vec2::new(0.9, 0.9), &tri));
        assert!(!point_in_convex(Vec2::new(0.2, 0.2), &tri));

        // A horizontal ray at y=0.2: the triangle there spans x∈[0.8, 1.0]. A ray reaching x=0.95 enters
        // the triangle → hit; a ray that stops at x=0.5 stays in the cut corner → miss.
        assert!(
            swept_point_convex_toi(Vec2::new(-1.0, 0.2), Vec2::new(0.95, 0.2), &tri).is_some(),
            "a shot crossing the solid triangle side must hit",
        );
        assert!(
            swept_point_convex_toi(Vec2::new(-1.0, 0.2), Vec2::new(0.5, 0.2), &tri).is_none(),
            "a shot that only crosses the cut-away corner must miss",
        );
    }

    // R58 — a sub-shape's area_factor (mass weight) is its true fraction of the unit cell: full 1.0,
    // half 0.5, quarter 0.125. `Full`'s centroid stays the cell centre (byte-identical mass/COM path).
    #[test]
    fn sub_shape_area_factor_and_full_centroid() {
        use crate::fitting::CellShape;
        assert_eq!(CellShape::Full.area_factor(), 1.0);
        assert_eq!(CellShape::HalfNE.area_factor(), 0.5);
        assert_eq!(CellShape::QuarterSW.area_factor(), 0.125);
        assert_eq!(CellShape::Full.centroid(3, 7), Vec2::new(3.5, 7.5));
    }
}
