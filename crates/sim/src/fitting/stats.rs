//! Fit-derived effective stats — the `ShipStats` component that **replaces the
//! global `Tuning`** as the per-ship flight + weapon source (FR-014/015/016/017,
//! ADR-0008, AD-003).
//!
//! [`derive_ship_stats`] turns a [`Fit`] (+ its [`Hull`] and the [`ModuleCatalog`])
//! into a [`ShipStats`] the flight/weapon systems read. The flight-model formulae
//! are **unchanged** — only the source of the numbers moves from a single global
//! resource to a per-entity component derived from the active fit (HINT-002):
//!
//! - thrust / torque / strafe sum from the installed thruster modules;
//! - `total_mass = hull.hull_base_mass + Σ module.mass` (every kind's mass, FR-015);
//! - the flight-feel constants the modules do not supply (`linear_drag`,
//!   `angular_drag`, `angular_inertia`, `turn_power_share`) are **base constants**
//!   sourced from [`Tuning::default`] so `Tuning` is demoted to the base-constant +
//!   seed-baseline source (not deleted);
//! - `can_fire` + the optional [`WeaponProfile`] come from the installed weapon
//!   module(s) (FR-016).
//!
//! **Graceful floors (INV-F07/F14, FR-017)**: every denominator the flight model
//! divides by (`total_mass`, `linear_drag`, `angular_drag`, `angular_inertia`) is
//! strictly `> 0`, and thrust/torque are floored to a small `> 0` when no thruster
//! is fitted — so a crippled fit is *near-immobile but finite*, never `NaN`/`inf`
//! or a divide-by-zero. `total_mass >= hull_base_mass > 0` always (INV-F14).
//!
//! Derive discipline matches the rest of the fitting domain: serde as a
//! replication/persistence seam (E003/E004; not exercised this epic), value
//! semantics so a round-trip compares equal.

use bevy_ecs::component::Component;
use glam::Vec2;
use serde::{Deserialize, Serialize};

use super::content::ModuleCatalog;
use super::fit::Fit;
use super::hull::{Hull, SlotId, CELL_WORLD_SIZE};
use super::layout::{
    layout_com_with, layout_inertia_with, layout_mass_with, CellOccupant, FitLayout,
};
use super::materials::CellMaterials;
use super::module::{Module, ModuleKind, ModuleSpecifics};
use crate::components::{
    ThrusterControls, CTRL_ALL, CTRL_FORWARD, CTRL_REVERSE, CTRL_STRAFE_PORT,
    CTRL_STRAFE_STARBOARD, CTRL_TURN_CCW, CTRL_TURN_CW,
};
use crate::damage::{Channel, StatScalingConfig, DEFAULT_SHIELD_HP, DEFAULT_SHIELD_REGEN};
use crate::tuning::{SimTuning, Tuning};

/// Smallest thrust force a fit can derive to (FR-017): no thruster ⇒ this floor,
/// never `0`, so the ship is near-immobile but `top_speed = floor / linear_drag`
/// stays finite (no divide-by-zero, no `NaN`).
pub const THRUST_FLOOR: f32 = 1.0;
/// Smallest angular drive torque a fit can derive to (FR-017): no thruster ⇒ this
/// floor `> 0`, so `max_turn_rate = floor / angular_drag` stays finite.
pub const TORQUE_FLOOR: f32 = 1.0;
/// Smallest lateral (strafe) thrust a fit can derive to. Floored `> 0` to keep the
/// stat strictly positive and the value semantics simple (FR-017).
pub const STRAFE_FLOOR: f32 = 1.0;
/// Reverse (retro) thrust as a fraction of forward thrust — the retros are weaker
/// than the main drive, so reverse top speed sits below forward (mirrors the E002
/// `Tuning` relationship `reverse_force = thrust_force / 2`).
pub const REVERSE_FRACTION: f32 = 0.5;

/// The weapon fire profile a fitted ship derives from its installed weapon
/// module(s) (FR-016). `None` on a [`ShipStats`] without a weapon module ⇒
/// `can_fire == false` ⇒ `weapon_fire_system` spawns nothing.
///
/// Mirrors the E002 [`Weapon`](crate::components::Weapon) fire params, now
/// fit-sourced: `muzzle_speed`/`fire_rate` from the weapon module's
/// [`ModuleSpecifics::Weapon`], plus the per-shot `damage`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeaponProfile {
    /// Phase C — the damage [`Channel`] this weapon deals (from the weapon module's
    /// `damage_type`). The fire path stamps it onto the projectile so the armor/resistance
    /// system mitigates by the real channel instead of a hardcoded `Kinetic`.
    pub channel: Channel,
    /// Projectile launch speed (`> 0`).
    pub muzzle_speed: f32,
    /// Shots per second (`> 0`).
    pub fire_rate: f32,
    /// Damage per shot (`> 0`).
    pub damage: f32,
    /// Phase M5 — the fired projectile's inertial **mass** (`> 0`): sets the shot's knockback on a
    /// target + the shooter's recoil (`momentum = projectile_mass · muzzle_velocity`). NOT
    /// health-scaled (a slug's mass is a physical property, not a working-condition output).
    pub projectile_mass: f32,
    /// Phase E — **heat generated per shot** (from the weapon module's authored `heat`). Builds the
    /// ship's [`Heat`](crate::components::Heat) pool when firing; not health-scaled (a physical
    /// property, like `projectile_mass`).
    pub heat: f32,
    /// Refinement 18 — the firing weapon cell's BODY-FRAME muzzle offset from the hull grid
    /// centre (row → forward `+x`, col → lateral `+y`, × [`CELL_WORLD_SIZE`]), so the shot spawns
    /// at the actual installed-gun location (rotated by the ship's `Heading`) instead of the ship
    /// centre. `Vec2::ZERO` for a centred/legacy weapon. Render/feel only — does not change the
    /// shot's velocity or damage.
    pub muzzle_offset: Vec2,
    /// R42 — the fired projectile's RADIUS in world units (visual + collision), derived from
    /// `caliber_mm · SimTuning.mm_to_world`. `0` ⇒ the legacy point projectile.
    pub projectile_radius: f32,
    /// R42 — rotary spool-up time (s) to reach full RPM; `0` = instant. The fire system ramps
    /// `Weapon.spool` and gates firing until full (vulcan/gatling wind-up).
    pub spin_up_time: f32,
    /// R42 — shot dispersion half-angle in RADIANS (`dispersion_deg.to_radians()`); `0` = pinpoint.
    /// Applied as deterministic per-shot angular noise (no RNG).
    pub dispersion_rad: f32,
    /// R42 — the projectile's time-to-live (s) = `range_units / muzzle_speed` (per-weapon range),
    /// replacing the global `SimTuning.projectile_lifetime` for fitted shots.
    pub lifetime: f32,
}

/// R42 — the game-space outputs of a weapon design, physics-DERIVED from its real specs
/// (caliber/velocity/rpm) via the [`SimTuning`] scales, with any `Some(..)` per-field override
/// honored. Built by [`derive_weapon`]; the dev panel shows these as read-only readouts and
/// `derive_ship_stats` turns them into the runtime [`WeaponProfile`] (health-scaling `damage`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DerivedWeapon {
    pub muzzle_speed: f32,
    pub fire_rate: f32,
    pub damage: f32,
    pub projectile_mass: f32,
    pub projectile_radius: f32,
    pub dispersion_rad: f32,
    pub spin_up_time: f32,
    pub lifetime: f32,
}

/// R42 — derive a weapon design's game-space outputs from its authored real specs via the
/// [`SimTuning`] physics scales, honoring per-field `Some(..)` overrides. Returns `None` for a
/// non-weapon `ModuleSpecifics`. Pure + deterministic (no RNG). Reused by the dev-panel readouts.
///
/// - `muzzle_speed = muzzle_velocity_ms · velocity_scale`
/// - `fire_rate    = rpm · rpm_scale`
/// - `projectile_radius = caliber_mm · mm_to_world`
/// - `projectile_mass = projectile_density · caliber_mm³`
/// - `damage = ½ · mass · muzzle_velocity_ms² · damage_per_joule`
/// - `lifetime = range_units / muzzle_speed`
pub fn derive_weapon(spec: &ModuleSpecifics, sim: &SimTuning) -> Option<DerivedWeapon> {
    let ModuleSpecifics::Weapon {
        caliber_mm,
        muzzle_velocity_ms,
        rpm,
        spin_up_time,
        dispersion_deg,
        range_units,
        muzzle_speed,
        fire_rate,
        damage,
        projectile_mass,
        ..
    } = *spec
    else {
        return None;
    };
    let projectile_radius = caliber_mm * sim.mm_to_world;
    let mass = projectile_mass.unwrap_or(sim.projectile_density * caliber_mm.powi(3));
    let speed = muzzle_speed.unwrap_or(muzzle_velocity_ms * sim.velocity_scale);
    let rate = fire_rate.unwrap_or(rpm * sim.rpm_scale);
    let dmg = damage
        .unwrap_or(0.5 * mass * muzzle_velocity_ms * muzzle_velocity_ms * sim.damage_per_joule);
    let lifetime = if speed > 0.0 {
        range_units / speed
    } else {
        sim.projectile_lifetime
    };
    Some(DerivedWeapon {
        muzzle_speed: speed,
        fire_rate: rate,
        damage: dmg,
        projectile_mass: mass,
        projectile_radius,
        dispersion_rad: dispersion_deg.to_radians(),
        spin_up_time,
        lifetime,
    })
}

