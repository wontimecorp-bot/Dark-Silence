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
    resolve_hit, Cell, Fit, FitLayout, HitResolution, Hull, HullCatalog, ModuleCatalog, ModuleRef,
    SectionId,
};

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
/// outcome the HUD reads + the destruction flag the re-derive/destruction worker
/// consumes (later phases).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageOutcome {
    /// The module struck (the entry module for a ricochet/non-pen, the module
    /// behind for a clean/over penetration), or `None` on a shield-absorb / spill /
    /// no-module hit.
    pub struck: Option<ModuleRef>,
    /// The magnitude that actually landed on the struck target (the shield-absorbed
    /// amount for a `ShieldAbsorbed`, the post-traversal damage otherwise).
    pub applied: f32,
    /// The deepest [`DefenseLayer`] the surviving damage reached.
    pub layer_reached: DefenseLayer,
    /// The legibility tag (FR-024).
    pub result: HitKind,
    /// Whether the struck target was destroyed (`health <= 0`) by this hit
    /// (INV-D01).
    pub destroyed: bool,
}

/// Apply a [`DamageEvent`] to `target` through the full ordered traversal
/// **Shields → Armor → Hull → Systems** (FR-002/003/004/009/011, data-model.md
/// "Resolution Order" §254-264), mutating the live `FitLayout`/`Shields`/
/// `HullStructure` components and returning the legible [`DamageOutcome`].
///
/// The running magnitude is **monotonically non-increasing** across the layers
/// (every factor is `≤ 1`: shield absorption removes some, each matrix cell removes
/// `layer_resist ∈ [0,1)`, each penetration tier is `≤ 1`) — T018 guards this.
///
/// Total + server-authoritative (INV-D16): a hit on an empty/structural/off-grid
/// cell yields `DamageOutcome { result: NoModule, .. }` (never a panic). Health is
/// clamped `>= 0` (INV-D01). The borrow checker is managed by reading the small,
/// `Copy`/`Clone` component snapshots up front, then writing the mutated layout/
/// structure/shields back at the end.
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
    let mut hull_structure = world.get::<HullStructure>(target).copied();
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
    let catalog = match world.get_resource::<ModuleCatalog>() {
        Some(c) => c.clone(),
        None => return no_module(),
    };
    let matrix = match world.get_resource::<ResistanceMatrix>() {
        Some(m) => *m,
        None => return no_module(),
    };
    let pen_cfg = world
        .get_resource::<PenetrationConfig>()
        .copied()
        .unwrap_or_default();

    let channel = ev.channel;

    // --- 2. Geometry: resolve the entry point --------------------------------
    let Some(entry) = resolve_entry_point(&fit, &hull, &catalog, &ev) else {
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
            };
        }
        m = surviving;
    }

    // --- 4. Armor gate -------------------------------------------------------
    let facet = section_of_cell(&hull, entry.cell)
        .and_then(|section| section_armor.facet(section).copied())
        .unwrap_or_else(|| default_facet(&ev));

    // Impact angle from the REAL hit geometry, not the seeded facet `normal`.
    //
    // Fixes the live-demo ricochet bug: the per-section [`ArmorFacet::normal`]
    // (e.g. the centred core's fixed `CORE_FALLBACK_NORMAL = -X` from
    // `seed_defense_layers`) is constant, but the entry-ray transform routes every
    // shot through the grid centre, so the centred core is the entry for shots from
    // ANY direction. Measuring the angle off that fixed `-X` face meant a shot from
    // any side but `+X` met it at a steep angle → permanent `Ricochet`, and once the
    // shield was down the enemy could never be hull-killed.
    //
    // The obliquity should instead reflect WHERE on the hull silhouette the shot
    // landed. Derive the outward surface normal at the entry from the entry cell's
    // radial position on the hull: `radial = entry_cell_center − grid_centre_local`.
    // A cell on the +x rim faces +x, one on the −y rim faces −y, etc. A shot striking
    // the hull roughly perpendicular to that local surface penetrates from any
    // approach; only a genuinely glancing/edge hit ricochets.
    //
    // The dead-centre core cell has `radial ≈ 0` (no outward direction) — a centre
    // hit means the shot already reached the core, so treat it as **head-on**
    // (angle 0, never a ricochet). `cos_impact = (-dir)·surface_normal` is clamped to
    // `[0, 1]`: a negative value would mean the entry geometry put us on a back face
    // (unphysical for an entry), so floor it at 0 (grazing) rather than letting it
    // become a 180° bounce. `angle ∈ [0, π/2]`. The facet's `thickness`/`material`
    // (and thus armored-section toughness) are still used below for the EFFECTIVE
    // armor — only the ANGLE source changed, so angling still matters (edge hits
    // ricochet, armored sections deflect more), but a head-on shot from any side
    // reliably penetrates. (`facet.normal` is now unused for the angle.)
    let grid_centre_local = Vec2::new(hull.grid_dims.0 as f32 * 0.5, hull.grid_dims.1 as f32 * 0.5);
    let entry_cell_center = Vec2::new(entry.cell.0 as f32 + 0.5, entry.cell.1 as f32 + 0.5);
    let radial = entry_cell_center - grid_centre_local;
    let dir_n = if ev.dir.length_squared() > f32::EPSILON {
        ev.dir.normalize()
    } else {
        Vec2::X
    };
    let angle = if radial.length_squared() <= f32::EPSILON {
        // Dead-centre core hit: no outward direction → head-on, never a ricochet.
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

    // The kind tag + the cell behind the entry the surviving damage routes to. The
    // behind cell is resolved **once** (route_behind is re-entry-safe but pure, so
    // a single call is enough) and reused for both the struck-ref and the mutation.
    let result;
    let behind = match pen {
        PenetrationResult::Ricochet { .. } => {
            // Bounced: no module-behind damage. The entry module takes nothing.
            write_back(world, target, layout, shields, hull_structure);
            return DamageOutcome {
                struck: Some(entry.module),
                applied: 0.0,
                layer_reached: DefenseLayer::Armor,
                result: HitKind::Ricochet,
                destroyed: false,
            };
        }
        PenetrationResult::NonPenetration { surviving, .. } => {
            // Stopped by the plate: the tier (≈0) spills to HullStructure; nothing
            // routes to a module behind.
            m *= surviving;
            result = HitKind::Penetrated;
            None
        }
        PenetrationResult::Penetration { surviving, .. } => {
            // Clean pass-into: route the surviving tier to the cell BEHIND the entry
            // (INV-D06, outer-before-inner).
            m *= surviving;
            result = HitKind::Penetrated;
            route_behind(&fit, &hull, &catalog, &entry, &ev)
        }
        PenetrationResult::OverPenetration { surviving, .. } => {
            // Pass-through: the reduced over tier routes to the cell behind.
            m *= surviving;
            result = HitKind::OverPenetrated;
            route_behind(&fit, &hull, &catalog, &entry, &ev)
        }
    };

    let struck_ref = behind.map(|b| b.module);
    // The deepest layer the surviving damage reaches: Systems when a module-behind
    // is struck, HullStructure for spillover.
    let layer_reached = if struck_ref.is_some() {
        DefenseLayer::Systems
    } else {
        DefenseLayer::HullStructure
    };

    // --- 5. Hull then Systems mitigation -------------------------------------
    m *= 1.0 - layer_resist(&matrix, DefenseLayer::HullStructure, channel);
    m *= 1.0 - layer_resist(&matrix, DefenseLayer::Systems, channel);

    // --- 6. Apply to the struck target (clamped >= 0, INV-D01) ---------------
    let mut layout = layout;
    let mut destroyed = false;
    let applied = m.max(0.0);

    if let Some(behind) = behind {
        // Reduce the behind cell's live module health in the FitLayout (FR-009/011).
        if let Some(occ) = layout.cells.get_mut(&behind.cell) {
            occ.health = (occ.health - applied).max(0.0);
            destroyed = occ.health <= 0.0;
        }
    } else if let Some(hs) = hull_structure.as_mut() {
        // Spillover (non-pen, or nothing behind a penetration): structural HP.
        hs.current = (hs.current - applied).max(0.0);
        destroyed = hs.current <= 0.0;
    }

    write_back(world, target, layout, shields, hull_structure);

    DamageOutcome {
        struck: struck_ref,
        applied,
        layer_reached,
        result,
        destroyed,
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
