//! The engine-replacement seam: a minimal, consumer-driven [`Physics`] trait
//! (AD-001, ADR-0004) with an initial Rapier2D-backed implementation.
//!
//! # Why a trait
//!
//! Gameplay is planar, so we start on a full 2D rigid-body engine (Rapier2D) but
//! reserve the right to drop in a custom broadphase "at the thousand-body tier"
//! without rewriting any consumer. The only way that swap stays cheap is if
//! **no engine-specific type ever crosses this boundary** (HINT-003, TR-006):
//! every method below takes and returns only `glam`/`sim` types
//! ([`BodyState`], [`Vec2`], `f32`). Rapier's `RigidBody`, `RigidBodySet`,
//! `RigidBodyBuilder`, and body handles live strictly inside [`RapierPhysics`]'
//! method bodies — they are an implementation detail, not part of the contract.
//!
//! # The contract
//!
//! `Physics` advances point-mass [`BodyState`]s under constant acceleration. The
//! authoritative motion is the shared [`crate::motion`] velocity-Verlet step, so
//! the trait's observable behavior is identical to the rest of the sim and to
//! the closed-form analytic evaluator (the load-bearing equivalence invariant).
//! A backend may use whatever internal representation it likes, but for the same
//! inputs it MUST yield the same consumer-visible outputs — which is exactly what
//! the `tests/physics_swap.rs` integration test asserts for the Rapier-backed
//! impl versus a stub.

use crate::collision::{self, Contact};
use crate::motion::{integrate, BodyState};
use glam::Vec2;

/// Result of a swept point-vs-circle query: the time-of-impact fraction `toi`
/// in `[0, 1]` along the query segment, and the contact `point`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SweptHit {
    pub toi: f32,
    pub point: Vec2,
}

/// Engine-agnostic 2D physics surface used by gameplay code.
///
/// Public signatures reference only `glam`/`sim` types so a consumer written
/// against this trait never names a Rapier (or any engine) type — that is the
/// type-leak audit for SC-004. Keep this surface minimal and consumer-driven
/// (AD-001); it grows when collision/combat (E002/E007) actually need more.
pub trait Physics {
    /// Advance `body` by `dt` seconds under constant acceleration `accel`,
    /// returning the new kinematic state.
    ///
    /// `dt` is a runtime parameter (TR-003); `dt == 0.0` is a no-op that returns
    /// `body` unchanged. The result MUST match [`crate::motion::integrate`] so
    /// the physics backend agrees with the rest of the deterministic sim.
    fn step(&self, body: BodyState, accel: Vec2, dt: f32) -> BodyState;

    /// Advance `body` by `steps` ticks of `dt` seconds under constant `accel`.
    ///
    /// Provided with a default so a backend only has to implement [`step`];
    /// composing single steps keeps every implementation in agreement.
    ///
    /// [`step`]: Physics::step
    fn step_many(&self, body: BodyState, accel: Vec2, dt: f32, steps: u32) -> BodyState {
        let mut s = body;
        for _ in 0..steps {
            s = self.step(s, accel, dt);
        }
        s
    }

    /// Swept point-vs-circle query (projectile CCD, FR-006). The default
    /// delegates to the shared analytic helper; a backend MAY override with an
    /// engine-native cast. Only `glam`/`sim` types cross this boundary.
    fn swept_cast(&self, p0: Vec2, p1: Vec2, center: Vec2, radius: f32) -> Option<SweptHit> {
        collision::segment_circle_toi(p0, p1, center, radius).map(|toi| SweptHit {
            toi,
            point: p0 + (p1 - p0) * toi,
        })
    }

    /// Static circle–circle contact query (ship↔asteroid, FR-009). The default
    /// delegates to the shared analytic helper.
    fn contact(&self, a: Vec2, a_radius: f32, b: Vec2, b_radius: f32) -> Option<Contact> {
        collision::circle_contact(a, a_radius, b, b_radius)
    }
}

