//! Weapon timing: pure cooldown helpers plus the fixed-step firing and
//! projectile-advance systems.

use crate::clock::FixedDt;
use crate::components::{
    Damage, Heading, Lifetime, Position, PrevPosition, Projectile, ProjectileOwner, Ship, Velocity,
    Weapon,
};
use crate::intent::ShipIntent;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Default damage a projectile carries (Damage > 0, INV-04).
const PROJECTILE_DAMAGE: f32 = 10.0;
/// Projectile time-to-live in seconds.
const PROJECTILE_LIFETIME: f32 = 3.0;

/// A weapon may fire only once its cooldown has elapsed (INV-03).
pub fn can_fire(cooldown: f32) -> bool {
    cooldown <= 0.0
}

/// The cooldown (seconds) set immediately after firing, from the fire rate.
pub fn cooldown_after_fire(fire_rate: f32) -> f32 {
    debug_assert!(fire_rate > 0.0, "fire_rate must be positive (INV-10)");
    1.0 / fire_rate
}

/// Fixed-step weapon firing (FR-005): tick the cooldown down, and on a `fire`
/// intent (when cool) spawn a projectile along the ship's heading at muzzle
/// speed. The projectile records its spawn position as `PrevPosition` so the
/// very first swept test has a valid segment.
pub fn weapon_fire_system(
    intent: Res<ShipIntent>,
    dt: Res<FixedDt>,
    mut commands: Commands,
    mut ship_q: Query<(Entity, &Position, &Heading, &mut Weapon), With<Ship>>,
) {
    let dt = dt.0;
    for (owner, pos, heading, mut weapon) in &mut ship_q {
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        if intent.fire && can_fire(weapon.cooldown) {
            let vel = Vec2::from_angle(heading.0) * weapon.muzzle_speed;
            commands.spawn((
                Projectile,
                Position(pos.0),
                PrevPosition(pos.0),
                Velocity(vel),
                Damage(PROJECTILE_DAMAGE),
                Lifetime(PROJECTILE_LIFETIME),
                ProjectileOwner(owner),
            ));
            weapon.cooldown = cooldown_after_fire(weapon.fire_rate);
        }
    }
}

/// Fixed-step projectile advance (FR-006): record the previous position (the
/// tail of the swept segment), move by velocity, age the lifetime, and despawn
/// when it expires (INV-06).
pub fn projectile_step_system(
    dt: Res<FixedDt>,
    mut commands: Commands,
    mut q: Query<
        (
            Entity,
            &mut Position,
            &mut PrevPosition,
            &Velocity,
            &mut Lifetime,
        ),
        With<Projectile>,
    >,
) {
    let dt = dt.0;
    for (e, mut pos, mut prev, vel, mut life) in &mut q {
        prev.0 = pos.0;
        pos.0 += vel.0 * dt;
        life.0 -= dt;
        if life.0 <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_only_when_cool() {
        assert!(can_fire(0.0));
        assert!(can_fire(-0.1));
        assert!(!can_fire(0.2));
    }

    #[test]
    fn firing_sets_positive_cooldown_from_rate() {
        assert!(cooldown_after_fire(5.0) > 0.0);
        assert!((cooldown_after_fire(5.0) - 0.2).abs() < 1e-6);
    }
}
