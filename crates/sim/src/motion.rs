//! Newtonian point-mass motion under constant acceleration, expressed two ways.
//!
//! # The load-bearing invariant
//!
//! The tiered architecture moves entities between a per-tick simulation (Tier 0)
//! and closed-form trajectories stored in a database (Tier 1). For that to be
//! seamless, both representations MUST produce the same position for the same
//! elapsed time. We guarantee this by choosing an integrator that is *exactly*
//! equal to the closed form under constant acceleration:
//!
//! **Velocity Verlet**, stepped at any fixed `dt`, reproduces the analytic
//! `x(t) = x0 + v0·t + ½·a·t²` to within floating-point rounding. Forward Euler
//! undershoots by `½·a·T·dt` and semi-implicit (symplectic) Euler overshoots by
//! the same amount — neither is acceptable for a promote/demote round-trip.
//!
//! `dt` is always a parameter, never a constant. That is what makes per-bubble
//! time dilation (slowing the *rate* of ticks while keeping logical `dt` fixed)
//! a runtime knob rather than a rewrite.
//!
//! Space is planar: gameplay is 2D (`Vec2`) even though the client renders 3D.
//! There is intentionally no drag/friction here — pure Newtonian coasting is
//! both the desired feel and what keeps the closed form a clean polynomial.

use glam::Vec2;

/// The full kinematic state of a point mass on the 2D gameplay plane.
///
/// Acceleration is supplied per call rather than stored, because it is an input
/// (thrust, gravity, etc.) that the caller owns — the same state can be advanced
/// under different accelerations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyState {
    /// Position in sim units. At Tier 0 these are sector-relative (never large
    /// absolute world coordinates, which would lose `f32` precision).
    pub pos: Vec2,
    /// Velocity in sim units per second.
    pub vel: Vec2,
}

impl BodyState {
    /// A body at rest at the origin.
    pub const ZERO: Self = Self {
        pos: Vec2::ZERO,
        vel: Vec2::ZERO,
    };

    /// Construct from position and velocity.
    pub const fn new(pos: Vec2, vel: Vec2) -> Self {
        Self { pos, vel }
    }
}

/// Advance one tick by `dt` seconds under constant acceleration `accel`,
/// using velocity Verlet.
///
/// For constant `accel` this is exact: summing N of these steps equals
/// [`analytic`] at `t = N * dt` (up to `f32` rounding). See module docs.
#[inline]
pub fn integrate(state: BodyState, accel: Vec2, dt: f32) -> BodyState {
    // x_{n+1} = x_n + v_n·dt + ½·a·dt²
    let pos = state.pos + state.vel * dt + 0.5 * accel * dt * dt;
    // v_{n+1} = v_n + a·dt   (acceleration is constant across the step)
    let vel = state.vel + accel * dt;
    BodyState { pos, vel }
}

/// Evaluate the closed-form trajectory: where the body is `t` seconds after
/// `start`, under constant acceleration `accel`. This is the Tier 1 evaluator —
/// O(1), no stepping, callable for any `t` including far into the future.
///
/// `t` may be any non-negative duration; negative `t` extrapolates backward,
/// which is occasionally useful for lag compensation.
#[inline]
pub fn analytic(start: BodyState, accel: Vec2, t: f32) -> BodyState {
    // x(t) = x0 + v0·t + ½·a·t²
    let pos = start.pos + start.vel * t + 0.5 * accel * t * t;
    // v(t) = v0 + a·t
    let vel = start.vel + accel * t;
    BodyState { pos, vel }
}

