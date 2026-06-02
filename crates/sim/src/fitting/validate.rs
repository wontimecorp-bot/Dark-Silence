//! Fit validation — slot type/size gating + per-axis budgets (FR-006/007/008/
//! 009/010/011; INV-F01/F02/F03/F05/F09/F13).
//!
//! These are **pure functions** over a `Fit` value and its `Hull`/`ModuleCatalog`
//! content (no world mutation): the fitting UI calls them per change for live
//! readouts and before-commit previews, and a future authoritative server runs
//! the identical functions as the validation gate (Principle I; contracts/
//! fitting-api.md §1). A `Fit` stores ids, so the [`ModuleCatalog`] is threaded
//! in to resolve each [`ModuleId`]'s stat block.
//!
//! Surface:
//! - [`budget_usage`] / [`BudgetUsage`] / [`AxisUsage`] — the live per-axis
//!   readout (FR-009): used vs capacity, with an `over` flag (INV-F03).
//! - [`check_slot_fit`] — the per-slot type/size gate helper (INV-F01/F02).
//! - [`validate_fit`] / [`FitValidation`] — the total validation: collects
//!   per-slot mismatches, per-axis `OverBudget`, and dangling-id rejects into the
//!   `violations` list; `valid == violations.is_empty()` (INV-F09). The empty fit
//!   is the valid baseline (INV-F05).
//!
//! Derive discipline matches the rest of the fitting domain: serde as a
//! replication/persistence seam (not exercised this epic), value semantics.

use bevy_ecs::component::Component;
use serde::{Deserialize, Serialize};

use super::content::ModuleCatalog;
use super::fit::Fit;
use super::hull::Hull;
use super::module::{Axis, Violation};

/// One budget axis's live readout (FR-009): how much is `used` against the
/// `capacity` ceiling, plus the derived `over` flag (`used > capacity`, INV-F03).
///
/// `used`/`capacity` are both `>= 0`; `over` is the per-axis exceedance the
/// validation surfaces as an [`Violation::OverBudget`] (data-model.md).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AxisUsage {
    /// Amount of this axis consumed by the fit (`>= 0`).
    pub used: f32,
    /// The hull's ceiling on this axis (`>= 0`; for power, hull cap + reactor gen).
    pub capacity: f32,
    /// `true` iff `used > capacity` — this axis is over budget (INV-F03).
    pub over: bool,
}

impl AxisUsage {
    /// Build an [`AxisUsage`], computing the `over` flag from `used`/`capacity`.
    fn new(used: f32, capacity: f32) -> Self {
        Self {
            used,
            capacity,
            over: used > capacity,
        }
    }
}

/// The live per-axis budget readout for a fit (FR-009; data-model.md
/// `BudgetUsage`). Surfaced to the fitting UI as power/CPU/mass bars and diffed
/// against a candidate fit for the before-commit preview (FR-013).
///
/// - **power**: `capacity = hull.power_capacity + Σ reactor.power_gen`,
///   `used = Σ power_draw`.
/// - **cpu**: `capacity = hull.cpu_capacity`, `used = Σ cpu_draw`.
/// - **mass**: `capacity = hull.mass_capacity`,
///   `used = hull.hull_base_mass + Σ module.mass`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetUsage {
    /// Power axis: draws vs (hull cap + reactor gen).
    pub power: AxisUsage,
    /// CPU/control axis: draws vs hull cap.
    pub cpu: AxisUsage,
    /// Mass axis: (base + module mass) vs hull cap.
    pub mass: AxisUsage,
}

