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
use serde::{Deserialize, Serialize};

use super::content::ModuleCatalog;
use super::fit::Fit;
use super::hull::Hull;
use super::module::{ModuleKind, ModuleSpecifics};
use crate::tuning::Tuning;

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
    /// Projectile launch speed (`> 0`).
    pub muzzle_speed: f32,
    /// Shots per second (`> 0`).
    pub fire_rate: f32,
    /// Damage per shot (`> 0`).
    pub damage: f32,
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
    /// Main-drive forward thrust force (`>= THRUST_FLOOR > 0`); Σ thruster thrust.
    pub thrust_force: f32,
    /// Reverse (retro) thrust force (`> 0`); a fraction of forward thrust.
    pub reverse_force: f32,
    /// Lateral RCS (strafe) thrust force (`>= STRAFE_FLOOR > 0`); Σ thruster strafe.
    pub strafe_force: f32,
    /// Total ship mass (`>= hull_base_mass > 0`); `hull_base_mass + Σ module.mass`.
    pub total_mass: f32,
    /// Linear drag coefficient (`> 0`); emergent top speed = `thrust_force / linear_drag`.
    pub linear_drag: f32,
    /// Angular drive torque (`>= TORQUE_FLOOR > 0`); Σ thruster torque.
    pub turn_torque: f32,
    /// Angular drag (`> 0`); emergent max turn rate = `turn_torque / angular_drag`.
    pub angular_drag: f32,
    /// Angular inertia / moment (`> 0`); how quickly the turn rate responds.
    pub angular_inertia: f32,
    /// Fraction of translational thrust diverted at full turn input (`0..=1`).
    pub turn_power_share: f32,
    /// Power supplied to the budget: `hull.power_capacity + Σ reactor.power_gen`.
    pub power_supply: f32,
    /// Power consumed by the fit: `Σ module.power_draw` (`>= 0`).
    pub power_draw: f32,
    /// CPU/control consumed by the fit: `Σ module.cpu_draw` (`>= 0`).
    pub cpu_draw: f32,
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

    /// Emergent maximum turn rate (rad/s) = `turn_torque / angular_drag` (mirrors
    /// [`Tuning::max_turn_rate`]).
    pub fn max_turn_rate(&self) -> f32 {
        self.turn_torque / self.angular_drag
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
            && self.strafe_force.is_finite()
            && self.strafe_force >= STRAFE_FLOOR
            && self.total_mass.is_finite()
            && self.total_mass > 0.0
            && self.linear_drag.is_finite()
            && self.linear_drag > 0.0
            && self.turn_torque.is_finite()
            && self.turn_torque >= TORQUE_FLOOR
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
/// - `power_supply = hull.power_capacity + Σ reactor.power_gen`,
///   `power_draw = Σ module.power_draw`, `cpu_draw = Σ module.cpu_draw`;
/// - `can_fire` is `true` iff ≥1 **weapon** module is installed, and `weapon` is
///   that module's [`WeaponProfile`] (the first by deterministic `SlotId` order
///   when several are fitted this epic).
///
/// A dangling [`ModuleId`](super::module::ModuleId) contributes nothing (the
/// dangling-ref *rejection* is `validate_fit`'s concern, INV-F13); derivation
/// stays total and finite regardless.
pub fn derive_ship_stats(hull: &Hull, fit: &Fit, catalog: &ModuleCatalog) -> ShipStats {
    // Flight-feel constants the modules do not supply come from the demoted
    // `Tuning` baseline (HINT-002): the seed baseline fit reproduces these.
    let base = Tuning::default();

    let mut thrust_force = 0.0_f32;
    let mut turn_torque = 0.0_f32;
    let mut strafe_force = 0.0_f32;
    let mut total_module_mass = 0.0_f32;
    let mut power_gen = 0.0_f32;
    let mut power_draw = 0.0_f32;
    let mut cpu_draw = 0.0_f32;
    let mut weapon: Option<WeaponProfile> = None;

    // Iterate by SlotId (BTreeMap order) so derivation is deterministic — the
    // first weapon module by slot wins when several are fitted (Principle II).
    for module_id in fit.assignments.values() {
        let Some(module) = catalog.get(*module_id) else {
            // Dangling id: no contribution (validate_fit rejects the fit).
            continue;
        };

        // Universal budget costs apply on every kind (data-model.md).
        total_module_mass += module.mass;
        power_gen += module.power_gen;
        power_draw += module.power_draw;
        cpu_draw += module.cpu_draw;

        match module.specifics {
            ModuleSpecifics::Thruster {
                thrust_force: t,
                turn_torque: tq,
                strafe_force: s,
            } => {
                thrust_force += t;
                turn_torque += tq;
                strafe_force += s;
            }
            // First weapon module by deterministic slot order populates the fire
            // profile (FR-016; multi-weapon merge is a later epic).
            ModuleSpecifics::Weapon {
                muzzle_speed,
                fire_rate,
                damage,
            } if module.kind == ModuleKind::Weapon && weapon.is_none() => {
                weapon = Some(WeaponProfile {
                    muzzle_speed,
                    fire_rate,
                    damage,
                });
            }
            _ => {}
        }
    }

    // Graceful floors (FR-017, INV-F07): never zero a denominator or a drive.
    let thrust_force = thrust_force.max(THRUST_FLOOR);
    let turn_torque = turn_torque.max(TORQUE_FLOOR);
    let strafe_force = strafe_force.max(STRAFE_FLOOR);
    // `hull_base_mass > 0` by construction, so total mass is always `> 0`
    // (INV-F14); the extra `max` is defensive belt-and-suspenders.
    let total_mass = (hull.hull_base_mass + total_module_mass).max(f32::MIN_POSITIVE);

    ShipStats {
        thrust_force,
        reverse_force: thrust_force * REVERSE_FRACTION,
        strafe_force,
        total_mass,
        linear_drag: base.linear_drag,
        turn_torque,
        angular_drag: base.angular_drag,
        angular_inertia: base.angular_inertia,
        turn_power_share: base.turn_power_share,
        power_supply: hull.power_capacity + power_gen,
        power_draw,
        cpu_draw,
        can_fire: weapon.is_some(),
        weapon,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::content::{baseline_fit, baseline_hull, seed_catalogs};

    #[test]
    fn baseline_fit_reproduces_tuning_defaults() {
        // HINT-002 / T019 unit-level guard: the baseline seed fit derives to the
        // exact `Tuning::default()` flight magnitudes (the integration test in
        // tests/fitting.rs is the cross-crate guard).
        let (modules, _) = seed_catalogs();
        let hull = baseline_hull();
        let fit = baseline_fit();
        let stats = derive_ship_stats(&hull, &fit, &modules);
        let t = Tuning::default();
        assert!((stats.thrust_force - t.thrust_force).abs() < 1e-4);
        assert!((stats.reverse_force - t.reverse_force).abs() < 1e-4);
        assert!((stats.strafe_force - t.strafe_force).abs() < 1e-4);
        assert!((stats.total_mass - t.mass).abs() < 1e-4);
        assert!((stats.turn_torque - t.turn_torque).abs() < 1e-4);
        assert_eq!(stats.linear_drag, t.linear_drag);
        assert_eq!(stats.angular_drag, t.angular_drag);
        assert_eq!(stats.angular_inertia, t.angular_inertia);
        assert_eq!(stats.turn_power_share, t.turn_power_share);
        assert!((stats.top_speed() - t.top_speed()).abs() < 1e-3);
        assert!((stats.max_turn_rate() - t.max_turn_rate()).abs() < 1e-3);
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
        let stats = derive_ship_stats(&hull, &fit, &modules);
        assert!(stats.is_finite_and_floored());
        assert_eq!(stats.thrust_force, THRUST_FLOOR);
        assert_eq!(stats.turn_torque, TORQUE_FLOOR);
        assert!(!stats.can_fire);
        assert!(stats.weapon.is_none());
    }
}
