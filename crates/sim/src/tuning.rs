//! Tunable, grounded-but-scaled gameplay magnitudes (FR-015, ADR-0012).
//!
//! Every flight/weapon/collision magnitude lives here as a single `Resource`
//! so feel can be tuned in-engine without touching logic. Values are grounded
//! in real relationships but scaled for playability/readability — not real
//! units.
//!
//! Flight model (the "grounded arcade" / Silent-Death-but-better model): the
//! drive applies a thrust **force** opposed by a linear **drag**, so top speed
//! is the emergent terminal velocity `thrust_force / linear_drag` — no hard
//! clamp. Mass divides force into acceleration (agility). Turning has angular
//! inertia (`turn_torque` vs `angular_drag`, smoothed by `angular_inertia`),
//! and draws from the same drive: hard turning scales down available
//! translational thrust (`turn_power_share`), so you cannot boost and hard-turn
//! at once. A damaged engine simply lowers `thrust_force` → lower top speed AND
//! acceleration, emergently.

use bevy_ecs::prelude::Resource;

use crate::collision::{ASTEROID_MASS, SHIP_MASS};
use crate::components::WRECK_LIFETIME_SECS;
use crate::damage::layers::{
    CARVE_FALLOFF, CARVE_MIN_CELL_COST, CARVE_PEN_COST, RICOCHET_MIN_NEIGHBORS,
    SMOOTH_NORMAL_RADIUS,
};
use crate::fitting::content::{STRUCT_CELL_HP, STRUCT_CELL_MASS};
use crate::weapon::{
    PEN_PER_DAMAGE, PEN_SIZE, PROJECTILE_DAMAGE, PROJECTILE_LIFETIME, PROJECTILE_MASS,
};

/// Global gameplay tuning. One instance is inserted by the client at startup
/// and read by the `sim` systems. All magnitudes are positive (INV-10);
/// `turn_power_share` is a `0..=1` fraction.
#[derive(Resource, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tuning {
    /// Main-drive thrust force (sim units·mass/s²).
    pub thrust_force: f32,
    /// Reverse (retro) thrust force — weaker than the main drive, so reverse
    /// top speed (`reverse_force / linear_drag`) is below the forward top speed.
    pub reverse_force: f32,
    /// Lateral RCS thrust force (strafe).
    pub strafe_force: f32,
    /// Ship mass; acceleration = force / mass.
    pub mass: f32,
    /// Linear drag coefficient. Emergent top speed = `thrust_force / linear_drag`.
    pub linear_drag: f32,
    /// Angular drive torque.
    pub turn_torque: f32,
    /// Angular drag. Emergent max turn rate = `turn_torque / angular_drag` (rad/s).
    pub angular_drag: f32,
    /// Angular inertia (moment) — how quickly the turn rate responds.
    pub angular_inertia: f32,
    /// Fraction of translational thrust diverted to maneuvering at full turn
    /// input (`0..=1`): available thrust ×= `1 - turn_power_share * |turn|`.
    pub turn_power_share: f32,
    /// Projectile muzzle speed (sim units/s).
    pub muzzle_speed: f32,
    /// Weapon fire rate (shots/s).
    pub fire_rate: f32,
    /// Closing speed at/above which a ram destroys the ship (sim units/s).
    pub lethal_ram_speed: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            thrust_force: 30.0,
            reverse_force: 15.0, // retros at half the main drive → reverse top speed 40
            strafe_force: 18.0,
            mass: 1.0,
            linear_drag: 0.375, // top speed = 30 / 0.375 = 80
            turn_torque: 12.0,
            angular_drag: 4.0, // max turn rate = 12 / 4 = 3.0 rad/s
            angular_inertia: 1.2,
            turn_power_share: 0.7,
            muzzle_speed: 200.0,
            fire_rate: 5.0,
            lethal_ram_speed: 40.0,
        }
    }
}

