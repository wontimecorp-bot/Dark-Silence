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
use serde::{Deserialize, Serialize};

/// The full kinematic state of a point mass on the 2D gameplay plane.
///
/// Acceleration is supplied per call rather than stored, because it is an input
/// (thrust, gravity, etc.) that the caller owns — the same state can be advanced
/// under different accelerations.
///
/// `Serialize`/`Deserialize` are derived (TR-008, AD-002) so the same type can
/// be replicated over the wire (E003) and persisted (E004) without rework; the
/// round-trip is value-preserving under `PartialEq` (see the `serde_round_trip`
/// test below).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
///
/// # Runtime-`dt` contract (TR-003)
///
/// `dt` is a *runtime* parameter, never a compile-time constant. Callers supply
/// it per tick, so a time-dilated bubble can change its wall-clock tick rate
/// while keeping the logical step size whatever it chooses — the equivalence
/// with [`analytic`] holds for any `dt` (verified across {10,20,30,60,144} Hz).
/// `dt == 0.0` is a well-defined no-op: the returned state equals the input
/// (`pos`/`vel` unchanged), since every `dt`-scaled term vanishes.
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

/// Apply a **linear impulse** `j` (momentum, sim-units·mass/s) to a body of inertial `mass` →
/// the new velocity `v + j/mass` (Phase M4). An impulse is an instantaneous change in momentum
/// (`Δp = j`, `Δv = j/mass`), unlike a force applied over `dt`; this is the shared primitive for
/// projectile knockback + firing recoil. Pure; the caller writes the result back. `mass` is
/// floored away from `0` so a degenerate body never divides by zero.
#[inline]
pub fn apply_linear_impulse(vel: Vec2, j: Vec2, mass: f32) -> Vec2 {
    vel + j / mass.max(f32::MIN_POSITIVE)
}

/// Apply an **off-centre impulse** `j` at arm `r` (the contact point relative to the body's
/// centre of mass) to a body of moment of inertia `inertia` → the new angular velocity
/// `ω + (r × j)/inertia` (Phase M4). The 2D cross product `r × j = r.perp_dot(j)` is the torque
/// impulse, so a hit off the COM spins the body (tumble). Pure; `inertia` is floored away from
/// `0`. (Pair with [`apply_linear_impulse`] for the same `j` to get the full rigid-body response.)
#[inline]
pub fn apply_angular_impulse(omega: f32, r: Vec2, j: Vec2, inertia: f32) -> f32 {
    omega + r.perp_dot(j) / inertia.max(f32::MIN_POSITIVE)
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

    #[test]
    fn linear_impulse_changes_velocity_by_j_over_mass() {
        // Δv = j/m, added to the existing velocity.
        let v = apply_linear_impulse(Vec2::new(1.0, 0.0), Vec2::new(6.0, -4.0), 2.0);
        assert_close(v, Vec2::new(1.0 + 3.0, -2.0), 1e-6);
        // A zero/degenerate mass is floored (no NaN/inf), so the result stays finite.
        assert!(apply_linear_impulse(Vec2::ZERO, Vec2::new(1.0, 0.0), 0.0).is_finite());
    }

    #[test]
    fn angular_impulse_spins_by_cross_over_inertia() {
        // An off-centre impulse (arm ⟂ impulse) imparts spin = (r × j)/I.
        // arm (1,0), j (0,3): r×j = 1·3 − 0·0 = 3; I = 1.5 → Δω = 2.0.
        let w = apply_angular_impulse(0.5, Vec2::new(1.0, 0.0), Vec2::new(0.0, 3.0), 1.5);
        assert!((w - (0.5 + 2.0)).abs() < 1e-6, "ω += (r×j)/I; got {w}");
        // A CENTERED hit (arm ∥ impulse) imparts NO spin.
        let none = apply_angular_impulse(0.0, Vec2::new(2.0, 0.0), Vec2::new(-5.0, 0.0), 1.0);
        assert!(
            none.abs() < 1e-6,
            "a head-on (arm ∥ j) hit does not spin; got {none}"
        );
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

    /// A step with `dt == 0` must be a no-op: every `dt`-scaled term vanishes, so
    /// the returned state equals the input exactly (TR-003 degenerate input).
    /// This is the zero-`dt` boundary the spec calls out as a predictable no-op
    /// tick — important because a time-dilated bubble can legitimately tick at a
    /// logical `dt` of zero (fully paused) without corrupting state.
    #[test]
    fn zero_dt_step_is_a_no_op() {
        let start = BodyState::new(Vec2::new(7.0, -2.5), Vec2::new(1.5, 9.0));
        let accel = Vec2::new(3.0, -4.0); // non-zero accel must still not move it
        let after = integrate(start, accel, 0.0);
        assert_eq!(
            after, start,
            "dt == 0 must return the input state unchanged"
        );

        // The convenience loop is the same: zero steps OR zero dt is identity.
        assert_eq!(simulate(start, accel, 0.0, 100), start);
        assert_eq!(simulate(start, accel, 1.0 / 30.0, 0), start);
    }

    /// TR-008: serialize -> deserialize must round-trip to a value equal under
    /// `PartialEq`. Guards downstream replication (E003) and persistence (E004):
    /// a `BodyState` (and an ECS component) put on the wire and read back must be
    /// the *same* state. We use JSON as a concrete, self-describing format; the
    /// derive — not the format — is what TR-008 requires.
    #[test]
    fn serde_round_trip_preserves_value() {
        let body = BodyState::new(Vec2::new(-3.5, 12.0), Vec2::new(4.0, -2.0));
        let json = serde_json::to_string(&body).expect("BodyState serializes");
        let back: BodyState = serde_json::from_str(&json).expect("BodyState deserializes");
        assert_eq!(back, body, "BodyState must survive a serde round-trip");

        // Components share the requirement (TR-008 covers BodyState + components).
        let pos = crate::components::Position(Vec2::new(8.0, -1.0));
        let pos_json = serde_json::to_string(&pos).expect("Position serializes");
        let pos_back: crate::components::Position =
            serde_json::from_str(&pos_json).expect("Position deserializes");
        assert_eq!(pos_back, pos, "Position must survive a serde round-trip");

        let vel = crate::components::Velocity(Vec2::new(-2.0, 3.5));
        let vel_json = serde_json::to_string(&vel).expect("Velocity serializes");
        let vel_back: crate::components::Velocity =
            serde_json::from_str(&vel_json).expect("Velocity deserializes");
        assert_eq!(vel_back, vel, "Velocity must survive a serde round-trip");
    }
}
