//! Seed content — the authored [`ModuleCatalog`] + [`HullCatalog`] (FR-022,
//! FR-025; data-model.md seed-content tables).
//!
//! Module and Hull definitions are **content rows**, not code: [`seed_catalogs`]
//! authors the starting set — 2 hulls (`fighter`, `corvette`) on a scaling ladder
//! plus 6 module archetypes (reactor / thruster / weapon / shield / armor /
//! utility) — and they extend purely as data (FR-025, `NEW-CONFIG`). The catalogs
//! are `bevy_ecs` resources loaded at startup and immutable at runtime.
//!
//! **Numbers here are first-pass shape, not final balance.** The data-model
//! contract is the *shape* (larger hull = more slots/power but greater base mass
//! → lower agility; tank fits bind mass/power, damage fits bind cpu/power); the
//! balance-tuning pass that makes the no-fit-maxes-all guard hold is Phase 6
//! (T026). Effective-stat derivation, validation, and the hit-map are later
//! phases — this file only authors the rows the seam reuses.

use std::collections::BTreeMap;

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use super::fit::Fit;
use super::hull::{GridCell, Hull, HullId, SectionId, Slot, SlotId};
use super::module::{HardpointType, Module, ModuleId, ModuleKind, ModuleSpecifics, SlotSize};

/// Singleton resource: every authored [`Module`] keyed by its [`ModuleId`]
/// (data-model.md). Loaded from content at startup (FR-025); read by validation +
/// derivation; immutable at runtime (content reload only).
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModuleCatalog {
    /// Content rows by id; `BTreeMap` for deterministic iteration.
    pub modules: BTreeMap<ModuleId, Module>,
}

impl ModuleCatalog {
    /// Look up a module content row by id (null-safe; `None` if not authored).
    pub fn get(&self, id: ModuleId) -> Option<&Module> {
        self.modules.get(&id)
    }

    /// Number of authored module rows.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether the catalog has no authored modules.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Singleton resource: every authored [`Hull`] keyed by its [`HullId`]
/// (data-model.md). Loaded from content at startup (FR-025); immutable at runtime.
#[derive(Resource, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HullCatalog {
    /// Content rows by id; `BTreeMap` for deterministic iteration.
    pub hulls: BTreeMap<HullId, Hull>,
}

impl HullCatalog {
    /// Look up a hull content row by id (null-safe; `None` if not authored).
    pub fn get(&self, id: HullId) -> Option<&Hull> {
        self.hulls.get(&id)
    }

    /// Number of authored hull rows.
    pub fn len(&self) -> usize {
        self.hulls.len()
    }

    /// Whether the catalog has no authored hulls.
    pub fn is_empty(&self) -> bool {
        self.hulls.is_empty()
    }
}

// --- Stable seed content ids -------------------------------------------------
//
// Authored ids are stable so downstream phases/tests can reference exact rows
// (data-model.md: ids are wire/save-safe content keys).

/// Seed hull id: the agile, few-slot `fighter`.
pub const HULL_FIGHTER: HullId = HullId(1);
/// Seed hull id: the tankier, more-firepower, less-agile `corvette`.
pub const HULL_CORVETTE: HullId = HullId(2);

/// Seed module id: `reactor_basic` — supplies power; cost axis = mass.
pub const MODULE_REACTOR_BASIC: ModuleId = ModuleId(1);
/// Seed module id: `thruster_basic` — thrust/torque; cost = power_draw + mass.
pub const MODULE_THRUSTER_BASIC: ModuleId = ModuleId(2);
/// Seed module id: `autocannon` — weapon fire params; cost = power + cpu.
pub const MODULE_AUTOCANNON: ModuleId = ModuleId(3);
/// Seed module id: `shield_basic` — shield hp/regen; cost = power + cpu.
pub const MODULE_SHIELD_BASIC: ModuleId = ModuleId(4);
/// Seed module id: `armor_plate` — armor value; cost axis = mass.
pub const MODULE_ARMOR_PLATE: ModuleId = ModuleId(5);
/// Seed module id: `utility_basic` — extensibility seam; cost = cpu.
pub const MODULE_UTILITY_BASIC: ModuleId = ModuleId(6);

