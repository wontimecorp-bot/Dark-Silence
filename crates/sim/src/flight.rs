//! Flight dynamics: the "grounded arcade" flight-model — thrust *force* opposed
//! by linear *drag* (emergent top speed = `thrust_force / linear_drag`, no hard
//! clamp), angular inertia for weighty turns, and a shared power budget where
//! hard turning steals translational thrust — plus an optional decoupled /
//! Newtonian mode. Pure helpers are unit-tested; the ECS system composes them
//! with the `sim::motion` integrator (the E001 keystone, unchanged). Drag lives
//! only on piloted ships, so projectiles and Tier-1 transit stay exactly
//! ballistic and the integrator↔analytic invariant is untouched.

use crate::clock::FixedDt;
use crate::components::{AngularVelocity, FlightAssist, Heading, Position, Ship, Velocity};
use crate::damage::Wreck;
use crate::fitting::ShipStats;
use crate::intent::ShipIntent;
use crate::motion::{integrate, BodyState};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// The flight magnitudes one step of [`ship_motion_system`] consumes — the
/// override-or-fallback view (FR-014/015). A fitted ship's [`ShipStats`] and the
/// global [`Tuning`] both project onto this exact field set, so the motion math
/// runs **identical formulae** regardless of source: only the numbers move from
/// the global resource to the per-entity component.
#[derive(Clone, Copy)]
struct FlightParams {
    thrust_force: f32,
    reverse_force: f32,
    strafe_force: f32,
    mass: f32,
    linear_drag: f32,
    turn_torque: f32,
    angular_drag: f32,
    angular_inertia: f32,
    turn_power_share: f32,
}

impl FlightParams {
    /// Fallback source: the global [`Tuning`] (unfitted ships — E001/E002/E003).
    fn from_tuning(t: &Tuning) -> Self {
        Self {
            thrust_force: t.thrust_force,
            reverse_force: t.reverse_force,
            strafe_force: t.strafe_force,
            mass: t.mass,
            linear_drag: t.linear_drag,
            turn_torque: t.turn_torque,
            angular_drag: t.angular_drag,
            angular_inertia: t.angular_inertia,
            turn_power_share: t.turn_power_share,
        }
    }

    /// Override source: the ship's fit-derived [`ShipStats`] (fitted ships).
    fn from_ship_stats(s: &ShipStats) -> Self {
        Self {
            thrust_force: s.thrust_force,
            reverse_force: s.reverse_force,
            strafe_force: s.strafe_force,
            mass: s.total_mass,
            linear_drag: s.linear_drag,
            turn_torque: s.turn_torque,
            angular_drag: s.angular_drag,
            angular_inertia: s.angular_inertia,
            turn_power_share: s.turn_power_share,
        }
    }

    /// Emergent max turn rate (decoupled mode), mirroring [`Tuning::max_turn_rate`].
    fn max_turn_rate(&self) -> f32 {
        self.turn_torque / self.angular_drag
    }
}

/// Available-thrust scale from the shared power budget: hard turning diverts
/// drive power, so translational thrust is multiplied by `1 - share·|turn|`
/// (clamped to `0..=1`). You cannot boost and hard-turn at once.
pub fn turn_power_factor(turn_input: f32, share: f32) -> f32 {
    (1.0 - share * turn_input.abs()).clamp(0.0, 1.0)
}

/// One step of angular velocity under `torque` vs `angular_drag`, smoothed by
/// the angular `inertia`. The steady-state turn rate at full input is
/// `torque / angular_drag`.
pub fn step_angular(
    omega: f32,
    turn_input: f32,
    torque: f32,
    angular_drag: f32,
    inertia: f32,
    dt: f32,
) -> f32 {
    let alpha = (torque * turn_input - angular_drag * omega) / inertia;
    omega + alpha * dt
}

/// Linear acceleration from a thrust vector opposed by linear drag, per unit
/// mass. Terminal velocity under steady thrust `F` is `|F| / drag`.
pub fn linear_accel(vel: Vec2, thrust: Vec2, drag: f32, mass: f32) -> Vec2 {
    (thrust - vel * drag) / mass
}

