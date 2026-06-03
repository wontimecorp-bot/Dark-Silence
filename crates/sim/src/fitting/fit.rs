//! The Fit — a hull reference plus a slot→module assignment map (FR-002, FR-005).
//!
//! A `Fit` is the validated/saved unit: it names one [`HullId`] and maps each
//! occupied [`SlotId`] to the [`ModuleId`] installed there (one module per slot,
//! INV-F04). It lives on the ship entity as a `bevy_ecs` [`Component`]; derived
//! `ShipStats`/validation/layout are recomputed from it on change (later phases).
//!
//! This phase provides only the **bare map mutation** — raw install/remove that
//! enforces the one-module-per-slot structural invariant. The validate-then-apply
//! gate (slot type/size match + budget non-exceedance, returning a `FitRejection`)
//! is **Phase 3** (`validate.rs`); these raw ops do not yet check legality.
//!
//! Derive discipline matches the rest of the fitting domain: serde as a
//! replication/persistence seam, value semantics. A `BTreeMap` (not `HashMap`)
//! keys the assignments so iteration order is deterministic — the shared sim core
//! must be reproducible (Principle II).

use std::collections::BTreeMap;

use bevy_ecs::component::Component;
use serde::{Deserialize, Serialize};

use super::content::ModuleCatalog;
use super::hull::{Hull, HullId, SlotId};
use super::layout::build_layout;
use super::module::{Axis, ModuleId, Violation};
use super::stats::{derive_ship_stats, ShipStats};
use super::validate::{budget_usage, check_slot_fit, validate_fit, BudgetUsage};

/// A stable handle to an installed module — the identity the hit-map and the
/// fitting UI use, pairing the slot it sits in with the module content id
/// (data-model.md `ModuleRef`). Runtime-local like `ProjectileOwner`, but here
/// both fields are stable content/hull ids, so it is serde-safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModuleRef {
    /// The slot the module is installed in.
    pub slot: SlotId,
    /// The installed module's content id.
    pub module: ModuleId,
}

impl ModuleRef {
    /// Pair a slot with the module installed in it.
    pub const fn new(slot: SlotId, module: ModuleId) -> Self {
        Self { slot, module }
    }
}

/// The reason a validate-then-apply [`Fit::install_module`] was refused (FR-005/
/// 006/007; contracts/fitting-api.md §1 `FitRejection`). A rejected install is
/// **all-or-nothing**: the `Fit` is left unchanged.
///
/// The first three variants mirror the install-time slot/budget rules; the last
/// two guard catalog-id integrity (INV-F13) so install never commits a dangling
/// reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitRejection {
    /// The module's hardpoint type does not match the slot's `slot_type`
    /// (INV-F01, FR-006).
    SlotTypeMismatch { slot: SlotId, module: ModuleId },
    /// The module's hardpoint size exceeds the slot's `size` (INV-F02, FR-007).
    SlotSizeMismatch { slot: SlotId, module: ModuleId },
    /// Installing the module would push a budget axis over capacity (INV-F03,
    /// FR-008): the install is blocked, not silently applied.
    WouldExceedBudget { axis: Axis },
    /// The target [`SlotId`] does not exist on the fit's hull (INV-F13).
    UnknownSlot { slot: SlotId },
    /// The [`ModuleId`] does not resolve in the catalog (INV-F13).
    UnknownModule { module: ModuleId },
}

/// A ship's fit: the hull it is built on plus the module installed in each
/// occupied slot (FR-002). The empty fit (no assignments) is the valid baseline
/// (INV-F05). Attached to the ship entity as a `bevy_ecs` component.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fit {
    /// The hull this fit is built on (resolved in `HullCatalog`).
    pub hull: HullId,
    /// Slot → module assignments; a slot maps to 0 or 1 module (INV-F04).
    /// `BTreeMap` keeps iteration deterministic for the shared sim.
    pub assignments: BTreeMap<SlotId, ModuleId>,
}

impl Fit {
    /// A new empty fit on `hull` — the valid baseline (INV-F05): no modules
    /// installed, flies on floors (derivation phase), validates clean.
    pub fn new(hull: HullId) -> Self {
        Self {
            hull,
            assignments: BTreeMap::new(),
        }
    }

    /// Raw install: assign `module` to `slot`, returning the previously installed
    /// module (if any) it displaced.
    ///
    /// **Bare map mutation only** — this does NOT check slot type/size or budgets
    /// (validate-then-apply is Phase 3, `validate.rs`). It does enforce the
    /// structural one-module-per-slot invariant (INV-F04): a slot holds exactly
    /// 0 or 1 module, so a second install into an occupied slot replaces the
    /// first rather than double-occupying.
    pub fn install_raw(&mut self, slot: SlotId, module: ModuleId) -> Option<ModuleId> {
        self.assignments.insert(slot, module)
    }

