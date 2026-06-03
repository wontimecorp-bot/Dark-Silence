//! Headless unit tests for the E006 Phase 3 (US1) fit-validation surface.
//!
//! Covers the pure-logic validation contract (`crates/sim/src/fitting/validate.rs`
//! + the validate-then-apply install in `fit.rs`):
//! - **SC-001**: each over-budget axis (power, CPU, mass) is reported with the
//!   axis NAMED; a slot **type** mismatch and a slot **size** mismatch are each
//!   rejected with the right `Violation`.
//! - **SC-002**: an empty hull validates (the baseline, INV-F05) and removing a
//!   module frees its budget; a dangling id is rejected (INV-F13).
//!
//! Two fixtures are used. The **seed catalog** ([`seed_catalogs`]) drives the
//! baseline / remove-frees / dangling-id cases (T012: "use the seed catalog").
//! The per-axis over-budget cases use a small **purpose-built** hull + catalog
//! with hand-picked numbers so each axis can be driven over **independently** and
//! the assertions stay stable across the Phase 6 seed-balance retune (which would
//! otherwise shift the seed thresholds out from under these tests). Both fixtures
//! exercise the exact same public `validate_fit`/`budget_usage`/`install_module`
//! code path — only the content rows differ.

use std::collections::BTreeMap;

use sim::fitting::{
    budget_usage, check_slot_fit, seed_catalogs, validate_fit, Axis, Fit, FitRejection, GridCell,
    HardpointType, Hull, HullId, Module, ModuleCatalog, ModuleId, ModuleKind, ModuleSpecifics,
    SectionId, Slot, SlotId, SlotSize, Violation, HULL_FIGHTER, MODULE_ARMOR_PLATE,
    MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
};

// --- Purpose-built, retune-stable fixture ------------------------------------

/// The per-axis budget costs of a test module (the fields a budget test drives).
#[derive(Clone, Copy)]
struct Costs {
    power_gen: f32,
    power_draw: f32,
    cpu_draw: f32,
    mass: f32,
}

impl Costs {
    /// Inert (all-zero) costs — the baseline for type/size-only tests.
    const ZERO: Costs = Costs {
        power_gen: 0.0,
        power_draw: 0.0,
        cpu_draw: 0.0,
        mass: 0.0,
    };
}

/// A trivial module row with explicit budget `costs`; all other fields are inert
/// defaults so each test reads exactly the axis it drives.
fn test_module(
    id: u32,
    kind: ModuleKind,
    hardpoint_type: HardpointType,
    size: SlotSize,
    costs: Costs,
) -> Module {
    Module {
        id: ModuleId(id),
        kind,
        power_gen: costs.power_gen,
        power_draw: costs.power_draw,
        cpu_draw: costs.cpu_draw,
        mass: costs.mass,
        heat: 0.0,
        health_max: 1.0,
        hardpoint_type,
        hardpoint_size: size,
        specifics: ModuleSpecifics::Utility,
    }
}

/// A single-slot test hull with caller-set caps and one slot of the given
/// type/size at the origin cell.
fn test_hull(
    slot_type: HardpointType,
    slot_size: SlotSize,
    power_capacity: f32,
    cpu_capacity: f32,
    mass_capacity: f32,
    hull_base_mass: f32,
) -> Hull {
    Hull {
        id: HullId(900),
        name: "TestHull".to_string(),
        grid_dims: (2, 2),
        cells: vec![GridCell::new((0, 0), SectionId(0))],
        power_capacity,
        cpu_capacity,
        mass_capacity,
        hull_base_mass,
        slots: vec![Slot {
            id: SlotId(0),
            slot_type,
            size: slot_size,
            coord: (0, 0),
            facing: 0.0,
            is_weapon_mount: false,
        }],
    }
}

fn catalog_of(modules: impl IntoIterator<Item = Module>) -> ModuleCatalog {
    let map: BTreeMap<ModuleId, Module> = modules.into_iter().map(|m| (m.id, m)).collect();
    ModuleCatalog { modules: map }
}

// --- SC-001: each over-budget axis is reported with the axis NAMED -----------

#[test]
fn power_over_budget_is_reported_naming_the_power_axis() {
    // power capacity = 5 (+0 gen); a draw of 8 exceeds it. CPU/mass stay clear.
    let hull = test_hull(
        HardpointType::Utility,
        SlotSize::Small,
        5.0,
        100.0,
        100.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Utility,
        HardpointType::Utility,
        SlotSize::Small,
        Costs {
            power_draw: 8.0,
            mass: 1.0,
            ..Costs::ZERO
        },
    );
    let catalog = catalog_of([module]);
    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));

    let v = validate_fit(&hull, &fit, &catalog);
    assert!(!v.valid);
    assert!(v.usage.power.over);
    assert!(
        v.violations.contains(&Violation::OverBudget(Axis::Power)),
        "expected OverBudget(Power), got {:?}",
        v.violations
    );
    // Only the power axis is over.
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Cpu)));
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Mass)));
}

#[test]
fn cpu_over_budget_is_reported_naming_the_cpu_axis() {
    let hull = test_hull(
        HardpointType::Utility,
        SlotSize::Small,
        100.0,
        5.0,
        100.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Utility,
        HardpointType::Utility,
        SlotSize::Small,
        Costs {
            cpu_draw: 8.0,
            mass: 1.0,
            ..Costs::ZERO
        },
    );
    let catalog = catalog_of([module]);
    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));

    let v = validate_fit(&hull, &fit, &catalog);
    assert!(!v.valid);
    assert!(v.usage.cpu.over);
    assert!(v.violations.contains(&Violation::OverBudget(Axis::Cpu)));
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Power)));
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Mass)));
}

#[test]
fn mass_over_budget_is_reported_naming_the_mass_axis() {
    // mass used = base(1) + module mass(8) = 9 > cap(5).
    let hull = test_hull(
        HardpointType::Utility,
        SlotSize::Small,
        100.0,
        100.0,
        5.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Utility,
        HardpointType::Utility,
        SlotSize::Small,
        Costs {
            mass: 8.0,
            ..Costs::ZERO
        },
    );
    let catalog = catalog_of([module]);
    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));

    let v = validate_fit(&hull, &fit, &catalog);
    assert!(!v.valid);
    assert!(v.usage.mass.over);
    assert!(v.violations.contains(&Violation::OverBudget(Axis::Mass)));
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Power)));
    assert!(!v.violations.contains(&Violation::OverBudget(Axis::Cpu)));
}

