//! Phase 3 — the mining-skirmish economy loop. Each faction's AI **mining transport** runs a
//! `ToAsteroid → Loading → ToOutpost → Unloading` cycle: it cruises to the central asteroid, fills
//! its cargo over time, returns to its refinery outpost, and unloads — and every unit unloaded grows
//! that faction's [`RefinedResources`] total (the scenario score; no win condition).
//!
//! **Kinematic nav, no new integrator.** The transport is a `Target` (`TargetKind::Transport`), so
//! the existing [`crate::ai::seek_system`] already integrates its `Velocity → Position` each tick
//! (zero accel for a non-`Seeker` kind). [`mining_transport_system`] therefore only sets the
//! transport's `Velocity` toward its current target (clamped so it never overshoots) and advances
//! the state machine + cargo — no manual position integration, no flight model, nothing else moves
//! it (it carries no `Ship`/`Wreck` marker).
//!
//! **Determinism.** Gated on [`ScenarioActive`](crate::ScenarioActive) (`run_if`), so a no-op in
//! every non-scenario / determinism / botkit / test world. Pure f32 over a stable query order; no
//! RNG, no `HashMap`.

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::clock::FixedDt;
use crate::components::{Faction, Position, Velocity};

/// The mining transport's loop phase.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MiningState {
    /// Cruising out to the central asteroid to load.
    #[default]
    ToAsteroid,
    /// At the asteroid, filling cargo over time.
    Loading,
    /// Cruising home to the refinery outpost to unload.
    ToOutpost,
    /// At the outpost, unloading cargo into the faction's refined total over time.
    Unloading,
}

/// A transport's cargo hold (resource units).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Cargo {
    /// Current load (`0..=capacity`).
    pub current: f32,
    /// Maximum load before the transport heads home.
    pub capacity: f32,
}

/// The mining transport's loop endpoints + tunables. `Entity` fields are runtime-local (not
/// serialized), like [`crate::components::ProjectileOwner`].
#[derive(Component, Clone, Copy, Debug)]
pub struct MiningTransport {
    /// The faction's home refinery outpost — the unload target.
    pub home_outpost: Entity,
    /// The shared central asteroid — the load target.
    pub mine_node: Entity,
    /// Cargo gained per second while `Loading`.
    pub load_rate: f32,
    /// Cargo moved into the faction's refined total per second while `Unloading`.
    pub unload_rate: f32,
    /// Cruise speed (sim units/s) while navigating.
    pub nav_speed: f32,
    /// Distance at which the transport counts as "arrived" at a target.
    pub arrive_radius: f32,
}

/// Per-faction refined-resources tally — the scenario score. Grows as transports unload; there is no
/// win condition (the more a faction refines, the more it feeds its wider industry — flavor). A
/// world resource, inserted alongside [`ScenarioActive`](crate::ScenarioActive).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct RefinedResources {
    pub red: f32,
    pub blue: f32,
}

/// Fixed-step mining loop (gated on `ScenarioActive`). Each transport advances its state machine and
/// steers toward its current target by setting its `Velocity`; the existing `seek_system` integrates
/// that into `Position`. Reads other entities' positions (asteroid / outpost) via a shared-read
/// `positions` query.
pub fn mining_transport_system(
    dt: Res<FixedDt>,
    mut refined: ResMut<RefinedResources>,
    positions: Query<&Position>,
    mut transports: Query<(
        &Position,
        &mut Velocity,
        &mut Cargo,
        &mut MiningState,
        &MiningTransport,
        &Faction,
    )>,
) {
    let dt = dt.0;
    for (pos, mut vel, mut cargo, mut state, mt, faction) in &mut transports {
        match *state {
            MiningState::ToAsteroid => {
                if nav_toward(mt.mine_node, pos.0, &positions, mt, dt, &mut vel) {
                    *state = MiningState::Loading;
                }
            }
            MiningState::Loading => {
                vel.0 = Vec2::ZERO;
                cargo.current = (cargo.current + mt.load_rate * dt).min(cargo.capacity);
                if cargo.current >= cargo.capacity {
                    *state = MiningState::ToOutpost;
                }
            }
            MiningState::ToOutpost => {
                if nav_toward(mt.home_outpost, pos.0, &positions, mt, dt, &mut vel) {
                    *state = MiningState::Unloading;
                }
            }
            MiningState::Unloading => {
                vel.0 = Vec2::ZERO;
                let amount = (mt.unload_rate * dt).min(cargo.current);
                cargo.current -= amount;
                match faction {
                    Faction::Red => refined.red += amount,
                    Faction::Blue => refined.blue += amount,
                }
                if cargo.current <= 0.0 {
                    *state = MiningState::ToAsteroid;
                }
            }
        }
    }
}

/// Steer `vel` toward `target_entity`'s position at the transport's cruise speed, clamped so the
/// integrated move (`vel·dt`) never overshoots; returns `true` once within `arrive_radius`. If the
/// target entity is gone (e.g. a destroyed outpost), the transport idles in place.
fn nav_toward(
    target_entity: Entity,
    pos: Vec2,
    positions: &Query<&Position>,
    mt: &MiningTransport,
    dt: f32,
    vel: &mut Velocity,
) -> bool {
    let Ok(target) = positions.get(target_entity) else {
        vel.0 = Vec2::ZERO;
        return false;
    };
    let to = target.0 - pos;
    let dist = to.length();
    if dist < mt.arrive_radius {
        vel.0 = Vec2::ZERO;
        return true;
    }
    let speed = mt.nav_speed.min(dist / dt.max(f32::MIN_POSITIVE));
    vel.0 = to.normalize_or_zero() * speed;
    false
}