    /// Validate-then-apply install (FR-005/006/007/008; INV-F01/F02/F03/F13).
    ///
    /// Resolves `slot` on `hull` and `module` in `catalog`, runs the per-slot
    /// type/size gate ([`check_slot_fit`]) and then a full [`validate_fit`] on the
    /// **candidate** fit (the fit as it would be *after* the install). The install
    /// is committed via [`Fit::install_raw`] **only** if every check passes; on
    /// rejection the `Fit` is left untouched (all-or-nothing) and a
    /// [`FitRejection`] names the reason.
    ///
    /// Budget rejection maps the over axis to [`FitRejection::WouldExceedBudget`].
    /// Because a single install can only add load (never free it), the candidate
    /// validation catches a would-exceed on any of power/CPU/mass. Removing a
    /// module frees its budget — that path is the existing [`Fit::remove_raw`],
    /// which needs no validation (a strict subset of a valid fit stays valid,
    /// INV-F06).
    pub fn install_module(
        &mut self,
        slot: SlotId,
        module: ModuleId,
        hull: &Hull,
        catalog: &ModuleCatalog,
    ) -> Result<(), FitRejection> {
        // Catalog-id integrity first (INV-F13): never commit a dangling ref.
        let Some(slot_ref) = hull.slot(slot) else {
            return Err(FitRejection::UnknownSlot { slot });
        };
        let Some(module_ref) = catalog.get(module) else {
            return Err(FitRejection::UnknownModule { module });
        };

        // Per-slot type/size gate (INV-F01/F02) — the precise mismatch reason.
        if let Some(violation) = check_slot_fit(slot_ref, module_ref) {
            return Err(match violation {
                Violation::SlotTypeMismatch { slot, module } => {
                    FitRejection::SlotTypeMismatch { slot, module }
                }
                Violation::SlotSizeMismatch { slot, module } => {
                    FitRejection::SlotSizeMismatch { slot, module }
                }
                // check_slot_fit only ever returns the two mismatch variants.
                Violation::OverBudget(axis) => FitRejection::WouldExceedBudget { axis },
            });
        }

        // Budget non-exceedance (INV-F03): validate the candidate fit before
        // committing. Clone-and-probe keeps this all-or-nothing — the live `self`
        // is mutated only after the candidate validates clean on every axis.
        let mut candidate = self.clone();
        candidate.install_raw(slot, module);
        let validation = validate_fit(hull, &candidate, catalog);
        if let Some(Violation::OverBudget(axis)) = validation
            .violations
            .iter()
            .find(|v| matches!(v, Violation::OverBudget(_)))
        {
            return Err(FitRejection::WouldExceedBudget { axis: *axis });
        }

        // All checks passed — commit.
        self.install_raw(slot, module);
        Ok(())
    }

    /// Raw remove: clear the module from `slot`, returning a [`ModuleRef`] to what
    /// was removed (or `None` if the slot was empty). Frees its budget on the next
    /// re-derive (SC-002). Pairs with [`Fit::install_raw`].
    pub fn remove_raw(&mut self, slot: SlotId) -> Option<ModuleRef> {
        self.assignments
            .remove(&slot)
            .map(|module| ModuleRef::new(slot, module))
    }

    /// The module installed in `slot`, if any (null-safe lookup).
    pub fn module_in(&self, slot: SlotId) -> Option<ModuleId> {
        self.assignments.get(&slot).copied()
    }

    /// Whether this fit has no modules installed (the valid baseline, INV-F05).
    pub fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }
}

/// A stable handle for a saved [`FitPreset`] (FR-024; contracts/fitting-api.md §4
/// `PresetId`). The fitting UI mints one per `save_preset` and references the saved
/// fit by it; storage backend (in-memory this epic vs. persisted in E004) is out of
/// band — the id is the surface key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PresetId(pub u32);

/// A named, saved fit — reusable later by reloading onto a compatible hull (FR-024,
/// US5; data-model.md `FitPreset`). **In-memory only this epic**; durable save/load
/// is E004. The derive discipline matches the rest of the domain (serde as a
/// replication/persistence seam, value semantics so a round-trip compares equal).
///
/// Consumed by the **client fitting UI** only (contracts/fitting-api.md §4):
/// [`save_preset`] mints one from a working fit; [`load_preset`] reloads it onto a
/// **compatible** hull (re-validating, never touching a running ship).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitPreset {
    /// Display name for the saved fit (non-empty by convention; not enforced here).
    pub name: String,
    /// The saved fit value (a hull id + slot→module assignments).
    pub fit: Fit,
}

/// Save `fit` under `name` as a reusable [`FitPreset`] (FR-024; contracts/
/// fitting-api.md §4 `save_preset`). **Pure** — clones the fit into the preset,
/// mutates no running state (the invariant: presets operate on `Fit` values only).
///
/// In-memory only this epic; the durable backend is E004. The UI keys the returned
/// preset by a [`PresetId`] it mints in its own store.
pub fn save_preset(name: &str, fit: &Fit) -> FitPreset {
    FitPreset {
        name: name.to_string(),
        fit: fit.clone(),
    }
}