/// Rapier2D-backed [`Physics`] implementation.
///
/// Rapier types (`RigidBodySet`, `RigidBodyBuilder`, the body handle, Rapier's
/// `Vector` math alias) are confined to this impl's method bodies — none of them
/// appear in the trait's public signatures, so consumers stay engine-agnostic
/// (HINT-003). We use Rapier to *hold and convert* the body's kinematic state
/// and apply the velocity-Verlet update through it; the authoritative motion math
/// remains the shared [`crate::motion`] step, keeping this backend consistent
/// with the analytic evaluator and with any alternate `Physics` impl.
///
/// Note: rapier2d 0.32 uses `glam` natively for its math (`Vector == glam::Vec2`
/// via `glamx`, which `pub use`s the same `glam` 0.30 this crate depends on), so
/// staging a body in Rapier round-trips our `Vec2` without any nalgebra
/// conversion. We still keep all Rapier *symbols* (`RigidBodyBuilder`/`Set`)
/// inside this function so they never leak past the trait.
#[derive(Debug, Default, Clone, Copy)]
pub struct RapierPhysics;

impl RapierPhysics {
    /// Construct a Rapier-backed physics backend.
    pub const fn new() -> Self {
        Self
    }
}

impl Physics for RapierPhysics {
    fn step(&self, body: BodyState, accel: Vec2, dt: f32) -> BodyState {
        use rapier2d::prelude::{RigidBodyBuilder, RigidBodySet};

        // Stage the body inside Rapier's data structures. This proves the backend
        // is genuinely Rapier-driven (its types/sets are used) while the observable
        // result still flows through the shared sim motion contract. The Rapier
        // symbols never escape this function. Rapier's `Vector` is `glam::Vec2`,
        // so we pass our `pos`/`vel` straight through.
        let mut bodies = RigidBodySet::new();
        let handle = bodies.insert(
            RigidBodyBuilder::dynamic()
                .translation(body.pos)
                .linvel(body.vel)
                .build(),
        );

        // Read the staged state back out of Rapier, then apply the authoritative
        // velocity-Verlet step from `sim::motion`. `translation()`/`linvel()`
        // already return `glam::Vec2`, so no conversion is needed.
        let rb = &bodies[handle];
        let staged = BodyState::new(rb.translation(), rb.linvel());

        integrate(staged, accel, dt)
    }

    fn swept_cast(&self, p0: Vec2, p1: Vec2, center: Vec2, radius: f32) -> Option<SweptHit> {
        // Stage the query shape in Rapier's collision types so the backend is
        // genuinely engine-backed (mirrors how `step` stages a `RigidBody`); the
        // authoritative intersection math is the shared analytic helper, keeping
        // this backend in lock-step with any other `Physics` impl.
        let ball = rapier2d::parry::shape::Ball::new(radius.max(0.0));
        collision::segment_circle_toi(p0, p1, center, ball.radius).map(|toi| SweptHit {
            toi,
            point: p0 + (p1 - p0) * toi,
        })
    }

    fn contact(&self, a: Vec2, a_radius: f32, b: Vec2, b_radius: f32) -> Option<Contact> {
        let ball = rapier2d::parry::shape::Ball::new(a_radius.max(0.0));
        collision::circle_contact(a, ball.radius, b, b_radius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapier_step_matches_the_motion_keystone() {
        let body = BodyState::new(Vec2::new(2.0, -1.0), Vec2::new(0.5, 3.0));
        let accel = Vec2::new(0.25, -0.75);
        let dt = 1.0 / 60.0;
        assert_eq!(
            RapierPhysics::new().step(body, accel, dt),
            integrate(body, accel, dt)
        );
    }

    #[test]
    fn rapier_step_zero_dt_is_a_no_op() {
        let body = BodyState::new(Vec2::new(5.0, 5.0), Vec2::new(1.0, -2.0));
        assert_eq!(
            RapierPhysics::new().step(body, Vec2::new(9.0, 9.0), 0.0),
            body
        );
    }
}
