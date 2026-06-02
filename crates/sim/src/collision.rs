//! Collision: pure math (swept point-vs-circle CCD, circle–circle contact, the
//! closed-form elastic 2-body impulse, the lethal-ram threshold) plus the ECS
//! systems that apply them — projectile hits and ship↔asteroid rams.
//!
//! Glam-only math, deterministic, engine-agnostic — the authoritative collision
//! math behind the `Physics` trait (ADR-0004). Same inputs → same outputs, so
//! there is never per-frame flicker.

use crate::combat::{self, HitFeedback};
use crate::components::{
    CollisionRadius, Damage, Health, Position, PrevPosition, Projectile, Ship, Target, TargetKind,
    Velocity,
};
use crate::physics::{Physics, RapierPhysics};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Ship inertial mass for ram impulses (asteroids are heavier, so the ship
/// bounces more).
const SHIP_MASS: f32 = 1.0;
/// Asteroid inertial mass for ram impulses.
const ASTEROID_MASS: f32 = 6.0;

/// Earliest time-of-impact `t ∈ [0, 1]` at which the point sweeping `p0`→`p1`
/// first touches the circle `(center, radius)`, or `None` if it never does
/// within the segment.
///
/// A tangent (closest-approach distance exactly `radius`) counts as a hit
/// (CHK027). A point that starts inside the circle hits at `t = 0`. Because the
/// whole swept segment is tested — not the endpoints — a fast projectile cannot
/// tunnel through a small target between frames (FR-006).
pub fn segment_circle_toi(p0: Vec2, p1: Vec2, center: Vec2, radius: f32) -> Option<f32> {
    let d = p1 - p0;
    let f = p0 - center;
    let a = d.dot(d);
    let r2 = radius * radius;
    if a <= f32::EPSILON {
        return if f.dot(f) <= r2 { Some(0.0) } else { None };
    }
    if f.dot(f) <= r2 {
        return Some(0.0);
    }
    let b = 2.0 * f.dot(d);
    let c = f.dot(f) - r2;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / (2.0 * a);
    if (0.0..=1.0).contains(&t) {
        Some(t)
    } else {
        None
    }
}

/// Static overlap of two circles: the push-out `normal` (unit vector pointing
/// from `b` toward `a`) and penetration `depth`, or `None` when separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub normal: Vec2,
    pub depth: f32,
}

/// Detect a circle–circle overlap.
pub fn circle_contact(a: Vec2, a_radius: f32, b: Vec2, b_radius: f32) -> Option<Contact> {
    let delta = a - b;
    let dist = delta.length();
    let sum = a_radius + b_radius;
    if dist < sum {
        let normal = if dist > f32::EPSILON {
            delta / dist
        } else {
            Vec2::X
        };
        Some(Contact {
            normal,
            depth: sum - dist,
        })
    } else {
        None
    }
}

/// Closed-form elastic 2-body collision (restitution = 1). Returns the new
/// velocities `(a, b)`. Conserves total linear momentum; if the bodies are
/// already separating along the contact normal, velocities are unchanged.
pub fn elastic_velocities(
    pa: Vec2,
    va: Vec2,
    ma: f32,
    pb: Vec2,
    vb: Vec2,
    mb: f32,
) -> (Vec2, Vec2) {
    let delta = pa - pb;
    let dist = delta.length();
    let n = if dist > f32::EPSILON {
        delta / dist
    } else {
        Vec2::X
    };
    let vn = (va - vb).dot(n);
    if vn >= 0.0 {
        return (va, vb);
    }
    let inv = 1.0 / ma + 1.0 / mb;
    let j = -2.0 * vn / inv;
    let impulse = n * j;
    (va + impulse / ma, vb - impulse / mb)
}

/// A closing speed at or above the threshold is a lethal ram (boundary
/// inclusive, CHK010).
pub fn is_lethal_ram(closing_speed: f32, threshold: f32) -> bool {
    closing_speed >= threshold
}

/// Fixed-step projectile collision (FR-006/FR-007): each projectile is swept
/// from its previous to current position against every target circle. On the
/// first hit the target takes damage, the projectile despawns, and hit feedback
/// is raised. Damage is order-independent across simultaneous hits.
pub fn collision_detect_system(
    mut commands: Commands,
    mut feedback: ResMut<HitFeedback>,
    projectiles: Query<(Entity, &Position, &PrevPosition, &Damage), With<Projectile>>,
    mut targets: Query<(&Position, &CollisionRadius, &mut Health), With<Target>>,
) {
    let physics = RapierPhysics::new();
    for (projectile, pos, prev, dmg) in &projectiles {
        for (tpos, radius, mut health) in &mut targets {
            if physics
                .swept_cast(prev.0, pos.0, tpos.0, radius.0)
                .is_some()
            {
                health.0 = combat::apply_damage(health.0, dmg.0);
                feedback.hit_flash = combat::FLASH_TIME;
                commands.entity(projectile).despawn();
                break; // a projectile strikes at most one target
            }
        }
    }
}