/// Reload `preset` onto `hull`, returning the saved [`Fit`] **only** if it is
/// compatible with that hull (FR-024; contracts/fitting-api.md §4 `load_preset`).
/// **Pure** — never touches a running ship's authoritative state (the preset
/// invariant); operates on values only.
///
/// Compatibility is checked by re-running [`validate_fit`] on the saved fit against
/// the target `hull` + `catalog`:
/// - the target hull must be the hull the preset was saved on
///   ([`FitPreset::fit`]'s `hull` id must equal `hull.id`) — reloading onto a
///   *different* hull is rejected (the slot ids would not resolve);
/// - every saved module must still type/size-fit its slot and the fit must stay
///   within every budget axis.
///
/// On any incompatibility the first reported [`Violation`] is mapped to the
/// matching [`FitRejection`] (type/size mismatch, or the over-budget axis). A
/// valid, compatible reload yields a clone of the saved fit ready to apply.
pub fn load_preset(
    preset: &FitPreset,
    hull: &Hull,
    catalog: &ModuleCatalog,
) -> Result<Fit, FitRejection> {
    let fit = &preset.fit;

    // Reload only onto the hull the preset was built on: a mismatched hull's slot
    // ids would not resolve, so it is an incompatible reload (INV-F13).
    if fit.hull != hull.id {
        // Surface the first dangling slot (if any) as the precise reason; otherwise
        // an empty fit on the wrong hull is still incompatible — report its first
        // assignment, or a sentinel unknown slot for the empty case.
        let slot = fit
            .assignments
            .keys()
            .next()
            .copied()
            .unwrap_or(SlotId(u32::MAX));
        return Err(FitRejection::UnknownSlot { slot });
    }

    // Re-validate the saved fit against the target hull (re-run validate_fit, FR-024):
    // type/size gating, budget non-exceedance, and dangling-id integrity all apply.
    let validation = validate_fit(hull, fit, catalog);
    if let Some(violation) = validation.violations.first() {
        return Err(match *violation {
            Violation::SlotTypeMismatch { slot, module } => {
                FitRejection::SlotTypeMismatch { slot, module }
            }
            Violation::SlotSizeMismatch { slot, module } => {
                FitRejection::SlotSizeMismatch { slot, module }
            }
            Violation::OverBudget(axis) => FitRejection::WouldExceedBudget { axis },
        });
    }

    Ok(fit.clone())
}

/// Preview a candidate `fit`'s derived budget + flight/weapon stats **without
/// committing** to a live ship (FR-024, SC-006; contracts/fitting-api.md §4
/// `preview_stats`). **Pure** — a thin composition of [`budget_usage`] +
/// [`derive_ship_stats`]; touches no running-ship state (the preview invariant).
///
/// Returns `(BudgetUsage, ShipStats)` so the fitting UI can show live budget bars
/// and the before-commit stat deltas off the **same** code path the running sim and
/// a future authoritative server derive on (Principle II) — the preview is exactly
/// what a commit would produce.
pub fn preview_stats(hull: &Hull, fit: &Fit, catalog: &ModuleCatalog) -> (BudgetUsage, ShipStats) {
    // Preview is pre-commit — there is no live ship/layout yet — so build a
    // full-health layout to derive against. At full health every module's
    // health-factor is `1.0`, so the preview equals a spawn-time commit (SC-006).
    let layout = build_layout(hull, fit, catalog);
    (
        budget_usage(hull, fit, catalog),
        derive_ship_stats(hull, fit, catalog, &layout),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fit_is_the_empty_baseline() {
        let fit = Fit::new(HullId(1));
        assert!(fit.is_empty());
        assert_eq!(fit.module_in(SlotId(0)), None);
    }

    #[test]
    fn install_then_remove_round_trips_the_map() {
        let mut fit = Fit::new(HullId(1));
        // First install into an empty slot displaces nothing.
        assert_eq!(fit.install_raw(SlotId(0), ModuleId(10)), None);
        assert_eq!(fit.module_in(SlotId(0)), Some(ModuleId(10)));
        assert!(!fit.is_empty());

        // Remove frees the slot and returns the ref.
        assert_eq!(
            fit.remove_raw(SlotId(0)),
            Some(ModuleRef::new(SlotId(0), ModuleId(10)))
        );
        assert!(fit.is_empty());
        // Removing an empty slot is a no-op `None`.
        assert_eq!(fit.remove_raw(SlotId(0)), None);
    }

    #[test]
    fn install_into_occupied_slot_replaces_not_double_occupies() {
        // INV-F04: a slot holds 0 or 1 module — re-installing replaces.
        let mut fit = Fit::new(HullId(1));
        fit.install_raw(SlotId(0), ModuleId(10));
        let displaced = fit.install_raw(SlotId(0), ModuleId(20));
        assert_eq!(displaced, Some(ModuleId(10)));
        assert_eq!(fit.module_in(SlotId(0)), Some(ModuleId(20)));
        assert_eq!(fit.assignments.len(), 1);
    }
}
