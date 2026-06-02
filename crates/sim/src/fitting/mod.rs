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
pub use hull::{FiringArc, GridCell, Hull, HullId, SectionId, Slot, SlotId};
pub use layout::{
    build_layout, cell_map, hardpoint_arc, module_at, resolve_hit, Cell, CellMap, CellOccupant,
    FitLayout, HitResolution,
};
pub use module::{
    Axis, HardpointType, Module, ModuleId, ModuleKind, ModuleSpecifics, SlotSize, Violation,
};
pub use stats::{derive_ship_stats, ShipStats, WeaponProfile};
pub use validate::{
    budget_usage, check_slot_fit, validate_fit, AxisUsage, BudgetUsage, FitValidation,
};

use bevy_ecs::prelude::*;

/// Re-derive a ship's [`ShipStats`] **and** rebuild its [`FitLayout`] whenever its
/// [`Fit`] mutates (INV-F08, FR-014/019).
///
/// The fit-change recompute system: it runs only for entities whose [`Fit`]
/// changed this run ([`Changed<Fit>`]), resolving the hull in [`HullCatalog`] and
/// the modules in [`ModuleCatalog`], and overwrites the ship's [`ShipStats`] with
/// the freshly [`derive_ship_stats`]-d value — so a running ship's flight/weapon
/// behavior always reflects its current fit. The hit/armor map ([`FitLayout`]) is
/// rebuilt on the **same** trigger ([`build_layout`]) so the E007 hitbox stays in
/// lock-step with the live fit (INV-F08). Pure derivation, no other state is
/// touched.
///
/// Override-or-fallback contract (Phase 4/5): this attaches/updates `ShipStats`
/// only on entities that already carry both a [`Fit`] and a [`ShipStats`]
/// component, and rebuilds the [`FitLayout`] **only** when the entity also carries
/// one ([`Option<&mut FitLayout>`]). Unfitted ships (E001/E002/E003 fixtures,
/// server/bot ships) carry none of these and are untouched — they keep flying on
/// the global [`Tuning`](crate::tuning::Tuning) via the flight system's fallback
/// path and have no positional hit-map.
///
/// A [`Fit`] referencing an unknown hull is skipped (no panic) — derivation +
/// layout both require a resolvable hull; the dangling-ref *rejection* is
/// `validate_fit`'s concern (INV-F13).
pub fn recompute_ship_stats_system(
    hulls: Res<HullCatalog>,
    modules: Res<ModuleCatalog>,
    mut q: Query<(&Fit, &mut ShipStats, Option<&mut FitLayout>), Changed<Fit>>,
) {
    for (fit, mut stats, layout) in &mut q {
        let Some(hull) = hulls.get(fit.hull) else {
            // Unknown hull: cannot derive; leave the prior stats/layout untouched.
            continue;
        };
        *stats = derive_ship_stats(hull, fit, &modules);
        // Rebuild the hit/armor map on the same fit change (INV-F08), but only for
        // a fitted ship that already carries one — an unfitted entity has no layout.
        if let Some(mut layout) = layout {
            *layout = build_layout(hull, fit, &modules);
        }
    }
}
