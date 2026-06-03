//! ECS gameplay components — the shared simulation model's data layer.
//!
//! These are the `bevy_ecs` [`Component`]s that gameplay systems attach to
//! entities. `bevy_ecs` is pulled in with `default-features = false` (HINT-004):
//! we want the pure entity/component/system data model, not Bevy's render,
//! window, app, or scheduler-heavy stack — `sim` stays headless (TR-002).
//!
//! Every component derives:
//! - [`Component`] — so it can live on an ECS entity;
//! - `Serialize`/`Deserialize` — so it replicates (E003) and persists (E004)
//!   without rework (TR-008, AD-002);
//! - `Copy`/`Clone`/`Debug`/`PartialEq` — value semantics and round-trip
//!   equality (the serde round-trip test asserts `deserialize(serialize(x)) == x`).
//!
//! The wrapped math type is `glam::Vec2`: gameplay is planar (the client renders
//! 3D, the sim is 2D), matching `motion::BodyState`.

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// World-space position of an entity on the 2D gameplay plane, in sim units.
///
/// At Tier 0 these are sector-relative (never large absolute world coordinates,
/// which would lose `f32` precision) — see [`crate::motion::BodyState::pos`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position(pub Vec2);

/// Linear velocity of an entity, in sim units per second.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Velocity(pub Vec2);

impl Position {
    /// Position at the origin.
    pub const ZERO: Self = Self(Vec2::ZERO);

    /// Construct from a 2D vector.
    pub const fn new(value: Vec2) -> Self {
        Self(value)
    }
}

impl Velocity {
    /// Zero velocity (at rest).
    pub const ZERO: Self = Self(Vec2::ZERO);

    /// Construct from a 2D vector.
    pub const fn new(value: Vec2) -> Self {
        Self(value)
    }
}

// --- E002 gameplay components -------------------------------------------------
//
// Same derive discipline as `Position`/`Velocity` above: `Component` so they
// live on entities, serde so they replicate/persist later (E003/E004), and
// value semantics. `ProjectileOwner` is the one exception — it wraps an
// `Entity`, whose id is runtime-local and not meaningful across the wire, so it
// is deliberately not `Serialize`/`Deserialize`.

/// Marker: the player-controlled ship.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ship;

/// Facing angle in radians — the direction the nose (and the fixed weapon) points.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heading(pub f32);

/// Turn rate in radians/s — the ship's angular velocity, carried with inertia
/// (the flight-model spins it up/down rather than snapping).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AngularVelocity(pub f32);

/// Remaining hit points; an entity is destroyed at or below zero.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Health(pub f32);

/// Flight-assist mode: `On` damps drift toward heading; `Off` is decoupled,
/// full-momentum flight.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlightAssist {
    On,
    Off,
}

/// Marker: a fired projectile.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projectile;

/// Damage a projectile deals on hit (> 0, INV-04).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Damage(pub f32);

/// Remaining lifetime in seconds; the projectile despawns at zero (INV-06).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lifetime(pub f32);

/// The entity's position on the previous fixed step — the tail of the swept
/// segment used for continuous collision so fast projectiles never tunnel.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrevPosition(pub Vec2);

/// The ship that fired a projectile (so a projectile cannot hit its owner).
/// Not serialized: `Entity` ids are runtime-local, not stable across the wire.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectileOwner(pub Entity);

/// Marker: a destructible target.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target;

/// Which kind of target this is.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    /// Static practice dummy.
    Dummy,
    /// Drifts at constant velocity; also collides physically with the ship.
    Asteroid,
    /// Thrusts toward the player each step.
    Seeker,
}

impl TargetKind {
    /// Stable wire tag for the target sub-kind, carried in
    /// `protocol::EntityRecord.flags` so a networked client can pick the right
    /// visual — the wire `EntityKind` only distinguishes Ship/Projectile/Target.
    /// Additive; not part of any gameplay invariant.
    pub fn as_u8(self) -> u8 {
        match self {
            TargetKind::Dummy => 0,
            TargetKind::Asteroid => 1,
            TargetKind::Seeker => 2,
        }
    }

    /// Inverse of [`TargetKind::as_u8`]; `None` for an unknown tag.
    pub fn from_u8(v: u8) -> Option<TargetKind> {
        match v {
            0 => Some(TargetKind::Dummy),
            1 => Some(TargetKind::Asteroid),
            2 => Some(TargetKind::Seeker),
            _ => None,
        }
    }
}