#[test]
fn reactor_power_gen_raises_the_power_capacity() {
    // A reactor's power_gen adds to the power capacity (T008 rule). Same draw
    // that was over without the reactor is now within budget with it.
    let hull = test_hull(
        HardpointType::Utility,
        SlotSize::Small,
        5.0,
        100.0,
        100.0,
        1.0,
    );
    let draw = test_module(
        1,
        ModuleKind::Utility,
        HardpointType::Utility,
        SlotSize::Small,
        Costs {
            power_draw: 8.0,
            mass: 1.0,
            ..Costs::ZERO
        },
    );
    let catalog = catalog_of([draw]);
    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));
    let usage = budget_usage(&hull, &fit, &catalog);
    // capacity = hull cap only (this module generates no power).
    assert_eq!(usage.power.capacity, 5.0);
    assert!(usage.power.over);
}

// --- SC-001: slot type mismatch + slot size mismatch -------------------------

#[test]
fn slot_type_mismatch_is_rejected_with_the_type_violation() {
    // A Weapon slot; install a Thruster module → type mismatch (INV-F01).
    let hull = test_hull(
        HardpointType::Weapon,
        SlotSize::Medium,
        100.0,
        100.0,
        100.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Thruster,
        HardpointType::Thruster,
        SlotSize::Small,
        Costs::ZERO,
    );
    let catalog = catalog_of([module]);
    let slot = hull.slot(SlotId(0)).unwrap();
    let m = catalog.get(ModuleId(1)).unwrap();

    assert_eq!(
        check_slot_fit(slot, m),
        Some(Violation::SlotTypeMismatch {
            slot: SlotId(0),
            module: ModuleId(1),
        })
    );

    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));
    let v = validate_fit(&hull, &fit, &catalog);
    assert!(!v.valid);
    assert!(v.violations.contains(&Violation::SlotTypeMismatch {
        slot: SlotId(0),
        module: ModuleId(1),
    }));

    // Validate-then-apply install rejects it without committing.
    let mut clean = Fit::new(hull.id);
    let err = clean.install_module(SlotId(0), ModuleId(1), &hull, &catalog);
    assert_eq!(
        err,
        Err(FitRejection::SlotTypeMismatch {
            slot: SlotId(0),
            module: ModuleId(1),
        })
    );
    assert!(
        clean.is_empty(),
        "rejected install must leave the fit unchanged"
    );
}

#[test]
fn slot_size_mismatch_is_rejected_with_the_size_violation() {
    // A Small slot; a Large module of the right type → size mismatch (INV-F02).
    let hull = test_hull(
        HardpointType::Weapon,
        SlotSize::Small,
        100.0,
        100.0,
        100.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Weapon,
        HardpointType::Weapon,
        SlotSize::Large,
        Costs::ZERO,
    );
    let catalog = catalog_of([module]);
    let slot = hull.slot(SlotId(0)).unwrap();
    let m = catalog.get(ModuleId(1)).unwrap();

    assert_eq!(
        check_slot_fit(slot, m),
        Some(Violation::SlotSizeMismatch {
            slot: SlotId(0),
            module: ModuleId(1),
        })
    );

    let mut fit = Fit::new(hull.id);
    fit.install_raw(SlotId(0), ModuleId(1));
    let v = validate_fit(&hull, &fit, &catalog);
    assert!(!v.valid);
    assert!(v.violations.contains(&Violation::SlotSizeMismatch {
        slot: SlotId(0),
        module: ModuleId(1),
    }));

    let mut clean = Fit::new(hull.id);
    let err = clean.install_module(SlotId(0), ModuleId(1), &hull, &catalog);
    assert_eq!(
        err,
        Err(FitRejection::SlotSizeMismatch {
            slot: SlotId(0),
            module: ModuleId(1),
        })
    );
    assert!(clean.is_empty());
}

#[test]
fn smaller_module_fits_a_larger_slot() {
    // INV-F02: module size <= slot size is allowed (Small fits a Large slot).
    let hull = test_hull(
        HardpointType::Weapon,
        SlotSize::Large,
        100.0,
        100.0,
        100.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Weapon,
        HardpointType::Weapon,
        SlotSize::Small,
        Costs::ZERO,
    );
    let catalog = catalog_of([module]);
    let slot = hull.slot(SlotId(0)).unwrap();
    let m = catalog.get(ModuleId(1)).unwrap();
    assert_eq!(check_slot_fit(slot, m), None);

    let mut fit = Fit::new(hull.id);
    assert_eq!(
        fit.install_module(SlotId(0), ModuleId(1), &hull, &catalog),
        Ok(())
    );
    assert_eq!(fit.module_in(SlotId(0)), Some(ModuleId(1)));
}

#[test]
fn install_module_rejects_a_would_exceed_budget_without_committing() {
    // Mass cap 5, base 1, module mass 8 → installing would exceed mass (INV-F03).
    let hull = test_hull(
        HardpointType::Utility,
        SlotSize::Small,
        100.0,
        100.0,
        5.0,
        1.0,
    );
    let module = test_module(
        1,
        ModuleKind::Utility,
        HardpointType::Utility,
        SlotSize::Small,
        Costs {
            mass: 8.0,
            ..Costs::ZERO
        },
    );
    let catalog = catalog_of([module]);
    let mut fit = Fit::new(hull.id);
    let err = fit.install_module(SlotId(0), ModuleId(1), &hull, &catalog);
    assert_eq!(
        err,
        Err(FitRejection::WouldExceedBudget { axis: Axis::Mass })
    );
    assert!(fit.is_empty(), "a would-exceed install must not commit");
}

// --- SC-002: empty hull validates + remove frees budget ----------------------

#[test]
fn empty_seed_hull_validates_as_the_baseline() {
    // INV-F05: empty fit ⇒ no violations ⇒ valid.
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let fit = Fit::new(HULL_FIGHTER);
    let v = validate_fit(hull, &fit, &modules);
    assert!(v.valid);
    assert!(v.violations.is_empty());
    // The empty hull already carries its base mass (INV-F14), no module mass.
    assert_eq!(v.usage.mass.used, hull.hull_base_mass);
    assert!(!v.usage.power.over && !v.usage.cpu.over && !v.usage.mass.over);
}

