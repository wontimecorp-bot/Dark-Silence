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
use crate::intent::ShipIntent;
use crate::motion::{integrate, BodyState};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

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
pub fn ship_motion_system(
    intent: Res<ShipIntent>,
    tuning: Res<Tuning>,
    dt: Res<FixedDt>,
    mut q: Query<
        (
            &mut Position,
            &mut Velocity,
            &mut Heading,
            &mut AngularVelocity,
            &mut FlightAssist,
        ),
        With<Ship>,
    >,
) {
    let dt = dt.0;
    let t = &*tuning;
    for (mut pos, mut vel, mut heading, mut omega, mut assist) in &mut q {
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
            t.thrust_force
        } else {
            t.reverse_force
        };

        let accel = match *assist {
            FlightAssist::On => {
                omega.0 = step_angular(
                    omega.0,
                    intent.turn,
                    t.turn_torque,
                    t.angular_drag,
                    t.angular_inertia,
                    dt,
                );
                heading.0 += omega.0 * dt;
                let power = turn_power_factor(intent.turn, t.turn_power_share);
                let thrust = (nose * (intent.forward * fwd_force)
                    + left * (intent.strafe * t.strafe_force))
                    * power;
                linear_accel(vel.0, thrust, t.linear_drag, t.mass)
            }
            FlightAssist::Off => {
                // Decoupled: instant rotation, no drag (free Newtonian drift).
                omega.0 = intent.turn * t.max_turn_rate();
                heading.0 += omega.0 * dt;
                let thrust =
                    nose * (intent.forward * fwd_force) + left * (intent.strafe * t.strafe_force);
                thrust / t.mass
            }
        };

        // Keep the heading bounded for f32 precision over long sessions.
        heading.0 = heading.0.rem_euclid(std::f32::consts::TAU);

        let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
        pos.0 = stepped.pos;
        vel.0 = stepped.vel;
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