/// Compute the live per-axis [`BudgetUsage`] for `fit` on `hull`, resolving each
/// installed [`ModuleId`] through `catalog` (FR-009, INV-F03). **Pure** — reads
/// only its arguments, mutates nothing.
///
/// Power capacity sums the hull's structural cap and every installed reactor's
/// `power_gen`; mass used adds the hull's base mass to the sum of module masses
/// (so an empty hull already carries its base mass, INV-F14). A dangling
/// [`ModuleId`] (not in `catalog`) simply contributes nothing here — the
/// dangling-ref *rejection* is [`validate_fit`]'s concern (INV-F13); the live
/// budget bars stay defined regardless.
pub fn budget_usage(hull: &Hull, fit: &Fit, catalog: &ModuleCatalog) -> BudgetUsage {
    let mut power_gen = 0.0_f32;
    let mut power_draw = 0.0_f32;
    let mut cpu_draw = 0.0_f32;
    let mut module_mass = 0.0_f32;

    for module_id in fit.assignments.values() {
        let Some(module) = catalog.get(*module_id) else {
            // Dangling id: contributes no budget; validate_fit rejects the fit.
            continue;
        };
        power_gen += module.power_gen;
        power_draw += module.power_draw;
        cpu_draw += module.cpu_draw;
        module_mass += module.mass;
    }

    BudgetUsage {
        power: AxisUsage::new(power_draw, hull.power_capacity + power_gen),
        cpu: AxisUsage::new(cpu_draw, hull.cpu_capacity),
        mass: AxisUsage::new(hull.hull_base_mass + module_mass, hull.mass_capacity),
    }
}

/// The total validation result for a fit (FR-008/010/011; data-model.md
/// `FitValidation`). Carries the per-axis [`BudgetUsage`] plus every named
/// [`Violation`]; `valid == violations.is_empty()` (INV-F09).
///
/// Lives on the ship entity as a `bevy_ecs` [`Component`], recomputed on every
/// fit change (INV-F08, a later phase wires the recompute system). The empty fit
/// is the valid baseline (INV-F05).
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FitValidation {
    /// The live per-axis budget readout (FR-009) this validation was computed on.
    pub usage: BudgetUsage,
    /// Every named reason the fit is invalid (empty ⇒ valid, INV-F09).
    pub violations: Vec<Violation>,
    /// `true` iff `violations` is empty (INV-F09).
    pub valid: bool,
}

/// Per-slot type/size gate (INV-F01/F02, FR-006/007). Returns the [`Violation`]
/// a candidate `module` would incur in `slot`, or `None` if it fits.
///
/// - [`Violation::SlotTypeMismatch`] if `module.hardpoint_type != slot.slot_type`
///   (INV-F01).
/// - [`Violation::SlotSizeMismatch`] if `module.hardpoint_size > slot.size`
///   (INV-F02; uses the ordered [`SlotSize`](super::module::SlotSize) — a smaller
///   module fits a larger slot).
///
/// Type is checked before size (a slot of the wrong type is the primary mismatch);
/// at most one violation is reported per slot. **Pure** — reads only its args.
pub fn check_slot_fit(
    slot: &super::hull::Slot,
    module: &super::module::Module,
) -> Option<Violation> {
    if module.hardpoint_type != slot.slot_type {
        return Some(Violation::SlotTypeMismatch {
            slot: slot.id,
            module: module.id,
        });
    }
    if module.hardpoint_size > slot.size {
        return Some(Violation::SlotSizeMismatch {
            slot: slot.id,
            module: module.id,
        });
    }
    None
}

