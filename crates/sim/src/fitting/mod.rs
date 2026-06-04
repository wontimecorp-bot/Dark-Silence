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
    baseline_fit, baseline_hull, seed_catalogs, HullCatalog, ModuleCatalog, HULL_BASELINE,
    HULL_CORVETTE, HULL_FIGHTER, MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_BASELINE_THRUSTER,
    MODULE_REACTOR_BASIC, MODULE_SHIELD_BASIC, MODULE_THRUSTER_BASIC, MODULE_UTILITY_BASIC,
};
pub use fit::{
    load_preset, preview_stats, save_preset, Fit, FitPreset, FitRejection, ModuleRef, PresetId,
};
pub use hull::{
    hull_collision_radius, FiringArc, GridCell, Hull, HullId, SectionId, Slot, SlotId,
    CELL_WORLD_SIZE,
};
pub use layout::{
    build_layout, cell_map, hardpoint_arc, layout_center, module_at, resolve_hit, Cell, CellMap,
    CellOccupant, FitLayout, HitResolution,
};
pub use module::{
    Axis, HardpointType, Module, ModuleId, ModuleKind, ModuleSpecifics, SlotSize, Violation,
};
pub use stats::{derive_ship_stats, ShipStats, WeaponProfile};
pub use validate::{
    budget_usage, check_slot_fit, validate_fit, AxisUsage, BudgetUsage, FitValidation,
};

use bevy_ecs::prelude::*;

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
    mut q: Query<
        (Ref<Fit>, &mut ShipStats, &mut FitLayout),
        Or<(Changed<Fit>, Changed<FitLayout>)>,
    >,
) {
    for (fit, mut stats, mut layout) in &mut q {
        let Some(hull) = hulls.get(fit.hull) else {
            // Unknown hull: cannot derive; leave the prior stats/layout untouched.
            continue;
        };
        // A fit re-configure rebuilds the layout fresh (full health = repaired);
        // a layout-only change (damage) preserves the damaged health.
        if fit.is_changed() {
            *layout = build_layout(hull, &fit, &modules);
        }
        *stats = derive_ship_stats(hull, &fit, &modules, &layout);
    }
}