/// Fixed-step ship motion (FR-002/FR-003).
///
/// **Flight-model** (assist `On`, default): angular inertia turns the nose; the
/// shared power budget scales translational thrust by how hard you're turning;
/// drag gives an emergent top speed and bleeds momentum when thrust is cut.
/// **Decoupled** (assist `Off`): instant rotation, no drag — pure Newtonian
/// free-drift for advanced pilots.
///
/// Intent is **per-entity**: each piloted ship carries its own [`ShipIntent`]
/// component, so the server can drive N independently-controlled ships in one
/// shared step. A ship without the component is simply not piloted (no thrust);
/// AI-driven ships are steered by [`crate::ai::seek_system`] instead.
///
/// **Override-or-fallback flight source** (FR-014, the E006 rewire): a ship that
/// carries a fit-derived [`ShipStats`] component flies on **its** stats; a ship
/// without one falls back to the global [`Tuning`] resource. The two paths run
/// the **identical** motion math via [`step_ship_motion`] — only the source of the
/// numbers differs — so unfitted E001/E002/E003 ships keep their exact current
/// behavior while fitted ships are fit-driven.
pub fn ship_motion_system(
    tuning: Res<Tuning>,
    dt: Res<FixedDt>,
    // Fitted ships: per-entity ShipStats override the global Tuning.
    mut fitted: Query<
        (
            &ShipIntent,
            &mut Position,
            &mut Velocity,
            &mut Heading,
            &mut AngularVelocity,
            &mut FlightAssist,
            &ShipStats,
        ),
        With<Ship>,
    >,
    // Unfitted ships: fall back to the global Tuning (unchanged E001/E002/E003).
    mut unfitted: Query<
        (
            &ShipIntent,
            &mut Position,
            &mut Velocity,
            &mut Heading,
            &mut AngularVelocity,
            &mut FlightAssist,
        ),
        (With<Ship>, Without<ShipStats>),
    >,
) {
    let dt = dt.0;

    // Fitted: each ship uses its own fit-derived stats.
    for (intent, mut pos, mut vel, mut heading, mut omega, mut assist, stats) in &mut fitted {
        let params = FlightParams::from_ship_stats(stats);
        step_ship_motion(
            &params,
            dt,
            intent,
            &mut pos,
            &mut vel,
            &mut heading,
            &mut omega,
            &mut assist,
        );
    }

    // Unfitted: fall back to the single global Tuning.
    let fallback = FlightParams::from_tuning(&tuning);
    for (intent, mut pos, mut vel, mut heading, mut omega, mut assist) in &mut unfitted {
        step_ship_motion(
            &fallback,
            dt,
            intent,
            &mut pos,
            &mut vel,
            &mut heading,
            &mut omega,
            &mut assist,
        );
    }
}

/// One ship's fixed-step motion under the given [`FlightParams`] — the shared
/// formulae both the [`ShipStats`] override and the [`Tuning`] fallback run
/// (factored out so the two sources stay bit-identical; the math is unchanged
/// from the E002 model).
#[allow(clippy::too_many_arguments)]
fn step_ship_motion(
    p: &FlightParams,
    dt: f32,
    intent: &ShipIntent,
    pos: &mut Position,
    vel: &mut Velocity,
    heading: &mut Heading,
    omega: &mut AngularVelocity,
    assist: &mut FlightAssist,
) {
    if intent.toggle_assist {
        *assist = match *assist {
            FlightAssist::On => FlightAssist::Off,
            FlightAssist::Off => FlightAssist::On,
        };
    }

    let nose = Vec2::from_angle(heading.0);
    let left = Vec2::new(-nose.y, nose.x);
    // Reverse uses the weaker retro thrusters.
    let fwd_force = if intent.forward >= 0.0 {
        p.thrust_force
    } else {
        p.reverse_force
    };

    let accel = match *assist {
        FlightAssist::On => {
            omega.0 = step_angular(
                omega.0,
                intent.turn,
                p.turn_torque,
                p.angular_drag,
                p.angular_inertia,
                dt,
            );
            heading.0 += omega.0 * dt;
            let power = turn_power_factor(intent.turn, p.turn_power_share);
            let thrust = (nose * (intent.forward * fwd_force)
                + left * (intent.strafe * p.strafe_force))
                * power;
            linear_accel(vel.0, thrust, p.linear_drag, p.mass)
        }
        FlightAssist::Off => {
            // Decoupled: instant rotation, no drag (free Newtonian drift).
            omega.0 = intent.turn * p.max_turn_rate();
            heading.0 += omega.0 * dt;
            let thrust =
                nose * (intent.forward * fwd_force) + left * (intent.strafe * p.strafe_force);
            thrust / p.mass
        }
    };

    // Keep the heading bounded for f32 precision over long sessions.
    heading.0 = heading.0.rem_euclid(std::f32::consts::TAU);

    let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
    pos.0 = stepped.pos;
    vel.0 = stepped.vel;
}