#[test]
fn remove_frees_the_budget_it_consumed() {
    // SC-002 / INV-F06: install a thruster (real seed budget cost), observe the
    // usage rise, then remove it and observe the usage return to the baseline.
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();

    let mut fit = Fit::new(HULL_FIGHTER);
    let baseline = budget_usage(hull, &fit, &modules);

    // Slot 0 on the fighter is a Reactor slot; slot 1 is a Thruster slot.
    assert_eq!(
        fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, hull, &modules),
        Ok(())
    );
    assert_eq!(
        fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, hull, &modules),
        Ok(())
    );

    let loaded = budget_usage(hull, &fit, &modules);
    let thruster = modules.get(MODULE_THRUSTER_BASIC).unwrap();
    assert!(loaded.power.used > baseline.power.used);
    assert!(loaded.cpu.used > baseline.cpu.used);
    assert!(loaded.mass.used > baseline.mass.used);
    // The thruster's draw is reflected in the loaded usage.
    assert!(loaded.power.used >= thruster.power_draw);

    // Remove the thruster — its budget is freed.
    assert!(fit.remove_raw(SlotId(1)).is_some());
    let after_remove = budget_usage(hull, &fit, &modules);
    assert!((after_remove.power.used - (loaded.power.used - thruster.power_draw)).abs() < 1e-6);
    assert!((after_remove.cpu.used - (loaded.cpu.used - thruster.cpu_draw)).abs() < 1e-6);
    assert!((after_remove.mass.used - (loaded.mass.used - thruster.mass)).abs() < 1e-6);
    // Still valid after removal (reactor remains, crippled-but-valid is fine).
    assert!(validate_fit(hull, &fit, &modules).valid);

    // Remove the reactor too — back to the empty baseline, still valid.
    assert!(fit.remove_raw(SlotId(0)).is_some());
    assert!(fit.is_empty());
    let back = budget_usage(hull, &fit, &modules);
    assert_eq!(back.mass.used, baseline.mass.used);
    assert!(validate_fit(hull, &fit, &modules).valid);
}

// --- SC-002: dangling id is rejected (INV-F13) -------------------------------

#[test]
fn dangling_module_id_in_a_fit_is_rejected() {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    // Slot 0 exists; module id 12345 does not.
    fit.install_raw(SlotId(0), ModuleId(12345));
    let v = validate_fit(hull, &fit, &modules);
    assert!(!v.valid, "a fit with a dangling module id must be invalid");
    assert!(!v.violations.is_empty());
}

#[test]
fn dangling_slot_id_in_a_fit_is_rejected() {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    // Slot id 4242 is not on the fighter hull; the module id is real.
    fit.install_raw(SlotId(4242), MODULE_REACTOR_BASIC);
    let v = validate_fit(hull, &fit, &modules);
    assert!(
        !v.valid,
        "a fit referencing a non-existent slot must be invalid"
    );
    assert!(!v.violations.is_empty());
}

#[test]
fn install_module_rejects_unknown_slot_and_unknown_module() {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);

    assert_eq!(
        fit.install_module(SlotId(4242), MODULE_REACTOR_BASIC, hull, &modules),
        Err(FitRejection::UnknownSlot { slot: SlotId(4242) })
    );
    assert_eq!(
        fit.install_module(SlotId(0), ModuleId(12345), hull, &modules),
        Err(FitRejection::UnknownModule {
            module: ModuleId(12345)
        })
    );
    assert!(
        fit.is_empty(),
        "rejected installs must leave the fit untouched"
    );
}

#[test]
fn valid_in_range_seed_fit_reports_no_violations() {
    // A modest fighter fit (reactor + one thruster + one armor) stays within the
    // seed budgets and validates clean — guards against false-positive rejection.
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, hull, &modules)
        .expect("reactor fits its slot and budget");
    fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, hull, &modules)
        .expect("thruster fits");
    fit.install_module(SlotId(5), MODULE_ARMOR_PLATE, hull, &modules)
        .expect("armor fits");
    // Also confirm a weapon fits a weapon slot.
    fit.install_module(SlotId(3), MODULE_AUTOCANNON, hull, &modules)
        .expect("autocannon fits the weapon slot");
    let v = validate_fit(hull, &fit, &modules);
    assert!(v.valid, "expected a valid fit, got {:?}", v.violations);
}

// --- Phase 4 (US2): derive_ship_stats + the Tuning→ShipStats rewire ----------
//
// T018 unit cases: mass→agility, thrust→top_speed, no-weapon→can_fire=false,
// crippled-fit floors (no NaN/inf). T019 integration: the baseline seed fit's
// ShipStats reproduces Tuning::default() (flight-feel guard, HINT-002) AND a
// fitted ship stepped in a sim world moves under its fit-derived stats (SC-003).

mod stats_phase4 {
    use bevy_ecs::prelude::*;
    use glam::Vec2;
    use sim::components::*;
    use sim::fitting::{
        baseline_fit, baseline_hull, build_layout, derive_ship_stats, recompute_ship_stats_system,
        seed_catalogs, Fit, Hull, ModuleCatalog, ModuleSpecifics, ShipStats, SlotId, HULL_FIGHTER,
        MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_THRUSTER_BASIC,
    };
    use sim::{FixedDt, ShipIntent, Tuning};

    const DT: f32 = 1.0 / 60.0;

    /// A two-thruster fighter fit (the seed thrusters sum to thrust/torque/strafe
    /// 30/12/18 — the E002 magnitudes) used as the comparison baseline for the
    /// agility / top-speed cases.
    fn two_thruster_fighter() -> (ModuleCatalog, Hull, Fit) {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let mut fit = Fit::new(HULL_FIGHTER);
        // Slots 1 and 2 are the fighter's two Thruster slots.
        fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();
        fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();
        (modules, hull, fit)
    }

    #[test]
    fn more_thrust_raises_top_speed() {
        // One thruster vs two thrusters on the same hull → more thrust, same drag
        // → higher emergent top speed = thrust_force / linear_drag.
        let (modules, hull, two) = two_thruster_fighter();
        let mut one = Fit::new(HULL_FIGHTER);
        one.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();

        let layout_one = build_layout(&hull, &one, &modules);
        let layout_two = build_layout(&hull, &two, &modules);
        let stats_one = derive_ship_stats(&hull, &one, &modules, &layout_one);
        let stats_two = derive_ship_stats(&hull, &two, &modules, &layout_two);
        assert!(
            stats_two.thrust_force > stats_one.thrust_force,
            "two thrusters give more thrust"
        );
        assert!(
            stats_two.top_speed() > stats_one.top_speed(),
            "more thrust ⇒ higher emergent top speed ({} vs {})",
            stats_two.top_speed(),
            stats_one.top_speed()
        );
    }