/// Circular proxy hitbox radius, > 0 (INV-05).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionRadius(pub f32);

/// A short-lived per-entity hit-flash timer (seconds), refreshed each time a hit
/// lands on this entity and decayed toward `0` each fixed step
/// ([`damage_flash_decay_system`](crate::collision::damage_flash_decay_system)).
///
/// Presentation-only (E007 live-demo visual feedback): retained as the hull-hit
/// timing seam. The client no longer scale-pulses the ship from it (the "zoom in and
/// out" the user disliked is gone); the brief deflector shimmer is driven by
/// [`ShieldHitFlash`] instead. Deterministic — it ticks down by the fixed `dt` like
/// every other timer, so server and client agree. Defaults to `0` for entities never
/// hit.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DamageFlash(pub f32);

/// A short-lived per-entity **shield-hit** flash timer (seconds), refreshed each time
/// a hit is absorbed by this entity's shield while it is up
/// ([`HitKind::ShieldAbsorbed`](crate::damage::HitKind)) and decayed toward `0` each
/// fixed step
/// ([`shield_hit_flash_decay_system`](crate::collision::shield_hit_flash_decay_system)).
///
/// Presentation-only (E007 live-demo visual feedback): the client renders a brief
/// translucent cyan **deflector shimmer** enveloping the ship for the split-second a
/// shot strikes the shield, fading as this timer bleeds out — a sci-fi shield flash
/// on impact, NOT a persistent bubble. There is no flash once the shield is depleted
/// (shots reach the hull). Deterministic — it ticks down by the fixed `dt` like every
/// other timer, so server and client agree. Defaults to `0` for entities whose shield
/// has not just taken a hit.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShieldHitFlash(pub f32);

/// The most recent shield impact's **direction** (unit, ship-centre → impact) and a
/// short-lived fade `timer` (seconds) — the seam the client uses to flash the
/// deflector at WHERE the bullet hit the shield instead of over the whole ship.
///
/// Refreshed each time a hit is absorbed by this entity's shield
/// ([`HitKind::ShieldAbsorbed`](crate::damage::HitKind)) in
/// [`fitted_damage_system`](crate::collision::fitted_damage_system), and decayed
/// toward `0` in lock-step with [`ShieldHitFlash`] by
/// [`shield_hit_flash_decay_system`](crate::collision::shield_hit_flash_decay_system).
///
/// **Transient runtime render feedback — deliberately NOT serialized**, mirroring
/// [`ProjectileOwner`] / [`crate::damage::DamageEvent`]: it is a per-frame visual cue
/// derived from the impact geometry, not replicated or persisted state (it would be
/// re-derived from the next hit anyway). The `dir` is a unit vector in **world space**
/// (the client rotates it into the ship's local frame before placing the flash); it is
/// `Vec2::ZERO` when there is no meaningful direction (the client then hides the flash).
/// Deterministic decay (ticks by the fixed `dt` like every other timer). Defaults to a
/// zero dir / zero timer for an entity whose shield has not just taken a hit.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LastShieldHit {
    /// Unit direction from the ship centre toward the impact point, in world space.
    /// `Vec2::ZERO` when no direction could be resolved (flash hidden client-side).
    pub dir: Vec2,
    /// Seconds remaining on the directional-flash fade; bled toward `0` each fixed
    /// step alongside [`ShieldHitFlash`].
    pub timer: f32,
}

/// The ship's fixed forward weapon: fire timing + muzzle speed.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weapon {
    /// Seconds until the weapon can fire again (INV-03).
    pub cooldown: f32,
    /// Shots per second.
    pub fire_rate: f32,
    /// Projectile launch speed.
    pub muzzle_speed: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;

    /// The components must actually be usable as ECS data: spawn an entity with
    /// both, then read them back. This is the headless-ECS smoke test that proves
    /// `default-features = false` still gives us a working component model.
    #[test]
    fn components_attach_to_an_entity_and_read_back() {
        let mut world = World::new();
        let pos = Position::new(Vec2::new(1.0, 2.0));
        let vel = Velocity::new(Vec2::new(-3.0, 4.0));
        let entity = world.spawn((pos, vel)).id();

        assert_eq!(*world.get::<Position>(entity).unwrap(), pos);
        assert_eq!(*world.get::<Velocity>(entity).unwrap(), vel);
    }
}
