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

use std::collections::BTreeMap;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::Vec2;
use serde::{Deserialize, Serialize};

use super::content::{ArmorMaterial, PenetrationConfig};
use super::event::DamageEvent;
use super::penetration::{resolve_penetration, PenetrationResult};
use super::resist::{layer_resist, DefenseLayer, ResistanceMatrix};
use super::shields::shield_absorb;
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
/// spans it).
const REACH: f32 = 64.0;

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
const CARVE_PEN_COST: f32 = 8.0;

/// Multiplicative damage falloff applied to the surviving `damage` budget after a
/// cell is punched through (the shot loses energy crossing each cell). `∈ (0, 1)`: a
/// value near `1` carves a long channel from one strong shot, near `0` chips ~one
/// cell. With the per-cell `health` subtraction this gives a tunable, decaying
/// channel depth.
const CARVE_FALLOFF: f32 = 0.75;

/// The per-cell `health` floor used as the carve *work cost* for an empty module-slot
/// cell (`health == 0`, no installed device): such a cell still costs a little to
/// punch through so the channel does not carve infinitely free through hollow slots.
const CARVE_MIN_CELL_COST: f32 = 1.0;

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
const CARVE_CELL_RADIUS: f32 = 0.5;

/// Walk the present cells of `layout` the ray `p0 → p1` passes through, in the
/// deterministic order the shot would carve them: ascending time-of-impact along the
/// ray, ties broken outer-before-inner by occlusion `depth`, then by [`Cell`] order
/// (Principle II — no `HashMap` iteration; the source map is a `BTreeMap`).
///
/// Reuses the **existing** swept point-vs-circle primitive
/// ([`Physics::swept_cast`]) against each present cell's inscribed circle — the same
/// CCD `resolve_hit` uses, no new geometry. Returns each crossed cell paired with its
/// occupant snapshot (its live `health`/`module`), so the caller can carve it without
/// re-borrowing the layout per step. Pure; reads only its arguments.
fn carve_path(layout: &FitLayout, p0: Vec2, p1: Vec2) -> Vec<(Cell, CellOccupant)> {
    let physics = RapierPhysics::new();
    let mut hits: Vec<(f32, u16, Cell, CellOccupant)> = Vec::new();
    for (&cell, occ) in &layout.cells {
        let center = Vec2::new(cell.0 as f32 + 0.5, cell.1 as f32 + 0.5);
        if let Some(hit) = physics.swept_cast(p0, p1, center, CARVE_CELL_RADIUS) {
            hits.push((hit.toi, occ.depth, cell, *occ));
        }
    }
    // Ascending toi (first crossed first); ties → outer (lower depth) first; then the
    // BTreeMap-natural Cell order for a fully deterministic carve sequence.
    hits.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });
    hits.into_iter()
        .map(|(_, _, cell, occ)| (cell, occ))
        .collect()
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
    let Some(fit) = world.get::<Fit>(target).cloned() else {
        return no_module();
    };
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

    let Some(hulls) = world.get_resource::<HullCatalog>() else {
        return no_module();
    };
    let Some(hull) = hulls.get(fit.hull).cloned() else {
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

    let channel = ev.channel;

    // --- 2. Geometry: the carve path + its entry (surface) cell --------------
    // The carve walks the cells the ray crosses, entry-inward (deterministic). The
    // **entry cell** is the FIRST cell on this path — the genuine hull-surface cell
    // the shot meets, where the armor angle is read (NOT a deep module cell). An empty
    // path (the ray crosses no present cell — off-grid / fully eroded) is the
    // total-pipeline `NoModule` edge (never a panic).
    let p0 = ev.point;
    let p1 = ev.point + ev.dir * REACH;
    let path = carve_path(&layout, p0, p1);
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

    // Impact angle = the shot's obliquity against the **original outer hull surface**
    // at the entry cell — with a tunnel guard so a bored channel does not re-ricochet.
    //
    // The entry cell's radial (`entry_cell_center − grid_centre`) is its outward
    // surface normal: a +x-rim cell faces +x, a wing-tip faces outward, etc. A shot
    // meeting that surface square-on (`cos_impact ≈ 1`) penetrates; a glancing edge hit
    // (`cos_impact ≈ 0`) ricochets. BUT as a channel is carved inward, the first
    // surviving cell recedes toward the core, whose radial is perpendicular to a
    // side-on approach — which would wrongly flip a long-bored head-on burst into a
    // permanent `Ricochet`. The physical truth: a shot travelling down an
    // already-bored tunnel meets the cell at its end **head-on along the tunnel axis**
    // (no plate obliquity).
    //
    // Tunnel guard (erosion-aware, using the AUTHORED `hull`): if the **original**
    // silhouette had a cell *in front of* the entry cell along the approach (i.e. the
    // entry cell was buried in the intact hull and only exposed by carving), the shot
    // is down a tunnel → head-on (`angle 0`). Only when the entry cell is on the
    // original outer surface (no authored cell in front) does its radial obliquity
    // apply — so fresh glancing hits ricochet, but a bored channel never re-ricochets.
    let dir_n = if ev.dir.length_squared() > f32::EPSILON {
        ev.dir.normalize()
    } else {
        Vec2::X
    };
    // The cell one step in front of the entry along the approach (toward the shooter,
    // `-dir_n`), rounded to the dominant axis — the cell the shot would have passed
    // through just before reaching the entry on the original hull.
    let front = {
        let step = -dir_n;
        let (dc, dr) = if step.x.abs() >= step.y.abs() {
            (step.x.signum() as i32, 0)
        } else {
            (0, step.y.signum() as i32)
        };
        let fc = entry_cell.0 as i32 + dc;
        let fr = entry_cell.1 as i32 + dr;
        (fc, fr)
    };
    let buried = front.0 >= 0
        && front.1 >= 0
        && hull
            .cells
            .iter()
            .any(|gc| gc.coord == (front.0 as u16, front.1 as u16));

    let grid_centre_local = Vec2::new(hull.grid_dims.0 as f32 * 0.5, hull.grid_dims.1 as f32 * 0.5);
    let entry_cell_center = Vec2::new(entry_cell.0 as f32 + 0.5, entry_cell.1 as f32 + 0.5);
    let radial = entry_cell_center - grid_centre_local;
    let angle = if buried || radial.length_squared() <= f32::EPSILON {
        // Down a bored tunnel (entry was buried in the intact hull), or a centred
        // surface cell with no outward direction → head-on, never a ricochet.
        0.0
    } else {
        let surface_normal = radial.normalize();
        let cos_impact = (-dir_n).dot(surface_normal).clamp(0.0, 1.0);
        cos_impact.acos()
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
            let cost = remaining.max(CARVE_MIN_CELL_COST);
            applied += remaining;
            layout.cells.remove(cell);
            destroyed_cells.push(*cell);
            damage_budget = (damage_budget - cost).max(0.0) * CARVE_FALLOFF;
            pen_budget -= CARVE_PEN_COST;
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
}