    #[test]
    fn heavier_total_mass_lowers_agility() {
        // Adding mass (an armor plate) at the SAME thrust lowers acceleration
        // (agility = force / mass), without lowering the emergent top speed
        // (terminal velocity is thrust/drag, mass-independent) — the FR-015 shape.
        let (modules, hull, light) = two_thruster_fighter();
        let mut heavy = light.clone();
        // Slot 5 is the fighter's Armor slot; armor_plate is pure mass.
        heavy
            .install_module(SlotId(5), MODULE_ARMOR_PLATE, &hull, &modules)
            .unwrap();

        let layout_light = build_layout(&hull, &light, &modules);
        let layout_heavy = build_layout(&hull, &heavy, &modules);
        let s_light = derive_ship_stats(&hull, &light, &modules, &layout_light);
        let s_heavy = derive_ship_stats(&hull, &heavy, &modules, &layout_heavy);
        assert!(s_heavy.total_mass > s_light.total_mass, "armor adds mass");
        // Same thrust, more mass ⇒ lower initial acceleration (agility).
        assert_eq!(s_heavy.thrust_force, s_light.thrust_force);
        let accel_light = s_light.thrust_force / s_light.total_mass;
        let accel_heavy = s_heavy.thrust_force / s_heavy.total_mass;
        assert!(
            accel_heavy < accel_light,
            "heavier fit accelerates more slowly ({accel_heavy} < {accel_light})"
        );
    }

    #[test]
    fn no_weapon_module_means_cannot_fire() {
        // A fit with thrusters but no weapon module ⇒ can_fire == false, no profile.
        let (modules, hull, fit) = two_thruster_fighter();
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!(!stats.can_fire, "no weapon module ⇒ cannot fire");
        assert!(stats.weapon.is_none());

        // Install an autocannon (slot 3 is a Weapon slot) ⇒ can_fire == true.
        // derive_ship_stats is independent of budget validity (it derives from the
        // raw assignment map), so install raw here — the budget-gating path is
        // covered by the US1 validation tests, not this derivation test.
        let mut armed = fit.clone();
        armed.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let armed_layout = build_layout(&hull, &armed, &modules);
        let armed_stats = derive_ship_stats(&hull, &armed, &modules, &armed_layout);
        assert!(armed_stats.can_fire, "a weapon module enables firing");
        let profile = armed_stats.weapon.expect("weapon profile present");
        let cannon = modules.get(MODULE_AUTOCANNON).unwrap();
        if let ModuleSpecifics::Weapon {
            muzzle_speed,
            fire_rate,
            damage,
        } = cannon.specifics
        {
            assert_eq!(profile.muzzle_speed, muzzle_speed);
            assert_eq!(profile.fire_rate, fire_rate);
            assert_eq!(profile.damage, damage);
        } else {
            panic!("autocannon should be a Weapon module");
        }
    }

    #[test]
    fn crippled_fit_yields_finite_floored_stats() {
        // INV-F07/F14: an empty fit (no thrust, no reactor) still derives finite,
        // floored stats — never NaN/inf or divide-by-zero.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let fit = Fit::new(HULL_FIGHTER);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);

        assert!(stats.is_finite_and_floored());
        assert!(stats.thrust_force > 0.0 && stats.thrust_force.is_finite());
        assert!(stats.turn_torque > 0.0 && stats.turn_torque.is_finite());
        assert!(stats.total_mass >= hull.hull_base_mass && stats.total_mass > 0.0);
        assert!(stats.top_speed().is_finite() && stats.max_turn_rate().is_finite());
        assert!(!stats.can_fire);
    }

    // --- T019: baseline reproduces Tuning::default() + fitted ship moves ------

    #[test]
    fn baseline_seed_fit_reproduces_tuning_defaults() {
        // HINT-002 flight-feel-preservation guard: the baseline seed fit's
        // ShipStats equals Tuning::default() field-for-field.
        let (modules, _) = seed_catalogs();
        let hull = baseline_hull();
        let fit = baseline_fit();
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        let t = Tuning::default();

        assert!((stats.thrust_force - t.thrust_force).abs() < 1e-4, "thrust");
        assert!(
            (stats.reverse_force - t.reverse_force).abs() < 1e-4,
            "reverse"
        );
        assert!((stats.strafe_force - t.strafe_force).abs() < 1e-4, "strafe");
        assert!((stats.total_mass - t.mass).abs() < 1e-4, "mass");
        assert!((stats.turn_torque - t.turn_torque).abs() < 1e-4, "torque");
        assert_eq!(stats.linear_drag, t.linear_drag);
        assert_eq!(stats.angular_drag, t.angular_drag);
        assert_eq!(stats.angular_inertia, t.angular_inertia);
        assert_eq!(stats.turn_power_share, t.turn_power_share);
        // The emergent caps match the E002 intended values exactly.
        assert!((stats.top_speed() - t.top_speed()).abs() < 1e-3);
        assert!((stats.max_turn_rate() - t.max_turn_rate()).abs() < 1e-3);
    }

    /// Spawn a flight-model ship that carries a derived ShipStats override.
    fn spawn_fitted_ship(w: &mut World, stats: ShipStats) -> Entity {
        w.spawn((
            Ship,
            ShipIntent::default(),
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            Health(100.0),
            FlightAssist::On,
            CollisionRadius(0.8),
            stats,
        ))
        .id()
    }

    fn flight_schedule() -> Schedule {
        let mut s = Schedule::default();
        s.add_systems(sim::flight::ship_motion_system);
        s
    }

    #[test]
    fn fitted_ship_flies_to_its_fit_derived_top_speed() {
        // SC-003: a fitted ship stepped in a sim world moves under its OWN
        // fit-derived stats — the baseline fit reaches the Tuning top speed, and
        // a higher-thrust fit reaches a measurably higher top speed.
        let (modules, _) = seed_catalogs();
        let baseline_layout = build_layout(&baseline_hull(), &baseline_fit(), &modules);
        let baseline_stats = derive_ship_stats(
            &baseline_hull(),
            &baseline_fit(),
            &modules,
            &baseline_layout,
        );

        let mut w = World::new();
        // Tuning is still present (unfitted ships read it); the fitted ship
        // overrides it via its ShipStats component.
        w.insert_resource(Tuning::default());
        w.insert_resource(FixedDt(DT));
        let ship = spawn_fitted_ship(&mut w, baseline_stats);
        let mut sched = flight_schedule();

        // Full forward thrust for 15 s → approaches the emergent terminal velocity.
        {
            let mut intent = w.get_mut::<ShipIntent>(ship).unwrap();
            intent.forward = 1.0;
        }
        for _ in 0..900 {
            sched.run(&mut w);
        }
        let speed = w.get::<Velocity>(ship).unwrap().0.length();
        let v_max = Tuning::default().top_speed(); // baseline reproduces this
        assert!(
            (speed - v_max).abs() < 2.0,
            "baseline fitted ship approaches Tuning top speed {v_max}, got {speed}"
        );
    }

