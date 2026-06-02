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