/// Fixed-step ship↔asteroid rams (FR-009/FR-010): on contact, exchange momentum
/// via the closed-form elastic impulse (motion stays sim-authoritative, AD-003);
/// if the closing speed is lethal, deplete the ship's health (destruction is
/// handled by `combat::destruction_system`).
pub fn ram_collision_system(
    tuning: Res<Tuning>,
    mut ship_q: Query<(&Position, &mut Velocity, &mut Health, &CollisionRadius), With<Ship>>,
    mut asteroids: Query<
        (&Position, &mut Velocity, &CollisionRadius, &TargetKind),
        (With<Target>, Without<Ship>),
    >,
) {
    let Some((ship_pos, mut ship_vel, mut ship_health, ship_radius)) = ship_q.iter_mut().next()
    else {
        return;
    };
    let ship_pos = ship_pos.0;
    let ship_radius = ship_radius.0;
    let physics = RapierPhysics::new();

    for (apos, mut avel, aradius, kind) in &mut asteroids {
        if *kind != TargetKind::Asteroid {
            continue;
        }
        if physics
            .contact(ship_pos, ship_radius, apos.0, aradius.0)
            .is_some()
        {
            let closing = (ship_vel.0 - avel.0).length();
            let (new_ship, new_ast) = elastic_velocities(
                ship_pos,
                ship_vel.0,
                SHIP_MASS,
                apos.0,
                avel.0,
                ASTEROID_MASS,
            );
            ship_vel.0 = new_ship;
            avel.0 = new_ast;
            if is_lethal_ram(closing, tuning.lethal_ram_speed) {
                ship_health.0 = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2, tol: f32) -> bool {
        (a - b).length() <= tol
    }

    #[test]
    fn swept_hits_small_fast_target_no_tunnel() {
        let hit = segment_circle_toi(
            Vec2::new(-100.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::ZERO,
            0.5,
        );
        let t = hit.expect("fast sweep across the circle must register a hit");
        assert!((0.0..=1.0).contains(&t));
        assert!(t < 0.5, "entry should be on the approaching half");
    }

    #[test]
    fn swept_misses_when_path_clears_circle() {
        assert_eq!(
            segment_circle_toi(
                Vec2::new(-100.0, 5.0),
                Vec2::new(100.0, 5.0),
                Vec2::ZERO,
                0.5
            ),
            None
        );
    }

    #[test]
    fn grazing_tangent_counts_as_hit() {
        let r = 1.0;
        assert!(
            segment_circle_toi(Vec2::new(-10.0, r), Vec2::new(10.0, r), Vec2::ZERO, r).is_some()
        );
    }

    #[test]
    fn point_starting_inside_hits_at_zero() {
        assert_eq!(
            segment_circle_toi(Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::ZERO, 1.0),
            Some(0.0)
        );
    }

    #[test]
    fn circle_contact_detects_overlap_and_separation() {
        let c = circle_contact(Vec2::ZERO, 1.0, Vec2::new(1.5, 0.0), 1.0).expect("overlap");
        // Normal points from `b` (at +x) toward `a` (at origin), i.e. -x.
        assert!(close(c.normal, Vec2::new(-1.0, 0.0), 1e-4));
        assert!((c.depth - 0.5).abs() < 1e-4);
        assert_eq!(
            circle_contact(Vec2::ZERO, 1.0, Vec2::new(3.0, 0.0), 1.0),
            None
        );
    }

    #[test]
    fn elastic_equal_mass_headon_conserves_momentum_and_separates() {
        let (pa, va, pb, vb) = (
            Vec2::new(-1.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(-1.0, 0.0),
        );
        let (na, nb) = elastic_velocities(pa, va, 1.0, pb, vb, 1.0);
        assert!(close(va + vb, na + nb, 1e-4));
        let n = (pa - pb).normalize();
        assert!((na - nb).dot(n) >= 0.0);
    }

    #[test]
    fn elastic_separating_bodies_unchanged() {
        let (va, vb) = (Vec2::new(-1.0, 0.0), Vec2::new(1.0, 0.0));
        assert_eq!(
            elastic_velocities(Vec2::new(-1.0, 0.0), va, 1.0, Vec2::new(1.0, 0.0), vb, 1.0),
            (va, vb)
        );
    }

    #[test]
    fn lethal_ram_threshold_is_inclusive() {
        assert!(is_lethal_ram(40.0, 40.0));
        assert!(!is_lethal_ram(39.99, 40.0));
    }

    #[test]
    fn thin_target_still_hit_at_small_radius() {
        // A very small/thin target on a long fast sweep must still register
        // (the no-tunneling guarantee holds down to small radii — CHK028).
        let hit = segment_circle_toi(
            Vec2::new(-200.0, 0.0),
            Vec2::new(200.0, 0.0),
            Vec2::ZERO,
            0.1,
        );
        assert!(hit.is_some(), "swept test must hit even a thin target");
    }
}