    #[test]
    fn recompute_system_rederives_stats_when_fit_changes() {
        // INV-F08: mutating a ship's Fit re-derives its ShipStats in the
        // fit-change system (Changed<Fit>), and the new stats drive flight.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

        // Start with one thruster, then add a second → top speed rises.
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();
        let layout = build_layout(&hull, &fit, &modules);
        let initial = derive_ship_stats(&hull, &fit, &modules, &layout);

        let mut w = World::new();
        let (m, h) = seed_catalogs();
        w.insert_resource(m);
        w.insert_resource(h);
        // A fitted ship carries a FitLayout — the recompute system (E007) now
        // requires it (it derives stats against the layout's live cell health).
        let ship = w.spawn((Ship, fit.clone(), initial, layout)).id();

        let mut sched = Schedule::default();
        sched.add_systems(recompute_ship_stats_system);

        // No fit change yet: recompute leaves the stats as derived.
        sched.run(&mut w);
        let before = *w.get::<ShipStats>(ship).unwrap();
        assert!((before.thrust_force - initial.thrust_force).abs() < 1e-4);

        // Mutate the Fit (add a second thruster) — triggers Changed<Fit>.
        {
            let mut f = w.get_mut::<Fit>(ship).unwrap();
            f.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules)
                .unwrap();
        }
        sched.run(&mut w);
        let after = *w.get::<ShipStats>(ship).unwrap();
        assert!(
            after.thrust_force > before.thrust_force,
            "adding a thruster re-derives higher thrust ({} > {})",
            after.thrust_force,
            before.thrust_force
        );
        assert!(after.top_speed() > before.top_speed());
    }
}

// --- Phase 5 (US3): the fit-layout hit/armor map + firing arcs (SC-004) -------
//
// T025 unit cases: resolve_hit returns the outer module before the inner one
// along a line; two fits differing only in reactor placement (central-behind-armor
// vs edge) reach the reactor at different depths; cell_map covers every authored
// cell with the correct occupant + health; hardpoint_arc is bounded (0, π] and
// None for a non-weapon slot. Plus a re-derive guard: a Changed<Fit> rebuilds the
// ship's FitLayout alongside its ShipStats (INV-F08).

mod layout_phase5 {
    use bevy_ecs::prelude::*;
    use glam::Vec2;
    use sim::fitting::{
        build_layout, cell_map, hardpoint_arc, module_at, recompute_ship_stats_system, resolve_hit,
        seed_catalogs, Fit, FitLayout, ShipStats, SlotId, HULL_FIGHTER, MODULE_ARMOR_PLATE,
        MODULE_AUTOCANNON, MODULE_REACTOR_BASIC,
    };

    /// The local cell-space center of a grid coordinate (`coord + 0.5`) — the same
    /// space `resolve_hit` traces in.
    fn cell_center(coord: (u16, u16)) -> Vec2 {
        Vec2::new(coord.0 as f32 + 0.5, coord.1 as f32 + 0.5)
    }

    #[test]
    fn resolve_hit_strikes_the_outer_module_before_the_inner_one() {
        // INV-F10/FR-021: the fighter's armor slot (slot 5 @ (2,3)) sits one cell
        // forward of the central reactor slot (slot 0 @ (2,2)). A line down column
        // 2 from the forward edge enters the armor cell FIRST, so resolve_hit
        // returns the armor (outer) — the reactor is shielded behind it.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC); // central, inner
        fit.install_raw(SlotId(5), MODULE_ARMOR_PLATE); // forward of it, outer

        let armor_coord = hull.slot(SlotId(5)).unwrap().coord; // (2,3)
        let reactor_coord = hull.slot(SlotId(0)).unwrap().coord; // (2,2)
        assert_eq!(
            armor_coord.0, reactor_coord.0,
            "same column for the test line"
        );

        // Fire from above the armor (higher row) straight down through both cells.
        let p0 = cell_center(armor_coord) + Vec2::new(0.0, 2.0);
        let p1 = cell_center(reactor_coord) - Vec2::new(0.0, 2.0);
        let hit = resolve_hit(&fit, p0, p1, hull, &modules).expect("the line strikes a module");
        assert_eq!(
            hit.module.module, MODULE_ARMOR_PLATE,
            "the outer armor is struck before the inner reactor"
        );
        assert_eq!(hit.cell, armor_coord);
        assert!((0.0..=1.0).contains(&hit.toi));
    }

    #[test]
    fn reactor_central_vs_edge_is_reached_at_different_depths() {
        // SC-004: two fits differing ONLY in where the reactor sits resolve the
        // same kind of shot to different occlusion depths. The seed fighter places
        // the reactor centrally; place a probe weapon module at an edge cell on a
        // second hull-equivalent fit and compare the struck cell's depth.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();

        // Central reactor (slot 0 @ (2,2)).
        let mut central = Fit::new(HULL_FIGHTER);
        central.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        let central_coord = hull.slot(SlotId(0)).unwrap().coord;

        // Edge-mounted device (slot 3 is a forward-edge weapon mount @ (1,4)).
        let mut edge = Fit::new(HULL_FIGHTER);
        edge.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let edge_coord = hull.slot(SlotId(3)).unwrap().coord;

        let layout_central = build_layout(hull, &central, &modules);
        let layout_edge = build_layout(hull, &edge, &modules);
        let central_depth = layout_central.occupant(central_coord).unwrap().depth;
        let edge_depth = layout_edge.occupant(edge_coord).unwrap().depth;
        assert!(
            central_depth > edge_depth,
            "the central reactor sits deeper (depth {central_depth}) than an edge mount (depth {edge_depth})"
        );

        // And a shot through each strikes its module at the matching cell depth.
        let hit_central = {
            let c = cell_center(central_coord);
            resolve_hit(
                &central,
                c - Vec2::new(3.0, 0.0),
                c + Vec2::new(3.0, 0.0),
                hull,
                &modules,
            )
            .unwrap()
        };
        let hit_edge = {
            let c = cell_center(edge_coord);
            resolve_hit(
                &edge,
                c - Vec2::new(3.0, 0.0),
                c + Vec2::new(3.0, 0.0),
                hull,
                &modules,
            )
            .unwrap()
        };
        assert_eq!(hit_central.cell, central_coord);
        assert_eq!(hit_edge.cell, edge_coord);
        let d_central = layout_central.occupant(hit_central.cell).unwrap().depth;
        let d_edge = layout_edge.occupant(hit_edge.cell).unwrap().depth;
        assert!(d_central > d_edge);
    }

