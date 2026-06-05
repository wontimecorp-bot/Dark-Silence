//! Phase 3 + Refinement 3 — the mining-skirmish economy loop with **full Newtonian transport
//! motion**. Each faction's AI **mining transport** runs a `ToAsteroid → Loading → ToOutpost →
//! Unloading` cycle: it lumbers out to the central asteroid, fills its cargo over time, hauls back to
//! its refinery outpost, and unloads — every unit unloaded grows that faction's [`RefinedResources`]
//! total (the scenario score; no win condition).
//!
//! **Physics, not rails.** Earlier cuts hand-steered the velocity (snap to cruise, face the drift).
//! The transport now flies on the SAME force/mass/drag model the player ship uses
//! ([`crate::flight::linear_accel`] + [`crate::flight::step_angular`] + [`crate::motion::integrate`]):
//! it has real mass + linear drag (so it spins up to a slow cruise and coasts), and angular inertia
//! (so it PIVOTS to face its destination before it powers off — no sideways sliding). All the knobs
//! live in the [`MiningTuning`] resource so they can be tuned live in the dev panel.
//!
//! **Owns its own integration.** The transport is a `Target`; [`crate::ai::seek_system`] integrates
//! every *other* `Target`'s velocity→position, but it now SKIPS `TargetKind::Transport` (see
//! `seek_system`) and [`mining_transport_system`] does the full pos+vel+heading+spin integration here.
//!
//! **Determinism.** Gated on [`ScenarioActive`](crate::ScenarioActive) (`run_if`), so a no-op in
//! every non-scenario / determinism / botkit / test world. Pure f32 over a stable query order; no
//! RNG, no `HashMap`.

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::clock::FixedDt;
use crate::components::{AngularVelocity, Faction, Heading, Position, Velocity};
use crate::flight::{linear_accel, step_angular};
use crate::motion::{integrate, BodyState};

/// Proportional gain mapping the heading error (rad) to the angular model's turn input (clamped to
/// ±1, like a pilot's stick): a larger error commands full turn, easing off as the nose lines up.
const TURN_GAIN: f32 = 2.5;

/// The mining transport's loop phase.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MiningState {
    /// Cruising out to the central asteroid to load.
    #[default]
    ToAsteroid,
    /// Docked at the asteroid, filling cargo over time.
    Loading,
    /// Cruising home to the refinery outpost to unload.
    ToOutpost,
    /// Docked at the outpost, unloading cargo into the faction's refined total over time.
    Unloading,
}

/// A transport's cargo hold. Just the current load; the capacity is a live [`MiningTuning`] field.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Cargo {
    /// Current load (`0..=MiningTuning::cargo_capacity`).
    pub current: f32,
}

/// The mining transport's loop endpoints. `Entity` fields are runtime-local (not serialized), like
/// [`crate::components::ProjectileOwner`]. All movement/economy tunables now live in [`MiningTuning`].
#[derive(Component, Clone, Copy, Debug)]
pub struct MiningTransport {
    /// The faction's home refinery outpost — the unload target.
    pub home_outpost: Entity,
    /// The shared central asteroid — the load target.
    pub mine_node: Entity,
}

/// Live-tunable transport movement + economy stats — a world resource, shared by both factions'
/// transports and editable in the dev panel (same pattern as [`crate::Tuning`]). Read `Option`-ally by
/// [`mining_transport_system`] with a default fallback, so a world that never inserts it still runs.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct MiningTuning {
    /// Transport mass (kg-ish) — higher = more sluggish acceleration (`a = F/m`).
    pub mass: f32,
    /// Forward thrust force. Emergent cruise speed under steady thrust is `thrust_force / linear_drag`.
    pub thrust_force: f32,
    /// Linear drag coefficient (opposes velocity) — sets the emergent top/cruise speed + the coast feel.
    pub linear_drag: f32,
    /// Turn torque (angular drive). Steady turn rate at full input is `turn_torque / angular_drag`.
    pub turn_torque: f32,
    /// Angular drag (opposes spin).
    pub angular_drag: f32,
    /// Angular inertia — smooths the turn response (higher = more ponderous).
    pub angular_inertia: f32,
    /// Distance within which the transport throttles its thrust down for an "arrive" deceleration.
    pub slow_radius: f32,
    /// Distance at which the transport counts as "arrived" at a dock.
    pub arrive_radius: f32,
    /// Speed below which an arrived transport counts as fully docked (so it settles before loading).
    pub dock_speed: f32,
    /// Cargo gained per second while `Loading`.
    pub load_rate: f32,
    /// Cargo moved into the faction's refined total per second while `Unloading`.
    pub unload_rate: f32,
    /// Cargo hold capacity — the transport heads home once full.
    pub cargo_capacity: f32,
}

impl Default for MiningTuning {
    fn default() -> Self {
        // A heavy, slow barge: spins up to ~28 u/s cruise (thrust/drag), turns at ~1.25 rad/s, eases
        // into its docks. Starting point — tune live in the dev panel.
        Self {
            mass: 8.0,
            thrust_force: 14.0,
            linear_drag: 0.5,
            turn_torque: 5.0,
            angular_drag: 4.0,
            angular_inertia: 6.0,
            slow_radius: 260.0,
            arrive_radius: 55.0,
            dock_speed: 6.0,
            load_rate: 25.0,
            unload_rate: 50.0,
            cargo_capacity: 100.0,
        }
    }
}

/// Per-faction refined-resources tally — the scenario score. Grows as transports unload; there is no
/// win condition (the more a faction refines, the more it feeds its wider industry — flavor). A
/// world resource, inserted alongside [`ScenarioActive`](crate::ScenarioActive).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct RefinedResources {
    pub red: f32,
    pub blue: f32,
}