/// R45 — the per-ship list of EVERY alive weapon's runtime [`WeaponProfile`], by `SlotId`. A separate
/// component (not on the `Copy` [`ShipStats`]) so a fitted ship can fire all its guns; the fire system
/// pairs each with its [`WeaponState`](crate::components::WeaponState) +
/// [`FireMapping`](crate::components::FireMapping). Derived by [`derive_weapons`] +
/// `recompute_ship_stats_system`. `ShipStats.weapon` stays = `weapons[0]` (back-compat).
#[derive(Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShipWeapons {
    /// `(SlotId, profile)` for each alive weapon, in slot order (first = `ShipStats.weapon`).
    pub weapons: Vec<(SlotId, WeaponProfile)>,
}

/// R45 — collect EVERY alive weapon module's runtime [`WeaponProfile`] in `SlotId` order (so the
/// first matches `ShipStats.weapon`). Each carries its own `muzzle_offset` (its cell) + physics;
/// `damage` is health-scaled, the rest physical (mirrors the single-weapon derivation). The fitted
/// firing path fires all of these, gated by each weapon's fire group/trigger.
pub fn derive_weapons(
    hull: &Hull,
    fit: &Fit,
    catalog: &ModuleCatalog,
    layout: &FitLayout,
    sim: &SimTuning,
) -> Vec<(SlotId, WeaponProfile)> {
    let cfg = StatScalingConfig::default();
    let mut out = Vec::new();
    for (slot_id, module_id) in fit.assignments.iter() {
        let Some(module) = catalog.get(*module_id) else {
            continue;
        };
        if module.kind != ModuleKind::Weapon {
            continue;
        }
        let ModuleSpecifics::Weapon { damage_type, .. } = module.specifics else {
            continue;
        };
        // The alive layout cell carrying this weapon slot (a carved-off weapon → no cell → skipped).
        let Some(occ) = layout
            .cells
            .values()
            .find(|o| o.slot == *slot_id && o.module.is_some())
        else {
            continue;
        };
        let hf = health_factor(occ, module, &cfg);
        if hf <= 0.0 {
            continue;
        }
        let Some(d) = derive_weapon(&module.specifics, sim) else {
            continue;
        };
        let muzzle_offset = layout
            .cells
            .iter()
            .find(|(_, o)| o.slot == *slot_id && o.module.is_some())
            .map(|(cell, _)| {
                let (cols, rows) = hull.grid_dims;
                Vec2::new(
                    ((cell.1 as f32 + 0.5) - rows as f32 * 0.5) * CELL_WORLD_SIZE,
                    ((cell.0 as f32 + 0.5) - cols as f32 * 0.5) * CELL_WORLD_SIZE,
                )
            })
            .unwrap_or(Vec2::ZERO);
        out.push((
            *slot_id,
            WeaponProfile {
                channel: damage_type,
                muzzle_speed: d.muzzle_speed,
                fire_rate: d.fire_rate,
                damage: d.damage * hf,
                projectile_mass: d.projectile_mass,
                heat: module.heat,
                muzzle_offset,
                projectile_radius: d.projectile_radius,
                spin_up_time: d.spin_up_time,
                dispersion_rad: d.dispersion_rad,
                lifetime: d.lifetime,
            },
        ));
    }
    out
}

/// The fit-derived effective stats for a ship — the per-entity flight + weapon
/// source that **replaces the global `Tuning`** (FR-014, AD-003; data-model.md
/// `ShipStats`). Derived from the ship's [`Fit`] by [`derive_ship_stats`] and
/// recomputed on every fit change (INV-F08); attached to the ship entity as a
/// `bevy_ecs` [`Component`].
///
/// The flight magnitudes are the exact set the flight-model already consumes from
/// `Tuning` (force vs drag, angular inertia, the shared `turn_power_share`); the
/// formulae are unchanged. Every denominator is `> 0` and thrust/torque are
/// floored `> 0` so flight is always finite (INV-F07, FR-017).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShipStats {
    /// FORWARD channel (`>= THRUST_FLOOR > 0`): Σ jet force projected onto +nose (R92).
    pub thrust_force: f32,
    /// REVERSE (retro) channel (`> 0`): Σ retro-facing jet force + the baseline retro authority
    /// (R92 — no retro jets means the baseline is all you have → flip-and-burn).
    pub reverse_force: f32,
    /// STRAFE-PORT channel (`>= STRAFE_FLOOR > 0`): Σ port-facing jet force + baseline (R92).
    pub strafe_port: f32,
    /// STRAFE-STARBOARD channel (`>= STRAFE_FLOOR > 0`): Σ starboard-facing force + baseline (R92).
    pub strafe_starboard: f32,
    /// Total ship mass (`>= hull_base_mass > 0`); `hull_base_mass + Σ module.mass`.
    pub total_mass: f32,
    /// Linear drag coefficient (`> 0`); emergent top speed = `thrust_force / linear_drag`.
    pub linear_drag: f32,
    /// TURN-CCW channel (`>= TORQUE_FLOOR > 0`): Σ max(0, r × F) · lever_scale + baseline (R92 —
    /// torque about the mass CoM, so jet PLACEMENT is the authority).
    pub turn_ccw: f32,
    /// TURN-CW channel (`>= TORQUE_FLOOR > 0`): Σ max(0, −(r × F)) · lever_scale + baseline (R92).
    pub turn_cw: f32,
    /// Angular drag (`> 0`); emergent max turn rate = `turn / angular_drag`.
    pub angular_drag: f32,
    /// Angular inertia / moment (`> 0`); how quickly the turn rate responds. R92 — the REAL layout
    /// moment: `Tuning.angular_inertia + layout_inertia (Σ m·r² about the CoM) · inertia_scale`.
    pub angular_inertia: f32,
    /// Fraction of translational thrust diverted at full turn input (`0..=1`).
    pub turn_power_share: f32,
    /// RUNTIME power generation: `Σ reactor.power_gen` (health-scaled, working core-connected
    /// reactor cells only — no hull base). Feeds `energy_system` recharge + the shield "powered"
    /// gate. (The fitting-INSTALL budget in `validate.rs` is separate and still adds the hull base.)
    pub power_supply: f32,
    /// Power consumed by the fit: `Σ module.power_draw` (`>= 0`).
    pub power_draw: f32,
    /// CPU/control consumed by the fit: `Σ module.cpu_draw` (`>= 0`).
    pub cpu_draw: f32,
    /// Phase F — always-on power load: `Σ power_draw` of Shield/Sensor/Utility modules. The energy
    /// capacitor's net recharge is `power_supply − continuous_draw` (thruster/weapon draw is the
    /// ACTIVE drain, applied separately when you thrust/fire).
    pub continuous_draw: f32,
    /// Phase F — nominal armor capacity: `Σ armor_value` of fitted **Armor** modules. Seeds the
    /// depleting [`crate::components::ArmorHp`] pool's `max` at live ship spawn (a hull-protecting HP
    /// layer between shields and the hull carve). Summed flat (a capacity, not health-scaled).
    pub armor_value: f32,
    /// Shield capacity from the fitted **Shield** generator — `Σ shield_hp · health_factor` (so a
    /// DAMAGED generator degrades the cap and a CARVED-off one zeroes it). Falls back to
    /// [`crate::damage::DEFAULT_SHIELD_HP`] when the fit carries no shield module (matches
    /// `seed_defense_layers`). `recompute_ship_stats_system` syncs the live [`crate::damage::Shields`]
    /// pool's `max` to this, so shields follow the generator's health (Refinement 10).
    pub shield_max: f32,
    /// Shield regen/sec from the fitted **Shield** generator — `Σ regen · health_factor` (degrades
    /// with generator damage). Falls back to [`crate::damage::DEFAULT_SHIELD_REGEN`] with no shield
    /// module. Synced into [`crate::damage::Shields`]`::regen_rate` by `recompute_ship_stats_system`.
    pub shield_regen: f32,
    /// R92 — flat energy-pool capacity from fitted **EnergyStore** modules (capacitors/batteries):
    /// `Σ capacity · health_factor`. Added to `Energy.max`; with a dead reactor the stored charge
    /// persists (regen 0) and drains as used.
    pub energy_store: f32,
    /// R92 — cargo hold volume from fitted **CargoBay** modules: `Σ volume · health_factor`.
    /// v1 = a derived/displayed stat; pickup gameplay consumes it later.
    pub cargo_capacity: f32,
    /// R93 — a **Cockpit** or **FlightComputer** is FITTED (slot assigned, alive or dead). When
    /// `false` (every legacy/golden ship, which fit none) control is UNGATED — full input, today's
    /// behaviour. When `true`, the flags below govern control and a dead control source → derelict.
    pub control_fitted: bool,
    /// R93 — at least one LIVE (`hf > 0`) Cockpit or Flight Computer. `control_fitted && !has_control`
    /// → DERELICT: pilot input ignored, free Newtonian drift (no thrust, no dampers).
    pub has_control: bool,
    /// R93 — a live Flight Computer tier `>= 1` → strafe authority. `control_fitted && !can_strafe`
    /// (cockpit-only) → the strafe input is zeroed (a basic mover: forward + turn only).
    pub can_strafe: bool,
    /// R93 — a live Flight Computer tier `>= 2` → diagonal-direction keys. Derived now; the input
    /// layer (keybinds) + the assist features consume it in a later round.
    pub can_diagonal: bool,
    /// `true` iff at least one weapon module is installed (FR-016). When `false`
    /// the ship cannot fire and [`ShipStats::weapon`] is `None`.
    pub can_fire: bool,
    /// The fit-derived weapon fire profile, or `None` when no weapon is fitted.
    pub weapon: Option<WeaponProfile>,
}