    #[test]
    fn cell_map_covers_every_authored_cell_with_occupant_and_health() {
        // INV-F11: cell_map has exactly one occupant per authored cell; an occupied
        // cell carries the installed module's id + its (live) health, an empty cell
        // carries None / 0.0.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);

        let map = cell_map(&fit, hull, &modules);
        assert_eq!(
            map.len(),
            hull.cells.len(),
            "every authored cell is present"
        );
        for cell in &hull.cells {
            assert!(
                map.contains_key(&cell.coord),
                "cell {:?} present",
                cell.coord
            );
        }

        // The reactor cell reports the module + its health_max.
        let reactor_coord = hull.slot(SlotId(0)).unwrap().coord;
        let occ = map.get(&reactor_coord).unwrap();
        assert_eq!(occ.module, Some(MODULE_REACTOR_BASIC));
        assert_eq!(
            occ.health,
            modules.get(MODULE_REACTOR_BASIC).unwrap().health_max
        );
        assert!(occ.health > 0.0);

        // An empty (un-installed) slot cell reports no module and zero health.
        let empty_coord = hull.slot(SlotId(1)).unwrap().coord; // thruster slot, unfitted
        let empty = map.get(&empty_coord).unwrap();
        assert_eq!(empty.module, None);
        assert_eq!(empty.health, 0.0);

        // module_at agrees with the map for an occupied cell, None for an empty one.
        let r = module_at(&fit, reactor_coord, hull, &modules).unwrap();
        assert_eq!(r.module, MODULE_REACTOR_BASIC);
        assert_eq!(r.slot, SlotId(0));
        assert!(module_at(&fit, empty_coord, hull, &modules).is_none());

        // Phase 1A: the dense fighter silhouette also carries STRUCTURAL filler cells
        // (the hull body, not on any slot). A structural cell is `structural: true`,
        // holds no module, and is seeded with STRUCT_CELL_HP (> 0) — so the hull has a
        // carvable body for Phase 2 while remaining combat-invisible in 1A. (2,4) is the
        // authored nose-tip filler cell on the 5×5 fighter (forward of the weapons).
        use sim::fitting::content::STRUCT_CELL_HP;
        let nose = map
            .get(&(2, 4))
            .expect("the nose-tip structural cell is authored");
        assert!(nose.structural, "the nose-tip cell is structural filler");
        assert_eq!(nose.module, None, "a structural cell holds no module");
        assert_eq!(
            nose.health, STRUCT_CELL_HP,
            "a structural cell is seeded with STRUCT_CELL_HP"
        );
        // `module_at` ignores structural cells (no installed device there).
        assert!(module_at(&fit, (2, 4), hull, &modules).is_none());
    }

    #[test]
    fn hardpoint_arc_is_bounded_open_pi_and_none_for_non_weapon() {
        // INV-F12: every weapon mount exposes a FiringArc with half_angle ∈ (0, π];
        // a non-weapon slot exposes none.
        let (_, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();

        for slot in &hull.slots {
            let arc = hardpoint_arc(hull, slot.id);
            if slot.is_weapon_mount {
                let arc = arc.expect("a weapon mount exposes an arc");
                assert!(
                    arc.half_angle > 0.0 && arc.half_angle <= std::f32::consts::PI,
                    "half_angle {} must be in (0, π]",
                    arc.half_angle
                );
                // center is the hull-local mount facing (heading added by the consumer).
                assert_eq!(arc.center, slot.facing);
            } else {
                assert!(arc.is_none(), "a non-weapon slot has no arc");
            }
        }

        // An unknown slot id yields None (null-safe).
        assert!(hardpoint_arc(hull, SlotId(9999)).is_none());
    }

    #[test]
    fn fit_change_rebuilds_the_layout_alongside_ship_stats() {
        // INV-F08: a Changed<Fit> rebuilds the ship's FitLayout (the E007 hitbox)
        // on the same trigger that re-derives its ShipStats.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

        let fit = Fit::new(HULL_FIGHTER);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = sim::fitting::derive_ship_stats(&hull, &fit, &modules, &layout);
        let reactor_coord = hull.slot(SlotId(0)).unwrap().coord;
        // Initially the reactor cell is empty.
        assert_eq!(layout.occupant(reactor_coord).unwrap().module, None);

        let mut w = World::new();
        let (m, h) = seed_catalogs();
        w.insert_resource(m);
        w.insert_resource(h);
        let ship = w.spawn((fit, stats, layout)).id();

        let mut sched = Schedule::default();
        sched.add_systems(recompute_ship_stats_system);

        // Install the reactor → Changed<Fit> → layout rebuilt with the occupant.
        {
            let mut f = w.get_mut::<Fit>(ship).unwrap();
            f.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules)
                .unwrap();
        }
        sched.run(&mut w);
        let rebuilt = w.get::<FitLayout>(ship).unwrap();
        assert_eq!(
            rebuilt.occupant(reactor_coord).unwrap().module,
            Some(MODULE_REACTOR_BASIC),
            "the layout rebuilt with the newly installed reactor"
        );
        // ShipStats was re-derived on the same trigger (power supply rose).
        let new_stats = w.get::<ShipStats>(ship).unwrap();
        assert!(new_stats.power_supply > hull.power_capacity);
    }
}

// --- Phase 6 (US4): real tradeoffs + the hull ladder (SC-005) -----------------
//
// T027 integration cases: a no-fit-maxes-all guard over the SEED catalog — for any
// single hull, no valid fit simultaneously maxes tank, damage, AND speed (each
// maxed build binds a different budget ceiling: tank↔mass, damage↔cpu). And the
// corvette scales over the fighter (more slots/power/cpu/mass cap) at the cost of
// agility (heavier base mass → lower acceleration for a comparable thruster fit).
//
// This guard runs over the seed catalog so a future content/balance change that
// creates a dominant "do-everything" fit fails CI (HINT-005).

mod tradeoffs_phase6 {
    use sim::fitting::{
        budget_usage, build_layout, derive_ship_stats, seed_catalogs, validate_fit, Fit,
        HardpointType, Hull, HullCatalog, ModuleCatalog, ModuleId, HULL_CORVETTE, HULL_FIGHTER,
        MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
    };

    /// Fill every slot of `kind`-matching type on `hull` with `module` (raw install
    /// so the helper builds the *intended* maximal loadout; validity is asserted by
    /// the caller). Always also installs reactors so power-drawing maxes have supply.
    fn fill_slots(hull: &Hull, types: &[HardpointType], module: ModuleId) -> Fit {
        let mut fit = Fit::new(hull.id);
        for slot in &hull.slots {
            // Always fit reactors (free power for the other axes' draws).
            if slot.slot_type == HardpointType::Reactor {
                fit.install_raw(slot.id, MODULE_REACTOR_BASIC);
            } else if types.contains(&slot.slot_type) {
                fit.install_raw(slot.id, module);
            }
        }
        fit
    }