/// Fixed-step mining loop (gated on `ScenarioActive`). Each transport flies its Newtonian model toward
/// the current dock (asteroid or outpost) and advances its state machine + cargo. Reads other
/// entities' positions (asteroid / outpost) via a `Without<MiningTransport>` query so it stays
/// disjoint from the `&mut Position` transport query.
pub fn mining_transport_system(
    dt: Res<FixedDt>,
    tuning: Option<Res<MiningTuning>>,
    mut refined: ResMut<RefinedResources>,
    positions: Query<&Position, Without<MiningTransport>>,
    mut transports: Query<(
        &mut Position,
        &mut Velocity,
        &mut Heading,
        &mut AngularVelocity,
        &mut Cargo,
        &mut MiningState,
        &MiningTransport,
        &Faction,
    )>,
) {
    let dt = dt.0;
    let t = tuning.map(|r| *r).unwrap_or_default();

    for (mut pos, mut vel, mut heading, mut omega, mut cargo, mut state, mt, faction) in
        &mut transports
    {
        // The current dock: the asteroid while fetching, the home outpost while returning.
        let dock = match *state {
            MiningState::ToAsteroid | MiningState::Loading => mt.mine_node,
            MiningState::ToOutpost | MiningState::Unloading => mt.home_outpost,
        };
        let Ok(dock_pos) = positions.get(dock) else {
            continue; // Dock entity gone (shouldn't happen in the scenario) — hold this tick.
        };

        match *state {
            MiningState::ToAsteroid => {
                if nav_step(
                    dock_pos.0,
                    &mut pos,
                    &mut vel,
                    &mut heading,
                    &mut omega,
                    &t,
                    dt,
                ) {
                    *state = MiningState::Loading;
                }
            }
            MiningState::ToOutpost => {
                if nav_step(
                    dock_pos.0,
                    &mut pos,
                    &mut vel,
                    &mut heading,
                    &mut omega,
                    &t,
                    dt,
                ) {
                    *state = MiningState::Unloading;
                }
            }
            MiningState::Loading => {
                // Hold station (drag bleeds residual motion) and fill the hold.
                coast_step(&mut pos, &mut vel, &mut heading, &mut omega, &t, dt);
                cargo.current = (cargo.current + t.load_rate * dt).min(t.cargo_capacity);
                if cargo.current >= t.cargo_capacity {
                    *state = MiningState::ToOutpost;
                }
            }
            MiningState::Unloading => {
                coast_step(&mut pos, &mut vel, &mut heading, &mut omega, &t, dt);
                let amount = (t.unload_rate * dt).min(cargo.current);
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

/// **Newtonian "arrive" flight** toward `target`: pivot the nose to face the target (proportional turn
/// input through the ship angular model), then thrust FORWARD along the heading — throttled by the
/// arrive ramp (`dist/slow_radius`) so it decelerates into the dock, and by how aligned the nose is
/// (`max(0, heading·dir)`) so it turns before it powers off. `linear_accel` (thrust vs mass + drag) →
/// `integrate` advances pos+vel. Returns `true` once arrived (within `arrive_radius`) AND slow enough
/// (`< dock_speed`) to count as docked.
fn nav_step(
    target: Vec2,
    pos: &mut Position,
    vel: &mut Velocity,
    heading: &mut Heading,
    omega: &mut AngularVelocity,
    t: &MiningTuning,
    dt: f32,
) -> bool {
    let to = target - pos.0;
    let dist = to.length();
    let dir = to.normalize_or_zero();

    // Angular: steer the heading toward the target with bounded turn input → ship angular model.
    let err = wrap_angle(dir.to_angle() - heading.0);
    let turn_input = (err * TURN_GAIN).clamp(-1.0, 1.0);
    omega.0 = step_angular(
        omega.0,
        turn_input,
        t.turn_torque,
        t.angular_drag,
        t.angular_inertia,
        dt,
    );
    heading.0 = wrap_angle(heading.0 + omega.0 * dt);

    // Linear: forward thrust, throttled by the arrive ramp AND nose alignment.
    let heading_dir = Vec2::from_angle(heading.0);
    let throttle = (dist / t.slow_radius.max(f32::MIN_POSITIVE)).min(1.0);
    let align = heading_dir.dot(dir).max(0.0);
    let thrust = heading_dir * (t.thrust_force * throttle * align);
    let accel = linear_accel(vel.0, thrust, t.linear_drag, t.mass);
    let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
    pos.0 = stepped.pos;
    vel.0 = stepped.vel;

    dist < t.arrive_radius && vel.0.length() < t.dock_speed
}

/// Hold station at a dock: no thrust, so linear drag eases the transport to a stop where it arrived
/// (just outside the rock), and its residual spin decays. Used while `Loading`/`Unloading`.
fn coast_step(
    pos: &mut Position,
    vel: &mut Velocity,
    heading: &mut Heading,
    omega: &mut AngularVelocity,
    t: &MiningTuning,
    dt: f32,
) {
    omega.0 = step_angular(
        omega.0,
        0.0,
        t.turn_torque,
        t.angular_drag,
        t.angular_inertia,
        dt,
    );
    heading.0 = wrap_angle(heading.0 + omega.0 * dt);
    let accel = linear_accel(vel.0, Vec2::ZERO, t.linear_drag, t.mass);
    let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
    pos.0 = stepped.pos;
    vel.0 = stepped.vel;
}

/// Wrap an angle to `(-π, π]`.
fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}
