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

/// Global gameplay tuning. One instance is inserted by the client at startup
/// and read by the `sim` systems. All magnitudes are positive (INV-10);
/// `turn_power_share` is a `0..=1` fraction.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tuning_satisfies_inv10() {
        assert!(Tuning::default().is_valid());
    }

    #[test]
    fn emergent_caps_match_intended_values() {
        let t = Tuning::default();
        assert!((t.top_speed() - 80.0).abs() < 1e-3);
        assert!((t.max_turn_rate() - 3.0).abs() < 1e-3);
    }
}