/// Baseline reference hull id (HINT-002): a single-slot reference chassis whose
/// only purpose is the flight-feel-preservation guard — the [`baseline_fit`] on
/// it derives to **exactly** [`Tuning::default()`](crate::tuning::Tuning::default).
/// Not part of the player-facing scaling ladder (fighter/corvette).
pub const HULL_BASELINE: HullId = HullId(100);
/// Baseline reference thruster id (HINT-002): supplies the full
/// [`Tuning::default()`](crate::tuning::Tuning::default) thrust/torque/strafe
/// (`30/12/18`) in one module, mass-tuned so the baseline fit's `total_mass`
/// reproduces `Tuning::default().mass` (`1.0`). The flight-feel reference.
pub const MODULE_BASELINE_THRUSTER: ModuleId = ModuleId(100);

/// Author the seed [`ModuleCatalog`] + [`HullCatalog`] (FR-022, FR-025).
///
/// Returns the 6 module archetypes and 2 scaling hulls as data. This is the
/// single content-load entry point; both the client preview and a future server
/// authority load identical content (Principle II). Balance tuning is Phase 6.
pub fn seed_catalogs() -> (ModuleCatalog, HullCatalog) {
    (seed_modules(), seed_hulls())
}

/// The 6 seed module archetypes (one per [`ModuleKind`]).
///
/// **Axis-binding tune (T026, FR-022/023, SC-005)** — the archetypes deliberately
/// bind **different** budget ceilings so no single fit maxes tank + damage + speed
/// at once (the no-fit-maxes-all guard, T027):
///
/// | archetype  | gives        | dominant cost axis      |
/// |------------|--------------|-------------------------|
/// | armor_plate| armor (tank) | **mass** (heavy plate)  |
/// | shield_basic| shield (tank)| **power** (power-hungry)|
/// | autocannon | damage       | **cpu** (+ some power)  |
/// | thruster   | speed/agility| **power + mass**        |
/// | reactor    | + power      | **mass** (heavy core)   |
/// | utility    | seam         | cpu                     |
///
/// So a maxed **tank** fills the heavy armor/shield slots and binds **mass/power**;
/// a maxed **damage** fills the cpu-hungry weapon slots and binds **cpu**; a maxed
/// **speed** fills the power-hungry thruster slots and binds **power/mass** — three
/// different ceilings (FR-023). `thruster_basic`'s thrust/torque/strafe (15/6/9)
/// are **unchanged** so the seed two-thruster fighter still reproduces the E002
/// 30/12/18 flight magnitudes; the `baseline_thruster` (HINT-002) is untouched so
/// `baseline_fit()` still derives to `Tuning::default()` exactly.
fn seed_modules() -> ModuleCatalog {
    let rows = [
        // reactor_basic — supplies power_supply; dominant cost axis = mass (a heavy
        // power core). Fitting more reactors for power costs mass budget.
        Module {
            id: MODULE_REACTOR_BASIC,
            kind: ModuleKind::Reactor,
            power_gen: 20.0,
            power_draw: 0.0,
            cpu_draw: 0.0,
            mass: 6.0,
            heat: 2.0,
            health_max: 30.0,
            hardpoint_type: HardpointType::Reactor,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Reactor,
        },
        // thruster_basic — + thrust/torque (speed/agility); cost axes = power_draw
        // + mass. Thrust 15/6/9 UNCHANGED (two of them sum to the E002 30/12/18).
        Module {
            id: MODULE_THRUSTER_BASIC,
            kind: ModuleKind::Thruster,
            power_gen: 0.0,
            power_draw: 3.0,
            cpu_draw: 0.5,
            mass: 3.0,
            heat: 1.0,
            health_max: 20.0,
            hardpoint_type: HardpointType::Thruster,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Thruster {
                thrust_force: 15.0,
                turn_torque: 6.0,
                strafe_force: 9.0,
            },
        },
        // autocannon — weapon fire params (damage); dominant cost axis = cpu_draw
        // (with some power). Maxing weapons starves the CPU budget.
        Module {
            id: MODULE_AUTOCANNON,
            kind: ModuleKind::Weapon,
            power_gen: 0.0,
            power_draw: 3.0,
            cpu_draw: 4.0,
            mass: 1.5,
            heat: 3.0,
            health_max: 15.0,
            hardpoint_type: HardpointType::Weapon,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Weapon {
                muzzle_speed: 200.0,
                fire_rate: 5.0,
                damage: 12.0,
            },
        },
        // shield_basic — shield hp/regen (tank); dominant cost axis = power_draw
        // (a power-hungry projector). Maxing shields starves the power budget.
        Module {
            id: MODULE_SHIELD_BASIC,
            kind: ModuleKind::Shield,
            power_gen: 0.0,
            power_draw: 6.0,
            cpu_draw: 2.0,
            mass: 1.0,
            heat: 1.0,
            health_max: 15.0,
            hardpoint_type: HardpointType::Shield,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Shield {
                shield_hp: 60.0,
                regen: 5.0,
            },
        },
        // armor_plate — + armor_value (tank); dominant cost axis = mass (heavy
        // plate, no power/cpu). Maxing armor starves the mass budget → low agility.
        Module {
            id: MODULE_ARMOR_PLATE,
            kind: ModuleKind::Armor,
            power_gen: 0.0,
            power_draw: 0.0,
            cpu_draw: 0.0,
            mass: 8.0,
            heat: 0.0,
            health_max: 40.0,
            hardpoint_type: HardpointType::Armor,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Armor { armor_value: 80.0 },
        },
        // utility_basic — extensibility seam; cost = cpu_draw.
        Module {
            id: MODULE_UTILITY_BASIC,
            kind: ModuleKind::Utility,
            power_gen: 0.0,
            power_draw: 1.0,
            cpu_draw: 3.0,
            mass: 0.5,
            heat: 0.5,
            health_max: 10.0,
            hardpoint_type: HardpointType::Utility,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Utility,
        },
        // baseline_thruster — the flight-feel reference (HINT-002): supplies the
        // full `Tuning::default()` thrust/torque/strafe (30/12/18) in one module,
        // mass 0.6 so `hull_base_mass(0.4) + 0.6 = 1.0 = Tuning::default().mass`.
        // Outside the player-facing ladder; used only by `baseline_fit`.
        Module {
            id: MODULE_BASELINE_THRUSTER,
            kind: ModuleKind::Thruster,
            power_gen: 0.0,
            power_draw: 0.0,
            cpu_draw: 0.0,
            mass: 0.6,
            heat: 0.0,
            health_max: 20.0,
            hardpoint_type: HardpointType::Thruster,
            hardpoint_size: SlotSize::Small,
            specifics: ModuleSpecifics::Thruster {
                thrust_force: 30.0,
                turn_torque: 12.0,
                strafe_force: 18.0,
            },
        },
    ];

    let modules = rows.into_iter().map(|m| (m.id, m)).collect();
    ModuleCatalog { modules }
}