impl Tuning {
    /// Emergent linear top speed (terminal velocity) = `thrust_force / linear_drag`.
    pub fn top_speed(&self) -> f32 {
        self.thrust_force / self.linear_drag
    }

    /// Emergent maximum turn rate (rad/s) = `turn_torque / angular_drag`.
    pub fn max_turn_rate(&self) -> f32 {
        self.turn_torque / self.angular_drag
    }

    /// INV-10: magnitudes strictly positive and `turn_power_share` in `0..=1`.
    pub fn is_valid(&self) -> bool {
        self.thrust_force > 0.0
            && self.reverse_force > 0.0
            && self.strafe_force > 0.0
            && self.mass > 0.0
            && self.linear_drag > 0.0
            && self.turn_torque > 0.0
            && self.angular_drag > 0.0
            && self.angular_inertia > 0.0
            && (0.0..=1.0).contains(&self.turn_power_share)
            && self.muzzle_speed > 0.0
            && self.fire_rate > 0.0
            && self.lethal_ram_speed > 0.0
    }
}

/// **Promoted gameplay feel-consts** (Phase M6) — the carve / structural / projectile / wreck /
/// ram magnitudes that used to be compile-time `const`s, gathered into one runtime [`Resource`]
/// so the dev tuning panel can adjust them live. Inserted into the (authoritative, server) world;
/// the sim reads it via `get_resource::<SimTuning>().copied().unwrap_or_default()`, so a world that
/// never inserts it (e.g. the headless determinism harness) behaves byte-identically to the old
/// consts.
///
/// **`Default` reproduces every promoted const EXACTLY** because it references each source const
/// BY NAME (the consts stay defined at their read-sites + threaded into the pure carve/mass
/// helpers), so the resource and the consts can never silently diverge — the consts remain the
/// single source of truth. Editing a field is **solo / server-authoritative only** (a networked
/// client has no authority over server tuning, and divergent tuning would break reconciliation).
#[derive(Resource, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)] // R92 — older saved RONs (dev settings) missing newer fields fall back per-field.
pub struct SimTuning {
    /// Structural (filler-plating) cell hit points — hull erosion rate (`STRUCT_CELL_HP`).
    pub struct_cell_hp: f32,
    /// Structural cell mass — ship inertia + wreck mass per plating cell (`STRUCT_CELL_MASS`).
    pub struct_cell_mass: f32,
    /// Carve damage multiplier after punching through a cell (`CARVE_FALLOFF`).
    pub carve_falloff: f32,
    /// Penetration cost to tunnel through one carved cell (`CARVE_PEN_COST`).
    pub carve_pen_cost: f32,
    /// Floor work-cost for a hollow/empty cell so the channel is never free (`CARVE_MIN_CELL_COST`).
    pub carve_min_cell_cost: f32,
    /// Minimum present neighbours for a hit to be ricochet-eligible (`RICOCHET_MIN_NEIGHBORS`).
    pub ricochet_min_neighbors: u8,
    /// Smoothed-surface-normal kernel half-width (`SMOOTH_NORMAL_RADIUS`).
    pub smooth_normal_radius: i32,
    /// Fallback projectile slug mass for the unfitted gun (`PROJECTILE_MASS`).
    pub projectile_mass: f32,
    /// Fallback projectile damage for the unfitted gun (`PROJECTILE_DAMAGE`).
    pub projectile_damage: f32,
    /// Projectile time-to-live, seconds (`PROJECTILE_LIFETIME`).
    pub projectile_lifetime: f32,
    /// Penetration value per point of damage (`PEN_PER_DAMAGE`).
    pub pen_per_damage: f32,
    /// Penetrator size for the overmatch test (`PEN_SIZE`).
    pub pen_size: f32,
    /// Drift lifetime of a wreck before despawn, seconds (`WRECK_LIFETIME_SECS`).
    pub wreck_lifetime_secs: f32,
    /// Ship inertial mass for ram impulses (`SHIP_MASS`).
    pub ship_ram_mass: f32,
    /// Asteroid inertial mass for ram impulses (`ASTEROID_MASS`).
    pub asteroid_ram_mass: f32,
    /// Phase E — energy capacitor size, in seconds of reactor output (`Energy.max = power_supply · this`).
    pub energy_capacity_secs: f32,
    /// Phase E — weapon energy cost per point of shot damage (`shot_cost = damage · this`).
    pub weapon_energy_per_damage: f32,
    /// Phase E — heat overheat threshold (`Heat.max`); a weapon locks while `Heat.current >= this`.
    pub heat_capacity: f32,
    /// Phase E — heat cooling rate per second (`Heat.dissipation`).
    pub heat_dissipation: f32,
    /// Phase F — energy drained per second per unit of thrust input
    /// (`thrust_drain = this · (|forward| + |strafe| + turn_power_share·|turn|)`).
    pub thrust_energy_per_input: f32,
    /// Phase F — afterburner pool capacity (`Afterburner.max`).
    pub afterburner_capacity: f32,
    /// Phase F — afterburner pool drain per second while boosting.
    pub afterburner_drain_rate: f32,
    /// Phase F — afterburner pool recharge per second while NOT boosting.
    pub afterburner_regen_rate: f32,
    /// Phase F — translational thrust multiplier while boosting (`thrust ×= 1 + this`).
    pub afterburner_boost_factor: f32,
    // --- Refinement 42: ballistic weapon physics scales (real caliber/velocity/rpm → game space) ---
    /// R42 — projectile RADIUS per mm of caliber (`radius = caliber_mm · this`; visual + collision).
    pub mm_to_world: f32,
    /// R42 — real m/s → game muzzle speed (`muzzle_speed = muzzle_velocity_ms · this`).
    pub velocity_scale: f32,
    /// R42 — rounds/min → shots/s (`fire_rate = rpm · this`; `1/60` = the literal real rate).
    pub rpm_scale: f32,
    /// R42 — projectile slug mass per mm³ of caliber (`mass = projectile_density · caliber_mm³`).
    pub projectile_density: f32,
    /// R42 — damage per joule of muzzle KE (`damage = ½ · mass · muzzle_velocity_ms² · this`).
    pub damage_per_joule: f32,
    // --- Refinement 92: facing-resolved thruster physics (the "flight computer") ---
    /// R92 — torque per (force · world-unit lever arm): a thruster's turn contribution is
    /// `|r × F| · this`, where `r` is its cell's offset from the mass CoM. Placement IS the torque.
    pub thruster_lever_scale: f32,
    /// R92 — angular inertia per unit of the layout's real moment (`Σ m·r²`):
    /// `angular_inertia = Tuning.angular_inertia + layout_inertia · this`. Spread mass turns slower.
    pub thruster_inertia_scale: f32,
    /// R92 — the hull's built-in maneuvering-jet TURN authority (added to both turn channels), so
    /// every design stays flyable; placed thrusters add on top.
    pub baseline_turn_torque: f32,
    /// R92 — built-in STRAFE authority (added to both lateral channels).
    pub baseline_strafe_force: f32,
    /// R92 — built-in RETRO authority (added to the reverse channel; no retro jets → this is all
    /// you have → flip-and-burn).
    pub baseline_reverse_force: f32,
}

