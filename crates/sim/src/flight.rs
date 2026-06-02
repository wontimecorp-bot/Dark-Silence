//! Flight dynamics: pure helpers (input → acceleration, the flight-assist
//! transform, speed clamping) plus the fixed-step ECS ship-motion system that
//! composes them with the `sim::motion` integrator.

use crate::clock::FixedDt;
use crate::components::{FlightAssist, Heading, Position, Ship, Velocity};
use crate::intent::ShipIntent;
use crate::motion::{integrate, BodyState};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// World-space acceleration from the pilot's discrete inputs. `forward` and
/// `strafe` are each in `-1..=1`. Forward thrust acts along `heading`; strafe
/// acts perpendicular (to the left of heading).
pub fn input_accel(
    heading: f32,
    forward: f32,
    strafe: f32,
    thrust_accel: f32,
    strafe_accel: f32,
) -> Vec2 {
    let fwd = Vec2::from_angle(heading);
    let left = Vec2::new(-fwd.y, fwd.x);
    fwd * (forward * thrust_accel) + left * (strafe * strafe_accel)
}

/// Apply flight-assist for one step of `dt` seconds.
///
/// `Off` (decoupled) returns the velocity unchanged — full Newtonian momentum,
/// heading independent of motion. `On` damps drift by easing the velocity
/// toward `speed · heading` at rate `damping`, so the ship trends toward where
/// it points without changing speed abruptly. A single step only nudges the
/// vector — the toggle never snaps velocity (INV-07).
pub fn flight_assist(vel: Vec2, heading: f32, mode: FlightAssist, damping: f32, dt: f32) -> Vec2 {
    match mode {
        FlightAssist::Off => vel,
        FlightAssist::On => {
            let dir = Vec2::from_angle(heading);
            let target = dir * vel.length();
            let t = (damping * dt).clamp(0.0, 1.0);
            vel.lerp(target, t)
        }
    }
}

/// Clamp speed to `max` (INV-02), preserving direction.
pub fn clamp_speed(vel: Vec2, max: f32) -> Vec2 {
    let s = vel.length();
    if s > max && s > f32::EPSILON {
        vel / s * max
    } else {
        vel
    }
}

/// Fixed-step ship motion (FR-002/FR-003): turn from input, integrate thrust
/// via the E001 keystone, apply flight-assist, clamp speed. Coasts when input
/// is zero (no thrust → constant velocity).
pub fn ship_motion_system(
    intent: Res<ShipIntent>,
    tuning: Res<Tuning>,
    dt: Res<FixedDt>,
    mut q: Query<
        (
            &mut Position,
            &mut Velocity,
            &mut Heading,
            &mut FlightAssist,
        ),
        With<Ship>,
    >,
) {
    let dt = dt.0;
    for (mut pos, mut vel, mut heading, mut assist) in &mut q {
        if intent.toggle_assist {
            *assist = match *assist {
                FlightAssist::On => FlightAssist::Off,
                FlightAssist::Off => FlightAssist::On,
            };
        }
        heading.0 += intent.turn * tuning.rotation_rate * dt;
        let accel = input_accel(
            heading.0,
            intent.forward,
            intent.strafe,
            tuning.thrust_accel,
            tuning.strafe_accel,
        );
        let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
        let assisted = flight_assist(stepped.vel, heading.0, *assist, tuning.assist_damping, dt);
        pos.0 = stepped.pos;
        vel.0 = clamp_speed(assisted, tuning.max_speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assist_off_preserves_velocity_vector() {
        let v = Vec2::new(3.0, 4.0);
        assert_eq!(flight_assist(v, 1.2, FlightAssist::Off, 4.0, 1.0 / 60.0), v);
    }

    #[test]
    fn assist_on_trends_velocity_toward_heading() {
        let v = Vec2::new(0.0, 5.0); // moving +y
        let after = flight_assist(v, 0.0, FlightAssist::On, 4.0, 1.0 / 60.0); // heading +x
        assert!(
            after.x > v.x,
            "velocity should gain a component toward heading"
        );
        assert!(after.y < v.y, "off-heading drift should be damped");
        assert!(
            (after.length() - v.length()).abs() < 0.5,
            "assist eases direction, it does not bleed speed"
        );
    }

    #[test]
    fn assist_on_one_step_never_snaps() {
        let v = Vec2::new(0.0, 5.0);
        let after = flight_assist(v, 0.0, FlightAssist::On, 4.0, 1.0 / 60.0);
        assert!(
            after.distance(v) < 1.0,
            "a single 60 Hz step is a nudge, not a snap"
        );
    }

    #[test]
    fn clamp_caps_speed() {
        assert!((clamp_speed(Vec2::new(100.0, 0.0), 80.0).length() - 80.0).abs() < 1e-4);
        let slow = Vec2::new(1.0, 0.0);
        assert_eq!(clamp_speed(slow, 80.0), slow);
    }

    #[test]
    fn input_accel_thrust_is_along_heading() {
        let a = input_accel(0.0, 1.0, 0.0, 30.0, 20.0);
        assert!((a - Vec2::new(30.0, 0.0)).length() < 1e-4);
    }
}
