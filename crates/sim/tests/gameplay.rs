//! Headless integration tests for the E002 gameplay systems.
//!
//! Each test builds a `bevy_ecs` `World` + a chained fixed-step `Schedule` (the
//! same system order the client runs in `FixedUpdate`) and advances it a known
//! number of ticks with scripted `ShipIntent` — no Bevy app, no rendering, no
//! window. This is what makes the fixed-step sim deterministic and testable
//! (FR-016/FR-017).

use bevy_ecs::prelude::*;
use glam::Vec2;
use sim::components::*;
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

const DT: f32 = 1.0 / 60.0;

fn make_world() -> World {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(DT));
    w.insert_resource(ShipIntent::default());
    w.insert_resource(HitFeedback::default());
    w
}

/// The fixed-step pipeline in the same order the client schedules it.
fn make_schedule() -> Schedule {
    let mut s = Schedule::default();
    s.add_systems(
        (
            sim::ai::seek_system,
            sim::flight::ship_motion_system,
            sim::weapon::weapon_fire_system,
            sim::weapon::projectile_step_system,
            sim::collision::collision_detect_system,
            sim::collision::ram_collision_system,
            sim::combat::destruction_system,
            sim::combat::feedback_decay_system,
        )
            .chain(),
    );
    s
}

fn spawn_ship(w: &mut World, pos: Vec2, heading: f32, vel: Vec2) -> Entity {
    w.spawn((
        Ship,
        Position(pos),
        Velocity(vel),
        Heading(heading),
        Health(100.0),
        FlightAssist::Off,
        CollisionRadius(1.0),
        Weapon {
            cooldown: 0.0,
            fire_rate: 5.0,
            muzzle_speed: 200.0,
        },
    ))
    .id()
}

fn count<F: bevy_ecs::query::QueryFilter>(w: &mut World) -> usize {
    w.query_filtered::<Entity, F>().iter(w).count()
}

#[test]
fn thrust_accelerates_then_releasing_coasts() {
    let mut w = make_world();
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x
    let mut sched = make_schedule();

    w.resource_mut::<ShipIntent>().forward = 1.0;
    for _ in 0..30 {
        sched.run(&mut w);
    }
    let after_thrust = w.get::<Velocity>(ship).unwrap().0;
    assert!(after_thrust.x > 0.0, "thrust should build +x velocity");

    // Release thrust → coast at constant velocity (no friction).
    w.resource_mut::<ShipIntent>().forward = 0.0;
    sched.run(&mut w);
    let v1 = w.get::<Velocity>(ship).unwrap().0;
    sched.run(&mut w);
    let v2 = w.get::<Velocity>(ship).unwrap().0;
    assert!(
        (v1 - v2).length() < 1e-4,
        "no thrust → constant velocity (coasting): {v1:?} vs {v2:?}"
    );
}

#[test]
fn firing_destroys_a_target_ahead() {
    let mut w = make_world();
    spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(20.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(5.0), // < projectile damage (10) → one hit destroys it
    ));
    let mut sched = make_schedule();

    w.resource_mut::<ShipIntent>().fire = true;
    for _ in 0..20 {
        sched.run(&mut w);
    }

    assert_eq!(
        count::<With<Target>>(&mut w),
        0,
        "a swept projectile hit should destroy and despawn the target (no tunneling)"
    );
}

