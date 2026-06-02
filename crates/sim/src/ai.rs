//! Seeking-target AI: a pure steering helper plus the fixed-step target-motion
//! system. Seekers thrust toward the player; asteroids drift at constant
//! velocity; dummies stay put. Observable behaviour (a thrust vector pointing
//! at the target) is what the spec requires (CHK013).

use crate::clock::FixedDt;
use crate::components::{Position, Ship, Target, TargetKind, Velocity};
use crate::motion::{integrate, BodyState};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Acceleration that steers `seeker` toward `target` at magnitude `thrust`.
/// Zero when the two coincide (avoids normalizing a zero vector to NaN).
pub fn seek_accel(seeker: Vec2, target: Vec2, thrust: f32) -> Vec2 {
    let d = target - seeker;
    let len = d.length();
    if len > f32::EPSILON {
        d / len * thrust
    } else {
        Vec2::ZERO
    }
}

/// Fixed-step motion for all targets (FR-008/FR-012). Seekers accelerate toward
/// the player; asteroids and dummies receive zero acceleration, so an asteroid
/// drifts on its constant velocity and a dummy (zero velocity) stays put. All
/// reuse the E001 `integrate` keystone.
pub fn seek_system(
    tuning: Res<Tuning>,
    dt: Res<FixedDt>,
    ship_q: Query<&Position, With<Ship>>,
    mut targets: Query<(&mut Position, &mut Velocity, &TargetKind), (With<Target>, Without<Ship>)>,
) {
    let dt = dt.0;
    let player = ship_q.iter().next().map(|p| p.0);
    for (mut pos, mut vel, kind) in &mut targets {
        let accel = match (*kind, player) {
            (TargetKind::Seeker, Some(player_pos)) => {
                seek_accel(pos.0, player_pos, tuning.thrust_force / tuning.mass)
            }
            _ => Vec2::ZERO,
        };
        let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
        pos.0 = stepped.pos;
        vel.0 = stepped.vel;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeks_toward_target() {
        let a = seek_accel(Vec2::ZERO, Vec2::new(10.0, 0.0), 5.0);
        assert!((a - Vec2::new(5.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn coincident_is_zero_not_nan() {
        let p = Vec2::new(1.0, 1.0);
        assert_eq!(seek_accel(p, p, 5.0), Vec2::ZERO);
    }
}