impl ShipStats {
    /// Emergent linear top speed (terminal velocity) = `thrust_force / linear_drag`
    /// — the same closed form the E002 flight model uses (mirrors
    /// [`Tuning::top_speed`]).
    pub fn top_speed(&self) -> f32 {
        self.thrust_force / self.linear_drag
    }

    /// Emergent maximum turn rate (rad/s) = the STRONGER turn channel `/ angular_drag` (mirrors
    /// [`Tuning::max_turn_rate`]; R92 — the channels can be asymmetric).
    pub fn max_turn_rate(&self) -> f32 {
        self.turn_ccw.max(self.turn_cw) / self.angular_drag
    }

    /// INV-F07/F14: every denominator the flight model divides by is strictly
    /// positive and finite, thrust/torque are floored `> 0`, and
    /// `turn_power_share` is a `0..=1` fraction — so derived flight is never
    /// `NaN`/`inf` or a divide-by-zero. The flight-feel guard for a crippled fit.
    pub fn is_finite_and_floored(&self) -> bool {
        self.thrust_force.is_finite()
            && self.thrust_force >= THRUST_FLOOR
            && self.reverse_force.is_finite()
            && self.reverse_force > 0.0
            && self.strafe_port.is_finite()
            && self.strafe_port >= STRAFE_FLOOR
            && self.strafe_starboard.is_finite()
            && self.strafe_starboard >= STRAFE_FLOOR
            && self.total_mass.is_finite()
            && self.total_mass > 0.0
            && self.linear_drag.is_finite()
            && self.linear_drag > 0.0
            && self.turn_ccw.is_finite()
            && self.turn_ccw >= TORQUE_FLOOR
            && self.turn_cw.is_finite()
            && self.turn_cw >= TORQUE_FLOOR
            && self.angular_drag.is_finite()
            && self.angular_drag > 0.0
            && self.angular_inertia.is_finite()
            && self.angular_inertia > 0.0
            && (0.0..=1.0).contains(&self.turn_power_share)
            && self.power_supply.is_finite()
            && self.power_draw.is_finite()
            && self.cpu_draw.is_finite()
            && self.top_speed().is_finite()
            && self.max_turn_rate().is_finite()
    }
}

/// The live contribution factor of one installed module, read from its hit-map
/// occupant (FR-012/013, INV-D13). **Internal to derivation.**
///
/// - A **destroyed** module (`health <= 0`) returns `0.0` — a hard binary-off, so
///   it contributes nothing to any stat (FR-013).
/// - A **damaged-but-alive** module returns `(health / health_max)` clamped to
///   `[0, 1]` and then floored at `cfg.stat_health_floor`, so a barely-alive drive
///   still gives *some* output (a continuous degrade, not a cliff).
/// - At **full** health the factor is exactly `1.0`, so derivation reproduces the
///   pre-E007 contribution bit-for-bit (the baseline guard).
///
/// Pure / total / finite: `health_max > 0` by construction, and the `clamp`+`max`
/// keep the result in `[stat_health_floor, 1]` (or exactly `0` when destroyed) —
/// never `NaN`/`inf` (INV-F07 preserved).
fn health_factor(occupant: &CellOccupant, module: &Module, cfg: &StatScalingConfig) -> f32 {
    if occupant.health <= 0.0 {
        return 0.0; // destroyed = hard binary off (INV-D13)
    }
    (occupant.health / module.health_max)
        .clamp(0.0, 1.0)
        .max(cfg.stat_health_floor) // damaged-but-alive, floored
}

/// Derive a ship's effective [`ShipStats`] from its [`Fit`] (FR-014/015/016/017).
///
/// **Pure** — reads only its arguments, mutates nothing — so the running flight/
/// weapon systems, the fitting UI preview, and a future authoritative server all
/// derive identical stats on the same code path (Principle II). The fit stores
/// ids, so the [`Hull`] and [`ModuleCatalog`] are threaded in to resolve them.
///
/// Derivation (data-model.md "Derivation Rules"):
/// - thrust / torque / strafe = the sum over installed **thruster** modules of
///   the matching [`ModuleSpecifics::Thruster`] field, then floored (`THRUST_FLOOR`
///   / `TORQUE_FLOOR` / `STRAFE_FLOOR`) so no-thruster yields a near-immobile but
///   *finite* ship (FR-017);
/// - `reverse_force = thrust_force * REVERSE_FRACTION`;
/// - `total_mass = hull.hull_base_mass + Σ module.mass` over **every** kind
///   (FR-015, INV-F14) — heavier fit ⇒ lower agility/accel, emergently;
/// - `linear_drag` / `angular_drag` / `angular_inertia` / `turn_power_share` are
///   the base constants from [`Tuning::default`] (the demoted-but-not-deleted
///   flight-feel source) so denominators are always `> 0`;
/// - `power_supply = Σ reactor.power_gen` (runtime generation, reactor cells only — no hull base;
///   the separate fitting-install budget in `validate.rs` still adds `hull.power_capacity`),
///   `power_draw = Σ module.power_draw`, `cpu_draw = Σ module.cpu_draw`;
/// - `can_fire` is `true` iff ≥1 **weapon** module is installed, and `weapon` is
///   that module's [`WeaponProfile`] (the first by deterministic `SlotId` order
///   when several are fitted this epic).
///
/// A dangling [`ModuleId`](super::module::ModuleId) contributes nothing (the
/// dangling-ref *rejection* is `validate_fit`'s concern, INV-F13); derivation
/// stays total and finite regardless.
///
/// **Emergent damage (E007, FR-012/013, INV-D13)**: each module's *output*
/// contribution (thrust/torque/strafe, reactor `power_gen`, weapon `damage`) is
/// scaled by its live [`health_factor`] read from `layout` — a *damaged-but-alive*
/// module scales linearly, clamped to `[stat_health_floor, 1]`; a *destroyed*
/// module (health `0`) contributes exactly `0` (binary off). At **full** module
/// health every factor is `1.0`, so the derivation reproduces the pre-E007
/// numbers bit-for-bit (the baseline-reproduces-`Tuning` guard stays green).
/// **Costs/physical** (`mass`, `power_draw`, `cpu_draw`) are **never** scaled by
/// health — a damaged module still weighs and draws as much (INV-D13). The
/// INV-F07 floors stay **after** scaling, so an all-drives-destroyed ship is
/// near-immobile-but-finite, never `NaN`/`inf`.
pub fn derive_ship_stats(
    hull: &Hull,
    fit: &Fit,
    catalog: &ModuleCatalog,
    layout: &FitLayout,
) -> ShipStats {
    derive_ship_stats_with(
        hull,
        fit,
        catalog,
        layout,
        &SimTuning::default(),
        &CellMaterials::default(),
        None,
    )
}

