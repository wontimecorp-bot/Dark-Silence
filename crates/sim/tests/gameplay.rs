//! Headless integration tests for the E002 gameplay systems.
//!
//! Each test builds a `bevy_ecs` `World` + a chained fixed-step `Schedule` (the
//! same system order the client runs in `FixedUpdate`) and advances it a known
//! number of ticks with scripted per-ship `ShipIntent` components — no Bevy app,
//! no rendering, no window. This is what makes the fixed-step sim deterministic
//! and testable (FR-016/FR-017).
//!
//! Intent is per-entity: each ship carries its own `ShipIntent` component, so a
//! test scripts a ship by mutating that ship's component (see [`set_intent`]).

use bevy_ecs::prelude::*;
use glam::Vec2;
use sim::components::*;
use sim::{
    Cargo, FixedDt, HitFeedback, MiningState, MiningTransport, MiningTuning, RefinedResources,
    ScenarioActive, ShipIntent, Tuning, Turret, TurretSpec,
};

const DT: f32 = 1.0 / 60.0;

fn make_world() -> World {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(DT));
    w.insert_resource(HitFeedback::default());
    w
}

/// Mutate a ship's own `ShipIntent` component (intent is per-entity).
fn set_intent(w: &mut World, ship: Entity, f: impl FnOnce(&mut ShipIntent)) {
    let mut intent = w.get_mut::<ShipIntent>(ship).expect("ship has ShipIntent");
    f(&mut intent);
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
        ShipIntent::default(),
        Position(pos),
        Velocity(vel),
        Heading(heading),
        AngularVelocity(0.0),
        Health(100.0),
        // Decoupled mode (no drag) so the coast/ram/seek/determinism tests see
        // pure Newtonian motion; the flight-model is exercised in its own tests.
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

    set_intent(&mut w, ship, |i| i.forward = 1.0);
    for _ in 0..30 {
        sched.run(&mut w);
    }
    let after_thrust = w.get::<Velocity>(ship).unwrap().0;
    assert!(after_thrust.x > 0.0, "thrust should build +x velocity");

    // Release thrust → coast at constant velocity (no friction).
    set_intent(&mut w, ship, |i| i.forward = 0.0);
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
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(20.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(5.0), // < projectile damage (10) → one hit destroys it
    ));
    let mut sched = make_schedule();

    set_intent(&mut w, ship, |i| i.fire = true);
    for _ in 0..20 {
        sched.run(&mut w);
    }

    assert_eq!(
        count::<With<Target>>(&mut w),
        0,
        "a swept projectile hit should destroy and despawn the target (no tunneling)"
    );
}

/// Phase 2 friend/foe gate (mining skirmish): a FACTIONED shot does NOT damage a SAME-faction
/// target (friendly fire off by default) but DOES destroy an enemy-faction target. The unfactioned
/// case (today's free-for-all) is covered by `firing_destroys_a_target_ahead`.
#[test]
fn factioned_fire_spares_friendlies_but_destroys_enemies() {
    // A FRIENDLY (same-faction) target is not hit by a same-faction shot.
    let mut w = make_world();
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x, faction Red
    w.entity_mut(ship).insert(Faction::Red);
    let friendly = w
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(Vec2::new(20.0, 0.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(1.0),
            Health(5.0),
            Faction::Red,
        ))
        .id();
    let mut sched = make_schedule();
    set_intent(&mut w, ship, |i| i.fire = true);
    for _ in 0..20 {
        sched.run(&mut w);
    }
    assert_eq!(
        w.get::<Health>(friendly).map(|h| h.0),
        Some(5.0),
        "a same-faction shot passes through a friendly target (no friendly fire)"
    );

    // An ENEMY (different-faction) target IS destroyed by the same shot.
    let mut w2 = make_world();
    let ship2 = spawn_ship(&mut w2, Vec2::ZERO, 0.0, Vec2::ZERO);
    w2.entity_mut(ship2).insert(Faction::Red);
    w2.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(20.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.0),
        Health(5.0),
        Faction::Blue,
    ));
    let mut sched2 = make_schedule();
    set_intent(&mut w2, ship2, |i| i.fire = true);
    for _ in 0..20 {
        sched2.run(&mut w2);
    }
    assert_eq!(
        count::<With<Target>>(&mut w2),
        0,
        "an enemy-faction target is destroyed by a factioned shot"
    );
}