/// Validate `fit` against its `hull` and the `catalog` (FR-008/010/011; INV-F03/
/// F05/F09/F13). **Pure** — reads only its arguments, mutates no world state.
///
/// Collects, in deterministic order (assignments iterate by `SlotId`):
/// 1. **Dangling-id rejects** (INV-F13): a [`SlotId`](super::hull::SlotId) not on
///    `hull`, or a [`ModuleId`](super::module::ModuleId) not in `catalog`, makes
///    the fit invalid. A dangling slot is reported as a
///    [`Violation::SlotTypeMismatch`] (the slot does not exist to accept it); a
///    dangling module as a [`Violation::SlotSizeMismatch`] keyed by the slot it
///    sits in (it cannot be sized/typed). Either way the fit is rejected.
/// 2. **Per-slot type/size mismatches** ([`check_slot_fit`], INV-F01/F02).
/// 3. **Per-axis over-budget** ([`budget_usage`], INV-F03): one
///    [`Violation::OverBudget`] per over axis (power, then CPU, then mass).
///
/// The empty fit yields no violations ⇒ the valid baseline (INV-F05). The final
/// `valid` flag is exactly `violations.is_empty()` (INV-F09).
pub fn validate_fit(hull: &Hull, fit: &Fit, catalog: &ModuleCatalog) -> FitValidation {
    let mut violations = Vec::new();

    // (1)+(2) Per-slot integrity + type/size gate, in deterministic SlotId order.
    for (slot_id, module_id) in fit.assignments.iter() {
        match (hull.slot(*slot_id), catalog.get(*module_id)) {
            (None, _) => {
                // Dangling slot id: the hull has no such slot to accept a module
                // (INV-F13). Report it as a type mismatch keyed by the bad slot.
                violations.push(Violation::SlotTypeMismatch {
                    slot: *slot_id,
                    module: *module_id,
                });
            }
            (Some(_), None) => {
                // Dangling module id: the catalog has no such module (INV-F13).
                // Report it as a size mismatch keyed by its slot.
                violations.push(Violation::SlotSizeMismatch {
                    slot: *slot_id,
                    module: *module_id,
                });
            }
            (Some(slot), Some(module)) => {
                if let Some(v) = check_slot_fit(slot, module) {
                    violations.push(v);
                }
            }
        }
    }

    // (3) Per-axis budget non-exceedance (INV-F03): one OverBudget per over axis.
    let usage = budget_usage(hull, fit, catalog);
    if usage.power.over {
        violations.push(Violation::OverBudget(Axis::Power));
    }
    if usage.cpu.over {
        violations.push(Violation::OverBudget(Axis::Cpu));
    }
    if usage.mass.over {
        violations.push(Violation::OverBudget(Axis::Mass));
    }

    let valid = violations.is_empty();
    FitValidation {
        usage,
        violations,
        valid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::content::{
        seed_catalogs, HULL_FIGHTER, MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC,
    };
    use crate::fitting::hull::SlotId;
    use crate::fitting::module::ModuleId;

    #[test]
    fn empty_fit_is_the_valid_baseline() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let fit = Fit::new(HULL_FIGHTER);
        let v = validate_fit(hull, &fit, &modules);
        assert!(v.valid);
        assert!(v.violations.is_empty());
        // Empty hull still carries its base mass on the mass axis (INV-F14).
        assert_eq!(v.usage.mass.used, hull.hull_base_mass);
        assert!(!v.usage.mass.over);
    }

    #[test]
    fn dangling_module_id_is_rejected() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        // Slot 0 is a real Reactor slot; module id 999 does not exist.
        fit.install_raw(SlotId(0), ModuleId(999));
        let v = validate_fit(hull, &fit, &modules);
        assert!(!v.valid);
        assert!(v
            .violations
            .iter()
            .any(|x| matches!(x, Violation::SlotSizeMismatch { .. })));
    }

    #[test]
    fn dangling_slot_id_is_rejected() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        // Slot id 999 is not on the fighter hull.
        fit.install_raw(SlotId(999), MODULE_REACTOR_BASIC);
        let v = validate_fit(hull, &fit, &modules);
        assert!(!v.valid);
        assert!(v
            .violations
            .iter()
            .any(|x| matches!(x, Violation::SlotTypeMismatch { .. })));
    }

    #[test]
    fn budget_usage_counts_reactor_gen_into_power_capacity() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        let usage = budget_usage(hull, &fit, &modules);
        let reactor = modules.get(MODULE_REACTOR_BASIC).unwrap();
        assert_eq!(
            usage.power.capacity,
            hull.power_capacity + reactor.power_gen
        );
    }

    #[test]
    fn check_slot_fit_flags_type_then_size() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        // Slot 0 is a Reactor slot; an autocannon (Weapon) is a type mismatch.
        let reactor_slot = hull.slot(SlotId(0)).unwrap();
        let autocannon = modules.get(MODULE_AUTOCANNON).unwrap();
        assert!(matches!(
            check_slot_fit(reactor_slot, autocannon),
            Some(Violation::SlotTypeMismatch { .. })
        ));
        // An armor plate (Armor) into the Reactor slot is also a type mismatch.
        let armor = modules.get(MODULE_ARMOR_PLATE).unwrap();
        assert!(matches!(
            check_slot_fit(reactor_slot, armor),
            Some(Violation::SlotTypeMismatch { .. })
        ));
    }
}