/// [`derive_ship_stats`] with the live [`SimTuning`] (Phase M6 / R42): the flight `total_mass` uses
/// `sim.struct_cell_mass` instead of the compile-time default, and the weapon profile is physics-
/// derived from the live weapon-physics scales (caliber → size/rate/damage via [`derive_weapon`]).
/// The dev panel's re-derive passes the live resource so editing a scale updates every ship.
#[allow(clippy::too_many_arguments)]
pub fn derive_ship_stats_with(
    hull: &Hull,
    fit: &Fit,
    catalog: &ModuleCatalog,
    layout: &FitLayout,
    sim: &SimTuning,
    materials: &CellMaterials,
    // R94 — the per-thruster control masks (manual allocation via a Control Relay). `None` (every
    // legacy/golden ship + the const wrapper) → all channels enabled = today's full projection.
    thruster_controls: Option<&ThrusterControls>,
) -> ShipStats {
    // Flight-feel constants the modules do not supply come from the demoted
    // `Tuning` baseline (HINT-002): the seed baseline fit reproduces these.
    let base = Tuning::default();
    // The damaged-but-alive contribution floor is a named content reference (not a
    // hardcoded literal), keeping the signature locked while honoring INV-D13.
    let cfg = StatScalingConfig::default();

    // R92 — the six facing-resolved control channels (the "flight computer"): every jet's force is
    // projected per channel below; the SimTuning baselines (the hull's built-in maneuvering jets)
    // seed reverse/strafe/turn so a design with no jets on an axis stays flyable.
    let mut thrust_fwd = 0.0_f32;
    let mut thrust_rev = sim.baseline_reverse_force.max(0.0);
    let mut strafe_port = sim.baseline_strafe_force.max(0.0);
    let mut strafe_starboard = sim.baseline_strafe_force.max(0.0);
    let mut turn_ccw = sim.baseline_turn_torque.max(0.0);
    let mut turn_cw = sim.baseline_turn_torque.max(0.0);
    // R92 — jet torque is about the layout's MASS CoM (cell space; recomputed every re-derive, so a
    // carved hull's shifted CoM updates the lever arms automatically).
    let com = layout_com_with(layout, catalog, sim.struct_cell_mass, materials);
    let mut power_gen = 0.0_f32;
    let mut power_draw = 0.0_f32;
    let mut cpu_draw = 0.0_f32;
    // Phase F: the always-on power load (shields/sensors/utility) that offsets the energy
    // capacitor's regen each tick. Thruster/Weapon draw is NOT counted here — they drain the
    // capacitor when ACTIVE (thrust/fire), so counting their static draw too would double-charge.
    let mut continuous_draw = 0.0_f32;
    // Phase F: nominal armor capacity — Σ over fitted Armor modules (a capacity, summed flat).
    let mut armor_value = 0.0_f32;
    // R93 — control-source flags (opt-in: false unless a Cockpit/FlightComputer is fitted, so every
    // legacy/golden ship that fits none stays fully controllable, today's behaviour).
    let mut control_fitted = false;
    let mut has_control = false;
    let mut can_strafe = false;
    let mut can_diagonal = false;
    // Refinement 10: shield capacity/regen from the fitted Shield generator — health-scaled (a
    // damaged generator degrades, a carved one zeroes). `has_shield_module` distinguishes "no
    // generator fitted" (→ the default fallback, like `seed_defense_layers`) from "generator fitted
    // but carved away" (→ 0, no shields), since a carved module is gone from the layout but its
    // slot ASSIGNMENT remains in the fit.
    let mut shield_max = 0.0_f32;
    let mut shield_regen = 0.0_f32;
    let mut has_shield_module = false;
    // R92 — energy stores (capacitors/batteries) + cargo volume, both health-scaled.
    let mut energy_store = 0.0_f32;
    let mut cargo_capacity = 0.0_f32;
    let mut weapon: Option<WeaponProfile> = None;

    // R94 — health-factor lookup for a slot's installed module (a carved/severed/destroyed cell →
    // `0`); shared by the control-allocator pre-scan + the main loop.
    let hf_of = |slot_id: &SlotId, module: &Module| -> f32 {
        layout
            .cells
            .values()
            .find(|o| o.slot == *slot_id && o.module.is_some())
            .map(|occ| health_factor(occ, module, &cfg))
            .unwrap_or(0.0)
    };

    // R94 — pick the active control allocator BEFORE the thruster projection: a live **Flight
    // Computer** uses the FULL geometric projection (auto); a live **Control Relay** with no FC
    // applies the player's per-thruster channel masks (manual). `use_mask` gates the thruster arm.
    let (mut fc_live, mut relay_live) = (false, false);
    for (slot_id, module_id) in fit.assignments.iter() {
        let Some(m) = catalog.get(*module_id) else {
            continue;
        };
        match m.specifics {
            ModuleSpecifics::FlightComputer { .. } if hf_of(slot_id, m) > 0.0 => fc_live = true,
            ModuleSpecifics::ControlRelay if hf_of(slot_id, m) > 0.0 => relay_live = true,
            _ => {}
        }
    }
    let use_mask = relay_live && !fc_live;

    // Iterate by SlotId (BTreeMap order) so derivation is deterministic — the
    // first *alive* weapon module by slot wins when several are fitted (Principle II).
    for (slot_id, module_id) in fit.assignments.iter() {
        let Some(module) = catalog.get(*module_id) else {
            // Dangling id: no contribution (validate_fit rejects the fit).
            continue;
        };

        // Live health factor for this module, read from the layout's occupied cell
        // (the one carrying this slot + an installed module). With Phase 2 carving, a
        // module cell that has been **carved away or severed off** is no longer in the
        // layout — that means the module is GONE, so a missing cell is treated as
        // **destroyed** (`hf == 0`), exactly as a `health <= 0` cell. So a carved-off
        // weapon drops `can_fire`, a carved-off reactor collapses `power_supply`, etc.
        // (FR-012/013, INV-D13) — the emergent degrade the carving model relies on. A
        // module that has no built cell at all (an impossible state post-`build_layout`,
        // which authors a cell per slot) likewise contributes nothing, keeping
        // derivation total.
        let hf = hf_of(slot_id, module);

        // Universal budget costs apply on every kind and are NOT scaled by health
        // (a damaged module still draws power+cpu, INV-D13). Mass is no longer summed
        // here — Phase M5 derives `total_mass` bottom-up from the layout's cells below.
        power_draw += module.power_draw;
        cpu_draw += module.cpu_draw;
        if matches!(
            module.kind,
            ModuleKind::Shield | ModuleKind::Sensor | ModuleKind::Utility
        ) {
            continuous_draw += module.power_draw;
        }

        // Reactor power generation is an OUTPUT — scaled by health so a destroyed
        // reactor (hf == 0) adds no `power_gen`, collapsing `power_supply` (FR-013).
        power_gen += module.power_gen * hf;

        match module.specifics {
            ModuleSpecifics::Thruster {
                thrust_force: jet_force,
                .. // `propulsion` tag is categorization only — not used in derivation.
            } => {
                // R92 — the facing-resolved "flight computer": this jet's force vector points along
                // its SLOT's authored facing (body frame: +x = nose, +y = port — the muzzle
                // convention), applied AT its cell. Project onto the six channels; torque is the 2D
                // cross `r × F` about the mass CoM (world units), so PLACEMENT is the authority.
                // Health-scales like every thruster output (FR-012).
                let f = jet_force * hf;
                if f > 0.0 {
                    let facing = hull
                        .slots
                        .iter()
                        .find(|sl| sl.id == *slot_id)
                        .map(|sl| sl.facing)
                        .unwrap_or(0.0);
                    let dir = Vec2::from_angle(facing);
                    // R94 — the per-thruster control MASK (manual allocation): bites only when a
                    // Control Relay is the live allocator (no FC). Otherwise `CTRL_ALL` → every
                    // channel admitted = the full geometric projection (today's behaviour).
                    let mask = if use_mask {
                        thruster_controls.map_or(CTRL_ALL, |tc| tc.for_slot(*slot_id))
                    } else {
                        CTRL_ALL
                    };
                    if mask & CTRL_FORWARD != 0 {
                        thrust_fwd += f * dir.x.max(0.0);
                    }
                    if mask & CTRL_REVERSE != 0 {
                        thrust_rev += f * (-dir.x).max(0.0);
                    }
                    if mask & CTRL_STRAFE_PORT != 0 {
                        strafe_port += f * dir.y.max(0.0);
                    }
                    if mask & CTRL_STRAFE_STARBOARD != 0 {
                        strafe_starboard += f * (-dir.y).max(0.0);
                    }
                    // The jet's body-frame position relative to the CoM (cell space → body axes:
                    // x = row − com.y, y = col − com.x; × CELL_WORLD_SIZE → world units).
                    if let Some((cell, occ)) = layout
                        .cells
                        .iter()
                        .find(|(_, o)| o.slot == *slot_id && o.module.is_some())
                    {
                        let centroid = occ.shape.centroid(cell.0, cell.1);
                        let r = Vec2::new(centroid.y - com.y, centroid.x - com.x) * CELL_WORLD_SIZE;
                        let tau = (r.x * dir.y - r.y * dir.x) * f * sim.thruster_lever_scale;
                        if mask & CTRL_TURN_CCW != 0 {
                            turn_ccw += tau.max(0.0);
                        }
                        if mask & CTRL_TURN_CW != 0 {
                            turn_cw += (-tau).max(0.0);
                        }
                    }
                }
            }
            // First *alive* weapon module by deterministic slot order populates the
            // fire profile (FR-013/016): a destroyed weapon (hf == 0) is skipped so
            // `can_fire` stays false; the surviving profile's `damage` scales by hf
            // (at full health hf == 1.0 → identical, baseline-preserving).
            ModuleSpecifics::Weapon { damage_type, .. }
                if module.kind == ModuleKind::Weapon && weapon.is_none() && hf > 0.0 =>
            {
                // Refinement 18 — the firing weapon's grid cell → BODY-FRAME muzzle offset so the
                // shot spawns at the installed gun, not the ship centre. Convention matches the
                // client hull mesh: row → forward (`+x`), col → lateral (`+y`), measured from the
                // grid centre `(cols·0.5, rows·0.5)`. The cell is the alive layout cell carrying
                // this weapon slot (the same one `hf` was read from).
                let muzzle_offset = layout
                    .cells
                    .iter()
                    .find(|(_, o)| o.slot == *slot_id && o.module.is_some())
                    .map(|(cell, _)| {
                        let (cols, rows) = hull.grid_dims;
                        Vec2::new(
                            ((cell.1 as f32 + 0.5) - rows as f32 * 0.5) * CELL_WORLD_SIZE,
                            ((cell.0 as f32 + 0.5) - cols as f32 * 0.5) * CELL_WORLD_SIZE,
                        )
                    })
                    .unwrap_or(Vec2::ZERO);
                // R42 — physics-derive the game-space outputs (size/rate/damage/mass/spin/range) from
                // the weapon's real specs via the live `SimTuning` scales; only `damage` is
                // health-scaled (a battered gun hits softer); size/mass/spin/dispersion/range are
                // physical properties, like the slug mass + per-shot heat below.
                let d = derive_weapon(&module.specifics, sim)
                    .expect("weapon-kind module has Weapon specifics");
                weapon = Some(WeaponProfile {
                    channel: damage_type,
                    muzzle_speed: d.muzzle_speed,
                    fire_rate: d.fire_rate,
                    damage: d.damage * hf,
                    projectile_mass: d.projectile_mass,
                    heat: module.heat,
                    muzzle_offset,
                    projectile_radius: d.projectile_radius,
                    spin_up_time: d.spin_up_time,
                    dispersion_rad: d.dispersion_rad,
                    lifetime: d.lifetime,
                });
            }
            // Armor plates contribute their nominal `armor_value` to the depleting ArmorHp pool's
            // capacity. Summed flat (a capacity stat, like power_supply — not health-scaled).
            ModuleSpecifics::Armor { armor_value: av } => {
                armor_value += av;
            }
            // Shield generator: shield capacity + regen, HEALTH-SCALED (a battered generator gives
            // less, a destroyed/carved one — `hf == 0` — gives none → the live `Shields` pool the
            // sync drives drops to 0). `has_shield_module` is set even when `hf == 0` so a carved
            // generator does NOT fall back to the default pool below.
            ModuleSpecifics::Shield { shield_hp, regen } => {
                has_shield_module = true;
                shield_max += shield_hp * hf;
                shield_regen += regen * hf;
            }
            // R92 — energy storage (capacitor/battery): flat pool capacity, health-scaled (a shot-up
            // battery holds less; a carved-off one is gone).
            ModuleSpecifics::EnergyStore { capacity } => {
                energy_store += capacity * hf;
            }
            // R92 — cargo hold volume, health-scaled.
            ModuleSpecifics::CargoBay { volume } => {
                cargo_capacity += volume * hf;
            }
            // R93 — a fitted Cockpit opts the ship into the control-source model; alive → a live
            // control source (basic fly + turn).
            ModuleSpecifics::Cockpit => {
                control_fitted = true;
                if hf > 0.0 {
                    has_control = true;
                }
            }
            // R93 — a fitted Flight Computer: also a control source (automated brain), and its live
            // tier grants strafe (≥1) / diagonal (≥2).
            ModuleSpecifics::FlightComputer { tier } => {
                control_fitted = true;
                if hf > 0.0 {
                    has_control = true;
                    if tier >= 1 {
                        can_strafe = true;
                    }
                    if tier >= 2 {
                        can_diagonal = true;
                    }
                }
            }
            // R93 — a reaction wheel / CMG: placement-FREE torque to BOTH turn channels (no lever
            // arm, no thrust/strafe), health-scaled.
            ModuleSpecifics::ReactionWheel { torque } => {
                let t = torque * hf;
                turn_ccw += t;
                turn_cw += t;
            }
            // R94 — a Control Relay: the MANUAL allocator. A control source that unlocks strafe; the
            // per-thruster masks it enables are applied in the thruster arm above (gated on `use_mask`).
            ModuleSpecifics::ControlRelay => {
                control_fitted = true;
                if hf > 0.0 {
                    has_control = true;
                    can_strafe = true;
                }
            }
            _ => {}
        }
    }

    // No shield generator fitted at all → the small default pool (matches `seed_defense_layers`,
    // keeping default-shield ships — e.g. the demo enemy — byte-identical). A fitted-but-carved
    // generator keeps `has_shield_module == true`, so it stays at the health-scaled 0.
    if !has_shield_module {
        shield_max = DEFAULT_SHIELD_HP;
        shield_regen = DEFAULT_SHIELD_REGEN;
    }

    // Graceful floors (FR-017, INV-F07): never zero a denominator or a drive (per channel, R92).
    let thrust_force = thrust_fwd.max(THRUST_FLOOR);
    let reverse_force = thrust_rev.max(THRUST_FLOOR * REVERSE_FRACTION);
    let strafe_port = strafe_port.max(STRAFE_FLOOR);
    let strafe_starboard = strafe_starboard.max(STRAFE_FLOOR);
    let turn_ccw = turn_ccw.max(TORQUE_FLOOR);
    let turn_cw = turn_cw.max(TORQUE_FLOOR);
    // Phase M5 — **mass is the sum of the body's cells** ([`layout_mass`]): each module cell
    // weighs its module's mass, each structural cell `STRUCT_CELL_MASS`. This is the SAME mass
    // basis the projectile-impulse + wreck drift use (`fitted_damage_system`), so a ship's mass is
    // continuous as it erodes into a wreck. (The authored `hull.hull_base_mass` is no longer part
    // of the flight mass — it remains only the fitting-screen mass-**budget** axis.) Floored `> 0`
    // (INV-F14) — a no-cell layout never zeroes the flight denominator.
    let total_mass =
        layout_mass_with(layout, catalog, sim.struct_cell_mass, materials).max(f32::MIN_POSITIVE);

    // R92 — the REAL moment of inertia: the legacy base responsiveness + the layout's Σ m·r² about
    // the CoM (so spread-out mass turns sluggishly), live-scaled by `thruster_inertia_scale`.
    let angular_inertia = base.angular_inertia
        + layout_inertia_with(layout, catalog, sim.struct_cell_mass, materials)
            * sim.thruster_inertia_scale.max(0.0);

    ShipStats {
        thrust_force,
        reverse_force,
        strafe_port,
        strafe_starboard,
        total_mass,
        linear_drag: base.linear_drag,
        turn_ccw,
        turn_cw,
        angular_drag: base.angular_drag,
        angular_inertia,
        turn_power_share: base.turn_power_share,
        // Refinement 20: RUNTIME power generation comes ONLY from the working, core-connected
        // reactor cells (`power_gen`, already health-scaled + dropped for any reactor cell carved
        // away or severed off the layout). The hull contributes NO free base here — a ship with no
        // working reactor generates 0, so `energy_system` drains its pool and the shield "powered"
        // gate goes false. (`hull.power_capacity` is unchanged and still feeds the SEPARATE
        // fitting-install budget in `validate.rs`; this is runtime generation only.)
        power_supply: power_gen,
        power_draw,
        cpu_draw,
        continuous_draw,
        armor_value,
        shield_max,
        shield_regen,
        energy_store,
        cargo_capacity,
        control_fitted,
        has_control,
        can_strafe,
        can_diagonal,
        can_fire: weapon.is_some(),
        weapon,
    }
}