/// The 2 seed hulls on the scaling ladder: `fighter` (small/agile) and
/// `corvette` (larger/tankier, more slots+power, more base mass → less agile).
fn seed_hulls() -> HullCatalog {
    let hulls = [seed_fighter(), seed_corvette()];
    let map = hulls.into_iter().map(|h| (h.id, h)).collect();
    HullCatalog { hulls: map }
}

/// `fighter` — 5×5 grid, low budgets, ~7 small slots; agile, few slots.
fn seed_fighter() -> Hull {
    // Slot layout (col,row) on a 5×5 grid; one section per slot for the coarse
    // section-granularity authoring (cell-upgrade-ready, HINT-004). Weapon mounts
    // sit forward (high row) and exposed; the reactor sits central (protected).
    let slots = vec![
        slot(
            0,
            HardpointType::Reactor,
            SlotSize::Small,
            (2, 2),
            0.0,
            false,
        ),
        slot(
            1,
            HardpointType::Thruster,
            SlotSize::Small,
            (1, 0),
            0.0,
            false,
        ),
        slot(
            2,
            HardpointType::Thruster,
            SlotSize::Small,
            (3, 0),
            0.0,
            false,
        ),
        slot(3, HardpointType::Weapon, SlotSize::Small, (1, 4), 0.0, true),
        slot(4, HardpointType::Weapon, SlotSize::Small, (3, 4), 0.0, true),
        slot(5, HardpointType::Armor, SlotSize::Small, (2, 3), 0.0, false),
        slot(
            6,
            HardpointType::Utility,
            SlotSize::Small,
            (2, 1),
            0.0,
            false,
        ),
    ];
    Hull {
        id: HULL_FIGHTER,
        name: "Fighter".to_string(),
        grid_dims: (5, 5),
        cells: cells_for_slots(&slots),
        // Caps tuned (T026) so each single-axis-max fit is valid but filling every
        // slot with its strongest module over-runs a budget (cpu, here) — no fit
        // maxes tank + damage + speed at once (T027). Generous power so two
        // thrusters fly reactor-less; tight cpu so a full weapon loadout (2 cannons
        // = 8 cpu) sits at the ceiling and adding the thrusters' cpu over-runs it.
        power_capacity: 10.0,
        cpu_capacity: 8.0,
        mass_capacity: 36.0,
        hull_base_mass: 8.0,
        slots,
    }
}