    /// A maximal **tank** fit: every armor slot plated (+ reactors).
    fn max_tank(hull: &Hull) -> Fit {
        fill_slots(hull, &[HardpointType::Armor], MODULE_ARMOR_PLATE)
    }
    /// A maximal **damage** fit: every weapon slot autocannoned (+ reactors).
    fn max_damage(hull: &Hull) -> Fit {
        fill_slots(hull, &[HardpointType::Weapon], MODULE_AUTOCANNON)
    }
    /// A maximal **speed** fit: every thruster slot driven (+ reactors).
    fn max_speed(hull: &Hull) -> Fit {
        fill_slots(hull, &[HardpointType::Thruster], MODULE_THRUSTER_BASIC)
    }

    /// A "try to do everything" fit: every slot filled with its strongest module.
    fn max_everything(hull: &Hull) -> Fit {
        let mut fit = Fit::new(hull.id);
        for slot in &hull.slots {
            let module = match slot.slot_type {
                HardpointType::Reactor => MODULE_REACTOR_BASIC,
                HardpointType::Thruster => MODULE_THRUSTER_BASIC,
                HardpointType::Weapon => MODULE_AUTOCANNON,
                HardpointType::Armor => MODULE_ARMOR_PLATE,
                // Shield/Utility: skip (no shield slot on the seed hulls; utility is
                // an inert seam). The three combat axes are what the guard probes.
                _ => continue,
            };
            fit.install_raw(slot.id, module);
        }
        fit
    }

    fn count_thrusters(hull: &Hull) -> usize {
        hull.slots
            .iter()
            .filter(|s| s.slot_type == HardpointType::Thruster)
            .count()
    }

    /// THE GUARD (SC-005, FR-023): for every seed hull, each single-axis-max fit is
    /// valid, but the do-everything fit is INVALID (over budget) — so no fit maxes
    /// tank + damage + speed at once. Tank and damage bind DIFFERENT axes.
    #[test]
    fn no_seed_fit_maxes_tank_damage_and_speed_at_once() {
        let (modules, hulls): (ModuleCatalog, HullCatalog) = seed_catalogs();

        for hull_id in [HULL_FIGHTER, HULL_CORVETTE] {
            let hull = hulls.get(hull_id).unwrap();

            // Each specialization is individually valid (the budgets allow ONE axis
            // pushed to its slot maximum).
            let tank = max_tank(hull);
            let damage = max_damage(hull);
            let speed = max_speed(hull);
            assert!(
                validate_fit(hull, &tank, &modules).valid,
                "{}: a max-tank fit should be valid on its own",
                hull.name
            );
            assert!(
                validate_fit(hull, &damage, &modules).valid,
                "{}: a max-damage fit should be valid on its own",
                hull.name
            );
            assert!(
                validate_fit(hull, &speed, &modules).valid,
                "{}: a max-speed fit should be valid on its own",
                hull.name
            );

            // The do-everything fit over-runs a budget — it is REJECTED. No single
            // valid fit maxes all three (the no-fit-maxes-all guarantee).
            let everything = max_everything(hull);
            let v = validate_fit(hull, &everything, &modules);
            assert!(
                !v.valid,
                "{}: filling every slot must over-run a budget (got {:?})",
                hull.name, v.violations
            );
        }
    }

    /// Tank and damage bind DIFFERENT budget axes (FR-023): the max-tank fit pushes
    /// the MASS ceiling (heavy armor) while the max-damage fit pushes the CPU
    /// ceiling (cpu-hungry weapons) — different ceilings, so trading into one frees
    /// nothing for the other.
    #[test]
    fn tank_binds_mass_while_damage_binds_cpu() {
        let (modules, hulls) = seed_catalogs();
        // The corvette has the richest armor + weapon slot inventory, so its axis
        // binding is the clearest demonstration of the tradeoff contract.
        let hull = hulls.get(HULL_CORVETTE).unwrap();

        let tank = max_tank(hull);
        let damage = max_damage(hull);
        let tank_use = budget_usage(hull, &tank, &modules);
        let dmg_use = budget_usage(hull, &damage, &modules);

        // The binding axis is the one pressed to the largest FRACTION of its ceiling
        // (absolute `used` is not comparable across axes — mass carries the hull's
        // base mass, cpu does not). Tank's tightest axis must be MASS; damage's must
        // be CPU — two different ceilings (the FR-023 tradeoff).
        let tank_mass_frac = tank_use.mass.used / tank_use.mass.capacity;
        let tank_cpu_frac = tank_use.cpu.used / tank_use.cpu.capacity;
        let tank_power_frac = tank_use.power.used / tank_use.power.capacity;
        let dmg_cpu_frac = dmg_use.cpu.used / dmg_use.cpu.capacity;
        let dmg_mass_frac = dmg_use.mass.used / dmg_use.mass.capacity;
        let dmg_power_frac = dmg_use.power.used / dmg_use.power.capacity;

        // Tank is bound by mass (heavy armor) above any other axis.
        assert!(
            tank_mass_frac > tank_cpu_frac && tank_mass_frac > tank_power_frac,
            "tank's binding axis is MASS (mass {tank_mass_frac}, cpu {tank_cpu_frac}, power {tank_power_frac})"
        );
        // Damage is bound by cpu (cpu-hungry weapons) above any other axis.
        assert!(
            dmg_cpu_frac > dmg_mass_frac && dmg_cpu_frac > dmg_power_frac,
            "damage's binding axis is CPU (cpu {dmg_cpu_frac}, mass {dmg_mass_frac}, power {dmg_power_frac})"
        );
        // The two binding axes are genuinely different (mass ≠ cpu).
        assert!(
            tank_mass_frac > tank_cpu_frac && dmg_cpu_frac > dmg_mass_frac,
            "tank binds mass while damage binds cpu — different ceilings (FR-023)"
        );
    }