/// Phase 3 + Refinement 3 mining loop: a transport flies its Newtonian model to the asteroid, loads,
/// hauls back to its outpost, and unloads — and the faction's `RefinedResources` grows on each unload.
/// (`mining_transport_system` now owns the FULL pos+vel+heading integration; `seek_system` skips
/// `TargetKind::Transport`.)
#[test]
fn mining_transport_runs_the_loop_and_grows_the_score() {
    let mut w = make_world();
    w.insert_resource(ScenarioActive);
    w.insert_resource(RefinedResources::default());
    // A brisk tuning so the loop converges fast in this tiny (~30-unit) test arena: light + punchy,
    // small arrive/cargo. (The shipped default is a slow heavy barge for the ±1200 game arena.)
    w.insert_resource(MiningTuning {
        mass: 1.0,
        thrust_force: 40.0,
        linear_drag: 2.0,
        turn_torque: 20.0,
        angular_drag: 4.0,
        angular_inertia: 1.0,
        slow_radius: 12.0,
        arrive_radius: 6.0,
        dock_speed: 12.0,
        load_rate: 80.0,
        unload_rate: 100.0,
        cargo_capacity: 40.0,
    });
    let mine = w
        .spawn((
            Target,
            TargetKind::MineNode,
            Position(Vec2::new(0.0, 0.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(5.0),
            Health(1.0e6),
        ))
        .id();
    let outpost = w
        .spawn((
            Target,
            TargetKind::Outpost,
            Position(Vec2::new(30.0, 0.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(3.0),
            Health(800.0),
            Faction::Red,
        ))
        .id();
    let transport = w
        .spawn((
            Target,
            TargetKind::Transport,
            Position(Vec2::new(20.0, 0.0)),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            CollisionRadius(1.6),
            Health(200.0),
            Faction::Red,
            AngularVelocity(0.0),
            MiningTransport {
                home_outpost: outpost,
                mine_node: mine,
            },
            Cargo { current: 0.0 },
            MiningState::default(),
        ))
        .id();

    // seek_system runs (and skips the Transport); mining_transport_system drives the whole loop.
    let mut s = Schedule::default();
    s.add_systems((sim::ai::seek_system, sim::mining::mining_transport_system).chain());
    // Enough ticks (at 1/60 dt) for several full loops.
    for _ in 0..3000 {
        s.run(&mut w);
    }

    // The transport actually moved (it didn't sit at its spawn).
    assert!(
        (w.get::<Position>(transport).unwrap().0 - Vec2::new(20.0, 0.0)).length() > 1.0,
        "the transport navigated away from its spawn"
    );
    // The faction refined a positive amount — at least one full deliver→unload cycle completed.
    assert!(
        w.resource::<RefinedResources>().red > 0.0,
        "the transport delivered refined resources (got {})",
        w.resource::<RefinedResources>().red
    );
    // Only Red ran a transport → Blue's tally stays zero.
    assert_eq!(w.resource::<RefinedResources>().blue, 0.0);
}

/// Phase 4 turret: a mounted turret acquires the nearest ENEMY-factioned body in range, leads +
/// fires along its own heading, and its shots (factioned) destroy the enemy via the friend/foe
/// pipeline — while a SAME-faction body in range is never targeted.
#[test]
fn turret_acquires_and_destroys_an_enemy_but_spares_a_friendly() {
    let mut w = make_world();
    w.insert_resource(ScenarioActive);
    w.insert_resource(sim::SimTuning::default());
    // Host (Red) at origin — the turret reads its position/velocity.
    let host = w
        .spawn((Position(Vec2::ZERO), Velocity(Vec2::ZERO), Faction::Red))
        .id();
    // Turret (Red, outpost preset = better aim) mounted on the host, aimed along +x to start.
    w.spawn((
        Turret::heavy(host, Vec2::ZERO),
        TurretSpec::outpost_preset(),
        Faction::Red,
        Heading(0.0),
    ));
    // A FRIENDLY (Red) body sitting closer than the enemy — must NOT be targeted/hit.
    let friendly = w
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(Vec2::new(0.0, 10.0)),
            Velocity(Vec2::ZERO),
            CollisionRadius(1.5),
            Health(30.0),
            Faction::Red,
        ))
        .id();
    // An ENEMY (Blue) along +x, in range — the turret's target.
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(20.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(1.5),
        Health(30.0),
        Faction::Blue,
    ));

    let mut s = Schedule::default();
    s.add_systems(
        (
            sim::turret::turret_system,
            sim::weapon::projectile_step_system,
            sim::collision::collision_detect_system,
            sim::combat::destruction_system,
        )
            .chain(),
    );
    for _ in 0..600 {
        s.run(&mut w);
    }

    // The enemy is destroyed; the friendly survives untouched (only the Blue Target remains gone).
    assert!(
        w.get::<Health>(friendly).map(|h| h.0) == Some(30.0),
        "the friendly (same-faction) body is never targeted or hit"
    );
    assert_eq!(
        count::<With<Target>>(&mut w),
        1,
        "the turret destroyed only the enemy (the friendly Target remains)"
    );
}

/// Refinement 5 — lazy voxelization: a flat-`Health` structure marked `VoxelizeOnHit` is NOT a carve
/// target until shot. The first hit tags it (no flat damage), `voxelize_pending_system` swaps in its
/// cell hull (gains `FitLayout` + `Destructible`, loses `Health`), and from then on it carves like a
/// ship. Proves the cheap-box → carve-hull conversion end to end.
#[test]
fn voxelize_on_hit_converts_a_structure_to_a_carve_hull() {
    use sim::fitting::{station_hull, HullCatalog, HullId, ModuleCatalog};

    let mut w = make_world();
    w.insert_resource(ScenarioActive);
    w.insert_resource(ModuleCatalog::default());
    let hull_id = HullId(9001);
    let mut hulls = HullCatalog::default();
    hulls
        .hulls
        .insert(hull_id, station_hull(hull_id, "TestStation", 7, 7, 2));
    w.insert_resource(hulls);

    let radius = sim::fitting::hull_collision_radius((7, 7));
    let structure = w
        .spawn((
            Target,
            TargetKind::Outpost,
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            CollisionRadius(radius),
            Health(1.0e6),
            sim::VoxelizeOnHit {
                hull: hull_id,
                cell_hp: 4.0,
            },
        ))
        .id();
    // A shot sweeping straight across the structure circle.
    w.spawn((
        Projectile,
        Position(Vec2::new(radius + 1.0, 0.0)),
        PrevPosition(Vec2::new(-radius - 1.0, 0.0)),
        Velocity(Vec2::new(100.0, 0.0)),
        Damage(10.0),
    ));

    // Pre: a flat box — has `Health`, is NOT a carve target.
    assert!(w.get::<Health>(structure).is_some());
    assert!(w.get::<sim::fitting::FitLayout>(structure).is_none());

    // collision_detect tags it (consuming the shot) → voxelize_pending converts it. Two ticks so the
    // tag set via deferred `Commands` is applied before the conversion reads it.
    let mut s = Schedule::default();
    s.add_systems(
        (
            sim::collision::collision_detect_system,
            sim::voxelize_pending_system,
        )
            .chain(),
    );
    for _ in 0..2 {
        s.run(&mut w);
    }

    // Post: converted to a carve-hull — gained `FitLayout` + `Destructible`, lost flat `Health`.
    let layout = w.get::<sim::fitting::FitLayout>(structure);
    assert!(layout.is_some(), "gained a FitLayout (now carveable)");
    assert!(
        w.get::<Destructible>(structure).is_some(),
        "gained Destructible"
    );
    assert!(
        w.get::<Health>(structure).is_none(),
        "flat Health removed on conversion"
    );
    assert!(
        !layout.unwrap().cells.is_empty(),
        "the cell hull has cells to carve"
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

    fn step(w: &mut World, sched: &mut Schedule, ship: Entity, input: (f32, f32, f32, bool)) {
        set_intent(w, ship, |intent| {
            intent.forward = input.0;
            intent.strafe = input.1;
            intent.turn = input.2;
            intent.fire = input.3;
            intent.toggle_assist = false;
        });
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
        step(&mut wa, &mut sa, ship_a, input);
        step(&mut wb, &mut sb, ship_b, input);
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
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x
    w.spawn((
        Target,
        TargetKind::Asteroid,
        Position(Vec2::new(15.0, 0.0)),
        Velocity(Vec2::new(0.0, 1.0)), // drifting slowly across the shot line
        CollisionRadius(0.9),
        Health(15.0),
    ));
    let mut sched = make_schedule();
    set_intent(&mut w, ship, |i| i.fire = true);
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
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO);
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(Vec2::new(8.0, 0.0)),
        Velocity(Vec2::ZERO),
        CollisionRadius(0.9),
        Health(5.0),
    ));
    let mut sched = make_schedule();
    set_intent(&mut w, ship, |i| i.fire = true);
    for _ in 0..10 {
        sched.run(&mut w);
    }
    assert_eq!(count::<With<Target>>(&mut w), 0, "target destroyed");

    // Stop firing and let any in-flight projectiles fly on past the empty space
    // and expire (lifetime 3 s = 180 ticks).
    set_intent(&mut w, ship, |i| i.fire = false);
    for _ in 0..200 {
        sched.run(&mut w);
    }
    assert_eq!(
        count::<With<Projectile>>(&mut w),
        0,
        "projectiles with no target expire harmlessly"
    );
}