impl Default for SimTuning {
    fn default() -> Self {
        // The promoted consts ARE the default — single source of truth, so the resource can never
        // drift from the consts (and the consts stay "live" non-test code, no dead-code warning).
        // The consts remain defined at their read-sites + threaded into the pure carve/mass helpers.
        Self {
            struct_cell_hp: STRUCT_CELL_HP,
            struct_cell_mass: STRUCT_CELL_MASS,
            carve_falloff: CARVE_FALLOFF,
            carve_pen_cost: CARVE_PEN_COST,
            carve_min_cell_cost: CARVE_MIN_CELL_COST,
            ricochet_min_neighbors: RICOCHET_MIN_NEIGHBORS,
            smooth_normal_radius: SMOOTH_NORMAL_RADIUS,
            projectile_mass: PROJECTILE_MASS,
            projectile_damage: PROJECTILE_DAMAGE,
            projectile_lifetime: PROJECTILE_LIFETIME,
            pen_per_damage: PEN_PER_DAMAGE,
            pen_size: PEN_SIZE,
            wreck_lifetime_secs: WRECK_LIFETIME_SECS,
            ship_ram_mass: SHIP_MASS,
            asteroid_ram_mass: ASTEROID_MASS,
            // Phase E energy/heat feel — first-pass; tuned live in the dev panel. (No source const:
            // these are read only via this resource, so a literal default has nothing to drift from.)
            energy_capacity_secs: 4.0,
            weapon_energy_per_damage: 1.0,
            heat_capacity: 45.0,
            heat_dissipation: 6.0,
            thrust_energy_per_input: 35.0,
            afterburner_capacity: 100.0,
            afterburner_drain_rate: 40.0,
            afterburner_regen_rate: 20.0,
            afterburner_boost_factor: 0.6,
            // R42 ballistic physics — calibrated so the seed 30mm / 1000 m·s / 300 rpm autocannon
            // reproduces today's feel: muzzle 200 u/s, 5 shots/s, ~0.03 slug, ~12 dmg, ~0.2 radius.
            mm_to_world: 1.0 / 150.0, // 30 mm → 0.2 radius (today's mesh)
            velocity_scale: 0.2,      // 1000 m/s → 200 u/s
            rpm_scale: 1.0 / 60.0,    // 300 rpm → 5 shots/s
            // caliber³ slug density: 30 mm → ~0.03 game slug (today's autocannon).
            projectile_density: 0.03 / (30.0 * 30.0 * 30.0),
            // KE → damage: 30 mm autocannon (½·0.03·1000²·this) → ~12 damage (today's value).
            damage_per_joule: 0.0008,
            // R92 facing-resolved thruster physics — calibrated against the SEED FIGHTER (one
            // 30-force thruster ~1.6 u aft / ~0.3 u port of the CoM): lever torque ≈ 30·0.3·1.0 ≈ 10
            // (vs. the old flat 12) on top of the baseline; baselines = the legacy Tuning trio so a
            // jet-less axis feels like today. All live-tunable in the dev panel.
            thruster_lever_scale: 1.0,
            thruster_inertia_scale: 0.015,
            baseline_turn_torque: 12.0,
            baseline_strafe_force: 18.0,
            baseline_reverse_force: 15.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tuning_satisfies_inv10() {
        assert!(Tuning::default().is_valid());
    }

    /// `SimTuning::default()` is well-formed (positive magnitudes; the gate counters in range).
    #[test]
    fn simtuning_default_is_sane() {
        let t = SimTuning::default();
        assert!(t.struct_cell_hp > 0.0 && t.struct_cell_mass > 0.0);
        assert!(t.carve_falloff > 0.0 && t.carve_pen_cost > 0.0 && t.carve_min_cell_cost >= 0.0);
        assert!(
            t.projectile_mass > 0.0 && t.projectile_damage > 0.0 && t.projectile_lifetime > 0.0
        );
        assert!(t.pen_per_damage > 0.0 && t.pen_size > 0.0 && t.wreck_lifetime_secs > 0.0);
        assert!(t.ship_ram_mass > 0.0 && t.asteroid_ram_mass > 0.0);
        // R42 weapon-physics scales are all positive.
        assert!(t.mm_to_world > 0.0 && t.velocity_scale > 0.0 && t.rpm_scale > 0.0);
        assert!(t.projectile_density > 0.0 && t.damage_per_joule > 0.0);
    }

    #[test]
    fn emergent_caps_match_intended_values() {
        let t = Tuning::default();
        assert!((t.top_speed() - 80.0).abs() < 1e-3);
        assert!((t.max_turn_rate() - 3.0).abs() < 1e-3);
    }
}
