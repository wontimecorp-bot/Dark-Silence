//! Combat resolution: pure damage/destroy helpers, the destruction system, and
//! the transient hit/destroy feedback the HUD reads.

use crate::clock::FixedDt;
use crate::components::Health;
use bevy_ecs::prelude::*;

/// Flash duration (seconds) set when a hit or destroy occurs.
pub const FLASH_TIME: f32 = 0.15;

/// Transient feedback for the HUD: non-zero for a short while after a hit or a
/// destroy. A resource (not an event) keeps `sim` off Bevy's event API and
/// makes the signal trivially testable (CHK016).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct HitFeedback {
    /// Seconds of "a projectile hit a target" feedback remaining.
    pub hit_flash: f32,
    /// Seconds of "a target was destroyed" feedback remaining.
    pub destroy_flash: f32,
}

/// Health after taking `damage`, clamped at zero (INV-01).
pub fn apply_damage(health: f32, damage: f32) -> f32 {
    (health - damage).max(0.0)
}

/// An entity is destroyed once its health is depleted.
pub fn is_destroyed(health: f32) -> bool {
    health <= 0.0
}

/// Despawn anything whose health is depleted — targets, and the ship on a
/// lethal ram — exactly once (INV-09), raising the destroy feedback.
pub fn destruction_system(
    mut commands: Commands,
    mut feedback: ResMut<HitFeedback>,
    q: Query<(Entity, &Health)>,
) {
    for (e, health) in &q {
        if is_destroyed(health.0) {
            commands.entity(e).despawn();
            feedback.destroy_flash = FLASH_TIME;
        }
    }
}

/// Bleed the transient hit/destroy feedback toward zero each step.
pub fn feedback_decay_system(dt: Res<FixedDt>, mut feedback: ResMut<HitFeedback>) {
    let dt = dt.0;
    feedback.hit_flash = (feedback.hit_flash - dt).max(0.0);
    feedback.destroy_flash = (feedback.destroy_flash - dt).max(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_subtracts_and_clamps() {
        assert!((apply_damage(10.0, 3.0) - 7.0).abs() < 1e-6);
        assert_eq!(apply_damage(2.0, 5.0), 0.0, "health never goes negative");
    }

    #[test]
    fn destroyed_only_at_or_below_zero() {
        assert!(!is_destroyed(0.1));
        assert!(is_destroyed(0.0));
        assert!(is_destroyed(-1.0));
    }
}
