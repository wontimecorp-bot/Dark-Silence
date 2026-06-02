//! Tunable, grounded-but-scaled gameplay magnitudes (FR-015, ADR-0012).
//!
//! Every flight/weapon/collision magnitude lives here as a single `Resource`
//! so feel can be tuned in-engine without touching logic. Values are grounded
//! in real relationships but scaled for playability/readability — not real
//! units.

use bevy_ecs::prelude::Resource;

/// Global gameplay tuning. One instance is inserted by the client at startup
/// and read by the `sim` systems. All magnitudes are strictly positive (INV-10).
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Forward/reverse thrust acceleration (sim units/s²).
    pub thrust_accel: f32,
    /// Turn rate (radians/s).
    pub rotation_rate: f32,
    /// Lateral strafe acceleration (sim units/s²).
    pub strafe_accel: f32,
    /// Hard speed cap (sim units/s).
    pub max_speed: f32,
    /// Projectile muzzle speed (sim units/s).
    pub muzzle_speed: f32,
    /// Weapon fire rate (shots/s).
    pub fire_rate: f32,
    /// Closing speed at/above which a ram destroys the ship (sim units/s).
    pub lethal_ram_speed: f32,
    /// Flight-assist drift-damping coefficient (1/s); higher = snappier assist.
    pub assist_damping: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            thrust_accel: 30.0,
            rotation_rate: 3.0,
            strafe_accel: 20.0,
            max_speed: 80.0,
            muzzle_speed: 200.0,
            fire_rate: 5.0,
            lethal_ram_speed: 40.0,
            // Gentle: even in ASSIST mode the ship keeps visible inertia/drift
            // and eases toward the nose over ~1 s rather than snapping (which
            // read as "car-like"). MANUAL mode ignores this entirely.
            assist_damping: 2.5,
        }
    }
}

impl Tuning {
    /// INV-10: every magnitude must be strictly positive for the feel model to
    /// be well-defined (no zero/negative thrust, speed cap, fire rate, etc.).
    pub fn is_valid(&self) -> bool {
        self.thrust_accel > 0.0
            && self.rotation_rate > 0.0
            && self.strafe_accel > 0.0
            && self.max_speed > 0.0
            && self.muzzle_speed > 0.0
            && self.fire_rate > 0.0
            && self.lethal_ram_speed > 0.0
            && self.assist_damping > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tuning_satisfies_inv10() {
        assert!(Tuning::default().is_valid());
    }
}