/// `corvette` — 9×9 grid, high budgets, ~14 medium slots; tankier/more firepower,
/// less agile (greater base mass).
fn seed_corvette() -> Hull {
    let slots = vec![
        slot(
            0,
            HardpointType::Reactor,
            SlotSize::Medium,
            (4, 4),
            0.0,
            false,
        ),
        slot(
            1,
            HardpointType::Reactor,
            SlotSize::Medium,
            (3, 4),
            0.0,
            false,
        ),
        slot(
            2,
            HardpointType::Thruster,
            SlotSize::Medium,
            (2, 0),
            0.0,
            false,
        ),
        slot(
            3,
            HardpointType::Thruster,
            SlotSize::Medium,
            (4, 0),
            0.0,
            false,
        ),
        slot(
            4,
            HardpointType::Thruster,
            SlotSize::Medium,
            (6, 0),
            0.0,
            false,
        ),
        slot(
            5,
            HardpointType::Weapon,
            SlotSize::Medium,
            (1, 8),
            0.0,
            true,
        ),
        slot(
            6,
            HardpointType::Weapon,
            SlotSize::Medium,
            (3, 8),
            0.0,
            true,
        ),
        slot(
            7,
            HardpointType::Weapon,
            SlotSize::Medium,
            (5, 8),
            0.0,
            true,
        ),
        slot(
            8,
            HardpointType::Weapon,
            SlotSize::Medium,
            (7, 8),
            0.0,
            true,
        ),
        slot(
            9,
            HardpointType::Armor,
            SlotSize::Medium,
            (4, 6),
            0.0,
            false,
        ),
        slot(
            10,
            HardpointType::Armor,
            SlotSize::Medium,
            (3, 6),
            0.0,
            false,
        ),
        slot(
            11,
            HardpointType::Armor,
            SlotSize::Medium,
            (5, 6),
            0.0,
            false,
        ),
        slot(
            12,
            HardpointType::Utility,
            SlotSize::Medium,
            (4, 2),
            0.0,
            false,
        ),
        slot(
            13,
            HardpointType::Utility,
            SlotSize::Medium,
            (4, 3),
            0.0,
            false,
        ),
    ];
    Hull {
        id: HULL_CORVETTE,
        name: "Corvette".to_string(),
        grid_dims: (9, 9),
        cells: cells_for_slots(&slots),
        // Scales OVER the fighter on every capacity (more slots/power/cpu/mass) but
        // carries far more base mass → lower agility (SC-005). Caps tuned (T026) so
        // a max-tank fit binds **mass** (heavy armor) and a max-damage fit binds
        // **cpu** (weapon-heavy) — different ceilings — while filling every slot
        // over-runs both (T027 no-fit-maxes-all guard).
        power_capacity: 30.0,
        cpu_capacity: 20.0,
        mass_capacity: 70.0,
        hull_base_mass: 30.0,
        slots,
    }
}

/// The baseline reference hull (HINT-002): a 1×1 chassis with a single Thruster
/// slot and `hull_base_mass = 0.4`, sized so the [`baseline_fit`] derives to
/// **exactly** [`Tuning::default()`](crate::tuning::Tuning). It exists only to
/// pin the flight-feel-preservation guard (T019); it is **not** in the
/// [`HullCatalog`] (not a player-facing hull) — callers build it directly.
///
/// Budgets are generous (no module draws power/cpu) so the baseline fit validates
/// clean as the reference baseline.
pub fn baseline_hull() -> Hull {
    let slots = vec![slot(
        0,
        HardpointType::Thruster,
        SlotSize::Small,
        (0, 0),
        0.0,
        false,
    )];
    Hull {
        id: HULL_BASELINE,
        name: "Baseline".to_string(),
        grid_dims: (1, 1),
        cells: cells_for_slots(&slots),
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 0.4,
        slots,
    }
}

/// The baseline reference fit (HINT-002): the [`baseline_hull`] with the single
/// [`MODULE_BASELINE_THRUSTER`] installed. [`derive_ship_stats`] on this fit
/// reproduces [`Tuning::default()`](crate::tuning::Tuning) field-for-field — the
/// flight-feel-preservation guard (T019) so a baseline-fitted ship flies
/// identically to an unfitted `Tuning` ship.
pub fn baseline_fit() -> Fit {
    let mut fit = Fit::new(HULL_BASELINE);
    fit.install_raw(SlotId(0), MODULE_BASELINE_THRUSTER);
    fit
}