/// Fixed-step **drift + tumble for `Wreck` bodies** (Phase M4): severed chunks and
/// destroyed-ship hulks coast on the velocity + spin they inherited at sever/death.
///
/// The piloted [`ship_motion_system`] is `With<Ship>`-gated (and `destroy_ship` strips the
/// `Ship` marker), so wreckage never moved before — it is driven HERE instead, as pure
/// Newtonian integration with **no thrust and no drag** (space is frictionless; a wreck's only
/// lifetime bound is [`WreckLifetime`](crate::components::WreckLifetime), not friction). Reuses
/// the shared [`integrate`] with zero acceleration so the linear path stays bit-identical in
/// style to a coasting ship; `Heading` advances by `ω·dt`, kept bounded with `rem_euclid(TAU)`
/// exactly like [`step_ship_motion`]. `MeshAnchor`/render is unaffected — world `Position`
/// drives the rendered mesh via interpolation. A world with no wrecks is a no-op.
pub fn wreck_motion_system(
    dt: Res<FixedDt>,
    mut q: Query<(&mut Position, &Velocity, &mut Heading, &AngularVelocity), With<Wreck>>,
) {
    let dt = dt.0;
    for (mut pos, vel, mut heading, omega) in &mut q {
        // accel = 0 → `integrate` reduces to `pos += vel·dt`, vel unchanged (frictionless coast).
        let stepped = integrate(BodyState::new(pos.0, vel.0), Vec2::ZERO, dt);
        pos.0 = stepped.pos;
        heading.0 = (heading.0 + omega.0 * dt).rem_euclid(std::f32::consts::TAU);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_power_factor_diverts_thrust_with_turn() {
        assert!((turn_power_factor(0.0, 0.7) - 1.0).abs() < 1e-6);
        assert!((turn_power_factor(1.0, 0.7) - 0.3).abs() < 1e-6);
        assert!((turn_power_factor(-1.0, 0.7) - 0.3).abs() < 1e-6); // magnitude
        assert_eq!(turn_power_factor(2.0, 0.7), 0.0); // clamped to 0
    }

    #[test]
    fn step_angular_converges_to_max_rate() {
        let (torque, drag, inertia, dt) = (12.0, 4.0, 1.2, 1.0 / 60.0);
        let mut omega = 0.0;
        for _ in 0..600 {
            omega = step_angular(omega, 1.0, torque, drag, inertia, dt);
        }
        assert!(
            (omega - torque / drag).abs() < 1e-2,
            "omega -> torque/drag (3.0)"
        );
    }

    #[test]
    fn linear_drag_gives_terminal_velocity() {
        let (thrust_force, drag, mass, dt) = (30.0, 0.375, 1.0, 1.0 / 60.0);
        let thrust = Vec2::new(thrust_force, 0.0);
        let mut state = BodyState::new(Vec2::ZERO, Vec2::ZERO);
        for _ in 0..3000 {
            let a = linear_accel(state.vel, thrust, drag, mass);
            state = integrate(state, a, dt);
        }
        let v_max = thrust_force / drag; // 80
        assert!(
            (state.vel.x - v_max).abs() < 0.5,
            "speed approaches terminal velocity {v_max}"
        );
        assert!(
            state.vel.x <= v_max + 0.1,
            "never exceeds terminal velocity"
        );
    }

    #[test]
    fn drag_bleeds_speed_when_coasting_but_zero_drag_coasts() {
        let v = Vec2::new(10.0, 0.0);
        assert_eq!(linear_accel(v, Vec2::ZERO, 0.0, 1.0), Vec2::ZERO);
        assert!(
            linear_accel(v, Vec2::ZERO, 0.5, 1.0).x < 0.0,
            "drag opposes motion when thrust is cut"
        );
    }
}