#[test]
fn sub_lethal_ram_bounces_and_ship_survives() {
    let mut w = make_world();
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::new(5.0, 0.0)); // closing 5 < 40
    w.spawn((
        Target,
        TargetKind::Asteroid,
        Position(Vec2::new(1.5, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(1.0e6),
    ));
    let mut sched = make_schedule();
    sched.run(&mut w);

    assert!(
        w.get::<Health>(ship).unwrap().0 > 0.0,
        "a sub-lethal ram must not destroy the ship"
    );
    assert!(
        w.get::<Velocity>(ship).unwrap().0.x < 5.0,
        "the bounce should slow/reverse the ship off the heavier asteroid"
    );
}

#[test]
fn lethal_ram_destroys_ship() {
    let mut w = make_world();
    spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::new(60.0, 0.0)); // closing 60 >= 40
    w.spawn((
        Target,
        TargetKind::Asteroid,
        Position(Vec2::new(1.5, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(1.0e6),
    ));
    let mut sched = make_schedule();
    sched.run(&mut w);

    assert_eq!(
        count::<With<Ship>>(&mut w),
        0,
        "a ram at/above the lethal threshold must destroy the ship"
    );
}

#[test]
fn seeker_closes_distance_toward_the_player() {
    let mut w = make_world();
    spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO);
    let seeker = w
        .spawn((
            Target,
            TargetKind::Seeker,
            Position(Vec2::new(50.0, 0.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(1.0),
            Health(5.0),
        ))
        .id();
    let mut sched = make_schedule();
    for _ in 0..20 {
        sched.run(&mut w);
    }

    assert!(
        w.get::<Position>(seeker).unwrap().0.x < 50.0,
        "the seeker should accelerate toward the player at the origin"
    );
    assert!(
        w.get::<Velocity>(seeker).unwrap().0.x < 0.0,
        "the seeker's velocity should point toward the player (-x)"
    );
}

#[test]
fn fixed_step_is_bit_identical_under_identical_inputs() {
    // Two worlds, identical setup + identical per-tick inputs + the same tick
    // count must reach byte-identical sim state (FR-017, zero tolerance).
    fn build() -> (World, Schedule, Entity) {
        let mut w = make_world();
        let ship = spawn_ship(&mut w, Vec2::new(1.0, -2.0), 0.3, Vec2::ZERO);
        w.spawn((
            Target,
            TargetKind::Asteroid,
            Position(Vec2::new(12.0, 3.0)),
            Velocity(Vec2::new(-1.0, 0.5)),
            CollisionRadius(1.0),
            Health(50.0),
        ));
        w.spawn((
            Target,
            TargetKind::Seeker,
            Position(Vec2::new(-8.0, 4.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(1.0),
            Health(30.0),
        ));
        (w, make_schedule(), ship)
    }

    fn step(w: &mut World, sched: &mut Schedule, input: (f32, f32, f32, bool)) {
        {
            let mut intent = w.resource_mut::<ShipIntent>();
            intent.forward = input.0;
            intent.strafe = input.1;
            intent.turn = input.2;
            intent.fire = input.3;
            intent.toggle_assist = false;
        }
        sched.run(w);
    }

    let inputs: [(f32, f32, f32, bool); 3] = [
        (1.0, 0.0, 0.5, true),
        (0.0, 1.0, -1.0, false),
        (-1.0, 0.0, 0.0, true),
    ];

    let (mut wa, mut sa, ship_a) = build();
    let (mut wb, mut sb, ship_b) = build();
    for i in 0..120 {
        let input = inputs[i % inputs.len()];
        step(&mut wa, &mut sa, input);
        step(&mut wb, &mut sb, input);
    }

    assert_eq!(
        wa.get::<Position>(ship_a).unwrap(),
        wb.get::<Position>(ship_b).unwrap(),
        "ship position must be bit-identical"
    );
    assert_eq!(
        wa.get::<Velocity>(ship_a).unwrap(),
        wb.get::<Velocity>(ship_b).unwrap(),
        "ship velocity must be bit-identical"
    );
    assert_eq!(
        wa.get::<Heading>(ship_a).unwrap().0.to_bits(),
        wb.get::<Heading>(ship_b).unwrap().0.to_bits(),
        "ship heading must be bit-identical"
    );
    assert_eq!(
        count::<With<Target>>(&mut wa),
        count::<With<Target>>(&mut wb),
        "surviving target count must match"
    );
    assert_eq!(
        count::<With<Projectile>>(&mut wa),
        count::<With<Projectile>>(&mut wb),
        "live projectile count must match"
    );
}

#[test]
fn two_projectiles_one_target_destroy_once() {
    // Two projectiles overlapping one target in the same step: damage
    // accumulates order-independently and the target despawns exactly once —
    // no double-destroy artifact (SC-003 simultaneous-hit clause, CHK029).
    let mut w = make_world();
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(15.0), // 10 + 10 damage destroys it across the two hits
    ));
    for _ in 0..2 {
        w.spawn((
            Projectile,
            Position(Vec2::ZERO),
            PrevPosition(Vec2::ZERO),
            Velocity(Vec2::new(50.0, 0.0)),
            Damage(10.0),
            Lifetime(3.0),
        ));
    }
    let mut sched = make_schedule();
    sched.run(&mut w);

    assert_eq!(
        count::<With<Target>>(&mut w),
        0,
        "both hits apply and the target is destroyed exactly once"
    );
}

#[test]
fn shot_connects_with_a_drifting_asteroid() {
    // A straight shot must connect with a slowly drifting asteroid — the player
    // can hit moving targets (US2 scenario 4 / CHK012).
    let mut w = make_world();
    spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x
    w.spawn((
        Target,
        TargetKind::Asteroid,
        Position(Vec2::new(15.0, 0.0)),
        Velocity(Vec2::new(0.0, 1.0)), // drifting slowly across the shot line
        CollisionRadius(0.9),
        Health(15.0),
    ));
    let mut sched = make_schedule();
    w.resource_mut::<ShipIntent>().fire = true;
    for _ in 0..30 {
        sched.run(&mut w);
    }
    assert_eq!(
        count::<With<Target>>(&mut w),
        0,
        "a straight shot should connect with the slowly drifting asteroid"
    );
}

#[test]
fn projectiles_resolve_harmlessly_after_target_destroyed() {
    // After its target is gone, an in-flight projectile keeps going and expires
    // with no error and no leak (CHK017).
    let mut w = make_world();
    spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO);
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(8.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(0.9),
        Health(5.0),
    ));
    let mut sched = make_schedule();
    w.resource_mut::<ShipIntent>().fire = true;
    for _ in 0..10 {
        sched.run(&mut w);
    }
    assert_eq!(count::<With<Target>>(&mut w), 0, "target destroyed");

    // Stop firing and let any in-flight projectiles fly on past the empty space
    // and expire (lifetime 3 s = 180 ticks).
    w.resource_mut::<ShipIntent>().fire = false;
    for _ in 0..200 {
        sched.run(&mut w);
    }
    assert_eq!(
        count::<With<Projectile>>(&mut w),
        0,
        "projectiles with no target expire harmlessly"
    );
}
