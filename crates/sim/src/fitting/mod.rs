//! Ship fitting & modules — the unified, data-driven domain model (E006,
//! ADR-0008).
//!
//! All fitting domain logic lives here in the shared `sim` crate (Principle II)
//! so a future server validates fits and derives stats on the same code path the
//! client previews against. This module is the entry point; it re-exports the
//! public surface as the submodules grow across phases.
//!
//! Current surface (Phase 1 Setup + Phase 2 Foundational keystone):
//! - [`module`] — the uniform [`Module`] stat block + shared enums.
//! - [`hull`] — the 2D cell-grid [`Hull`] + positional [`Slot`] inventory.
//! - [`fit`] — the [`Fit`] (hull + slot→module map) + bare install/remove.
//! - [`content`] — the seed [`ModuleCatalog`]/[`HullCatalog`] ([`seed_catalogs`]).
//!
//! Current surface also includes Phase 3 (US1) validation:
//! - [`validate`] — per-axis budgets ([`budget_usage`]/[`BudgetUsage`]), the
//!   slot type/size gate ([`check_slot_fit`]), and total validation
//!   ([`validate_fit`]/[`FitValidation`]); install is now validate-then-apply
//!   ([`Fit::install_module`]/[`FitRejection`]).
//!
//! Phase 4 (US2) adds fit-derived effective stats:
//! - [`stats`] — the [`ShipStats`] component + [`WeaponProfile`] and
//!   [`derive_ship_stats`], the per-ship flight + weapon source that **replaces
//!   the global `Tuning`** (FR-014; `Tuning` is demoted to the base-constant +
//!   seed-baseline source). [`recompute_ship_stats_system`] re-derives a ship's
//!   `ShipStats` whenever its [`Fit`] mutates (INV-F08).
//!
//! Phase 5 (US3) adds the hit/armor map — the fit-layout IS the hitbox (ADR-0008):
//! - [`layout`] — the [`FitLayout`] component (per-cell [`CellOccupant`] +
//!   occlusion `depth`) built by [`build_layout`], the E007 per-cell queries
//!   [`module_at`]/[`cell_map`], the outer-before-inner [`resolve_hit`]
//!   ([`HitResolution`]), and the position/facing [`hardpoint_arc`] (FR-018/019/
//!   020/021). The fit-change system ([`recompute_ship_stats_system`]) also rebuilds
//!   a ship's `FitLayout` alongside its `ShipStats` (INV-F08).

pub mod content;
pub mod fit;
pub mod hull;
pub mod layout;
pub mod module;
pub mod stats;
pub mod validate;

pub use content::{
    baseline_fit, baseline_hull, parse_catalogs, seed_catalogs, HullCatalog, ModuleCatalog,
    HULL_BASELINE, HULL_CORVETTE, HULL_FIGHTER, HULL_MINENODE, HULL_OUTPOST, HULL_TRANSPORT,
    MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_BASELINE_THRUSTER, MODULE_REACTOR_BASIC,
    MODULE_SHIELD_BASIC, MODULE_THRUSTER_BASIC, MODULE_UTILITY_BASIC, STRUCT_CELL_MASS,
};
pub use fit::{
    load_preset, preview_stats, save_preset, Fit, FitPreset, FitRejection, ModuleRef, PresetId,
};
pub use hull::{
    disc_hull, hull_collision_radius, station_hull, FiringArc, GridCell, Hull, HullId, SectionId,
    ShipClass, ShipRole, Slot, SlotId, CELL_WORLD_SIZE,
};
pub use layout::{
    build_layout, build_layout_with, cell_map, cell_mass, cell_mass_with, center_or_anchor,
    hardpoint_arc, layout_center, layout_inertia, layout_inertia_with, layout_mass,
    layout_mass_with, module_at, resolve_hit, Cell, CellMap, CellOccupant, FitLayout,
    HitResolution,
};
pub use module::{
    AmmoType, Axis, HardpointType, Module, ModuleId, ModuleKind, ModuleSpecifics, PropulsionType,
    SensorType, SlotSize, Violation, WeaponClass,
};
pub use stats::{
    derive_ship_stats, derive_ship_stats_with, module_conditions, ModuleCondition, ShipStats,
    WeaponProfile,
};
pub use validate::{
    budget_usage, check_slot_fit, validate_fit, AxisUsage, BudgetUsage, FitValidation,
};

use bevy_ecs::prelude::*;

use crate::components::ArmorHp;
use crate::damage::Shields;