/// Build a [`Slot`] terse constructor for the seed tables.
fn slot(
    id: u32,
    slot_type: HardpointType,
    size: SlotSize,
    coord: (u16, u16),
    facing: f32,
    is_weapon_mount: bool,
) -> Slot {
    Slot {
        id: SlotId(id),
        slot_type,
        size,
        coord,
        facing,
        is_weapon_mount,
    }
}

/// Author one [`GridCell`] per slot, each its own section (coarse
/// section-granularity, cell-upgrade-ready — HINT-004). The cell-grid IS the
/// hit/armor map E007 reads; finer authoring is a later content upgrade.
fn cells_for_slots(slots: &[Slot]) -> Vec<GridCell> {
    slots
        .iter()
        .enumerate()
        .map(|(i, s)| GridCell::new(s.coord, SectionId(i as u32)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_catalogs_have_two_hulls_and_the_archetypes_plus_baseline() {
        let (modules, hulls) = seed_catalogs();
        // The 6 player-facing archetypes plus the baseline flight-feel reference
        // thruster (HINT-002) — 7 module rows in total.
        assert_eq!(
            modules.len(),
            7,
            "expected the 6 archetypes + baseline reference thruster"
        );
        assert!(modules.get(MODULE_BASELINE_THRUSTER).is_some());
        assert_eq!(hulls.len(), 2, "expected the fighter + corvette seed hulls");
    }

    #[test]
    fn baseline_fit_derives_to_tuning_defaults() {
        // HINT-002: the baseline reference fit reproduces Tuning::default() — the
        // flight-feel guard. (Full assertion lives in stats.rs / tests/fitting.rs.)
        use super::super::layout::build_layout;
        use super::super::stats::derive_ship_stats;
        let (modules, _) = seed_catalogs();
        let hull = baseline_hull();
        let fit = baseline_fit();
        // Full-health layout: every health-factor is 1.0, so the derive reproduces
        // the pre-E007 Tuning::default() numbers bit-for-bit (the green-keeper).
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        let t = crate::tuning::Tuning::default();
        assert!((stats.thrust_force - t.thrust_force).abs() < 1e-4);
        assert!((stats.total_mass - t.mass).abs() < 1e-4);
    }

    #[test]
    fn seed_modules_cover_every_kind_and_resolve_by_id() {
        let (modules, _) = seed_catalogs();
        let kinds: Vec<ModuleKind> = [
            MODULE_REACTOR_BASIC,
            MODULE_THRUSTER_BASIC,
            MODULE_AUTOCANNON,
            MODULE_SHIELD_BASIC,
            MODULE_ARMOR_PLATE,
            MODULE_UTILITY_BASIC,
        ]
        .into_iter()
        .map(|id| modules.get(id).expect("seed module resolves").kind)
        .collect();
        assert_eq!(
            kinds,
            vec![
                ModuleKind::Reactor,
                ModuleKind::Thruster,
                ModuleKind::Weapon,
                ModuleKind::Shield,
                ModuleKind::Armor,
                ModuleKind::Utility,
            ]
        );
    }

    #[test]
    fn corvette_scales_over_fighter() {
        // SC-005 shape: larger hull = more slots/power/mass-cap but greater base
        // mass (→ lower agility once derivation lands).
        let (_, hulls) = seed_catalogs();
        let fighter = hulls.get(HULL_FIGHTER).unwrap();
        let corvette = hulls.get(HULL_CORVETTE).unwrap();

        assert!(corvette.slots.len() > fighter.slots.len());
        assert!(corvette.power_capacity > fighter.power_capacity);
        assert!(corvette.cpu_capacity > fighter.cpu_capacity);
        assert!(corvette.mass_capacity > fighter.mass_capacity);
        assert!(corvette.hull_base_mass > fighter.hull_base_mass);
    }

    #[test]
    fn every_slot_sits_on_an_authored_cell_and_ids_are_unique() {
        let (_, hulls) = seed_catalogs();
        for hull in hulls.hulls.values() {
            let mut seen = std::collections::BTreeSet::new();
            for s in &hull.slots {
                assert!(seen.insert(s.id), "duplicate slot id in {}", hull.name);
                assert!(
                    s.coord.0 < hull.grid_dims.0 && s.coord.1 < hull.grid_dims.1,
                    "slot {:?} out of grid bounds in {}",
                    s.id,
                    hull.name
                );
                assert!(
                    hull.cells.iter().any(|c| c.coord == s.coord),
                    "slot {:?} not on an authored cell in {}",
                    s.id,
                    hull.name
                );
            }
        }
    }
}