/// One module TYPE's aggregate condition for the HUD (Refinement 14): the live condition of every
/// installed module of this [`ModuleKind`], in deterministic slot order. `modules[i]` is module i's
/// condition fraction `(health / health_max).clamp(0,1)` — `0.0` = destroyed (or carved away);
/// `sum_health / sum_health_max` is the aggregate %. `modules.len()` is the authored count; the
/// destroyed count is `modules.iter().filter(|&&f| f <= 0.0).count()`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleCondition {
    /// The module type this row aggregates.
    pub kind: ModuleKind,
    /// Each installed module's condition fraction (`0.0` destroyed … `1.0` full), slot order.
    pub modules: Vec<f32>,
    /// Σ live health across this type's modules.
    pub sum_health: f32,
    /// Σ max health across this type's modules (the aggregate denominator).
    pub sum_health_max: f32,
}

/// A `ModuleKind`'s fixed display/bucket order so the HUD rows are stable (Refinement 14).
fn kind_order(k: ModuleKind) -> usize {
    match k {
        ModuleKind::Reactor => 0,
        ModuleKind::Thruster => 1,
        ModuleKind::Weapon => 2,
        ModuleKind::Shield => 3,
        ModuleKind::Armor => 4,
        ModuleKind::Sensor => 5,
        ModuleKind::Utility => 6,
    }
}