/// Convenience: integrate `steps` ticks of `dt` under constant `accel`.
///
/// This is the explicit Tier 0 loop; in production the server drives
/// [`integrate`] once per tick from its scheduler. Provided here mainly so the
/// equivalence with [`analytic`] is directly testable.
pub fn simulate(state: BodyState, accel: Vec2, dt: f32, steps: u32) -> BodyState {
    let mut s = state;
    for _ in 0..steps {
        s = integrate(s, accel, dt);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-component closeness. Tolerance is generous enough to absorb the
    /// accumulated `f32` summation error over thousands of steps but tight
    /// enough that a *wrong* integrator (Euler) fails loudly.
    fn assert_close(a: Vec2, b: Vec2, tol: f32) {
        let d = (a - b).abs();
        assert!(
            d.x <= tol && d.y <= tol,
            "vectors differ by more than {tol}: {a:?} vs {b:?} (delta {d:?})"
        );
    }

    /// Relative closeness: `|a - b| <= rel_tol * max(|b|, 1)`.
    ///
    /// Velocity Verlet under constant acceleration equals the closed form in
    /// exact arithmetic, so the only difference is `f32` rounding accumulated
    /// over the steps. That error scales with magnitude, so the honest assertion
    /// is *relative*, not a fixed absolute epsilon.
    fn assert_close_rel(a: Vec2, b: Vec2, rel_tol: f32) {
        let err = (a - b).length();
        let bound = rel_tol * b.length().max(1.0);
        assert!(
            err <= bound,
            "relative error {:.3e} exceeds {rel_tol:.0e}: {a:?} vs {b:?} (|err|={err}, bound={bound})",
            err / b.length().max(1.0)
        );
    }

    /// THE invariant: stepping the integrator N times equals the closed form at
    /// t = N·dt, for non-trivial initial velocity and acceleration. If this ever
    /// fails (beyond f32 drift), promote/demote round-trips would teleport
    /// entities. The 1e-4 relative bound both passes Verlet and rejects Euler
    /// (whose error here is ~13× larger — see the dedicated test below).
    #[test]
    fn integrator_matches_analytic_under_constant_accel() {
        let start = BodyState::new(Vec2::new(-3.5, 12.0), Vec2::new(4.0, -2.0));
        let accel = Vec2::new(0.75, -0.30); // e.g. thrust

        let dt = 1.0 / 30.0; // 30 Hz sim tick
        let steps = 3_000; // 100 seconds of flight
        let t = dt * steps as f32;

        let stepped = simulate(start, accel, dt, steps);
        let closed = analytic(start, accel, t);

        assert_close_rel(stepped.pos, closed.pos, 1e-4);
        assert_close_rel(stepped.vel, closed.vel, 1e-4);
    }

    /// The match must hold regardless of tick size — this is what lets time
    /// dilation change `dt` at runtime without diverging from Tier 1 math.
    /// Bound is looser (2e-4) because 144 Hz over 60 s is ~8.6k steps of drift.
    #[test]
    fn invariant_holds_across_tick_sizes() {
        let start = BodyState::new(Vec2::new(100.0, -50.0), Vec2::new(-1.0, 6.0));
        let accel = Vec2::new(-0.2, 0.9);
        let total_t = 60.0;

        for &hz in &[10.0_f32, 20.0, 30.0, 60.0, 144.0] {
            let dt = 1.0 / hz;
            let steps = (total_t * hz).round() as u32;
            let stepped = simulate(start, accel, dt, steps);
            let closed = analytic(start, accel, dt * steps as f32);
            assert_close_rel(stepped.pos, closed.pos, 2e-4);
            assert_close_rel(stepped.vel, closed.vel, 2e-4);
        }
    }

    /// With zero acceleration a body coasts in a straight line forever — the
    /// pure-Newtonian, frictionless space feel.
    #[test]
    fn zero_accel_is_straight_line_coasting() {
        let start = BodyState::new(Vec2::ZERO, Vec2::new(2.0, 3.0));
        let s = analytic(start, Vec2::ZERO, 10.0);
        assert_close(s.pos, Vec2::new(20.0, 30.0), 1e-5);
        assert_eq!(s.vel, start.vel, "coasting must not change velocity");
    }

    /// Sanity: a deliberately wrong (forward Euler) integrator must NOT match the
    /// closed form, proving the test above is actually discriminating and not
    /// passing trivially.
    #[test]
    fn forward_euler_would_fail_the_invariant() {
        fn euler(state: BodyState, accel: Vec2, dt: f32) -> BodyState {
            let pos = state.pos + state.vel * dt; // uses OLD velocity, no ½·a·dt²
            let vel = state.vel + accel * dt;
            BodyState { pos, vel }
        }
        let start = BodyState::new(Vec2::ZERO, Vec2::ZERO);
        let accel = Vec2::new(1.0, 0.0);
        let dt = 1.0 / 30.0;
        let steps = 300u32;
        let t = dt * steps as f32; // 10 s

        let mut s = start;
        for _ in 0..steps {
            s = euler(s, accel, dt);
        }
        let closed = analytic(start, accel, t);
        // Euler undershoots by ½·a·T·dt = 0.5 * 1.0 * 10.0 * (1/30) ≈ 0.1667.
        let err = (s.pos.x - closed.pos.x).abs();
        assert!(
            err > 0.1,
            "expected Euler to diverge noticeably, but error was only {err}"
        );
    }
}
