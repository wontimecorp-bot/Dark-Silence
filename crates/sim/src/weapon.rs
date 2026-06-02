//! Weapon timing: pure cooldown helpers plus the fixed-step firing and
//! projectile-advance systems.

use crate::clock::FixedDt;
use crate::components::{
    Damage, Heading, Lifetime, Position, PrevPosition, Projectile, ProjectileOwner, Ship, Velocity,
    Weapon,
};
use crate::fitting::ShipStats;
use crate::intent::ShipIntent;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Default damage a projectile carries (Damage > 0, INV-04) — the unfitted-ship
/// fallback when the shot's source is the [`Weapon`] component, which has no
/// per-shot damage field. Fitted ships use their [`ShipStats`] weapon profile's
/// `damage`.
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

/// Fixed-step weapon firing (FR-005): tick each ship's cooldown down, and on
/// that ship's own `fire` intent (when cool) spawn a projectile along its
/// heading at muzzle speed. The projectile records its spawn position as
/// `PrevPosition` so the very first swept test has a valid segment.
///
/// Intent is **per-entity**: the ship query carries each ship's own
/// [`ShipIntent`] component, so N independently-controlled ships fire from their
/// own inputs in one shared step. A ship without the component is not piloted
/// and does not fire.
///
/// **Override-or-fallback weapon source** (FR-014/016, the E006 rewire): a ship
/// that carries a fit-derived [`ShipStats`] component is gated on
/// [`ShipStats::can_fire`] (no weapon module ⇒ no fire) and fires with that fit's
/// [`WeaponProfile`](crate::fitting::WeaponProfile) params + damage; a ship
/// without [`ShipStats`] keeps the exact E002 [`Weapon`]-component behavior. The
/// [`Weapon`] component still owns the cooldown state machine (INV-03) on both
/// paths, so the cooldown gate is unchanged. A fitted ship that cannot fire still
/// has its cooldown ticked harmlessly.
pub fn weapon_fire_system(
    dt: Res<FixedDt>,
    mut commands: Commands,
    // Fitted ships: ShipStats gates firing + supplies the profile; the optional
    // Weapon component (present when a weapon module is installed) holds cooldown.
    mut fitted: Query<
        (
            Entity,
            &ShipIntent,
            &Position,
            &Heading,
            &ShipStats,
            Option<&mut Weapon>,
        ),
        With<Ship>,
    >,
    // Unfitted ships: the E002 Weapon-component behavior, unchanged.
    mut unfitted: Query<
        (Entity, &ShipIntent, &Position, &Heading, &mut Weapon),
        (With<Ship>, Without<ShipStats>),
    >,
) {
    let dt = dt.0;

    // Fitted path: fit-derived can_fire + WeaponProfile (FR-016).
    for (owner, intent, pos, heading, stats, weapon) in &mut fitted {
        // No weapon module ⇒ cannot fire; if a Weapon component lingers, still
        // tick its cooldown so it stays a valid (idle) state machine.
        let (Some(profile), Some(mut weapon)) = (stats.weapon, weapon) else {
            continue;
        };
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        if stats.can_fire && intent.fire && can_fire(weapon.cooldown) {
            let vel = Vec2::from_angle(heading.0) * profile.muzzle_speed;
            commands.spawn((
                Projectile,
                Position(pos.0),
                PrevPosition(pos.0),
                Velocity(vel),
                Damage(profile.damage),
                Lifetime(PROJECTILE_LIFETIME),
                ProjectileOwner(owner),
            ));
            weapon.cooldown = cooldown_after_fire(profile.fire_rate);
        }
    }

    // Unfitted path: the original Weapon-component behavior (E001/E002/E003).
    for (owner, intent, pos, heading, mut weapon) in &mut unfitted {
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
