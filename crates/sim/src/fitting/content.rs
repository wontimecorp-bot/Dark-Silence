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

/// Live structural HP seeded onto every **structural** hull cell in
/// [`build_layout`](super::layout::build_layout) (Phase 1A). Module cells take their
/// installed module's `health_max`; structural filler cells take this so the dense
/// hull body has hit points Phase 2 can carve away cell-by-cell (ADR-0008, GDD §5).
///
/// Tunable: first-pass shape, not final balance (Phase 3 tunes carve feel). It is
/// **not** wired into combat in Phase 1A — `resolve_hit` still resolves to module
/// cells only, so structural HP changes no combat outcome this phase; it exists so the
/// per-cell health store is populated for the Phase 2 carving model.
pub const STRUCT_CELL_HP: f32 = 10.0;

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

/// `fighter` — 9×11 grid, low budgets, 7 small slots; agile, few slots.
///
/// Revise-A re-authors the fighter at **finer cell fidelity** (was a coarse 17-cell
/// 5×5 blob): a smoother, recognizable fighter silhouette on a 9×11 grid that reads
/// as a ship (pointed nose, swept wings, engine block) and gives Phase 2 a much
/// finer-grained body to erode. The 7 hardpoint slots keep the **one-slot-one-cell**
/// model and are re-placed at sensible fine-grid coords inside the silhouette.
fn seed_fighter() -> Hull {
    // Slot layout (col,row) on the 9×11 grid; one section per slot for the coarse
    // section-granularity authoring (cell-upgrade-ready, HINT-004). Forward = +row
    // (the nose sits at the high rows). Placement rationale on the finer grid:
    //   - reactor (4,4): central column, deepest interior cell (depth 4 — the max on
    //     this silhouette and the smallest such cell), so it is the `core_cell` the
    //     sever/shatter chain treats as the ship core (protected behind the body);
    //   - armor (4,5): one cell FORWARD of the reactor in the same column, so a
    //     forward→aft (decreasing-row) shot strikes the armor cover before the buried
    //     reactor (the outer-before-inner survivability property, FR-021);
    //   - weapons (2,6)/(6,6): forward, toward the wing-root/fore, exposed (low depth);
    //   - thrusters (3,0)/(5,0): aft engine block (row 0), an exposed perimeter ring;
    //   - utility (4,2): aft-central fuselage.
    let slots = vec![
        slot(
            0,
            HardpointType::Reactor,
            SlotSize::Small,
            (4, 4),
            0.0,
            false,
        ),
        slot(
            1,
            HardpointType::Thruster,
            SlotSize::Small,
            (3, 0),
            0.0,
            false,
        ),
        slot(
            2,
            HardpointType::Thruster,
            SlotSize::Small,
            (5, 0),
            0.0,
            false,
        ),
        slot(3, HardpointType::Weapon, SlotSize::Small, (2, 6), 0.0, true),
        slot(4, HardpointType::Weapon, SlotSize::Small, (6, 6), 0.0, true),
        slot(5, HardpointType::Armor, SlotSize::Small, (4, 5), 0.0, false),
        slot(
            6,
            HardpointType::Utility,
            SlotSize::Small,
            (4, 2),
            0.0,
            false,
        ),
    ];
    // Dense fighter silhouette on the 9×11 grid (forward = +row; centre column = 4). A
    // smoother, recognizable arrow-fighter (revise-A): a pointed nose tapering down into
    // a fore fuselage, full-beam swept wings (widest at row 5, tips at cols 0/8), a
    // narrow fuselage neck, and a wider engine block aft. 51 cells (was a 17-cell blob),
    // covering every slot coord. Deterministic per-row column spans (row 10 = nose tip,
    // row 0 = aft engines):
    //   row 10 (nose tip):  col  4        (1)
    //   row  9 (nose):      cols 3..=5    (3)
    //   row  8 (nose):      cols 3..=5    (3)
    //   row  7 (fore):      cols 2..=6    (5)   weapons line up just below here
    //   row  6 (fore):      cols 2..=6    (5)   weapons (2,6)/(6,6)
    //   row  5 (wing beam): cols 0..=8    (9)   full wing span; armor (4,5)
    //   row  4 (wing root): cols 1..=7    (7)   reactor (4,4) central/deep
    //   row  3 (neck):      cols 3..=5    (3)
    //   row  2 (engines):   cols 2..=6    (5)   utility (4,2)
    //   row  1 (engines):   cols 2..=6    (5)
    //   row  0 (engines):   cols 2..=6    (5)   thrusters (3,0)/(5,0)
    let fighter_shape = |col: u16, row: u16| match row {
        10 => col == 4,
        9 | 8 | 3 => (3..=5).contains(&col),
        7 | 6 | 2 | 1 | 0 => (2..=6).contains(&col),
        5 => col <= 8, // full beam (wings)
        4 => (1..=7).contains(&col),
        _ => false,
    };
    Hull {
        id: HULL_FIGHTER,
        name: "Fighter".to_string(),
        grid_dims: (9, 11),
        cells: dense_cells((9, 11), &slots, fighter_shape),
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

/// `corvette` — 13×15 grid, high budgets, 14 medium slots; tankier/more firepower,
/// less agile (greater base mass).
///
/// Revise-A re-authors the corvette proportionally larger than the finer fighter (was
/// a coarse 57-cell 9×9): a bigger, beamier capital silhouette on a 13×15 grid (103
/// cells) with the 14 hardpoint slots re-placed at sensible fine-grid coords. Same
/// one-slot-one-cell model and the same module-vs-structural scheme as the fighter.
fn seed_corvette() -> Hull {
    // Slot layout (col,row) on the 13×15 grid; one section per slot. Forward = +row;
    // centre column = 6. Placement rationale on the finer grid:
    //   - reactors (6,6)/(5,6): central, deep; (6,6) is the deepest interior cell
    //     (depth 6, the smallest such cell) → the `core_cell` the sever/shatter chain
    //     treats as the core, the second reactor flanks it;
    //   - armor (6,7)/(5,7)/(7,7): the mid armor band one row FORWARD of the reactors
    //     ((6,7) directly covers the core reactor in column 6 — outer-before-inner);
    //   - weapons (4,11)/(8,11)/(5,12)/(7,12): the forward weapon prow (exposed);
    //   - thrusters (4,0)/(6,0)/(8,0): the aft engine block (perimeter ring, depth 0);
    //   - utility (6,4)/(6,3): the fuselage neck / engine spread, central-aft.
    let slots = vec![
        slot(
            0,
            HardpointType::Reactor,
            SlotSize::Medium,
            (6, 6),
            0.0,
            false,
        ),
        slot(
            1,
            HardpointType::Reactor,
            SlotSize::Medium,
            (5, 6),
            0.0,
            false,
        ),
        slot(
            2,
            HardpointType::Thruster,
            SlotSize::Medium,
            (4, 0),
            0.0,
            false,
        ),
        slot(
            3,
            HardpointType::Thruster,
            SlotSize::Medium,
            (6, 0),
            0.0,
            false,
        ),
        slot(
            4,
            HardpointType::Thruster,
            SlotSize::Medium,
            (8, 0),
            0.0,
            false,
        ),
        slot(
            5,
            HardpointType::Weapon,
            SlotSize::Medium,
            (4, 11),
            0.0,
            true,
        ),
        slot(
            6,
            HardpointType::Weapon,
            SlotSize::Medium,
            (8, 11),
            0.0,
            true,
        ),
        slot(
            7,
            HardpointType::Weapon,
            SlotSize::Medium,
            (5, 12),
            0.0,
            true,
        ),
        slot(
            8,
            HardpointType::Weapon,
            SlotSize::Medium,
            (7, 12),
            0.0,
            true,
        ),
        slot(
            9,
            HardpointType::Armor,
            SlotSize::Medium,
            (6, 7),
            0.0,
            false,
        ),
        slot(
            10,
            HardpointType::Armor,
            SlotSize::Medium,
            (5, 7),
            0.0,
            false,
        ),
        slot(
            11,
            HardpointType::Armor,
            SlotSize::Medium,
            (7, 7),
            0.0,
            false,
        ),
        slot(
            12,
            HardpointType::Utility,
            SlotSize::Medium,
            (6, 4),
            0.0,
            false,
        ),
        slot(
            13,
            HardpointType::Utility,
            SlotSize::Medium,
            (6, 3),
            0.0,
            false,
        ),
    ];
    // Dense corvette silhouette on the 13×15 grid (forward = +row; centre column = 6).
    // A larger, beamier capital ship proportional to the finer fighter (revise-A): a
    // pointed weapon prow, a full-beam swept-wing midsection (widest at row 7, tips at
    // cols 0/12), and a broad engine block aft. 103 cells (was 57); covers every slot
    // coord. Deterministic per-row column spans (row 14 = prow tip, row 0 = aft engines):
    //   row 14 (prow tip):   cols 5..=7    (3)
    //   row 13 (prow):       cols 5..=7    (3)
    //   row 12 (prow):       cols 4..=8    (5)   weapons (5,12)/(7,12)
    //   row 11 (prow):       cols 4..=8    (5)   weapons (4,11)/(8,11)
    //   row 10 (fore):       cols 3..=9    (7)
    //   row  9 (fore):       cols 3..=9    (7)
    //   row  8 (fore):       cols 2..=10   (9)
    //   row  7 (wing beam):  cols 0..=12   (13)  full wing span; armor (5,7)/(6,7)/(7,7)
    //   row  6 (wing root):  cols 1..=11   (11)  reactors (5,6)/(6,6) central/deep
    //   row  5 (mid):        cols 2..=10   (9)
    //   row  4 (neck):       cols 4..=8    (5)   utility (6,4)
    //   row  3 (engines):    cols 3..=9    (7)   utility (6,3)
    //   row  2 (engines):    cols 3..=9    (7)
    //   row  1 (engines):    cols 3..=9    (7)
    //   row  0 (engines):    cols 4..=8    (5)   thrusters (4,0)/(6,0)/(8,0)
    let corvette_shape = |col: u16, row: u16| match row {
        14 | 13 => (5..=7).contains(&col),
        12 | 11 => (4..=8).contains(&col),
        10 | 9 | 3 | 2 | 1 => (3..=9).contains(&col),
        8 | 5 => (2..=10).contains(&col),
        7 => col <= 12, // full beam (wings)
        6 => (1..=11).contains(&col),
        4 | 0 => (4..=8).contains(&col),
        _ => false,
    };
    Hull {
        id: HULL_CORVETTE,
        name: "Corvette".to_string(),
        grid_dims: (13, 15),
        cells: dense_cells((13, 15), &slots, corvette_shape),
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
    // The 1×1 baseline stays a single cell — its one cell is the Thruster slot's module
    // cell at (0,0) (no structural filler on a 1×1 reference chassis). `baseline_fit`
    // must still derive to `Tuning::default()` exactly, so nothing dense is added here.
    let baseline_shape = |col: u16, row: u16| (col, row) == (0, 0);
    Hull {
        id: HULL_BASELINE,
        name: "Baseline".to_string(),
        grid_dims: (1, 1),
        cells: dense_cells((1, 1), &slots, baseline_shape),
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

/// The single [`SectionId`] every **structural** (filler) cell shares on a dense hull
/// (Phase 1A). It sits above the per-slot module-cell section ids (which run
/// `0..slots.len()`), so module cells keep their historical `SectionId(slot_index)`
/// — preserving the E007 armor-gate / kill-timing behavior, which only ever looks up
/// the entry **module** cell's section — while all filler plating groups into one
/// coarse structural section.
///
/// Grouping all filler into one section keeps section growth minimal and fully
/// deterministic. Phase 2 moves destruction to **cell** granularity, so this coarse
/// section grouping is transitional (GDD §5); `shatter_ship` still severs correctly
/// because destroying this section removes all filler at once, isolating the surviving
/// module cells into drifting chunks.
const STRUCTURAL_SECTION: SectionId = SectionId(10_000);

/// Author the **dense filled silhouette** for a hull (Phase 1A): every `(col, row)`
/// inside `shape` becomes a [`GridCell`]. A cell on a slot's `coord` is a **module
/// cell** ([`GridCell::new`], `structural == false`) keyed to that slot's section
/// `SectionId(slot_index)` (the historical per-slot section, so the E007 armor gate is
/// unchanged); every other cell in the silhouette is a **structural** filler cell
/// ([`GridCell::structural`], `structural == true`) in the shared [`STRUCTURAL_SECTION`].
///
/// `shape` is a deterministic predicate `(col, row) -> bool` selecting the silhouette;
/// the caller guarantees it covers **every** slot coord (asserted in tests via
/// `every_slot_sits_on_an_authored_cell_and_ids_are_unique`). Cells are emitted in
/// row-major order so authoring is reproducible (Principle II) — no `HashMap` order.
fn dense_cells(
    grid_dims: (u16, u16),
    slots: &[Slot],
    shape: impl Fn(u16, u16) -> bool,
) -> Vec<GridCell> {
    let (cols, rows) = grid_dims;
    let mut cells = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            if !shape(col, row) {
                continue;
            }
            // A slot sitting on this cell makes it a MODULE cell in that slot's section
            // (the historical SectionId(slot_index)); otherwise it is STRUCTURAL filler.
            match slots.iter().position(|s| s.coord == (col, row)) {
                Some(slot_index) => {
                    cells.push(GridCell::new((col, row), SectionId(slot_index as u32)));
                }
                None => {
                    cells.push(GridCell::structural((col, row), STRUCTURAL_SECTION));
                }
            }
        }
    }
    cells
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
    fn seed_hulls_are_dense_silhouettes_with_module_and_structural_cells() {
        // Phase 1A: each seed hull is authored as a DENSE filled silhouette — every
        // slot coord is a module cell (structural == false), the rest of the shape is
        // structural filler (structural == true). Lock the authored cell counts so the
        // silhouette shapes do not silently change (the Phase 1B renderer reads them).
        let (_, hulls) = seed_catalogs();

        let fighter = hulls.get(HULL_FIGHTER).unwrap();
        // 51-cell fighter silhouette on the 9×11 grid (revise-A, finer fidelity):
        // 1+3+3+5+5+9+7+3+5+5+5 over rows 10..0.
        assert_eq!(
            fighter.cells.len(),
            51,
            "fighter dense silhouette is 51 cells (revise-A finer 9×11)"
        );

        let corvette = hulls.get(HULL_CORVETTE).unwrap();
        // 103-cell corvette silhouette on the 13×15 grid (revise-A, proportionally
        // larger): 3+3+5+5+7+7+9+13+11+9+5+7+7+7+5 over rows 14..0.
        assert_eq!(
            corvette.cells.len(),
            103,
            "corvette dense silhouette is 103 cells (revise-A finer 13×15)"
        );

        for hull in [fighter, corvette] {
            // A module cell sits on each slot coord (structural == false); the count of
            // module cells equals the slot count, and every other cell is structural.
            let module_cells = hull.cells.iter().filter(|c| !c.structural).count();
            let structural_cells = hull.cells.iter().filter(|c| c.structural).count();
            assert_eq!(
                module_cells,
                hull.slots.len(),
                "{}: one module cell per slot",
                hull.name
            );
            assert!(
                structural_cells > 0,
                "{}: the dense body has structural filler",
                hull.name
            );
            // Every NON-structural cell is exactly on a slot coord, and vice-versa.
            for cell in hull.cells.iter().filter(|c| !c.structural) {
                assert!(
                    hull.slots.iter().any(|s| s.coord == cell.coord),
                    "{}: module cell {:?} sits on a slot",
                    hull.name,
                    cell.coord
                );
            }
            // All structural cells share the one transitional structural section.
            for cell in hull.cells.iter().filter(|c| c.structural) {
                assert_eq!(
                    cell.section, STRUCTURAL_SECTION,
                    "{}: structural cells group into STRUCTURAL_SECTION",
                    hull.name
                );
            }
            // No duplicate coords in the dense authoring.
            let mut coords = std::collections::BTreeSet::new();
            for cell in &hull.cells {
                assert!(
                    coords.insert(cell.coord),
                    "{}: duplicate authored cell {:?}",
                    hull.name,
                    cell.coord
                );
            }
        }

        // The 1×1 baseline stays a single (module) cell — no structural filler, so
        // baseline_fit still derives to Tuning::default() exactly.
        let baseline = baseline_hull();
        assert_eq!(baseline.cells.len(), 1, "baseline stays one cell");
        assert!(
            !baseline.cells[0].structural,
            "baseline cell is a module cell"
        );
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