/// Spawn a ship in flight-model (assist On) mode for the drag/power-budget tests.
fn spawn_flight_model_ship(w: &mut World) -> Entity {
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        Health(100.0),
        FlightAssist::On,
        CollisionRadius(0.8),
        Weapon {
            cooldown: 0.0,
            fire_rate: 5.0,
            muzzle_speed: 200.0,
        },
    ))
    .id()
}

#[test]
fn flight_model_caps_speed_at_terminal_velocity() {
    let mut w = make_world();
    let ship = spawn_flight_model_ship(&mut w);
    let mut sched = make_schedule();
    set_intent(&mut w, ship, |i| i.forward = 1.0);
    for _ in 0..900 {
        sched.run(&mut w); // 15 s of full thrust
    }
    let speed = w.get::<Velocity>(ship).unwrap().0.length();
    let v_max = Tuning::default().top_speed(); // 80
    assert!(
        (speed - v_max).abs() < 2.0,
        "speed approaches the emergent terminal velocity {v_max}, got {speed}"
    );
    assert!(
        speed <= v_max + 0.5,
        "drag caps speed — never blows past v_max"
    );
}

#[test]
fn hard_turn_diverts_thrust_and_bleeds_speed() {
    let mut w = make_world();
    let ship = spawn_flight_model_ship(&mut w);
    let mut sched = make_schedule();
    set_intent(&mut w, ship, |i| i.forward = 1.0);
    for _ in 0..900 {
        sched.run(&mut w);
    }
    let cruise = w.get::<Velocity>(ship).unwrap().0.length();
    assert!(cruise > 70.0, "should be near top speed before turning");

    // Hold full thrust AND full turn: the shared power budget cuts available
    // thrust to 30%, so the reduced terminal velocity bleeds speed off.
    set_intent(&mut w, ship, |intent| {
        intent.forward = 1.0;
        intent.turn = 1.0;
    });
    for _ in 0..600 {
        sched.run(&mut w);
    }
    let turning = w.get::<Velocity>(ship).unwrap().0.length();
    assert!(
        turning < 50.0,
        "hard turning should bleed speed via the shared power budget (was {cruise}, now {turning})"
    );
}