/// Re-derive a ship's [`ShipStats`] whenever its [`Fit`] **or** its [`FitLayout`]
/// changes (INV-F08, FR-012/013/014/019, INV-D13) — the E007 emergent-damage hook.
///
/// The recompute system fires on `Or<(Changed<Fit>, Changed<FitLayout>)>` and folds
/// the two triggers carefully, because a fit re-configure and a damage event mean
/// opposite things for the layout:
///
/// - **`Changed<Fit>`** — the fit was re-configured (install/remove). The layout is
///   **rebuilt** fresh ([`build_layout`], full health: re-fitting repairs), then
///   stats derive from it.
/// - **only `Changed<FitLayout>`** — `apply_damage` mutated a cell's health. The
///   layout is **NOT** rebuilt (that would erase the damage); stats derive from the
///   existing *damaged* layout, so a battered ship derives degraded numbers
///   (SC-002, FR-012/013).
///
/// Either way the ship's [`ShipStats`] is overwritten with the freshly
/// [`derive_ship_stats`]-d value, so flight/weapon behavior always reflects the
/// current fit **and** its live damage. Pure derivation, no other state touched.
///
/// One-tick echo (harmless): rebuilding the layout on a fit change sets
/// `Changed<FitLayout>`, so the system runs again next tick — but with `Fit`
/// unchanged it takes the no-rebuild branch and re-derives idempotently.
///
/// Override-or-fallback contract: this updates only entities that carry **all** of
/// [`Fit`] + [`ShipStats`] + [`FitLayout`] (a fitted ship always has a layout).
/// Unfitted ships (E001/E002/E003 fixtures, server/bot ships) carry none of these
/// and are untouched — they keep flying on the global
/// [`Tuning`](crate::tuning::Tuning) via the flight system's fallback path.
///
/// A [`Fit`] referencing an unknown hull is skipped (no panic) — derivation +
/// layout both require a resolvable hull; the dangling-ref *rejection* is
/// `validate_fit`'s concern (INV-F13).
pub fn recompute_ship_stats_system(
    hulls: Res<HullCatalog>,
    modules: Res<ModuleCatalog>,
    // Phase M6: read the live tuning so a re-derive (incl. the dev panel's "Apply / Re-derive"
    // which `set_changed`s every `Fit`) rebuilds layouts + stats at the live structural-cell
    // HP/mass. Absent (a minimal world) → the const defaults.
    sim: Option<Res<crate::tuning::SimTuning>>,
    mut q: Query<
        (
            Ref<Fit>,
            &mut ShipStats,
            &mut FitLayout,
            Option<&mut Shields>,
            Option<&mut ArmorHp>,
        ),
        Or<(Changed<Fit>, Changed<FitLayout>)>,
    >,
) {
    let sim = sim.map(|s| *s).unwrap_or_default();
    for (fit, mut stats, mut layout, shields, armor) in &mut q {
        let Some(hull) = hulls.get(fit.hull) else {
            // Unknown hull: cannot derive; leave the prior stats/layout untouched.
            continue;
        };
        // A fit re-configure rebuilds the layout fresh (full health = repaired);
        // a layout-only change (damage) preserves the damaged health.
        if fit.is_changed() {
            *layout = build_layout_with(hull, &fit, &modules, sim.struct_cell_hp);
        }
        *stats = derive_ship_stats_with(hull, &fit, &modules, &layout, sim.struct_cell_mass);

        // Refinement 10: sync the live defense pools' CAPS to the freshly-derived stats so a
        // carved/damaged generator (or armor module) shrinks — or zeroes — the pool. Shields now
        // follow the shield generator's health (a destroyed generator → `shield_max == 0` → no
        // shields); armor follows the fitted armor modules. For an UNDAMAGED ship the derived caps
        // equal the spawn-time seed, so this is a no-op (determinism-safe); only a damaged/carved
        // module moves them. `current` is clamped down so a shrunken cap can't leave an over-full
        // pool.
        if let Some(mut shields) = shields {
            shields.max = stats.shield_max;
            shields.regen_rate = stats.shield_regen;
            shields.current = shields.current.min(shields.max);
        }
        if let Some(mut armor) = armor {
            armor.max = stats.armor_value;
            armor.current = armor.current.min(armor.max);
        }
    }
}

/// Force EVERY fitted ship to re-derive next tick (Phase M6 — the dev panel's "Apply / Re-derive"
/// button): marks each [`Fit`] changed so [`recompute_ship_stats_system`] rebuilds its
/// [`FitLayout`] + [`ShipStats`] at the **live** [`SimTuning`](crate::SimTuning) + catalog values
/// (editing a module's mass/thrust or `struct_cell_*` doesn't touch a ship's cached stats until
/// it re-derives). **Caveat:** a `Changed<Fit>` rebuilds the layout FRESH (full health) — it
/// previews new balance but **repairs** the ship, wiping live battle damage. Solo / dev only.
pub fn force_rederive_all(world: &mut World) {
    let mut q = world.query::<&mut Fit>();
    // `set_changed` trips `Changed<Fit>` even though the value is untouched.
    for mut fit in q.iter_mut(world) {
        fit.set_changed();
    }
}

/// Re-derive every fitted ship's [`ShipStats`] from the live catalog/tuning **without repairing**
/// (Refinement 39 — the dev panel's live "edit a module DESIGN and see all ships update"). Marks each
/// [`FitLayout`] changed (NOT [`Fit`]), so [`recompute_ship_stats_system`] takes the no-rebuild branch
/// (`fit.is_changed()` is false → the layout + its per-cell health are preserved) and only re-derives
/// the stats. Unlike [`force_rederive_all`] this keeps live battle damage. Solo / dev only.
pub fn force_rederive_keep_health(world: &mut World) {
    let mut q = world.query::<&mut FitLayout>();
    for mut layout in q.iter_mut(world) {
        layout.set_changed();
    }
}
