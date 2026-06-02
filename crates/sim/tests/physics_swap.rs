//! Integration test for the `Physics` engine-swap guarantee (TR-006 / SC-004).
//!
//! Proves two things at once:
//!
//! 1. **Drop-in swap** — a generic consumer (`advance_path`) is written against
//!    the `sim::physics::Physics` trait using only `glam`/`sim` types. The same
//!    consumer code runs unchanged over the Rapier2D-backed `RapierPhysics` and
//!    over the `StubPhysics` defined here. The fact that this file names no
//!    Rapier (or other engine) type anywhere is the type-leak audit.
//!
//! 2. **Identical consumer behavior** — "same outputs for the same inputs":
//!    for an identical set of initial states / accelerations / timesteps, both
//!    backends produce the same resulting `BodyState`s.

use glam::Vec2;
use sim::motion::integrate;
use sim::physics::{Physics, RapierPhysics};
use sim::BodyState;

/// A second, fully independent `Physics` implementation with no Rapier behind
/// it — pure `sim::motion`. Its surface is the full trait surface taking and
/// returning only `glam`/`sim` types, so it is a drop-in substitute for
/// `RapierPhysics` requiring no consumer change (the swap guarantee).
#[derive(Debug, Default, Clone, Copy)]
struct StubPhysics;

impl Physics for StubPhysics {
    fn step(&self, body: BodyState, accel: Vec2, dt: f32) -> BodyState {
        // Same authoritative motion contract as every backend: velocity-Verlet.
        integrate(body, accel, dt)
    }
}

/// A generic gameplay consumer written purely against the trait. It never names
/// a backend type, so it is identical regardless of which `Physics` is passed.
/// Returns the trajectory (state after each tick) so we can compare backends
/// step-by-step, not just at the endpoint.
fn advance_path<P: Physics>(
    physics: &P,
    start: BodyState,
    accel: Vec2,
    dt: f32,
    ticks: u32,
) -> Vec<BodyState> {
    let mut path = Vec::with_capacity(ticks as usize);
    let mut state = start;
    for _ in 0..ticks {
        state = physics.step(state, accel, dt);
        path.push(state);
    }
    path
}

/// Representative input set spanning non-trivial velocity/acceleration, several
/// runtime tick sizes, and the degenerate zero-`dt`/zero-accel cases.
fn cases() -> Vec<(BodyState, Vec2, f32, u32)> {
    let start = BodyState::new(Vec2::new(-3.5, 12.0), Vec2::new(4.0, -2.0));
    vec![
        (start, Vec2::new(0.75, -0.30), 1.0 / 30.0, 300),
        (start, Vec2::new(-0.2, 0.9), 1.0 / 144.0, 1000),
        (start, Vec2::new(2.0, 2.0), 1.0 / 60.0, 120),
        (start, Vec2::ZERO, 1.0 / 30.0, 200),  // coasting
        (start, Vec2::new(9.0, 9.0), 0.0, 50), // zero-dt no-op
    ]
}

#[test]
fn rapier_and_stub_produce_identical_consumer_behavior() {
    let rapier = RapierPhysics::new();
    let stub = StubPhysics;

    for (start, accel, dt, ticks) in cases() {
        let rapier_path = advance_path(&rapier, start, accel, dt, ticks);
        let stub_path = advance_path(&stub, start, accel, dt, ticks);
        assert_eq!(
            rapier_path, stub_path,
            "Physics backends diverged for start={start:?} accel={accel:?} dt={dt} ticks={ticks}"
        );
    }
}

#[test]
fn swap_requires_no_consumer_change_step_many() {
    // The default `step_many` is part of the swappable surface too: same inputs,
    // same outputs across backends, and consistent with stepping one tick at a
    // time. The consumer call site is byte-for-byte identical for both backends.
    let rapier = RapierPhysics::new();
    let stub = StubPhysics;

    let start = BodyState::new(Vec2::new(1.0, 1.0), Vec2::new(2.0, -1.0));
    let accel = Vec2::new(0.5, 0.5);
    let dt = 1.0 / 30.0;
    let steps = 90;

    let r = rapier.step_many(start, accel, dt, steps);
    let s = stub.step_many(start, accel, dt, steps);
    assert_eq!(r, s, "step_many must agree across backends");

    // And step_many == repeated step for the same backend.
    let r_manual = *advance_path(&rapier, start, accel, dt, steps)
        .last()
        .unwrap();
    assert_eq!(r, r_manual, "step_many must equal repeated step");
}