// --- Phase M4 (Phase C): recoil + projectile velocity inheritance ----------------

/// Firing recoils the shooter opposite the muzzle, conserving momentum: the ship's change in
/// momentum equals minus the momentum the gun gave the slug (`PROJECTILE_MASS · muzzle`).
#[test]
fn firing_recoils_the_shooter_conserving_momentum() {
    let mut w = make_world();
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO); // nose +x, at rest
    let mass = w.get_resource::<Tuning>().unwrap().mass;
    let mut sched = make_schedule();

    set_intent(&mut w, ship, |i| i.fire = true);
    sched.run(&mut w); // one tick: cooldown starts at 0 → exactly one shot fires

    let v = w.get::<Velocity>(ship).unwrap().0;
    let muzzle = Vec2::new(200.0, 0.0); // heading 0 × muzzle_speed 200
    let expected = -sim::weapon::PROJECTILE_MASS * muzzle / mass;
    assert!(
        (v - expected).length() < 1e-4,
        "the shooter recoils opposite the muzzle (got {v:?}, expected {expected:?})"
    );
    assert!(v.x < 0.0, "a +x shot recoils the shooter backward (−x)");
    // Momentum conservation: ship Δp (= mass·Δv) = −(projectile muzzle momentum).
    let ship_dp = mass * v;
    assert!(
        (ship_dp + sim::weapon::PROJECTILE_MASS * muzzle).length() < 1e-4,
        "ship Δp = −(PROJECTILE_MASS·muzzle); got Δp = {ship_dp:?}"
    );
}

/// A moving ship's projectiles carry its velocity (a true Newtonian gun): the shot's velocity is
/// the muzzle velocity PLUS the shooter's velocity at fire time.
#[test]
fn a_moving_ships_shot_inherits_its_velocity() {
    let mut w = make_world();
    let drift = Vec2::new(0.0, 10.0);
    let ship = spawn_ship(&mut w, Vec2::ZERO, 0.0, drift); // nose +x, drifting +y
    let mut sched = make_schedule();

    set_intent(&mut w, ship, |i| i.fire = true);
    sched.run(&mut w);

    let pv = {
        let mut q = w.query_filtered::<&Velocity, With<Projectile>>();
        let vs: Vec<Vec2> = q.iter(&w).map(|v| v.0).collect();
        assert_eq!(vs.len(), 1, "exactly one projectile fired");
        vs[0]
    };
    let expected = Vec2::new(200.0, 0.0) + drift; // muzzle (+x·200) + the shooter's drift (+y·10)
    assert!(
        (pv - expected).length() < 1e-3,
        "the projectile inherits the shooter's velocity (got {pv:?}, expected {expected:?})"
    );
}