/// Per-module-TYPE condition for a fitted ship (Refinement 14) — for each [`ModuleKind`] present in
/// the fit, the live condition of every installed module of that type, plus the summed live/max
/// health for the aggregate bar. Reuses the SAME per-cell health lookup as [`derive_ship_stats`]
/// (iterate `fit.assignments`, find the module's live cell in `layout`, a carved-away cell = `0` =
/// destroyed), so the HUD bars always agree with the emergent stat degrade. Returns one entry per
/// PRESENT kind in [`kind_order`] order. **Pure** — reads only its arguments; no system / no
/// schedule / no RNG, so it is determinism-neutral.
pub fn module_conditions(
    fit: &Fit,
    layout: &FitLayout,
    catalog: &ModuleCatalog,
) -> Vec<ModuleCondition> {
    let mut buckets: [Option<ModuleCondition>; 7] = Default::default();
    for (slot_id, module_id) in fit.assignments.iter() {
        let Some(module) = catalog.get(*module_id) else {
            continue; // dangling id (validate_fit rejects) — no contribution
        };
        // Live health of this module's cell; a carved-away / severed cell is gone from the layout →
        // destroyed (0), exactly as `derive_ship_stats` treats it.
        let health = layout
            .cells
            .values()
            .find(|o| o.slot == *slot_id && o.module.is_some())
            .map(|o| o.health.max(0.0))
            .unwrap_or(0.0);
        let frac = if module.health_max > 0.0 {
            (health / module.health_max).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let entry = buckets[kind_order(module.kind)].get_or_insert_with(|| ModuleCondition {
            kind: module.kind,
            modules: Vec::new(),
            sum_health: 0.0,
            sum_health_max: 0.0,
        });
        entry.modules.push(frac);
        entry.sum_health += health;
        entry.sum_health_max += module.health_max;
    }
    buckets.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::content::{baseline_fit, baseline_hull, seed_catalogs};
    use crate::fitting::layout::build_layout;

    #[test]
    fn baseline_fit_reproduces_tuning_defaults() {
        // HINT-002 / T019 unit-level guard: the baseline seed fit derives to the
        // exact `Tuning::default()` flight magnitudes (the integration test in
        // tests/fitting.rs is the cross-crate guard).
        let (modules, _) = seed_catalogs();
        let hull = baseline_hull();
        let fit = baseline_fit();
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        let t = Tuning::default();
        assert!((stats.thrust_force - t.thrust_force).abs() < 1e-4);
        // R92 — reverse/strafe/turn come from the SimTuning BASELINES (the baseline hull is a single
        // module cell → zero lever arm, zero extra inertia), whose defaults equal the legacy Tuning
        // trio — so the flight-feel guard still holds exactly.
        assert!((stats.reverse_force - t.reverse_force).abs() < 1e-4);
        assert!((stats.strafe_port - t.strafe_force).abs() < 1e-4);
        assert!((stats.strafe_starboard - t.strafe_force).abs() < 1e-4);
        assert!((stats.total_mass - t.mass).abs() < 1e-4);
        assert!((stats.turn_ccw - t.turn_torque).abs() < 1e-4);
        assert!((stats.turn_cw - t.turn_torque).abs() < 1e-4);
        assert_eq!(stats.linear_drag, t.linear_drag);
        assert_eq!(stats.angular_drag, t.angular_drag);
        // R92 — the single-cell baseline layout has ~zero moment about its own centroid, so the
        // derived inertia collapses to the legacy base constant (within f32).
        assert!((stats.angular_inertia - t.angular_inertia).abs() < 1e-4);
        assert_eq!(stats.turn_power_share, t.turn_power_share);
        assert!((stats.top_speed() - t.top_speed()).abs() < 1e-3);
        assert!((stats.max_turn_rate() - t.max_turn_rate()).abs() < 1e-3);
    }

    #[test]
    fn shield_generator_health_drives_the_shield_cap() {
        // Refinement 10: shields come from the fitted Shield generator, HEALTH-SCALED — a damaged
        // generator degrades the cap and a carved-off one zeroes it; no generator → the small
        // default pool (matching `seed_defense_layers`, so default-shield ships are unchanged).
        use crate::fitting::content::{HULL_FIGHTER, MODULE_SHIELD_BASIC};
        use crate::fitting::SlotId;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

        // No shield module → the default fallback pool (byte-identical to the old behaviour).
        let bare = Fit::new(hull.id);
        let bare_layout = build_layout(&hull, &bare, &modules);
        let bare_stats = derive_ship_stats(&hull, &bare, &modules, &bare_layout);
        assert_eq!(bare_stats.shield_max, DEFAULT_SHIELD_HP);
        assert_eq!(bare_stats.shield_regen, DEFAULT_SHIELD_REGEN);

        // A full-health generator (slot 6 is the fighter's Shield hardpoint) → its full cap/regen.
        let mut fit = Fit::new(hull.id);
        fit.install_module(SlotId(6), MODULE_SHIELD_BASIC, &hull, &modules)
            .unwrap();
        let mut layout = build_layout(&hull, &fit, &modules);
        let full = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!(
            (full.shield_max - 60.0).abs() < 1e-4,
            "full generator → 60 cap, got {}",
            full.shield_max
        );
        assert!((full.shield_regen - 5.0).abs() < 1e-4);

        // The generator's cell, halved → ~half the cap (continuous degrade, not a cliff).
        let shield_cell = *layout
            .cells
            .iter()
            .find(|(_, o)| o.module == Some(MODULE_SHIELD_BASIC))
            .map(|(c, _)| c)
            .expect("the installed shield generator has a cell");
        let hp_max = modules.get(MODULE_SHIELD_BASIC).unwrap().health_max;
        layout.cells.get_mut(&shield_cell).unwrap().health = hp_max * 0.5;
        let half = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!(
            (half.shield_max - 30.0).abs() < 1.0,
            "half-health generator → ~30 cap, got {}",
            half.shield_max
        );

        // Carved away (the cell is gone from the layout, but the slot ASSIGNMENT remains in the
        // fit) → NO shields (it does NOT fall back to the default pool).
        layout.cells.remove(&shield_cell);
        let carved = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert_eq!(
            carved.shield_max, 0.0,
            "a carved-off generator gives no shields"
        );
        assert_eq!(carved.shield_regen, 0.0);
    }

    #[test]
    fn module_conditions_aggregate_per_kind_and_count_destroyed() {
        // Refinement 14: per-module-type condition — a carved module reads 0 (destroyed) and drops
        // its kind's aggregate, while the slot count stays (so "1 of 2 destroyed" is legible).
        use crate::fitting::content::{HULL_FIGHTER, MODULE_THRUSTER_BASIC};
        use crate::fitting::SlotId;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        // The fighter has TWO thruster hardpoints (slots 1 + 2) — fit both.
        let mut fit = Fit::new(hull.id);
        fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();
        fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules)
            .unwrap();
        let mut layout = build_layout(&hull, &fit, &modules);

        // Full health: the Thruster row has two full modules → aggregate 1.0.
        let full = module_conditions(&fit, &layout, &modules);
        let thr = full
            .iter()
            .find(|c| matches!(c.kind, ModuleKind::Thruster))
            .expect("a thruster row");
        assert_eq!(thr.modules.len(), 2, "two thrusters fitted");
        assert!(thr.modules.iter().all(|&f| (f - 1.0).abs() < 1e-4));
        assert!((thr.sum_health / thr.sum_health_max - 1.0).abs() < 1e-4);

        // Carve ONE thruster cell away → it reads 0 (destroyed); the row stays count 2, aggregate 0.5.
        let thruster_cell = *layout
            .cells
            .iter()
            .find(|(_, o)| o.slot == SlotId(2) && o.module == Some(MODULE_THRUSTER_BASIC))
            .map(|(c, _)| c)
            .expect("the 2nd thruster has a cell");
        layout.cells.remove(&thruster_cell);
        let carved = module_conditions(&fit, &layout, &modules);
        let thr2 = carved
            .iter()
            .find(|c| matches!(c.kind, ModuleKind::Thruster))
            .expect("a thruster row");
        assert_eq!(
            thr2.modules.len(),
            2,
            "the destroyed thruster's slot still counts"
        );
        assert_eq!(
            thr2.modules.iter().filter(|&&f| f <= 0.0).count(),
            1,
            "exactly one thruster is destroyed"
        );
        assert!(
            (thr2.sum_health / thr2.sum_health_max - 0.5).abs() < 1e-4,
            "the aggregate halves to 50% (got {})",
            thr2.sum_health / thr2.sum_health_max
        );
    }

    #[test]
    fn crippled_fit_is_finite_and_floored() {
        // INV-F07: an empty fit (no thruster, no reactor) still yields finite,
        // floored stats — near-immobile, never NaN/inf or divide-by-zero.
        let (modules, hulls) = seed_catalogs();
        let hull = hulls
            .get(crate::fitting::content::HULL_FIGHTER)
            .unwrap()
            .clone();
        let fit = Fit::new(hull.id);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!(stats.is_finite_and_floored());
        assert_eq!(stats.thrust_force, THRUST_FLOOR);
        // R92 — no jets: forward collapses to the floor, but the hull's BUILT-IN baseline
        // maneuvering authority (SimTuning) keeps the turn channels alive (flyable, not mobile).
        let sim = SimTuning::default();
        assert_eq!(stats.turn_ccw, sim.baseline_turn_torque);
        assert_eq!(stats.turn_cw, sim.baseline_turn_torque);
        assert!(!stats.can_fire);
        assert!(stats.weapon.is_none());
    }

    /// R92 — a jet OFF the CoM axis feeds exactly one turn channel: placement IS the authority
    /// (the channels go asymmetric), and the lever contribution sits on top of the baseline.
    #[test]
    fn jet_placement_drives_the_turn_channels() {
        use crate::fitting::content::{HULL_FIGHTER, MODULE_THRUSTER_BASIC};
        use crate::fitting::SlotId;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let mut fit = Fit::new(hull.id);
        // The fighter's thruster slot sits a column off the centreline → a real lever arm.
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        let sim = SimTuning::default();
        // One off-axis forward jet → torque flows into ONE direction only; the other side stays at
        // the baseline. Asymmetric authority is the point.
        let max = stats.turn_ccw.max(stats.turn_cw);
        let min = stats.turn_ccw.min(stats.turn_cw);
        assert!(
            max > sim.baseline_turn_torque + 1e-3,
            "the off-axis jet must add lever torque on one side (ccw {} cw {})",
            stats.turn_ccw,
            stats.turn_cw
        );
        assert!((min - sim.baseline_turn_torque).abs() < 1e-3);
    }

    /// R92 — facing resolves the jet's channel: rotate the same thruster's slot to face AFT and its
    /// force moves from the forward channel to the reverse channel.
    #[test]
    fn retro_facing_jet_fills_the_reverse_channel() {
        use crate::fitting::content::{HULL_FIGHTER, MODULE_THRUSTER_BASIC};
        use crate::fitting::SlotId;
        let (modules, hulls) = seed_catalogs();
        let mut hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let slot = hull
            .slots
            .iter_mut()
            .find(|sl| sl.id == SlotId(1))
            .expect("fighter thruster slot");
        slot.facing = std::f32::consts::PI; // retro: thrust pushes AFT
        let mut fit = Fit::new(hull.id);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        let sim = SimTuning::default();
        assert_eq!(stats.thrust_force, THRUST_FLOOR, "no forward jets → floor");
        assert!(
            stats.reverse_force > sim.baseline_reverse_force + 1e-3,
            "the retro jet must add to the reverse channel ({})",
            stats.reverse_force
        );
    }

    /// R92 — a real multi-cell layout carries a real moment of inertia: the fighter's derived
    /// `angular_inertia` exceeds the legacy base constant (mass spread out turns slower), while the
    /// single-cell baseline collapses to the base (asserted in `baseline_fit_reproduces_…`).
    #[test]
    fn spread_mass_raises_angular_inertia() {
        use crate::fitting::content::HULL_FIGHTER;
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let fit = Fit::new(hull.id);
        let layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!(stats.angular_inertia > Tuning::default().angular_inertia + 1e-3);
    }

    /// R92 — EnergyStore + CargoBay derive health-scaled capacity: full at full health, GONE when
    /// the module's cell is carved off the layout.
    #[test]
    fn energy_store_and_cargo_derive_health_scaled() {
        use crate::fitting::content::HULL_FIGHTER;
        use crate::fitting::{HardpointType, ModuleId, Slot, SlotId, SlotSize};
        let (modules, hulls) = seed_catalogs();
        let mut hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        // Author two Utility slots on existing STRUCTURAL cells (the editor's normal path) — picked
        // from the hull data so the test is silhouette-agnostic.
        let taken: Vec<(u16, u16)> = hull.slots.iter().map(|s| s.coord).collect();
        let mut free = hull
            .cells
            .iter()
            .map(|c| c.coord)
            .filter(|c| !taken.contains(c));
        let c1 = free.next().expect("a free structural cell");
        let c2 = free.next().expect("a second free structural cell");
        // A slot cell is non-structural (the editor's `set_cell_module` flips this when placing).
        for cell in hull.cells.iter_mut() {
            if cell.coord == c1 || cell.coord == c2 {
                cell.structural = false;
            }
        }
        hull.slots.push(Slot {
            id: SlotId(98),
            slot_type: HardpointType::Utility,
            size: SlotSize::Small,
            coord: c1,
            facing: 0.0,
            is_weapon_mount: false,
        });
        hull.slots.push(Slot {
            id: SlotId(99),
            slot_type: HardpointType::Utility,
            size: SlotSize::Small,
            coord: c2,
            facing: 0.0,
            is_weapon_mount: false,
        });
        let mut fit = Fit::new(hull.id);
        fit.install_raw(SlotId(98), ModuleId(25)); // Battery Bank (capacity 80)
        fit.install_raw(SlotId(99), ModuleId(26)); // Cargo Bay (volume 50)
        let mut layout = build_layout(&hull, &fit, &modules);
        let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert!((stats.energy_store - 80.0).abs() < 1e-3);
        assert!((stats.cargo_capacity - 50.0).abs() < 1e-3);
        // Carve the battery's cell off → its stored capacity is gone; the cargo bay survives.
        layout.cells.retain(|coord, _| *coord != c1);
        let carved = derive_ship_stats(&hull, &fit, &modules, &layout);
        assert_eq!(carved.energy_store, 0.0);
        assert!((carved.cargo_capacity - 50.0).abs() < 1e-3);
    }

    /// R93 test helper — author one Utility slot per `(slot, module)` on a free structural cell of
    /// the fighter and fit that module there. Returns the catalog + hull + fit + layout + the slot
    /// coords (for carving).
    #[allow(clippy::type_complexity)]
    fn fighter_with_utilities(
        mods: &[(crate::fitting::SlotId, crate::fitting::ModuleId)],
    ) -> (ModuleCatalog, Hull, Fit, FitLayout, Vec<(u16, u16)>) {
        use crate::fitting::content::HULL_FIGHTER;
        use crate::fitting::{HardpointType, Slot, SlotSize};
        let (modules, hulls) = seed_catalogs();
        let mut hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let taken: Vec<(u16, u16)> = hull.slots.iter().map(|s| s.coord).collect();
        let free: Vec<(u16, u16)> = hull
            .cells
            .iter()
            .map(|c| c.coord)
            .filter(|c| !taken.contains(c))
            .collect();
        let mut fit = Fit::new(hull.id);
        let mut coords = Vec::new();
        for (i, (slot, module)) in mods.iter().enumerate() {
            let c = free[i];
            for cell in hull.cells.iter_mut() {
                if cell.coord == c {
                    cell.structural = false;
                }
            }
            hull.slots.push(Slot {
                id: *slot,
                slot_type: HardpointType::Utility,
                size: SlotSize::Small,
                coord: c,
                facing: 0.0,
                is_weapon_mount: false,
            });
            fit.install_raw(*slot, *module);
            coords.push(c);
        }
        let layout = build_layout(&hull, &fit, &modules);
        (modules, hull, fit, layout, coords)
    }

    /// R93 — a reaction wheel adds its torque to BOTH turn channels, regardless of placement (no
    /// lever arm), health-scaled.
    #[test]
    fn reaction_wheel_adds_placement_free_turn_authority() {
        use crate::fitting::{ModuleId, SlotId};
        let (m0, h0, f0, l0, _) = fighter_with_utilities(&[]);
        let base = derive_ship_stats(&h0, &f0, &m0, &l0);
        let (m1, h1, f1, l1, _) = fighter_with_utilities(&[(SlotId(98), ModuleId(30))]); // Reaction Wheel (8)
        let w = derive_ship_stats(&h1, &f1, &m1, &l1);
        assert!(
            (w.turn_ccw - base.turn_ccw - 8.0).abs() < 1e-3,
            "ccw += torque"
        );
        assert!(
            (w.turn_cw - base.turn_cw - 8.0).abs() < 1e-3,
            "cw += torque"
        );
    }

    /// R93 — Cockpit/Flight Computer set the control flags; a bare ship (no control module) stays
    /// UNGATED (`control_fitted == false`) — backward-compat with every legacy/golden ship.
    #[test]
    fn cockpit_and_flight_computer_set_control_flags() {
        use crate::fitting::{ModuleId, SlotId};
        let (m0, h0, f0, l0, _) = fighter_with_utilities(&[]);
        let bare = derive_ship_stats(&h0, &f0, &m0, &l0);
        assert!(
            !bare.control_fitted,
            "no control module → ungated (legacy default)"
        );

        let (m1, h1, f1, l1, _) = fighter_with_utilities(&[(SlotId(98), ModuleId(27))]); // Cockpit
        let c = derive_ship_stats(&h1, &f1, &m1, &l1);
        assert!(c.control_fitted && c.has_control && !c.can_strafe && !c.can_diagonal);

        let (m2, h2, f2, l2, _) = fighter_with_utilities(&[(SlotId(98), ModuleId(28))]); // FC I
        let fc1 = derive_ship_stats(&h2, &f2, &m2, &l2);
        assert!(fc1.control_fitted && fc1.has_control && fc1.can_strafe && !fc1.can_diagonal);

        let (m3, h3, f3, l3, _) = fighter_with_utilities(&[(SlotId(98), ModuleId(29))]); // FC II
        let fc2 = derive_ship_stats(&h3, &f3, &m3, &l3);
        assert!(fc2.control_fitted && fc2.has_control && fc2.can_strafe && fc2.can_diagonal);
    }

    /// R93 — a cockpit-fitted ship whose cockpit cell is carved off keeps `control_fitted` (the slot
    /// is still assigned) but loses `has_control` → DERELICT.
    #[test]
    fn carved_cockpit_makes_the_ship_derelict() {
        use crate::fitting::{ModuleId, SlotId};
        let (m, h, f, mut l, coords) = fighter_with_utilities(&[(SlotId(98), ModuleId(27))]);
        let alive = derive_ship_stats(&h, &f, &m, &l);
        assert!(alive.control_fitted && alive.has_control);
        l.cells.retain(|coord, _| *coord != coords[0]); // carve the cockpit cell
        let dead = derive_ship_stats(&h, &f, &m, &l);
        assert!(
            dead.control_fitted && !dead.has_control,
            "fitted a cockpit but no live control source survives → derelict"
        );
    }

    /// R94 — a Control Relay is a control source that unlocks strafe (the manual allocator).
    #[test]
    fn control_relay_is_a_control_source_that_unlocks_strafe() {
        use crate::fitting::{ModuleId, SlotId};
        let (m, h, f, l, _) = fighter_with_utilities(&[(SlotId(98), ModuleId(31))]); // Control Relay
        let s = derive_ship_stats(&h, &f, &m, &l);
        assert!(s.control_fitted && s.has_control && s.can_strafe && !s.can_diagonal);
    }

    /// R94 test helper — a fighter with one OFF-AXIS thruster (facing 45° → feeds forward + strafe-port
    /// + a little turn) and an optional control-allocator module fitted to a Utility slot.
    fn fighter_thruster_and_allocator(
        allocator: Option<crate::fitting::ModuleId>,
    ) -> (ModuleCatalog, Hull, Fit, FitLayout) {
        use crate::fitting::content::{HULL_FIGHTER, MODULE_THRUSTER_BASIC};
        use crate::fitting::{HardpointType, Slot, SlotId, SlotSize};
        let (modules, hulls) = seed_catalogs();
        let mut hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        if let Some(sl) = hull.slots.iter_mut().find(|s| s.id == SlotId(1)) {
            sl.facing = std::f32::consts::FRAC_PI_4;
        }
        let mut fit = Fit::new(hull.id);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        if let Some(alloc) = allocator {
            let taken: Vec<(u16, u16)> = hull.slots.iter().map(|s| s.coord).collect();
            let c = hull
                .cells
                .iter()
                .map(|c| c.coord)
                .find(|c| !taken.contains(c))
                .expect("a free structural cell");
            for cell in hull.cells.iter_mut() {
                if cell.coord == c {
                    cell.structural = false;
                }
            }
            hull.slots.push(Slot {
                id: SlotId(98),
                slot_type: HardpointType::Utility,
                size: SlotSize::Small,
                coord: c,
                facing: 0.0,
                is_weapon_mount: false,
            });
            fit.install_raw(SlotId(98), alloc);
        }
        let layout = build_layout(&hull, &fit, &modules);
        (modules, hull, fit, layout)
    }

    /// R94 — the per-thruster mask routes channels on a RELAY ship (manual), but a Flight Computer
    /// OVERRIDES it (auto = full projection).
    #[test]
    fn relay_mask_routes_thruster_channels_but_an_fc_overrides() {
        use crate::fitting::{ModuleId, SlotId};
        let sim = SimTuning::default();
        let mut tc = ThrusterControls::default();
        tc.mask.insert(SlotId(1), CTRL_FORWARD); // the off-axis jet allowed for FORWARD only

        // No allocator, no mask → full projection: the off-axis jet feeds strafe-port.
        let (m0, h0, f0, l0) = fighter_thruster_and_allocator(None);
        let base =
            derive_ship_stats_with(&h0, &f0, &m0, &l0, &sim, &CellMaterials::default(), None);
        assert!(
            base.strafe_port > sim.baseline_strafe_force + 1e-3,
            "with no mask the jet feeds strafe"
        );

        // Relay + forward-only mask → the jet is removed from strafe (mask bites), forward kept.
        let (m1, h1, f1, l1) = fighter_thruster_and_allocator(Some(ModuleId(31))); // Control Relay
        let relay = derive_ship_stats_with(
            &h1,
            &f1,
            &m1,
            &l1,
            &sim,
            &CellMaterials::default(),
            Some(&tc),
        );
        assert!(
            (relay.strafe_port - sim.baseline_strafe_force).abs() < 1e-3,
            "the relay mask removes the jet from the strafe channel"
        );
        assert!(
            relay.thrust_force > THRUST_FLOOR + 1e-3,
            "the forward channel is still fed"
        );

        // Flight Computer + the SAME mask → auto overrides → the jet feeds strafe again.
        let (m2, h2, f2, l2) = fighter_thruster_and_allocator(Some(ModuleId(28))); // FC I
        let fc = derive_ship_stats_with(
            &h2,
            &f2,
            &m2,
            &l2,
            &sim,
            &CellMaterials::default(),
            Some(&tc),
        );
        assert!(
            fc.strafe_port > sim.baseline_strafe_force + 1e-3,
            "an FC overrides the mask → full projection (auto)"
        );
    }

    /// R94 — an all-ON mask (or `None`) derives byte-identical to today's full projection.
    #[test]
    fn all_on_mask_is_identical_to_no_thruster_controls() {
        use crate::fitting::ModuleId;
        let sim = SimTuning::default();
        let (m, h, f, l) = fighter_thruster_and_allocator(Some(ModuleId(31))); // relay fitted
        let none = derive_ship_stats_with(&h, &f, &m, &l, &sim, &CellMaterials::default(), None);
        let allon = derive_ship_stats_with(
            &h,
            &f,
            &m,
            &l,
            &sim,
            &CellMaterials::default(),
            Some(&ThrusterControls::default()),
        );
        assert_eq!(none.strafe_port, allon.strafe_port);
        assert_eq!(none.turn_ccw, allon.turn_ccw);
        assert_eq!(none.thrust_force, allon.thrust_force);
    }
}