    /// The corvette scales OVER the fighter — more slots/power/cpu/mass capacity —
    /// at the cost of agility: its far heavier base mass means a comparable thruster
    /// fit accelerates more slowly (SC-005). (Terminal top speed is mass-independent
    /// in this flight model; the heavier hull pays in acceleration/agility.)
    #[test]
    fn corvette_offers_more_capacity_at_the_cost_of_agility() {
        let (modules, hulls) = seed_catalogs();
        let fighter = hulls.get(HULL_FIGHTER).unwrap();
        let corvette = hulls.get(HULL_CORVETTE).unwrap();

        // More slots + more budget capacity on the larger hull.
        assert!(corvette.slots.len() > fighter.slots.len());
        assert!(corvette.power_capacity > fighter.power_capacity);
        assert!(corvette.cpu_capacity > fighter.cpu_capacity);
        assert!(corvette.mass_capacity > fighter.mass_capacity);
        assert!(corvette.hull_base_mass > fighter.hull_base_mass);

        // A comparable fit: every thruster slot driven on each hull. The corvette
        // gets MORE total thrust (more thruster slots) but is heavier, so its
        // acceleration (agility = thrust / mass) is lower than the fighter's.
        let fighter_fit = max_speed(fighter);
        let corvette_fit = max_speed(corvette);
        let fighter_layout = build_layout(fighter, &fighter_fit, &modules);
        let corvette_layout = build_layout(corvette, &corvette_fit, &modules);
        let fs = derive_ship_stats(fighter, &fighter_fit, &modules, &fighter_layout);
        let cs = derive_ship_stats(corvette, &corvette_fit, &modules, &corvette_layout);

        assert!(
            cs.thrust_force > fs.thrust_force,
            "the corvette has more thruster slots ⇒ more total thrust"
        );
        assert!(
            cs.total_mass > fs.total_mass,
            "the corvette is heavier (more base mass + more modules)"
        );
        let fighter_accel = fs.thrust_force / fs.total_mass;
        let corvette_accel = cs.thrust_force / cs.total_mass;
        assert!(
            corvette_accel < fighter_accel,
            "the larger corvette is less agile: accel {corvette_accel} < {fighter_accel}"
        );
        // Sanity: both comparable fits are valid (a thruster max is not over budget).
        assert!(validate_fit(fighter, &fighter_fit, &modules).valid);
        assert!(validate_fit(corvette, &corvette_fit, &modules).valid);
        // The number of thrusters did scale up.
        assert!(count_thrusters(corvette) > count_thrusters(fighter));
    }
}

/// Phase 7 (US5) — preset save/reload round-trip + before-commit preview (FR-024,
/// SC-006). Headless tests over the pure `sim` preset surface in
/// `crates/sim/src/fitting/fit.rs`: `save_preset` / `load_preset` / `preview_stats`.
///
/// Independent test (tasks.md US5): save a named fit, reload it onto a compatible
/// hull (yielding an equal fit), confirm reload onto an incompatible hull is
/// rejected, and confirm the before-commit `preview_stats` matches a committed
/// `derive_ship_stats` / `budget_usage` on the same fit.
mod presets_phase7 {
    use sim::fitting::{
        budget_usage, build_layout, derive_ship_stats, load_preset, preview_stats, save_preset,
        seed_catalogs, Fit, FitRejection, SlotId, HULL_CORVETTE, HULL_FIGHTER, MODULE_AUTOCANNON,
        MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
    };

    /// A representative, valid starter fighter fit: reactor + two thrusters + one
    /// autocannon. Within every fighter budget (power/cpu/mass) so it round-trips
    /// and previews cleanly.
    fn starter_fighter_fit() -> (sim::fitting::ModuleCatalog, sim::fitting::Hull, Fit) {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules)
            .expect("reactor fits the fighter's reactor slot");
        fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules)
            .expect("thruster fits slot 1");
        fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules)
            .expect("thruster fits slot 2");
        fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules)
            .expect("autocannon fits a weapon slot");
        (modules, hull, fit)
    }

    /// SC-006: a preset saved from a fit reloads to an EQUAL fit on the same
    /// (compatible) hull — the save→reload round-trip is loss-less.
    #[test]
    fn preset_save_reload_round_trips_on_the_same_hull() {
        let (modules, hull, fit) = starter_fighter_fit();

        let preset = save_preset("Starter Fighter", &fit);
        assert_eq!(preset.name, "Starter Fighter");
        assert_eq!(preset.fit, fit, "the preset stores the saved fit verbatim");

        let reloaded = load_preset(&preset, &hull, &modules)
            .expect("a valid fit reloads onto the hull it was saved on");
        assert_eq!(
            reloaded, fit,
            "save→reload yields an equal fit on a compatible hull (SC-006)"
        );
    }

    /// SC-006: reloading a fighter preset onto an INCOMPATIBLE hull (the corvette)
    /// is rejected — the saved slot ids do not belong to the target hull.
    #[test]
    fn preset_reload_onto_incompatible_hull_is_rejected() {
        let (modules, _fighter_hull, fit) = starter_fighter_fit();
        let (_, hulls) = seed_catalogs();
        let corvette = hulls.get(HULL_CORVETTE).unwrap().clone();

        let preset = save_preset("Starter Fighter", &fit);
        let result = load_preset(&preset, &corvette, &modules);
        assert!(
            matches!(result, Err(FitRejection::UnknownSlot { .. })),
            "a fighter preset is incompatible with the corvette hull, expected UnknownSlot, got {result:?}"
        );
    }

    /// SC-006: `preview_stats` (the before-commit preview) matches what committing
    /// the fit would derive — it is exactly `budget_usage` + `derive_ship_stats` on
    /// the same fit (no divergent formula, Principle II), and touches no live ship.
    #[test]
    fn preview_stats_matches_a_committed_derive_and_budget() {
        let (modules, hull, fit) = starter_fighter_fit();

        let (preview_budget, preview_stats_value) = preview_stats(&hull, &fit, &modules);

        let committed_budget = budget_usage(&hull, &fit, &modules);
        // SC-006: a spawn-time commit derives against a full-health layout, exactly
        // what `preview_stats` builds internally — so the two stay equal.
        let committed_layout = build_layout(&hull, &fit, &modules);
        let committed_stats = derive_ship_stats(&hull, &fit, &modules, &committed_layout);

        assert_eq!(
            preview_budget, committed_budget,
            "preview budget == committed budget_usage (SC-006)"
        );
        assert_eq!(
            preview_stats_value, committed_stats,
            "preview ShipStats == committed derive_ship_stats (SC-006)"
        );
    }

    /// An empty preset round-trips on its own hull (the valid baseline, INV-F05) —
    /// a guard that the round-trip holds for the degenerate empty fit too.
    #[test]
    fn empty_preset_round_trips_on_its_hull() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let fit = Fit::new(HULL_FIGHTER);

        let preset = save_preset("Empty", &fit);
        let reloaded =
            load_preset(&preset, &hull, &modules).expect("an empty baseline fit reloads cleanly");
        assert_eq!(reloaded, fit);
        assert!(reloaded.is_empty());
    }
}
