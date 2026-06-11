//! Headless integration tests for the 00008-ship-ai OBJ1 steering substrate
//! (T008 VC1 intent-replay equivalence + V-6 invariant + cross-module steering
//! behaviors; T009 VC2 formation-hold), the OBJ2 deterministic brain
//! (T015: VC1 same-state selection, VC2 event re-think + zero idle thinks,
//! stable tiebreaks, and the TR-004 strict-f32 CI grep), and the OBJ3
//! behavior-LOD layer (T020: VC2 no-pop aggregate round-trip + VC3 mutual
//! hostile promotion / no dormant combat; T021: VC4 anchor-death re-derive +
//! squad-of-1 degrade + VC1 per-tier think counters — decisions O(squads)),
//! and the OBJ4 combat AI (T028: VC1 engage-close-aim-destroy + the energy/
//! heat fire gates, VC2 ram/no-ram decision pair, OBJ2-VC3 archetype range
//! bands — TR-006/TR-011/TR-012), and the OBJ5 perception + sensor network
//! (T031: VC1 unseen-never-targeted at Active AND Dormant tiers, VC2 fused
//! share + jammed/severed local-only fallbacks, newest-wins fusion dedupe
//! across members — TR-013/TR-014).
//!
//! Pattern mirrors `gameplay.rs`: a plain `bevy_ecs` `World` + a fixed-step
//! `Schedule` advanced a known number of ticks. Ships here are UNFITTED (no
//! `ShipStats`), so `ship_motion_system` flies them on the global `Tuning`
//! fallback — the simplest real flight model for trajectory equivalence.
//!
//! The "AI" in these tests is the test loop itself acting as a brain: each tick
//! it reads the ship's kinematics, calls the PURE steering substrate
//! (`sim::ai::steering`), and writes the returned [`ShipIntent`] VALUE to the
//! ship's component — exactly the V-6 seam the T010+ brains will use. Steering
//! itself never touches `Velocity`/`Heading`/`Position` (TR-001).

use bevy_ecs::prelude::*;
use glam::Vec2;
use sim::ai::steering::{
    arrive, avoid, compose_intent, formation_keep, intercept_point, pursue_intercept,
    reachability_bias, seek, slot_dir, steer_to_intent, waypoint_follow, wrap_angle, ContextMap,
};
use sim::ai::{
    ai_think_system, cadence_for_tier, select_behavior, spawn_squad, AiBrain, AiEvent,
    AiIdAllocator, AiStableId, AiTuning, AoiTier, Behavior, CombatStance, FormationDef, GlideState,
    Gliding, HostileContact, MovementProfile, PlayerShip, RethinkQueue, Squad, SquadOrder, Tier,
};
use sim::broadphase::{CoarseIndex, ObstacleField};
use sim::components::*;
use sim::{
    CurrentTick, FixedDt, HitFeedback, RefinedResources, ScenarioActive, ShipIntent, Tuning,
};

const DT: f32 = 1.0 / 60.0;

fn make_world() -> World {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(DT));
    w
}

/// The flight model alone — steering emits intents; `ship_motion_system` is the
/// only thing allowed to move a ship (TR-001/V-6). No combat systems needed.
fn make_schedule() -> Schedule {
    let mut s = Schedule::default();
    s.add_systems(sim::flight::ship_motion_system);
    s
}

/// An UNFITTED ship in flight-model (assist On) mode: flies on the `Tuning`
/// fallback through the exact same `step_ship_motion` math as a player ship.
fn spawn_ai_ship(w: &mut World, pos: Vec2, heading: f32, vel: Vec2) -> Entity {
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(vel),
        Heading(heading),
        AngularVelocity(0.0),
        FlightAssist::On,
    ))
    .id()
}

fn kinematics(w: &World, e: Entity) -> (Vec2, Vec2, f32) {
    (
        w.get::<Position>(e).expect("ship has Position").0,
        w.get::<Velocity>(e).expect("ship has Velocity").0,
        w.get::<Heading>(e).expect("ship has Heading").0,
    )
}

/// Raw f32 bits of the full kinematic state — the VC1 comparison is BIT
/// identity (exact `to_bits`), not epsilon closeness.
fn state_bits(w: &World, e: Entity) -> [u32; 6] {
    let (p, v, h) = kinematics(w, e);
    let omega = w
        .get::<AngularVelocity>(e)
        .expect("ship has AngularVelocity")
        .0;
    [
        p.x.to_bits(),
        p.y.to_bits(),
        v.x.to_bits(),
        v.y.to_bits(),
        h.to_bits(),
        omega.to_bits(),
    ]
}

/// The caller-writes-the-intent half of the V-6 seam (what a brain system does
/// with the value `steer_to_intent` returns).
fn write_intent(w: &mut World, ship: Entity, intent: ShipIntent) {
    *w.get_mut::<ShipIntent>(ship).expect("ship has ShipIntent") = intent;
}

// ---------------------------------------------------------------------------
// T008 — VC1 / SC-001: AI trajectory ≡ replayed-intent trajectory, bit for bit
// ---------------------------------------------------------------------------

/// World A is driven by the AI: each tick the steering substrate computes a
/// `ShipIntent` toward a fixed waypoint (`waypoint_follow` → `steer_to_intent`)
/// and the loop records it before writing it to the ship. World B replays the
/// RECORDED sequence as if a player had typed it. Because the AI shares the
/// human control path (one `ship_motion_system`, intents only), the per-tick
/// trajectories must be BIT-identical (position/velocity/heading — zero
/// tolerance), and the AI ship must make real progress toward its waypoint.
#[test]
fn ai_intent_trajectory_is_bit_identical_to_replayed_intents() {
    const TICKS: usize = 240;
    let waypoint = Vec2::new(150.0, 100.0);
    let route = [waypoint];
    let arrive_radius = 8.0;
    let authority = Tuning::default().max_turn_rate();

    // World A — the AI emits the intents (and we record them).
    let mut wa = make_world();
    let mut sa = make_schedule();
    let ship_a = spawn_ai_ship(&mut wa, Vec2::ZERO, 0.0, Vec2::ZERO);
    let initial_dist = (kinematics(&wa, ship_a).0 - waypoint).length();
    let mut recorded: Vec<ShipIntent> = Vec::with_capacity(TICKS);
    let mut trace_a: Vec<[u32; 6]> = Vec::with_capacity(TICKS);
    let mut idx = 0usize;
    for _ in 0..TICKS {
        let (pos, vel, heading) = kinematics(&wa, ship_a);
        let (dir, throttle, next_idx) = waypoint_follow(pos, vel, &route, idx, arrive_radius);
        idx = next_idx;
        let intent = steer_to_intent(dir, throttle, heading, vel, authority);
        recorded.push(intent);
        write_intent(&mut wa, ship_a, intent);
        sa.run(&mut wa);
        trace_a.push(state_bits(&wa, ship_a));
    }

    // World B — the SAME intent sequence injected as if from player input.
    let mut wb = make_world();
    let mut sb = make_schedule();
    let ship_b = spawn_ai_ship(&mut wb, Vec2::ZERO, 0.0, Vec2::ZERO);
    let mut trace_b: Vec<[u32; 6]> = Vec::with_capacity(TICKS);
    for &intent in &recorded {
        write_intent(&mut wb, ship_b, intent);
        sb.run(&mut wb);
        trace_b.push(state_bits(&wb, ship_b));
    }

    assert_eq!(
        trace_a, trace_b,
        "AI-driven and intent-replayed trajectories must be bit-identical \
         per tick (VC1/SC-001: one control path, zero tolerance)"
    );

    // The run is MEANINGFUL: the AI actually burned and closed on the waypoint.
    assert!(
        recorded.iter().any(|i| i.forward > 0.5),
        "the AI emitted real thrust intents"
    );
    let final_dist = (kinematics(&wa, ship_a).0 - waypoint).length();
    assert!(
        final_dist < 0.5 * initial_dist,
        "the AI ship flew toward its waypoint through the real flight model \
         (distance {initial_dist} -> {final_dist})"
    );
}

// ---------------------------------------------------------------------------
// T008 — V-6: steering never mutates kinematics directly
// ---------------------------------------------------------------------------

/// The static guarantee is COMPILE-LEVEL: every steering function takes plain
/// values/slices and returns values — none can even name an ECS component.
/// This test makes it explicit at runtime: exercise EVERY public steering entry
/// point with a live ship's state as inputs (and even write the resulting
/// intent component), then assert the ship's `Position`/`Velocity`/`Heading`/
/// `AngularVelocity` bits are untouched without a schedule step.
#[test]
fn steering_never_mutates_kinematics_directly() {
    let mut w = make_world();
    let ship = spawn_ai_ship(&mut w, Vec2::new(3.0, -4.0), 0.7, Vec2::new(12.0, -5.0));
    w.get_mut::<AngularVelocity>(ship).unwrap().0 = 0.4; // arbitrary spin too
    let before = state_bits(&w, ship);

    // Call the ENTIRE steering surface with the ship's live state as inputs.
    let (pos, vel, heading) = kinematics(&w, ship);
    let goal = Vec2::new(100.0, 50.0);
    let _ = seek(pos, goal);
    let _ = arrive(pos, vel, goal, 30.0);
    let _ = intercept_point(pos, 80.0, goal, Vec2::new(0.0, 20.0));
    let _ = pursue_intercept(pos, 80.0, goal, Vec2::new(0.0, 20.0));
    let _ = waypoint_follow(pos, vel, &[Vec2::new(40.0, 0.0), goal], 0, 8.0);
    let _ = formation_keep(
        pos,
        vel,
        Vec2::ZERO,
        Vec2::new(5.0, 0.0),
        0.3,
        Vec2::new(-6.0, 6.0),
    );
    let _ = avoid(pos, vel, &[(pos + Vec2::new(2.0, 0.0), 5.0)]);
    let _ = reachability_bias(Vec2::Y, vel, 3.0);
    let _ = wrap_angle(heading + 7.0);
    let _ = slot_dir(3, 16);
    let mut map = ContextMap::default();
    map.add_interest_dir(seek(pos, goal), 1.0, 16);
    map.add_danger_dir(Vec2::X, 0.5, 16);
    map.add_danger_threat(pos + Vec2::new(10.0, 0.0), pos, 40.0, 1.0, 16);
    let resolved = map.resolve(16, AiTuning::default().danger_mask_floor);
    let intent = match resolved {
        Some((dir, strength)) => steer_to_intent(dir, strength, heading, vel, 3.0),
        None => compose_intent(Vec2::ZERO, 0.0, heading),
    };
    assert!(
        (-1.0..=1.0).contains(&intent.turn) && (-1.0..=1.0).contains(&intent.forward),
        "the substrate's only output is a clamped ShipIntent VALUE"
    );

    // Even writing the intent component (the caller's half of the seam) moves
    // nothing — only `ship_motion_system` consumes intents into kinematics.
    write_intent(&mut w, ship, intent);
    assert_eq!(
        before,
        state_bits(&w, ship),
        "computing (and writing) intents without stepping the schedule must \
         leave pos/vel/heading/omega bit-untouched (TR-001/V-6)"
    );
}

// ---------------------------------------------------------------------------
// T008 — cross-module steering behaviors through the real flight model
// ---------------------------------------------------------------------------

/// Context-map masking at integration level: a danger zone sits dead on the
/// straight line to the goal. The per-tick "brain" writes goal interest plus a
/// weak omnidirectional explore ring (so masking always has an open lane to
/// pick), and Fray masking deflects the resolved heading off the blocked line —
/// yet the ship still makes real progress and reaches the goal area.
#[test]
fn context_map_danger_deflects_around_a_blocked_path() {
    const TICKS: usize = 900;
    let ai = AiTuning::default();
    let n = ai.slot_count as usize;
    let goal = Vec2::new(240.0, 0.0);
    let threat = Vec2::new(90.0, 0.0); // squarely on the straight path
    let threat_radius = 60.0;
    let authority = Tuning::default().max_turn_rate();

    let mut w = make_world();
    let mut sched = make_schedule();
    let ship = spawn_ai_ship(&mut w, Vec2::ZERO, 0.0, Vec2::ZERO);

    let mut max_lateral = 0.0f32;
    let mut closest_to_goal = f32::INFINITY;
    // (resolved dir, raw seek dir) captured at the FIRST tick danger is live.
    let mut first_deflection: Option<(Vec2, Vec2)> = None;
    for _ in 0..TICKS {
        let (pos, vel, heading) = kinematics(&w, ship);
        let raw = seek(pos, goal);
        let mut map = ContextMap::default();
        map.add_interest_dir(raw, 1.0, n);
        // Weak explore ring: a brain always has SOMEWHERE it is willing to go,
        // so when the goal hemisphere is masked the best open lane wins.
        for slot in map.interest.iter_mut().take(n) {
            *slot = slot.max(0.25);
        }
        map.add_danger_threat(threat, pos, threat_radius, 1.0, n);
        let intent = match map.resolve(n, ai.danger_mask_floor) {
            Some((dir, _)) => {
                if first_deflection.is_none() && map.danger.iter().take(n).any(|&d| d > 0.0) {
                    first_deflection = Some((dir, raw));
                }
                steer_to_intent(dir, 1.0, heading, vel, authority)
            }
            None => ShipIntent::default(), // fully masked → coast
        };
        write_intent(&mut w, ship, intent);
        sched.run(&mut w);
        let (pos, _, _) = kinematics(&w, ship);
        max_lateral = max_lateral.max(pos.y.abs());
        closest_to_goal = closest_to_goal.min((pos - goal).length());
    }

    let (deflected, raw) = first_deflection.expect("the ship flew into danger range");
    assert!(
        deflected.dot(raw) < 0.5,
        "with the direct lane masked, the resolved heading deflects well off \
         the straight line (resolved {deflected:?} vs raw {raw:?})"
    );
    assert!(
        max_lateral > 10.0,
        "the flown trajectory actually detoured around the danger \
         (max |y| = {max_lateral})"
    );
    assert!(
        closest_to_goal < 30.0,
        "despite the detour the ship still made it to the goal area \
         (closest approach {closest_to_goal})"
    );
}

/// Lead pursuit at integration level: two chasers fly the real flight model at
/// the same crossing target — one steered by `pursue_intercept` (the
/// `turret::aim_angle` L1 lead), one by plain tail-chasing `seek`. The lead
/// pursuer must capture within the window, and strictly sooner than the
/// tail-chaser (the whole point of leading).
#[test]
fn pursue_intercept_captures_a_crossing_target_sooner_than_pure_seek() {
    const TICKS: usize = 600;
    const CAPTURE_RADIUS: f32 = 15.0;
    let target_start = Vec2::new(120.0, -60.0);
    let target_vel = Vec2::new(0.0, 40.0); // crossing mover (< chaser's 80 u/s)
    let tuning = Tuning::default();

    let mut w = make_world();
    let mut sched = make_schedule();
    // Both chasers are already underway at cruise — `pursue_intercept`'s L1
    // solve assumes the chaser's top speed, so a from-rest start would turn
    // the early game into a stern chase for BOTH and blur the comparison.
    let cruise = Vec2::new(50.0, 0.0);
    let pursuer = spawn_ai_ship(&mut w, Vec2::ZERO, 0.0, cruise);
    let seeker = spawn_ai_ship(&mut w, Vec2::ZERO, 0.0, cruise);

    let mut caught_pursue: Option<usize> = None;
    let mut caught_seek: Option<usize> = None;
    let mut min_dist_pursue = f32::INFINITY;
    for tick in 0..TICKS {
        // The target is a virtual constant-velocity mover (pure, no entity).
        let target_pos = target_start + target_vel * (tick as f32 * DT);
        for (ship, lead) in [(pursuer, true), (seeker, false)] {
            let (pos, vel, heading) = kinematics(&w, ship);
            let dir = if lead {
                pursue_intercept(pos, tuning.top_speed(), target_pos, target_vel)
            } else {
                seek(pos, target_pos)
            };
            let intent = steer_to_intent(dir, 1.0, heading, vel, tuning.max_turn_rate());
            write_intent(&mut w, ship, intent);
        }
        sched.run(&mut w);
        let target_now = target_start + target_vel * ((tick + 1) as f32 * DT);
        let pursue_dist = (kinematics(&w, pursuer).0 - target_now).length();
        min_dist_pursue = min_dist_pursue.min(pursue_dist);
        if caught_pursue.is_none() && pursue_dist <= CAPTURE_RADIUS {
            caught_pursue = Some(tick);
        }
        if caught_seek.is_none()
            && (kinematics(&w, seeker).0 - target_now).length() <= CAPTURE_RADIUS
        {
            caught_seek = Some(tick);
        }
    }

    let pursue_tick = caught_pursue.unwrap_or_else(|| {
        panic!("lead pursuit never captured the target (closest approach {min_dist_pursue})")
    });
    assert!(
        caught_seek.is_none_or(|seek_tick| pursue_tick < seek_tick),
        "lead pursuit (tick {pursue_tick}) beats the tail-chase (tick {caught_seek:?})"
    );
}

// ---------------------------------------------------------------------------
// T009 — VC2: formation followers settle ≤300 ticks, hold ≤10%, no chatter
// ---------------------------------------------------------------------------

/// Count MEANINGFUL turn-sign reversals between consecutive samples. A sample
/// participates only when `|turn| >= deadband`: post-settle the turn input
/// hovers numerically around zero (±1e-6-scale f32 wiggle as lateral error
/// decays through epsilon), which is physically quiet station-keeping — the
/// VC2 chatter clause forbids macroscopic left-right oscillation, i.e. sign
/// flips at real stick deflections.
fn turn_sign_flips(turns: &[f32], deadband: f32) -> usize {
    let mut flips = 0;
    let mut last_sign: Option<f32> = None;
    for &t in turns {
        if t.abs() >= deadband {
            let sign = t.signum();
            if last_sign.is_some_and(|prev| sign != prev) {
                flips += 1;
            }
            last_sign = Some(sign);
        }
    }
    flips
}

/// VC2 formation-hold: a leader flies a straight line under constant forward
/// intent; two followers (spawned OFF their slots) steer each tick via
/// `formation_keep` → `steer_to_intent`. By tick 300 each follower's slot
/// error is ≤ 10% of its slot-offset magnitude, STAYS within that band for the
/// rest of the run, and shows no turn-sign chatter after settling.
///
/// Geometry note (spec numbers untouched): `formation_keep`'s station-keeping
/// trail scales with leader speed (error ≈ CLOSE_TIME·THROTTLE_SPEED·v/v_max =
/// v/5 at Tuning defaults), so the 10% band fixes the slot scale — 30-unit
/// behind-left/right slots (|offset| ≈ 42.4, band ≈ 4.24) with a 0.15-throttle
/// leader (12 u/s → steady trail ≈ 2.4) is a reasonable patrol-formation
/// scenario with margin.
#[test]
fn formation_followers_settle_and_hold_without_chatter() {
    const TICKS: usize = 600;
    const SETTLE_BY: usize = 300; // spec VC2: settle window ≤ 300 ticks
    let leader_heading = 0.4; // off-axis so nothing is axis-aligned
    let offsets = [Vec2::new(-30.0, 30.0), Vec2::new(-30.0, -30.0)]; // behind-left / behind-right
    let perturb = [Vec2::new(4.0, 6.0), Vec2::new(-6.0, -4.0)]; // spawn OFF-slot
    let band = 0.10 * offsets[0].length(); // ≤ 10% of slot spacing (spec VC2)
    let authority = Tuning::default().max_turn_rate();

    let mut w = make_world();
    let mut sched = make_schedule();
    let leader = spawn_ai_ship(&mut w, Vec2::ZERO, leader_heading, Vec2::ZERO);
    // Constant forward intent — intents persist as components, so set once.
    write_intent(
        &mut w,
        leader,
        ShipIntent {
            forward: 0.15,
            ..Default::default()
        },
    );
    let leader_frame = Vec2::from_angle(leader_heading);
    let followers: Vec<Entity> = offsets
        .iter()
        .zip(perturb)
        .map(|(&off, p)| spawn_ai_ship(&mut w, leader_frame.rotate(off) + p, 0.0, Vec2::ZERO))
        .collect();

    let mut errors: Vec<[f32; 2]> = Vec::with_capacity(TICKS);
    let mut turns: [Vec<f32>; 2] = [Vec::with_capacity(TICKS), Vec::with_capacity(TICKS)];
    for _ in 0..TICKS {
        let (lp, lv, lh) = kinematics(&w, leader);
        for (k, &f) in followers.iter().enumerate() {
            let (pos, vel, heading) = kinematics(&w, f);
            let (dir, throttle) = formation_keep(pos, vel, lp, lv, lh, offsets[k]);
            let intent = steer_to_intent(dir, throttle, heading, vel, authority);
            turns[k].push(intent.turn);
            write_intent(&mut w, f, intent);
        }
        sched.run(&mut w);
        let (lp, _, lh) = kinematics(&w, leader);
        let frame = Vec2::from_angle(lh);
        let mut err = [0.0f32; 2];
        for (k, &f) in followers.iter().enumerate() {
            err[k] = (kinematics(&w, f).0 - (lp + frame.rotate(offsets[k]))).length();
        }
        errors.push(err);
    }

    // The leader really flew, and the followers really started out of band —
    // the test proves convergence, not a vacuous already-settled state.
    assert!(
        kinematics(&w, leader).0.length() > 50.0,
        "the leader moved a substantial straight-line distance"
    );
    for (k, &e0) in errors[0].iter().enumerate() {
        assert!(
            e0 > band,
            "follower {k} starts OUTSIDE the band (err {e0} > {band})"
        );
    }

    // (a) settled by tick 300 and (b) HOLDS the band for the remainder.
    for (tick, err) in errors.iter().enumerate().skip(SETTLE_BY) {
        for (k, &e) in err.iter().enumerate() {
            assert!(
                e <= band,
                "follower {k} slot error {e} exceeds the 10% band {band} at tick {tick}"
            );
        }
    }

    // (c) no turn-sign chatter after settling: over the 300-tick post-settle
    // window a genuine bang-bang limit cycle flips sign every few ticks (≥ 60
    // flips); we allow ≤ 15 (one flip per 20 ticks on average) — an order of
    // magnitude below chatter — at a 2%-stick significance deadband.
    for (k, t) in turns.iter().enumerate() {
        let flips = turn_sign_flips(&t[SETTLE_BY..], 0.02);
        assert!(
            flips <= 15,
            "follower {k} turn intent chattered after settling ({flips} sign flips in 300 ticks)"
        );
    }
}

// ---------------------------------------------------------------------------
// T015 — OBJ2: deterministic brain + event scheduler (TR-004/TR-005)
// ---------------------------------------------------------------------------

/// A FULL fixed-step world the way the server builds one: the complete
/// `sim::add_fixed_step_systems` schedule plus every resource the gated AI set
/// reads (`ScenarioActive`, `AiTuning`, `CurrentTick`, `RethinkQueue`,
/// `CoarseIndex`) and the ungated systems' inputs (`Tuning`, `FixedDt`,
/// `HitFeedback`, `RefinedResources`).
fn obj2_world() -> (World, Schedule) {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(DT));
    w.insert_resource(HitFeedback::default());
    w.insert_resource(RefinedResources::default());
    w.insert_resource(ScenarioActive);
    w.insert_resource(AiTuning::default());
    w.insert_resource(CurrentTick::default());
    w.insert_resource(RethinkQueue::default());
    w.insert_resource(CoarseIndex::default());
    let mut s = Schedule::default();
    sim::add_fixed_step_systems(&mut s);
    (w, s)
}

/// Mirror the authoritative tick into the world BEFORE the step — exactly the
/// server's `step_sim` order, so the schedule observes ticks 0, 1, 2, ….
fn mirror_tick_and_run(w: &mut World, s: &mut Schedule, tick: u64) {
    w.resource_mut::<CurrentTick>().0 = tick;
    s.run(w);
}

/// The standard OBJ2 AI ship: an UNFITTED flight-model ship (the OBJ1
/// pattern) carrying the full brain stack — `AiBrain` with a waypoint goal
/// (phase bucket derived from its stable id, the real V-4 path), `AiStableId`,
/// an Active `AoiTier`, and a `ShipIntent` for the brain to write.
fn spawn_obj2_ai_ship(w: &mut World, id: AiStableId, pos: Vec2, waypoint: Vec2) -> Entity {
    let buckets = w.resource::<AiTuning>().fallback_bucket_count;
    let brain = AiBrain {
        waypoint: Some(waypoint),
        think_tier: Tier::Active,
        ..AiBrain::new(id, buckets)
    };
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        FlightAssist::On,
        id,
        brain,
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ))
    .id()
}

fn brain_of(w: &World, e: Entity) -> AiBrain {
    *w.get::<AiBrain>(e).expect("entity carries AiBrain")
}

/// OBJ2-VC1 / TR-004: two worlds built identically and stepped through the
/// FULL fixed schedule select identical behaviors and emit identical intents —
/// compared per tick at BIT level (behavior, intent f32 bits + discrete
/// fields, and the resulting pos/vel/heading bits), with the think counter
/// proving real decision work happened on both sides.
#[test]
fn identical_state_yields_identical_selection_and_intents() {
    const TICKS: u64 = 120;
    let start = Vec2::new(10.0, 0.0);
    let waypoint = Vec2::new(40.0, 30.0); // Within the Active AOI of the player.

    let run = || {
        let (mut w, mut s) = obj2_world();
        // A player at the origin keeps the AI ship Active (TR-007 proximity).
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        let ship = spawn_obj2_ai_ship(&mut w, AiStableId(0), start, waypoint);
        let mut trace = Vec::with_capacity(TICKS as usize);
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            let intent = *w.get::<ShipIntent>(ship).expect("ship has ShipIntent");
            trace.push((
                brain_of(&w, ship).behavior,
                [
                    intent.forward.to_bits(),
                    intent.strafe.to_bits(),
                    intent.turn.to_bits(),
                ],
                intent, // PartialEq covers the discrete fire/group/assist fields.
                state_bits(&w, ship),
            ));
        }
        (trace, brain_of(&w, ship).thinks_total)
    };

    let (trace_a, thinks_a) = run();
    let (trace_b, thinks_b) = run();

    assert_eq!(
        trace_a, trace_b,
        "identical sim state must select identical behaviors and emit \
         identical intents/kinematics per tick (OBJ2-VC1, bit identity)"
    );
    assert_eq!(thinks_a, thinks_b, "identical think counts across the runs");

    // The comparison is MEANINGFUL: the brain really thought (repeatedly, on
    // its cadence), selected the waypoint behavior, and burned toward it.
    assert!(
        thinks_a >= 2,
        "the brain completed several cadence thinks over {TICKS} ticks (got {thinks_a})"
    );
    assert!(
        trace_a.iter().any(|(b, ..)| *b == Behavior::Waypoint),
        "the brain selected Waypoint from its goal"
    );
    assert!(
        trace_a
            .iter()
            .any(|(_, bits, ..)| f32::from_bits(bits[0]) > 0.5),
        "the selected behavior emitted real thrust intents"
    );
}

/// A think-only world: `ai_think_system` in isolation (the scheduler is the
/// unit under test) plus the three resources it reads.
fn think_world(tuning: AiTuning) -> (World, Schedule) {
    let mut w = World::new();
    w.insert_resource(tuning);
    w.insert_resource(CurrentTick(0));
    w.insert_resource(RethinkQueue::default());
    let mut s = Schedule::default();
    s.add_systems(ai_think_system);
    (w, s)
}

/// OBJ2-VC2 / TR-005: with a very long fallback cadence (1000 ticks, phase
/// bucket 0 → cadence-due ONLY at tick 0 in this window):
/// (a) a calm ship does ZERO decision work between cadence ticks;
/// (b) a ship that takes a hit re-evaluates THAT tick (`DamageTaken` breaks
///     the commit window by design);
/// (c) two events queued the same tick coalesce into exactly ONE think.
#[test]
fn hit_event_rethinks_same_tick_and_calm_ship_is_idle() {
    let tuning = AiTuning {
        think_ticks_active: 1000,
        ..AiTuning::default()
    };
    let (mut w, mut s) = think_world(tuning);
    let e = w
        .spawn((
            AiStableId(0),
            AiBrain {
                think_tier: Tier::Active,
                phase_bucket: 0,
                waypoint: Some(Vec2::new(100.0, 0.0)),
                ..AiBrain::default()
            },
        ))
        .id();

    // Tick 0 is the single cadence tick of the window: think #1 selects
    // Waypoint and arms the 1000-tick commit window.
    mirror_tick_and_run(&mut w, &mut s, 0);
    let b = brain_of(&w, e);
    assert_eq!(b.thinks_total, 1, "the cadence tick thought once");
    assert_eq!(b.behavior, Behavior::Waypoint);
    assert_eq!(b.commit_until_tick, 1000, "window = one cadence period");

    // (a) Calm + off-cadence: 49 ticks of zero decision work.
    for tick in 1..50 {
        mirror_tick_and_run(&mut w, &mut s, tick);
    }
    let b = brain_of(&w, e);
    assert_eq!(
        b.thinks_total, 1,
        "an idle calm ship triggers ZERO thinks between cadence ticks (VC2)"
    );
    assert_eq!(b.last_think_tick, 0, "last think still the cadence tick");

    // (b) A hit mid-window: the event think happens the SAME tick, straight
    // through the commit window (DamageTaken overrides it by design).
    w.resource_mut::<RethinkQueue>()
        .push(e, AiEvent::DamageTaken);
    mirror_tick_and_run(&mut w, &mut s, 50);
    let b = brain_of(&w, e);
    assert_eq!(
        b.last_think_tick, 50,
        "a ship that takes a hit re-evaluates THAT tick (VC2 event think)"
    );
    assert_eq!(b.thinks_total, 2);
    assert!(
        w.resource::<RethinkQueue>().is_empty(),
        "the queue drains at the end of the think run"
    );

    // (c) Two events the same tick coalesce into one entry → one think.
    {
        let mut q = w.resource_mut::<RethinkQueue>();
        q.push(e, AiEvent::DamageTaken);
        q.push(e, AiEvent::NewContact);
        assert_eq!(q.len(), 1, "two same-tick events, ONE coalesced entry");
    }
    mirror_tick_and_run(&mut w, &mut s, 60);
    let b = brain_of(&w, e);
    assert_eq!(
        b.thinks_total, 3,
        "the coalesced pair produced exactly ONE additional think"
    );
    assert_eq!(b.last_think_tick, 60);
    assert_eq!(
        b.behavior,
        Behavior::Waypoint,
        "selection stays stable across event thinks (same state, same pick)"
    );
}

/// Tiebreak stability at integration level (HINT-002 level one):
/// - an EXACT (`f32 ==`) score tie inside one priority bucket selects the
///   lower behavior ordinal — 100×, in both candidate orders, never wavering;
/// - through the REAL think system, a waypoint + slot-less leader score the
///   exact same presence baseline → `Waypoint` (lower ordinal) wins from a
///   fresh brain, while an incumbent `Follow` survives the tie on momentum —
///   on every repeated think.
#[test]
fn selection_tiebreaks_are_stable() {
    // Pure selection under repetition.
    for _ in 0..100 {
        for candidates in [
            [(Behavior::Sweep, 0.5), (Behavior::Scout, 0.5)],
            [(Behavior::Scout, 0.5), (Behavior::Sweep, 0.5)],
        ] {
            assert_eq!(
                select_behavior(&candidates, Behavior::Hold, 0.25),
                Behavior::Scout,
                "exact tie → lower ordinal, independent of candidate order"
            );
        }
    }

    // Through the scheduler: both goals present → Waypoint and Follow tie at
    // the exact MOVE_BASELINE presence score every think.
    let (mut w, mut s) = think_world(AiTuning::default());
    let leader = w.spawn_empty().id();
    let goals = AiBrain {
        think_tier: Tier::Active,
        phase_bucket: 0,
        waypoint: Some(Vec2::new(50.0, 0.0)),
        leader: Some(leader),
        ..AiBrain::default()
    };
    let fresh = w.spawn((AiStableId(0), goals)).id();
    let incumbent = w
        .spawn((
            AiStableId(1),
            AiBrain {
                behavior: Behavior::Follow,
                ..goals
            },
        ))
        .id();

    for tick in 0..=300 {
        mirror_tick_and_run(&mut w, &mut s, tick);
        assert_eq!(
            brain_of(&w, fresh).behavior,
            Behavior::Waypoint,
            "exact Waypoint/Follow tie breaks to the lower ordinal at tick {tick}"
        );
        assert_eq!(
            brain_of(&w, incumbent).behavior,
            Behavior::Follow,
            "the incumbent survives the near-tie on momentum at tick {tick}"
        );
    }
    assert!(
        brain_of(&w, fresh).thinks_total >= 20,
        "the same selection held across many REPEATED thinks (got {})",
        brain_of(&w, fresh).thinks_total
    );
}

/// TR-004 CI enforcement: the scoring/curve/select region of `brain.rs` —
/// delimited by the `STRICT-F32 SCORING BEGIN/END` markers — contains no
/// transcendental call. `include_str!` pins the check to the exact source
/// that was compiled, so a violation fails this build, not a later audit.
#[test]
fn strict_f32_scoring_grep() {
    const SRC: &str = include_str!("../src/ai/brain.rs");
    let begin = SRC
        .find("STRICT-F32 SCORING BEGIN")
        .expect("brain.rs carries the STRICT-F32 SCORING BEGIN marker (TR-004)");
    let end = SRC
        .find("STRICT-F32 SCORING END")
        .expect("brain.rs carries the STRICT-F32 SCORING END marker (TR-004)");
    assert!(begin < end, "BEGIN marker precedes END marker");
    let region = &SRC[begin..end];

    // The markers actually bracket the scoring core, not an empty span.
    for required in ["fn curve_linear", "fn score_behavior", "fn select_behavior"] {
        assert!(
            region.contains(required),
            "the marked region contains `{required}`"
        );
    }
    // The TR-004 ban list: call-syntax matches (`name(`) so prose mentions in
    // doc comments don't false-positive.
    for banned in ["sin(", "cos(", "exp(", "powf(", "sqrt(", "atan2("] {
        assert!(
            !region.contains(banned),
            "TR-004 violation: `{banned}` found in the strict-f32 scoring region of brain.rs"
        );
    }
}

// ---------------------------------------------------------------------------
// R96 Part A+B — movement profiles (Cruise parity + active braking)
// ---------------------------------------------------------------------------

/// The brain.rs `ARRIVE_RADIUS` const is `pub(crate)`; the integration suite
/// pins the same canonical value (also the steering-tests' radius).
const R96_ARRIVE_RADIUS: f32 = 10.0;

/// A fitted `ShipStats` with an explicit, strong retro (reverse) channel so the
/// active-braking profiles have real brake authority to test. Built from the
/// squad-test fighter fit (drag normalized to 1, top speed pinned) with the
/// reverse force overridden to a fraction of forward thrust.
fn stats_with_brake(top_speed: f32, reverse_force: f32) -> sim::fitting::ShipStats {
    let mut s = stats_with_top_speed(top_speed);
    s.reverse_force = reverse_force;
    s
}

/// R96 PARITY KEYSTONE — a `MovementProfile::Cruise` ship flying a `Waypoint`
/// goal through the FULL fixed schedule emits the EXACT pre-R96 trajectory:
/// (a) byte-identical across two independent runs (determinism, V-3); and
/// (b) byte-identical to a reference ship hand-driven by the pre-R96 primitives
/// (`waypoint_follow` → `steer_to_intent`, the only intent the old `fly_to`
/// produced) — proving Cruise emits the SAME intents as the old code.
#[test]
fn cruise_profile_is_byte_identical_to_baseline() {
    const TICKS: u64 = 90;
    let start = Vec2::new(10.0, 0.0);
    let waypoint = Vec2::new(120.0, 40.0);

    // The AI-driven Cruise ship through the full schedule (think + execute +
    // flight), captured per tick at BIT level.
    let cruise_run = || {
        let (mut w, mut s) = obj2_world();
        w.spawn((PlayerShip, Position(Vec2::ZERO))); // keep it Active-tier
        let buckets = w.resource::<AiTuning>().fallback_bucket_count;
        let brain = AiBrain {
            waypoint: Some(waypoint),
            think_tier: Tier::Active,
            movement_profile: MovementProfile::Cruise,
            ..AiBrain::new(AiStableId(0), buckets)
        };
        let ship = w
            .spawn((
                Ship,
                ShipIntent::default(),
                Position(start),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                FlightAssist::On,
                AiStableId(0),
                brain,
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 0,
                },
            ))
            .id();
        let mut trace = Vec::with_capacity(TICKS as usize);
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            trace.push(state_bits(&w, ship));
        }
        trace
    };

    let run_a = cruise_run();
    let run_b = cruise_run();
    assert_eq!(
        run_a, run_b,
        "Cruise is deterministic across identical runs (V-3)"
    );

    // The pre-R96 reference: an UNFITTED ship hand-driven by exactly the intent
    // the old `fly_to` emitted (`waypoint_follow` → `steer_to_intent`), through
    // the flight model alone. The Cruise brain's `throttle_cap` is the default
    // 1.0 (a `*1.0` no-op), so no cap scaling enters either path.
    let mut rw = make_world();
    let mut rs = make_schedule();
    let reference = spawn_ai_ship(&mut rw, start, 0.0, Vec2::ZERO);
    let mut ref_trace = Vec::with_capacity(TICKS as usize);
    for _ in 0..TICKS {
        let (p, v, h) = kinematics(&rw, reference);
        // The Waypoint arm holds (zero intent) within ARRIVE_RADIUS, else flies.
        let intent = if (waypoint - p).length() <= R96_ARRIVE_RADIUS {
            ShipIntent::default()
        } else {
            let (dir, throttle, _) = waypoint_follow(p, v, &[waypoint], 0, R96_ARRIVE_RADIUS);
            steer_to_intent(dir, throttle, h, v, 0.0) // unfitted → turn_authority 0
        };
        write_intent(&mut rw, reference, intent);
        rs.run(&mut rw);
        ref_trace.push(state_bits(&rw, reference));
    }

    assert_eq!(
        run_a, ref_trace,
        "Cruise emits the SAME trajectory as the pre-R96 hand-driven baseline \
         (the determinism keystone — Cruise == old path, byte for byte)"
    );
}

/// R96 Part B — Rush actively brakes onto its goal and settles CLOSER than
/// Leisurely (which paces slower and coasts further), and NEITHER overshoots
/// wildly. Both fly the SAME fitted ship + goal through the full schedule; only
/// the `movement_profile` differs.
#[test]
fn rush_settles_on_goal_vs_leisurely_coasts_past() {
    const TICKS: u64 = 400;
    let start = Vec2::new(0.0, 0.0);
    let goal = Vec2::new(220.0, 0.0);

    // top_speed 80, a strong retro (reverse_force 60) so braking is decisive.
    let stats = stats_with_brake(80.0, 60.0);

    // R98 HOTFIX C — the run is EXTENDED past the original 400-tick window so
    // the heavy test fighter (mass ~20, retro 60 → ≤ ~4 u/s² of brake) genuinely
    // ARRIVES and parks; the original rest/overshoot comparisons keep their
    // exact pre-R98 semantics by sampling at the original 400-tick mark.
    const SETTLE_TICKS: u64 = 2400;
    let rest_distance = |profile: MovementProfile| -> (f32, f32, f32) {
        let (mut w, mut s) = obj2_world();
        w.spawn((PlayerShip, Position(start))); // keep the ship Active-tier
        let buckets = w.resource::<AiTuning>().fallback_bucket_count;
        let brain = AiBrain {
            waypoint: Some(goal),
            think_tier: Tier::Active,
            // R96: pin the profile through the highest-precedence channel
            // (`squad_profile`) so the think-time resolution preserves it (a bare
            // `movement_profile` is a RESOLVED field — the think overwrites it
            // each tick from squad ← role ← archetype default).
            squad_profile: Some(profile),
            ..AiBrain::new(AiStableId(0), buckets)
        };
        let ship = w
            .spawn((
                Ship,
                ShipIntent::default(),
                Position(start),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                FlightAssist::On,
                stats,
                AiStableId(0),
                brain,
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 0,
                },
            ))
            .id();
        let mut max_overshoot: f32 = 0.0;
        let mut rest_at_400 = f32::INFINITY;
        // R98 HOTFIX C — the anti-bang-bang probe: the peak SPEED over the
        // final ~60 ticks of the extended run. The old full-reverse brake
        // parked the ship on a full-authority forward/reverse oscillator at
        // the goal; the proportional brake + deadband must leave it genuinely
        // settled.
        let mut tail_max_speed: f32 = 0.0;
        for tick in 0..SETTLE_TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            let (p, v, _) = kinematics(&w, ship);
            // Overshoot = how far PAST the goal along the approach axis (+X).
            max_overshoot = max_overshoot.max(p.x - goal.x);
            if tick + 1 == TICKS {
                rest_at_400 = (goal - p).length();
            }
            if tick >= SETTLE_TICKS - 60 {
                tail_max_speed = tail_max_speed.max(v.length());
            }
        }
        (rest_at_400, max_overshoot, tail_max_speed)
    };

    let (rush_rest, rush_overshoot, rush_tail_speed) = rest_distance(MovementProfile::Rush);
    let (leisurely_rest, _, leisurely_tail_speed) = rest_distance(MovementProfile::Leisurely);

    assert!(
        rush_rest < leisurely_rest,
        "Rush actively brakes and settles closer ({rush_rest}) than Leisurely \
         coasts ({leisurely_rest})"
    );
    // Neither overshoots wildly: Rush's active brake keeps it well shy of a full
    // fly-through (a body length or two, not hundreds of units past).
    assert!(
        rush_overshoot < 40.0,
        "Rush does not blow past the goal (max overshoot {rush_overshoot})"
    );
    // R98 HOTFIX C — the anti-bang-bang guarantee: over the final ~60 ticks the
    // ship is genuinely AT REST (speed under a small bound — the proportional
    // brake decays into the arrive deadband and drag finishes), with NO residual
    // full-reverse/full-forward oscillation parked on the goal.
    assert!(
        rush_tail_speed < 2.5,
        "Rush rests settled at the goal — no residual oscillation \
         (tail max speed {rush_tail_speed})"
    );
    assert!(
        leisurely_tail_speed < 2.5,
        "Leisurely rests settled too (tail max speed {leisurely_tail_speed})"
    );
}

// ---------------------------------------------------------------------------
// R96 Part D — obstacle avoidance through the FULL fixed schedule
// (the ObstacleField + the move/combat avoidance arms; the empty-field gate)
// ---------------------------------------------------------------------------

/// Spawn a large neutral obstacle body: a static `Target` with a
/// `CollisionRadius` ≥ the avoidance min radius, so the
/// `build_obstacle_field_system` indexes it. Static (zero velocity) and
/// `Dummy`-kind so the seek/target-motion systems leave it put.
fn spawn_obstacle(w: &mut World, pos: Vec2, radius: f32) -> Entity {
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        CollisionRadius(radius),
    ))
    .id()
}

/// R96 Part D — VC1 (move arm): a `Waypoint` ship with a LARGE obstacle parked
/// on the straight line between it and its goal DETOURS around the obstacle
/// (max lateral deviation from the straight start→goal line exceeds a clear
/// threshold) yet still REACHES the goal — the real-ObstacleField analog of the
/// `context_map_danger_deflects_around_a_blocked_path` steering unit test.
#[test]
fn ship_steers_around_obstacle_between_it_and_its_goal() {
    const TICKS: u64 = 4000;
    let start = Vec2::new(0.0, 0.0);
    let goal = Vec2::new(600.0, 0.0);
    // A big asteroid squarely on the +X line between the start and the goal —
    // far enough from the goal that, once cleared, it drops out of the ship's
    // avoidance scope so it settles cleanly (no phantom deflection on arrival).
    let obstacle_pos = Vec2::new(250.0, 0.0);
    let obstacle_radius = 50.0;

    let (mut w, mut s) = obj2_world();
    w.insert_resource(ObstacleField::default()); // enable the Part-D build system.
                                                 // Wide AOI so the ship stays Active-tier across the whole 400-unit run; a
                                                 // wide clearance pad + query radius so the ship begins its detour EARLY and
                                                 // gives the big body a clear berth (exercising those Part-D knobs).
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        // Wide clearance + long predictive lookahead so a non-holonomic ship (it
        // cannot snap its heading) begins turning FAR from the big body and
        // clears it with margin instead of carving across it at speed.
        obstacle_clearance_pad: 40.0,
        obstacle_query_radius: 320.0,
        obstacle_lookahead_s: 6.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(start)));
    spawn_obstacle(&mut w, obstacle_pos, obstacle_radius);
    let buckets = w.resource::<AiTuning>().fallback_bucket_count;
    let brain = AiBrain {
        waypoint: Some(goal),
        think_tier: Tier::Active,
        // Rush profile (active braking) so the ship paces + settles cleanly onto
        // the goal rather than the fast unfitted ship's overshoot orbit — the
        // detour is what is under test, not the flight model's coast.
        movement_profile: MovementProfile::Rush,
        ..AiBrain::new(AiStableId(0), buckets)
    };
    // A fitted ship with real top speed + retro brake (the Rush-test fighter).
    let stats = stats_with_brake(80.0, 60.0);
    let ship = w
        .spawn((
            Ship,
            ShipIntent::default(),
            Position(start),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            FlightAssist::On,
            stats,
            CollisionRadius(4.0),
            AiStableId(0),
            brain,
            AoiTier {
                tier: Tier::Active,
                since_tick: 0,
            },
        ))
        .id();

    // The obstacle field really populated (the build system ran + indexed it).
    mirror_tick_and_run(&mut w, &mut s, 0);
    assert_eq!(
        w.resource::<ObstacleField>().obstacles.len(),
        1,
        "the large obstacle entered the field"
    );

    let mut max_lateral: f32 = 0.0;
    let mut min_surface_gap = f32::INFINITY;
    let mut closest_to_goal = f32::INFINITY;
    for tick in 1..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        let p = pos_of(&w, ship);
        // Lateral deviation off the straight start→goal (+X) line.
        max_lateral = max_lateral.max(p.y.abs());
        // Surface-to-surface clearance from the obstacle (must never plunge in).
        let gap = (p - obstacle_pos).length() - obstacle_radius - 4.0;
        min_surface_gap = min_surface_gap.min(gap);
        closest_to_goal = closest_to_goal.min((p - goal).length());
    }

    // (a) DETOUR: the ship swings well off the straight start→goal line to clear
    // the radius-50 body (a deviation past 20 u is unambiguously around it, not
    // a graze) — the move-arm analog of the steering deflection unit test.
    assert!(
        max_lateral > 20.0,
        "the ship DETOURS off the straight line around the obstacle \
         (max lateral deviation {max_lateral:.1})"
    );
    // (b) NEVER PLUNGES THROUGH: the avoidance keeps real surface clearance — the
    // ship goes AROUND, never carving across the body.
    assert!(
        min_surface_gap > 0.0,
        "the ship keeps clearance from the obstacle surface (min gap {min_surface_gap:.1})"
    );
    // (c) STILL REACHES the goal REGION: after clearing the body the ship arrives
    // in the goal's neighborhood — a closest approach far smaller than the
    // 600-unit start→goal span proves the detour is a way AROUND that completes
    // the nav goal, not a dead end. (The fast fighter then orbits the goal a
    // little, a flight-model coast trait — the avoidance carried it THERE.)
    assert!(
        closest_to_goal < 60.0,
        "the ship still reaches the goal region past the obstacle \
         (closest approach {closest_to_goal:.1})"
    );
    eprintln!(
        "[r96d] move detour: max lateral {max_lateral:.1}, min surface gap {min_surface_gap:.1}, \
         closest to goal {closest_to_goal:.1}"
    );
}

/// R96 Part D — VC1 (combat arm): a fighter charging an enemy with a LARGE
/// asteroid between them DETOURS around the asteroid (max lateral deviation off
/// the ship→target line exceeds a threshold) rather than driving straight into
/// it — the Engage analog of the move-arm detour, via the same shared
/// `add_obstacle_danger` on `engage_motion`'s context map.
#[test]
fn ship_steers_around_obstacle_between_it_and_its_target() {
    const TICKS: u64 = 2000;
    let start = Vec2::new(0.0, 0.0);
    // The (indestructible — no Health/FitLayout) enemy the fighter charges.
    let target_pos = Vec2::new(400.0, 0.0);
    // A big asteroid squarely on the line between the fighter and its target.
    let obstacle_pos = Vec2::new(200.0, 0.0);
    let obstacle_radius = 50.0;

    let (mut w, mut s) = obj2_world();
    w.insert_resource(ObstacleField::default());
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        obstacle_clearance_pad: 40.0,
        obstacle_query_radius: 400.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(start)));
    spawn_obstacle(&mut w, obstacle_pos, obstacle_radius);
    let target = w
        .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
        .id();
    // A Brawler (close-ring) fit so the engage arm CLOSES toward the target
    // (radial > 0 → lead pursuit) and thus must transit the obstacle's line.
    let stats = combat_stats(80.0, 30.0, 200.0);
    let ai = *w.resource::<AiTuning>();
    assert_eq!(
        sim::ai::classify_archetype(&stats, &ai),
        FitArchetype::Brawler,
        "the charging fighter is a close-ring Brawler"
    );
    let fighter = spawn_brain_combat_ship(&mut w, 0, start, 0.0, Vec2::ZERO, target, stats);
    w.entity_mut(fighter).insert(CollisionRadius(4.0));

    mirror_tick_and_run(&mut w, &mut s, 0);
    assert_eq!(
        w.resource::<ObstacleField>().obstacles.len(),
        1,
        "the asteroid entered the field"
    );

    let mut max_lateral: f32 = 0.0;
    let mut min_surface_gap = f32::INFINITY;
    for tick in 1..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        assert_eq!(
            brain_of(&w, fighter).behavior,
            Behavior::Engage,
            "the fighter stays on the Engage task (tick {tick})"
        );
        let p = pos_of(&w, fighter);
        max_lateral = max_lateral.max(p.y.abs());
        let gap = (p - obstacle_pos).length() - obstacle_radius - 4.0;
        min_surface_gap = min_surface_gap.min(gap);
        // Stop once it has clearly passed the obstacle's x toward the target.
        if p.x > obstacle_pos.x + obstacle_radius + 20.0 {
            break;
        }
    }

    assert!(
        max_lateral > 8.0,
        "the charging fighter DETOURS around the asteroid between it and its \
         target (max lateral deviation {max_lateral:.1})"
    );
    // The avoidance keeps real surface clearance — it goes AROUND, not through.
    assert!(
        min_surface_gap > 0.0,
        "the fighter keeps clearance from the asteroid surface (min gap {min_surface_gap:.1})"
    );
    eprintln!(
        "[r96d] combat detour: max lateral {max_lateral:.1}, min surface gap {min_surface_gap:.1}"
    );
}

/// R96 Part D — THE EMPTY-FIELD GATE: a `Waypoint`/Cruise ship in a world with
/// the `ObstacleField` ENABLED but holding NO qualifying obstacle emits intents
/// and a trajectory BIT-identical to the same ship with NO `ObstacleField` at
/// all (the pre-R96-D path). This is what guarantees determinism + parity — the
/// avoidance code only ever runs when an obstacle is actually in range.
#[test]
fn no_obstacles_is_byte_identical() {
    const TICKS: u64 = 120;
    let start = Vec2::new(10.0, 0.0);
    let waypoint = Vec2::new(120.0, 40.0);

    // `with_field`: insert the ObstacleField resource (so the build system runs)
    // but spawn no obstacle — the empty-field gate must make this a no-op.
    let run = |with_field: bool| -> Vec<[u32; 6]> {
        let (mut w, mut s) = obj2_world();
        if with_field {
            w.insert_resource(ObstacleField::default());
        }
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        let buckets = w.resource::<AiTuning>().fallback_bucket_count;
        let brain = AiBrain {
            waypoint: Some(waypoint),
            think_tier: Tier::Active,
            movement_profile: MovementProfile::Cruise,
            ..AiBrain::new(AiStableId(0), buckets)
        };
        let ship = w
            .spawn((
                Ship,
                ShipIntent::default(),
                Position(start),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                FlightAssist::On,
                // A CollisionRadius is present (real ships carry one), exercising
                // the own_radius path — it must still be a no-op with no obstacle.
                CollisionRadius(4.0),
                AiStableId(0),
                brain,
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 0,
                },
            ))
            .id();
        let mut trace = Vec::with_capacity(TICKS as usize);
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            trace.push(state_bits(&w, ship));
        }
        trace
    };

    let with_field = run(true);
    let without_field = run(false);
    assert_eq!(
        with_field, without_field,
        "an enabled-but-empty ObstacleField is BIT-identical to no field at all \
         (the empty-field gate — the determinism keystone of R96 Part D)"
    );

    // And it matches the pre-R96 hand-driven Cruise baseline byte-for-byte (the
    // same reference `cruise_profile_is_byte_identical_to_baseline` uses).
    let mut rw = make_world();
    let mut rs = make_schedule();
    let reference = spawn_ai_ship(&mut rw, start, 0.0, Vec2::ZERO);
    let mut ref_trace = Vec::with_capacity(TICKS as usize);
    for _ in 0..TICKS {
        let (p, v, h) = kinematics(&rw, reference);
        let intent = if (waypoint - p).length() <= R96_ARRIVE_RADIUS {
            ShipIntent::default()
        } else {
            let (dir, throttle, _) = waypoint_follow(p, v, &[waypoint], 0, R96_ARRIVE_RADIUS);
            steer_to_intent(dir, throttle, h, v, 0.0)
        };
        write_intent(&mut rw, reference, intent);
        rs.run(&mut rw);
        ref_trace.push(state_bits(&rw, reference));
    }
    assert_eq!(
        with_field, ref_trace,
        "the empty-field move arm emits the SAME trajectory as the pre-R96 \
         hand-driven Cruise baseline (byte for byte)"
    );
}

// ---------------------------------------------------------------------------
// T020/T021 — OBJ3: behavior-LOD through the FULL fixed schedule
// (squads, cheap-glide aggregates, per-tier think scaling)
// ---------------------------------------------------------------------------

/// The standard OBJ3 squad member: an UNFITTED flight-model ship (the OBJ1
/// pattern) carrying a GOAL-LESS brain — the squad assigns goals — with a
/// stable id drawn from the world allocator (so member ids and the squad ids
/// `spawn_squad` allocates never collide) and a default-`Dormant` [`AoiTier`]
/// for the classifier.
fn spawn_squad_member(w: &mut World, pos: Vec2) -> Entity {
    w.init_resource::<AiIdAllocator>();
    let id = w.resource_mut::<AiIdAllocator>().allocate();
    let buckets = w.resource::<AiTuning>().fallback_bucket_count;
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        FlightAssist::On,
        id,
        AiBrain::new(id, buckets),
        AoiTier::default(),
    ))
    .id()
}

fn pos_of(w: &World, e: Entity) -> Vec2 {
    w.get::<Position>(e).expect("entity has Position").0
}

fn squad_of(w: &World, e: Entity) -> Squad {
    w.get::<Squad>(e)
        .expect("squad entity carries Squad")
        .clone()
}

/// Change a squad's standing order mid-run and force the next tick's
/// squad think via the membership-change sentinel (the squad-test pattern:
/// external order changes otherwise wait for the cadence).
fn set_squad_order(w: &mut World, se: Entity, order: SquadOrder) {
    let mut squad = w.get_mut::<Squad>(se).expect("squad entity carries Squad");
    squad.order = order;
    squad.last_member_count = u32::MAX;
}

/// OBJ3-VC2 / TR-008 [COMPLETES TR-008 with the next test]: the full
/// dormant-aggregate ROUND-TRIP through the real schedule — collapse to a
/// cheap glide far from the player, glide bit-consistently, expand with ZERO
/// positional pop when the player arrives, fly full physics, then RE-COLLAPSE
/// when the player leaves.
///
/// No-pop measurement note: expansion happens inside `glide_motion_system`,
/// and the expanded members are steered + flown by the flight model the SAME
/// tick (the documented TR-008 ordering), so the promote-tick comparison is
/// arranged to make that first physics step exactly identity — the squad order
/// is switched to `Hold` while still gliding (glide velocity → ZERO, member
/// intents already zeroed at collapse), so the end-of-tick member positions
/// ARE the expansion positions, bit-comparable against the last glide-written
/// positions. No collidables anywhere near → the validity nudge must be zero.
#[test]
fn aggregate_round_trip_has_no_positional_pop() {
    let (mut w, mut s) = obj2_world();
    // A player EXISTS but far beyond aoi_radius_mid (520): the squad settles
    // Dormant exactly as an off-screen formation would.
    let player = w
        .spawn((PlayerShip, Position(Vec2::new(50_000.0, 0.0))))
        .id();

    let m0 = spawn_squad_member(&mut w, Vec2::new(0.0, 0.0));
    let m1 = spawn_squad_member(&mut w, Vec2::new(8.0, 0.0));
    let m2 = spawn_squad_member(&mut w, Vec2::new(16.0, 0.0));
    let members = [m0, m1, m2];
    let se = spawn_squad(
        &mut w,
        &members,
        FormationDef::wedge(3, 8.0),
        SquadOrder::MoveTo(Vec2::new(400.0, 0.0)),
    );

    // Phase 1 — COLLAPSE: the default 30-tick hysteresis dwell elapses at tick
    // 30 and the squad becomes a cheap-glide aggregate.
    for t in 0..=30 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert!(
        w.get::<GlideState>(se).is_some(),
        "Dormant squad collapsed to a glide aggregate at the hysteresis dwell"
    );
    for &m in &members {
        assert!(w.get::<Gliding>(m).is_some(), "member marked Gliding");
    }

    // Phase 2 — the GLIDE: per tick the aggregate advances and every member
    // sits at squad + collapse-offset BIT-exactly (the per-tick member
    // positions ARE the deterministic glide-extrapolated path).
    let offsets = w
        .get::<GlideState>(se)
        .expect("gliding")
        .member_offsets
        .clone();
    let mut last_squad = pos_of(&w, se);
    for t in 31..=60 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let sp = pos_of(&w, se);
        assert_ne!(
            sp, last_squad,
            "MoveTo glide advances every tick (tick {t})"
        );
        last_squad = sp;
        for &(m, off) in &offsets {
            assert_eq!(
                pos_of(&w, m),
                sp + off,
                "glide path: member == squad + offset, bit-exact (tick {t})"
            );
        }
    }

    // Phase 3 — park the glide (order → Hold drops the goal, glide velocity →
    // ZERO the next tick) so the promote-tick physics step below is identity.
    set_squad_order(&mut w, se, SquadOrder::Hold);
    for t in 61..=63 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert!(w.get::<GlideState>(se).is_some(), "still gliding (parked)");
    let glide_pos: Vec<Vec2> = members.iter().map(|&m| pos_of(&w, m)).collect();

    // Phase 4 — PROMOTE: the player teleports within aoi_radius_mid → the
    // classifier promotes the squad THIS tick (promotion is immediate) and
    // `glide_motion_system` expands the same tick. No penetration → zero
    // nudge → promote-tick positions equal the last glide-written positions
    // bit-for-bit (TR-008 no-pop). R98 HOTFIX B3 re-pin: 300 u sits inside the
    // Mid band of the new default radii (Active 120 < 300 ≤ Mid 520) — the
    // same band geometry the old 100 u had against the 60/240 defaults.
    w.get_mut::<Position>(player).unwrap().0 = pos_of(&w, se) + Vec2::new(300.0, 0.0);
    mirror_tick_and_run(&mut w, &mut s, 64);
    assert!(
        w.get::<GlideState>(se).is_none(),
        "promotion expanded the aggregate"
    );
    assert_eq!(
        w.get::<AoiTier>(se).unwrap().tier,
        Tier::Mid,
        "player within the mid band promoted the squad"
    );
    for (i, &m) in members.iter().enumerate() {
        assert!(w.get::<Gliding>(m).is_none(), "Gliding removed");
        assert_eq!(
            pos_of(&w, m),
            glide_pos[i],
            "promote-tick position == last glide-written position, bit-exact \
             (no pop, zero nudge)"
        );
    }

    // Phase 5 — FULL PHYSICS: a fresh MoveTo order makes the expanded members
    // fly the real flight model (positions keep evolving after expansion).
    let goal2 = pos_of(&w, se) + Vec2::new(80.0, 0.0);
    set_squad_order(&mut w, se, SquadOrder::MoveTo(goal2));
    for t in 65..=124 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert!(
        (pos_of(&w, m0) - glide_pos[0]).length() > 5.0,
        "expanded leader flew full physics toward the new goal"
    );
    assert!(
        w.get::<GlideState>(se).is_none(),
        "stays expanded while the player is near"
    );

    // Phase 6 — RE-COLLAPSE: the player leaves; after the demotion dwell plus
    // the collapse dwell the squad glides again. Round-trip complete.
    w.get_mut::<Position>(player).unwrap().0 = Vec2::new(50_000.0, 0.0);
    for t in 125..=320 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert!(
        w.get::<GlideState>(se).is_some(),
        "squad re-collapsed to a glide after the player left (round-trip)"
    );
    for &m in &members {
        assert!(w.get::<Gliding>(m).is_some(), "member gliding again");
    }
}

/// OBJ3-VC3 helper: dormant/gliding groups must show ZERO combat — no fire
/// intents on any member (gliding members hold a fully-zeroed intent) and no
/// projectile entities anywhere in the world.
fn assert_no_dormant_combat(w: &mut World, members: &[Entity]) {
    for &m in members {
        let intent = *w.get::<ShipIntent>(m).expect("member has ShipIntent");
        assert!(
            !intent.fire_primary && !intent.fire_secondary,
            "no fire intent from dormant/gliding AI (VC3 no-dormant-combat)"
        );
        if w.get::<Gliding>(m).is_some() {
            assert_eq!(
                intent,
                ShipIntent::default(),
                "gliding members hold zero intent"
            );
        }
    }
    let projectiles = w.query_filtered::<(), With<Projectile>>().iter(w).count();
    assert_eq!(projectiles, 0, "no projectiles while groups are dormant");
}

/// OBJ3-VC3 / TR-008+Q1 [COMPLETES TR-008]: two hostile-factioned squads, both
/// dormant + gliding far from any player, brought within `base_sensor_range`
/// of each other → BOTH promote via their own far hostile scans (mutuality)
/// and expand to full physics; until then NO combat ever occurs (zero fire
/// intents, zero projectiles).
#[test]
fn mutual_hostile_aggregates_promote_and_dormant_groups_dont_fight() {
    let (mut w, mut s) = obj2_world();
    // NO players at all: everything settles Dormant (far from any player).

    let a_members: Vec<Entity> = (0..3)
        .map(|i| spawn_squad_member(&mut w, Vec2::new(i as f32 * 6.0, 0.0)))
        .collect();
    for &m in &a_members {
        w.entity_mut(m).insert(Faction::Red);
    }
    let sa = spawn_squad(
        &mut w,
        &a_members,
        FormationDef::wedge(3, 8.0),
        SquadOrder::Hold,
    );
    // Squad B spawns 500 u away — OUTSIDE base_sensor_range (200), so both
    // groups settle into their glides without ever seeing each other.
    let b_members: Vec<Entity> = (0..3)
        .map(|i| spawn_squad_member(&mut w, Vec2::new(500.0 + i as f32 * 6.0, 0.0)))
        .collect();
    for &m in &b_members {
        w.entity_mut(m).insert(Faction::Blue);
    }
    let sb = spawn_squad(
        &mut w,
        &b_members,
        FormationDef::wedge(3, 8.0),
        SquadOrder::Hold,
    );
    let all_members: Vec<Entity> = a_members.iter().chain(&b_members).copied().collect();

    // Phase 1 — both groups collapse (tick 30) and stay gliding through a full
    // far-scan cadence (90) with nothing hostile in range; never any combat.
    for t in 0..=130 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert_no_dormant_combat(&mut w, &all_members);
    }
    for (se, ms) in [(sa, &a_members), (sb, &b_members)] {
        assert!(
            w.get::<GlideState>(se).is_some(),
            "gliding while out of sensor range"
        );
        assert_eq!(w.get::<AoiTier>(se).unwrap().tier, Tier::Dormant);
        for &m in ms {
            assert!(w.get::<Gliding>(m).is_some(), "member gliding");
        }
    }

    // Phase 2 — B's aggregate drifts within base_sensor_range of A (teleport
    // the squad Position; the zero-velocity glide re-projects its members to
    // squad + offset on the next tick). Each squad's OWN far scan must now
    // find the other's hostile members → both promote and expand — with zero
    // combat at every tick on the way.
    w.get_mut::<Position>(sb).unwrap().0 = Vec2::new(156.0, 0.0);
    let mut both_expanded_at = None;
    for t in 131..=400 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert_no_dormant_combat(&mut w, &all_members);
        if w.get::<GlideState>(sa).is_none() && w.get::<GlideState>(sb).is_none() {
            both_expanded_at = Some(t);
            break;
        }
    }
    both_expanded_at.expect("BOTH hostile aggregates promote within their far-scan cadences");

    for (se, ms) in [(sa, &a_members), (sb, &b_members)] {
        assert_eq!(
            w.get::<AoiTier>(se).unwrap().tier,
            Tier::Mid,
            "hostile-scan promotion targets Mid (mutual, no player involved)"
        );
        assert!(
            w.get::<HostileContact>(se).is_some(),
            "promotion pinned by a hostile-contact hold"
        );
        for &m in ms {
            assert!(
                w.get::<Gliding>(m).is_none(),
                "members expanded to full physics"
            );
        }
    }
}

/// Fighter-derived `ShipStats` pinned to an exact top speed (drag normalized
/// to 1) — the squad-test pattern for constructing varying member speeds.
fn stats_with_top_speed(top_speed: f32) -> sim::fitting::ShipStats {
    use sim::fitting::content::{MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC};
    use sim::fitting::{build_layout, derive_ship_stats, seed_catalogs, Fit, SlotId, HULL_FIGHTER};
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
    let layout = build_layout(hull, &fit, &modules);
    let mut stats = derive_ship_stats(hull, &fit, &modules, &layout);
    stats.linear_drag = 1.0;
    stats.thrust_force = top_speed; // top_speed = thrust / drag = thrust.
    stats
}

/// OBJ3-VC4 / TR-010 [COMPLETES TR-010 with the next test]: through the FULL
/// schedule, killing the pace anchor re-derives the squad the SAME tick (new
/// anchor among survivors, slots re-assigned onto the surviving leader, pace
/// cap re-derived — the re-pushed `OrderChanged` assignments observable as the
/// rewritten member brains), and dropping to one member degrades that member
/// to an INDIVIDUAL brain with the order-derived goal.
#[test]
fn anchor_death_rederives_and_squad_of_one_degrades() {
    let (mut w, mut s) = obj2_world();
    // A player at the origin keeps the squad Active: fast cadence, no glide.
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    let goal = Vec2::new(300.0, 0.0);
    let m0 = spawn_squad_member(&mut w, Vec2::new(0.0, 0.0));
    let m1 = spawn_squad_member(&mut w, Vec2::new(10.0, 0.0));
    let m2 = spawn_squad_member(&mut w, Vec2::new(20.0, 0.0));
    w.entity_mut(m0).insert(stats_with_top_speed(80.0));
    w.entity_mut(m1).insert(stats_with_top_speed(30.0)); // the slowest: pace anchor
    w.entity_mut(m2).insert(stats_with_top_speed(60.0));
    let formation = FormationDef::wedge(3, 10.0);
    let se = spawn_squad(
        &mut w,
        &[m0, m1, m2],
        formation.clone(),
        SquadOrder::MoveTo(goal),
    );

    // Baseline (first think at tick 0): m1 anchors, m0 leads at the capped pace.
    mirror_tick_and_run(&mut w, &mut s, 0);
    let sq = squad_of(&w, se);
    assert_eq!(sq.pace_anchor, Some(m1), "slowest member anchors the pace");
    assert_eq!(sq.anchor_speed, 30.0);
    assert_eq!(brain_of(&w, m0).behavior, Behavior::Waypoint);
    assert_eq!(brain_of(&w, m0).throttle_cap, 30.0 / 80.0);
    let wing = brain_of(&w, m2);
    assert_eq!(wing.behavior, Behavior::FormationKeep);
    assert_eq!(wing.leader, Some(m0));
    assert_eq!(wing.formation_slot, Some(formation.slots[2]));

    // VC4 stage 1 — the PACE ANCHOR dies: the membership change forces a
    // same-tick squad re-derive (new anchor m2, slots re-assigned, leader
    // re-paced — the OrderChanged assignment pass rewrote the member brains).
    w.despawn(m1);
    mirror_tick_and_run(&mut w, &mut s, 1);
    let sq = squad_of(&w, se);
    assert_eq!(sq.members, vec![m0, m2], "pruned, stable order preserved");
    assert_eq!(
        sq.pace_anchor,
        Some(m2),
        "anchor re-derived among survivors"
    );
    assert_eq!(sq.anchor_speed, 60.0);
    assert_eq!(
        sq.last_think_tick, 1,
        "anchor death forced a same-tick squad re-think"
    );
    assert_eq!(
        brain_of(&w, m0).throttle_cap,
        60.0 / 80.0,
        "leader re-paced to the new anchor"
    );
    let wing = brain_of(&w, m2);
    assert_eq!(wing.behavior, Behavior::FormationKeep);
    assert_eq!(wing.leader, Some(m0));
    assert_eq!(
        wing.formation_slot,
        Some(formation.slots[1]),
        "slot re-assigned (2 → 1) by the re-derive"
    );

    // VC4 stage 2 — down to ONE member: the survivor degrades to an
    // individual brain (order-derived goal, no leader, no slot, own pace).
    w.despawn(m0);
    mirror_tick_and_run(&mut w, &mut s, 2);
    let b = brain_of(&w, m2);
    assert_eq!(
        b.behavior,
        Behavior::Waypoint,
        "MoveTo degrades to an individual Waypoint goal"
    );
    assert_eq!(b.waypoint, Some(goal));
    assert_eq!((b.leader, b.formation_slot), (None, None));
    assert_eq!(b.throttle_cap, 1.0, "solo flies at its own pace");
    assert!(
        w.entities().contains(se),
        "the squad entity remains (inert, still tracked)"
    );
    let sq = squad_of(&w, se);
    assert_eq!(sq.members, vec![m2]);
    assert_eq!(sq.pace_anchor, Some(m2));
}

/// OBJ3-VC1 / TR-009 [COMPLETES TR-009]: decision work scales with SQUAD count,
/// not ship count. Four 8-ship squads far from the player settle Dormant +
/// gliding — over N ticks each of those 32 members accumulates only the
/// dormant-cadence handful of thinks (≤ N/think_ticks_dormant + 2) — while the
/// ONE squad near the player thinks at the Active cadence and dominates the
/// total think count with a quarter of the ships. Meanwhile EVERY squad's own
/// brain kept running on its cadence (`last_think_tick` advanced into the
/// final cadence window), so per-SQUAD decision work is present even where
/// member work ≈ 0: O(squads), not O(ships).
#[test]
fn decision_work_scales_with_squads_not_ships() {
    const N: u64 = 600;
    let tuning = AiTuning::default();
    let (mut w, mut s) = obj2_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    // 4 far squads × 8 ships (32 ships), all WAY beyond aoi_radius_mid.
    let mut far_members: Vec<Entity> = Vec::new();
    let mut far_squads: Vec<Entity> = Vec::new();
    for sq in 0..4 {
        let base = Vec2::new(10_000.0 + sq as f32 * 1_000.0, 0.0);
        let members: Vec<Entity> = (0..8)
            .map(|i| spawn_squad_member(&mut w, base + Vec2::new(i as f32 * 8.0, 0.0)))
            .collect();
        far_squads.push(spawn_squad(
            &mut w,
            &members,
            FormationDef::wedge(8, 10.0),
            SquadOrder::Hold,
        ));
        far_members.extend(members);
    }
    // ONE squad × 8 ships inside the Active AOI of the player.
    let near_members: Vec<Entity> = (0..8)
        .map(|i| spawn_squad_member(&mut w, Vec2::new(-20.0 + i as f32 * 5.0, 30.0)))
        .collect();
    let near_squad = spawn_squad(
        &mut w,
        &near_members,
        FormationDef::wedge(8, 10.0),
        SquadOrder::Hold,
    );

    for t in 0..N {
        mirror_tick_and_run(&mut w, &mut s, t);
    }

    // The far squads really are the cheap tier: Dormant + collapsed to glide.
    for &se in &far_squads {
        assert_eq!(w.get::<AoiTier>(se).unwrap().tier, Tier::Dormant);
        assert!(w.get::<GlideState>(se).is_some(), "far squad glides");
    }
    assert_eq!(w.get::<AoiTier>(near_squad).unwrap().tier, Tier::Active);

    // Per-tier member think counters (VC1): dormant members ≈ 0 thinks
    // (dormant cadence only), Active members at the fast cadence. The Active
    // floor grants one dormant cadence of startup latency: brains spawn with
    // a Dormant think-tier MIRROR that self-corrects at their first think
    // (the documented AiBrain::think_tier rule), so the fast cadence engages
    // within think_ticks_dormant ticks of spawn.
    let dormant_bound = N / u64::from(tuning.think_ticks_dormant) + 2; // 8 at defaults
    let active_floor = // 33 at defaults
        (N - u64::from(tuning.think_ticks_dormant)) / u64::from(tuning.think_ticks_active) - 1;
    let mut far_total = 0u64;
    for &m in &far_members {
        let thinks = brain_of(&w, m).thinks_total;
        assert!(
            thinks <= dormant_bound,
            "dormant member over-thought: {thinks} > {dormant_bound}"
        );
        far_total += thinks;
    }
    let mut near_total = 0u64;
    for &m in &near_members {
        let thinks = brain_of(&w, m).thinks_total;
        assert!(
            thinks >= active_floor,
            "near-player member under-thought: {thinks} < {active_floor}"
        );
        near_total += thinks;
    }
    assert!(
        near_total > far_total,
        "the ONE near squad (8 ships, {near_total} thinks) dominates the four \
         far squads (32 ships, {far_total} thinks): individual full thinks \
         concentrate on near-player ships"
    );

    // Per-SQUAD decision work ran for EVERY squad — each squad brain's
    // last_think_tick advanced into its final cadence window even where the
    // members' own think work was ≈ 0 (O(squads) scaling).
    for &se in &far_squads {
        let last = squad_of(&w, se).last_think_tick;
        assert!(
            last >= N - cadence_for_tier(Tier::Dormant, &tuning),
            "far squad kept thinking on its cadence (last think {last})"
        );
    }
    let last = squad_of(&w, near_squad).last_think_tick;
    assert!(
        last >= N - cadence_for_tier(Tier::Active, &tuning),
        "near squad thinks at the Active cadence (last think {last})"
    );
}

// ---------------------------------------------------------------------------
// T028 — OBJ4: combat AI through the FULL fixed schedule (TR-006/TR-011/TR-012)
// VC1 engage-close-aim-destroy + never-fires-gated; VC2 ram/no-ram pair;
// OBJ2-VC3 archetype range bands.
// ---------------------------------------------------------------------------

use sim::ai::{hull_fraction, ram_utility, standoff_distance, weapon_range, FitArchetype};
use sim::damage::{
    default_resistance_matrix, seed_defense_layers, PenetrationConfig, SalvageConfig, ShieldConfig,
    Wreck,
};
use sim::fitting::{
    build_layout, derive_ship_stats, hull_collision_radius, seed_catalogs, Fit, FitLayout, SlotId,
    HULL_FIGHTER, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
};

/// The live server's fixed step (30 Hz) — the combat tests use it so the VC1
/// "≤ 3600 ticks" budget and the damage substrate's tuned carve windows mean
/// the same wall-clock seconds as the E007 kill tests.
const COMBAT_DT: f32 = 1.0 / 30.0;

/// The AI shooter's pinned weapon REACH (world units; `lifetime` is re-derived
/// as `range / muzzle_speed`, the same closed form the fit derivation uses).
/// A short envelope keeps the whole engagement inside a believable dogfight
/// bubble — close enough that heading-aligned fire reliably connects with the
/// target's hull footprint.
const SHOOTER_RANGE: f32 = 120.0;

/// A FULL fixed-step combat world: the `obj2_world` resource set PLUS every
/// E007 fitted-damage resource (the `damage.rs` `insert_full_combat_resources`
/// set), at the live 30 Hz step. The `AiTuning` AOI radii are widened so the
/// whole engagement bubble stays Active-tier around the player marker (the
/// default 120/520 radii can be smaller than an engagement bubble — pure
/// scenario tuning, no logic change).
fn combat_world() -> (World, Schedule) {
    let mut w = World::new();
    let (modules, hulls) = seed_catalogs();
    w.insert_resource(modules);
    w.insert_resource(hulls);
    w.insert_resource(default_resistance_matrix());
    w.insert_resource(PenetrationConfig::default());
    w.insert_resource(ShieldConfig::default());
    w.insert_resource(SalvageConfig::default());
    w.insert_resource(Tuning::default());
    // T028 "weak target" tuning (live-tunable per SimTuning's contract):
    // 1-HP structural cells so the carve-to-core kill lands comfortably inside
    // the VC1 budget at AI standoff ranges (the AI fires from its archetype
    // ring, not the point-blank core-aimed line the E007 kill tests use).
    w.insert_resource(sim::SimTuning {
        struct_cell_hp: 1.0,
        ..sim::SimTuning::default()
    });
    w.insert_resource(FixedDt(COMBAT_DT));
    w.insert_resource(HitFeedback::default());
    w.insert_resource(RefinedResources::default());
    w.insert_resource(ScenarioActive);
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.insert_resource(CurrentTick::default());
    w.insert_resource(RethinkQueue::default());
    w.insert_resource(CoarseIndex::default());
    let mut s = Schedule::default();
    sim::add_fixed_step_systems(&mut s);
    (w, s)
}

/// The AI shooter's `ShipStats`: the REAL derived starter-fighter fit (the
/// `damage.rs` player loadout: reactor, two thrusters, autocannon), with two
/// scenario pins applied AFTER derivation:
/// - weapon `lifetime` → a [`SHOOTER_RANGE`]-unit envelope (see const docs);
/// - `armor_value` → 200 so [`classify_archetype`]'s cuts read it as a
///   BRAWLER (armed + tanky) — the close-standoff archetype, so the fight
///   happens at short range where aligned fire reliably lands.
///
/// The shooter carries NO `Fit`/`FitLayout`, so `recompute_ship_stats_system`
/// (whose query requires both) never overwrites the pins.
fn brawler_shooter_stats() -> sim::fitting::ShipStats {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(2), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
    let layout = build_layout(hull, &fit, &modules);
    let mut stats = derive_ship_stats(hull, &fit, &modules, &layout);
    assert!(stats.can_fire, "the shooter fit must be able to fire");
    let mut weapon = stats.weapon.expect("autocannon fitted");
    weapon.lifetime = SHOOTER_RANGE / weapon.muzzle_speed;
    stats.weapon = Some(weapon);
    stats.armor_value = 200.0;
    stats
}

/// Spawn an ARMED AI fighter: a fitted `Ship` whose fire path is the real
/// `weapon_fire_system` (the legacy single-weapon arm: `ShipStats.weapon`
/// profile + the `Weapon` cooldown component, exactly the `damage.rs` player
/// pattern) driven ONLY by the intent its brain emits — `fire_primary` comes
/// from `ai_execute_system`'s `fire_decision`, never from the test.
fn spawn_armed_ai_fighter(
    w: &mut World,
    id: u64,
    pos: Vec2,
    target: Entity,
    stats: sim::fitting::ShipStats,
) -> Entity {
    let heading = (pos_of(w, target) - pos).to_angle();
    let weapon = Weapon {
        cooldown: 0.0,
        fire_rate: stats.weapon.map(|p| p.fire_rate).unwrap_or(5.0),
        muzzle_speed: stats.weapon.map(|p| p.muzzle_speed).unwrap_or(200.0),
        spool: 1.0,
        shot_counter: 0,
    };
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(heading),
        AngularVelocity(0.0),
        FlightAssist::On,
        weapon,
        stats,
        AiStableId(id),
        AiBrain {
            target: Some(target),
            think_tier: Tier::Active,
            phase_bucket: 0,
            ..AiBrain::default()
        },
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ))
    .id()
}

/// Spawn a WEAK destructible fitted target (the `damage.rs` fitted-enemy
/// pattern: `FitLayout` + hull-footprint `CollisionRadius` + `Destructible` +
/// the three defense layers, stationary) — an EMPTY fit, so every cell
/// (including the deepest "core" cell, the `core_cell` convention) is a weak
/// structural cell and the carve-to-core kill lands well inside the VC1
/// budget.
fn spawn_weak_fitted_target(w: &mut World, pos: Vec2) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
    let fit = Fit::new(HULL_FIGHTER);
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    let (mut shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);
    // Spawn shield-stripped (the `live_ship_still_dies_via_carve_to_core`
    // pattern): the kill is about the CARVE, not the shield pool.
    shields.current = 0.0;
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius(hull.grid_dims)),
        Destructible,
        fit,
        layout,
        stats,
        shields,
        section_armor,
        hull_structure,
    ))
    .id()
}

/// Live projectile count — the "fire really happened / never happened" probe.
fn projectile_count(w: &mut World) -> usize {
    w.query_filtered::<(), With<Projectile>>().iter(w).count()
}

/// OBJ4-VC1 / TR-011 [COMPLETES TR-011 with the gated test]: an armed AI
/// fighter (brain `Engage` + target set, Active tier) spawned OUTSIDE its
/// weapon envelope CLOSES to range, AIMS (the gunnery-lead alignment gate —
/// `fire_primary` only rises pointed at the lead solution), FIRES real
/// projectiles through `weapon_fire_system`, and DESTROYS the fitted target
/// (carve-to-core → `Wreck`) within the ≤ 3600-tick budget.
#[test]
fn engage_closes_aims_and_destroys_within_budget() {
    const BUDGET: u64 = 3600;
    let (mut w, mut s) = combat_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO))); // Keeps the bubble Active.

    let target = spawn_weak_fitted_target(&mut w, Vec2::new(150.0, 0.0));
    let cells_at_start = w.get::<FitLayout>(target).unwrap().cells.len();
    assert!(cells_at_start > 0, "the target starts with hull cells");

    let stats = brawler_shooter_stats();
    let range = weapon_range(Some(&stats)).expect("armed shooter has a weapon range");
    let fighter = spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, target, stats);
    let start_dist = (pos_of(&w, fighter) - pos_of(&w, target)).length();
    assert!(
        start_dist > range,
        "the fighter spawns OUTSIDE its weapon envelope ({start_dist} > {range}) — \
         it must CLOSE before it can fire"
    );

    let mut first_fire: Option<(u64, f32, f32)> = None; // (tick, dist, alignment)
    let mut saw_projectile = false;
    let mut cells_carved = false;
    let mut destroyed_at: Option<u64> = None;
    for tick in 0..BUDGET {
        mirror_tick_and_run(&mut w, &mut s, tick);
        if tick == 0 {
            let brain = brain_of(&w, fighter);
            assert_eq!(brain.behavior, Behavior::Engage, "live target → Engage");
            assert_eq!(
                brain.archetype,
                FitArchetype::Brawler,
                "armed + tanky stats classify Brawler (TR-006)"
            );
        }
        let intent = *w.get::<ShipIntent>(fighter).expect("fighter has intent");
        if first_fire.is_none() && intent.fire_primary {
            let to_target = pos_of(&w, target) - pos_of(&w, fighter);
            let align = Vec2::from_angle(w.get::<Heading>(fighter).unwrap().0)
                .dot(to_target.normalize_or_zero());
            first_fire = Some((tick, to_target.length(), align));
        }
        saw_projectile |= projectile_count(&mut w) > 0;
        if let Some(layout) = w.get::<FitLayout>(target) {
            cells_carved |= layout.cells.len() < cells_at_start;
        }
        if w.get::<Wreck>(target).is_some() || w.get_entity(target).is_err() {
            destroyed_at = Some(tick);
            break;
        }
    }

    // The fighter pulled the trigger only INSIDE the envelope, pointed at the
    // gunnery lead (the fire gates: in-range + aligned, TR-011).
    let (fire_tick, fire_dist, fire_align) =
        first_fire.expect("the fighter held fire until its gates opened, then fired");
    assert!(
        fire_dist <= range,
        "first fire at {fire_dist} — within the {range} weapon envelope (tick {fire_tick})"
    );
    assert!(
        fire_align > 0.8,
        "first fire pointed at the target (alignment {fire_align})"
    );

    // The kill happened, VIA fire (real projectiles + a visibly carved hull).
    let destroyed = destroyed_at.unwrap_or_else(|| {
        panic!(
            "the AI fighter destroys the target within {BUDGET} ticks \
             (cells {} of {cells_at_start} left)",
            w.get::<FitLayout>(target).map_or(0, |l| l.cells.len())
        )
    });
    assert!(
        saw_projectile,
        "real projectiles existed during the engagement"
    );
    assert!(
        cells_carved,
        "the target's hull cells were carved before death"
    );
    eprintln!("[t028] engage-destroy: first fire @{fire_tick}, kill @{destroyed} ticks");
}

/// OBJ4-VC1 second half / TR-011 [COMPLETES TR-011]: the fire gates. The same
/// armed fighter, parked in range and aligned, NEVER sets `fire_primary` (and
/// no projectile ever spawns) while (a) its energy capacitor is held empty,
/// then (b) its heat pool is held at max — and in BOTH cases, releasing the
/// gate is the ONLY change that lets it open fire (proving the gate, not some
/// other condition, was the blocker).
#[test]
fn gated_ship_never_fires() {
    const GATED_TICKS: u64 = 300;
    const RELEASE_WINDOW: u64 = 600;

    // Each scenario: (name, seed pools, per-tick pre-step pin while gated).
    type SeedPools = fn(&sim::fitting::ShipStats) -> (Energy, Heat);
    type Pin = fn(&mut World, Entity);
    let scenarios: [(&str, SeedPools, Pin); 2] = [
        (
            "energy-depleted",
            |_stats| {
                (
                    Energy {
                        current: 0.0,
                        max: 60.0,
                        regen: 0.0,
                        rate: 0.0,
                    },
                    Heat::seed(),
                )
            },
            |w, e| w.get_mut::<Energy>(e).unwrap().current = 0.0,
        ),
        (
            "overheated",
            |stats| {
                let heat = Heat::seed();
                (
                    Energy::seed(stats.power_supply),
                    Heat {
                        current: heat.max,
                        ..heat
                    },
                )
            },
            |w, e| {
                let mut heat = w.get_mut::<Heat>(e).unwrap();
                heat.current = heat.max;
            },
        ),
    ];

    for (name, seed, pin) in scenarios {
        let (mut w, mut s) = combat_world();
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        let target = spawn_weak_fitted_target(&mut w, Vec2::new(40.0, 0.0));
        let mut stats = brawler_shooter_stats();
        // A beefy reactor so station-keeping thrust (35 energy/s at full input
        // vs the stock reactor's 30/s) can never starve the capacitor: once
        // the gate is RELEASED, recharge must be the only thing standing
        // between the brain and the trigger. (No `Fit`/`FitLayout` on the
        // shooter → the pin is never re-derived away.)
        stats.power_supply = 200.0;
        let range = weapon_range(Some(&stats)).expect("armed shooter");
        let pools = seed(&stats);
        let fighter = spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, target, stats);
        w.entity_mut(fighter).insert(pools);

        // Phase 1 — gated: the pin holds the pool closed BEFORE each step
        // (`energy_system` would otherwise regen/cool it back open), so the
        // brain's own gate decides every tick. ZERO fire intents, ZERO shots.
        for tick in 0..GATED_TICKS {
            pin(&mut w, fighter);
            mirror_tick_and_run(&mut w, &mut s, tick);
            let intent = *w.get::<ShipIntent>(fighter).expect("fighter has intent");
            assert!(
                !intent.fire_primary,
                "[{name}] the brain never sets fire_primary while gated (tick {tick})"
            );
            assert_eq!(
                projectile_count(&mut w),
                0,
                "[{name}] no projectile ever spawns while gated (tick {tick})"
            );
        }
        // The gate was the ONLY closed condition: engaged, in range, aligned.
        assert_eq!(brain_of(&w, fighter).behavior, Behavior::Engage);
        let to_target = pos_of(&w, target) - pos_of(&w, fighter);
        assert!(
            to_target.length() <= range,
            "[{name}] the fighter sits inside its weapon envelope"
        );
        assert!(
            Vec2::from_angle(w.get::<Heading>(fighter).unwrap().0)
                .dot(to_target.normalize_or_zero())
                > 0.8,
            "[{name}] the fighter is aimed at the target"
        );

        // Phase 2 — release: stop pinning; the schedule's `energy_system`
        // recharges/cools the pool and the SAME brain now opens fire.
        let mut fired_at = None;
        for tick in GATED_TICKS..GATED_TICKS + RELEASE_WINDOW {
            mirror_tick_and_run(&mut w, &mut s, tick);
            if w.get::<ShipIntent>(fighter).expect("intent").fire_primary {
                fired_at = Some(tick);
                break;
            }
        }
        let fired = fired_at
            .unwrap_or_else(|| panic!("[{name}] releasing the gate is all it takes to fire"));
        eprintln!("[t028] {name}: gated {GATED_TICKS} ticks silent, fired @{fired} after release");
    }
}

/// Synthetic combat stats pinning the three TR-006 classification axes (the
/// `brain.rs` `stats_with` pattern lifted to the integration suite): emergent
/// top speed (drag normalized to 1), primary-weapon DPS, armor — plus a small
/// pinned `total_mass` so the test ships accelerate snappily (time constant
/// `mass / drag` = 2 s) through the real flight model.
fn combat_stats(top_speed: f32, dps: f32, armor: f32) -> sim::fitting::ShipStats {
    let mut s = stats_with_top_speed(top_speed);
    s.total_mass = 2.0;
    s.armor_value = armor;
    let mut w = s.weapon.expect("seed fighter carries a weapon");
    w.fire_rate = 1.0;
    w.damage = dps; // DPS = damage · fire_rate = damage.
    s.weapon = Some(w);
    s
}

/// Spawn an UNARMED-component AI combat ship for the decision-level tests
/// (tests 3/4): a fitted `Ship` flying its pinned [`combat_stats`] through the
/// real flight model, brain `target` set, Active tier — but NO `Weapon`
/// cooldown component, so `weapon_fire_system`'s legacy arm never spawns a
/// projectile (the brain's decisions, not ballistics, are under test).
fn spawn_brain_combat_ship(
    w: &mut World,
    id: u64,
    pos: Vec2,
    heading: f32,
    vel: Vec2,
    target: Entity,
    stats: sim::fitting::ShipStats,
) -> Entity {
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(vel),
        Heading(heading),
        AngularVelocity(0.0),
        FlightAssist::On,
        stats,
        AiStableId(id),
        AiBrain {
            target: Some(target),
            think_tier: Tier::Active,
            phase_bucket: 0,
            ..AiBrain::default()
        },
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ))
    .id()
}

/// A dense fighter-silhouette `FitLayout` (empty fit — structural cells only)
/// plus its live cell count, for the `hull_fraction` carve-baseline inputs.
fn bare_fighter_layout() -> (FitLayout, u32) {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let layout = build_layout(hull, &Fit::new(HULL_FIGHTER), &modules);
    let cells = layout.cells.len() as u32;
    (layout, cells)
}

/// OBJ4-VC2 / TR-012: the ram/no-ram decision PAIR through the full think
/// path (schedule-stepped, not just `ram_utility`):
/// (a) a fast-closing heavier attacker vs a NEAR-DEAD target (carve baseline
///     `AuthoredCells` 5× its live cells → hull fraction 0.2 ≤ the 0.25
///     near-dead cut) → the brain picks `Ram` (survival bucket outranks the
///     `Engage` task) and HOLDS it across re-thinks;
/// (b) the same attacker vs a HEALTHY, STRONGER (4× mass) target → `Ram` is
///     triple-vetoed (healthy + mass margin) and the brain engages instead.
#[test]
fn ram_no_ram_decision_pair() {
    let tuning = AiTuning::default();

    // Setup sanity straight through the exported T027 utility: the (a) target
    // is rammable, the (b) target is vetoed — the schedule must agree.
    let (layout_a, cells_a) = bare_fighter_layout();
    let frac_a = hull_fraction(None, Some(&layout_a), Some(&AuthoredCells(cells_a * 5)));
    assert!(
        frac_a <= tuning.ram_target_hull_frac,
        "the (a) target reads near-dead (hull fraction {frac_a})"
    );
    assert!(
        ram_utility(frac_a, 60.0, 80.0, 8.0, 2.0, &tuning) > 0.0,
        "near-dead + heavier + fast-closing scores a positive ram"
    );
    assert_eq!(
        ram_utility(1.0, 60.0, 80.0, 8.0, 32.0, &tuning),
        0.0,
        "healthy + stronger is zero-vetoed"
    );

    // (a) RAM: near-dead light target, attacker closing at 60 u/s (top 80).
    {
        let (mut w, mut s) = obj2_world();
        w.spawn((PlayerShip, Position(Vec2::ZERO))); // Active bubble.
        let (layout, cells) = bare_fighter_layout();
        let mut tstats = stats_with_top_speed(30.0);
        tstats.total_mass = 2.0;
        let target = w
            .spawn((
                Position(Vec2::new(60.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                layout,
                AuthoredCells(cells * 5), // 5× baseline → hull fraction 0.2.
                tstats,
            ))
            .id();
        let mut astats = stats_with_top_speed(80.0);
        astats.total_mass = 8.0; // 4× the target: a clear projected-damage edge.
        let attacker = spawn_brain_combat_ship(
            &mut w,
            0,
            Vec2::ZERO,
            0.0,
            Vec2::new(60.0, 0.0),
            target,
            astats,
        );
        for tick in 0..=20 {
            mirror_tick_and_run(&mut w, &mut s, tick);
            assert_eq!(
                brain_of(&w, attacker).behavior,
                Behavior::Ram,
                "near-dead/disabled target + closing fast → the brain COMMITS \
                 to the ram and holds it (tick {tick})"
            );
        }
        // The ram arm steers a real full-throttle collision course.
        let intent = *w.get::<ShipIntent>(attacker).expect("attacker intent");
        assert!(intent.forward > 0.9, "ram = full-burn collision course");
    }

    // (b) NO-RAM: healthy, 4×-heavier target — same closing geometry.
    {
        let (mut w, mut s) = obj2_world();
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        let (layout, cells) = bare_fighter_layout();
        let mut tstats = stats_with_top_speed(30.0);
        tstats.total_mass = 32.0; // 4× the attacker: the strength veto.
        let target = w
            .spawn((
                Position(Vec2::new(60.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                layout,
                AuthoredCells(cells), // Full baseline → hull fraction 1.0.
                tstats,
            ))
            .id();
        let mut astats = stats_with_top_speed(80.0);
        astats.total_mass = 8.0;
        let attacker = spawn_brain_combat_ship(
            &mut w,
            0,
            Vec2::ZERO,
            0.0,
            Vec2::new(60.0, 0.0),
            target,
            astats,
        );
        for tick in 0..=20 {
            mirror_tick_and_run(&mut w, &mut s, tick);
            assert_eq!(
                brain_of(&w, attacker).behavior,
                Behavior::Engage,
                "healthy stronger target → NEVER ram; the brain engages \
                 conventionally instead (tick {tick})"
            );
        }
    }
}

/// OBJ2-VC3 / TR-006 [COMPLETES TR-011's archetype-tactics clause]: ships with
/// different FITS adopt different archetype tactics. A brawler-stats ship and
/// a kiter-stats ship engage the SAME (indestructible — no `Health`, no
/// `FitLayout`) target through the full schedule; over the settled window the
/// brawler's mean distance is strictly below the kiter's, and each holds its
/// own archetype standoff band (±40% tolerance around
/// `standoff_distance(archetype, weapon_range)`).
#[test]
fn archetype_range_bands_differ() {
    const TICKS: u64 = 1500;
    const WINDOW: usize = 500; // The settled tail the means are taken over.
    let (mut w, mut s) = obj2_world();
    // Wide AOI radii so the whole 1000-unit weapon envelope stays Active-tier
    // around the player marker (scenario tuning, defaults are 60/240).
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    // The immortal-ish target: kinematics only — nothing can damage it.
    let target = w
        .spawn((Position(Vec2::ZERO), Velocity(Vec2::ZERO), Heading(0.0)))
        .id();

    // Same gun, same speed — ONLY the armor axis differs, flipping the
    // archetype cut (armed+tanky = Brawler, armed+fast+glass = Kiter).
    let brawler_stats = combat_stats(80.0, 30.0, 200.0);
    let kiter_stats = combat_stats(80.0, 30.0, 0.0);
    let range = weapon_range(Some(&brawler_stats)).expect("armed");
    assert_eq!(
        weapon_range(Some(&kiter_stats)),
        Some(range),
        "both fits carry the same weapon envelope"
    );
    // Both start at the SAME 600-unit range, on opposite sides; each spawns
    // facing its travel direction (the brawler must close, the kiter open).
    let brawler = spawn_brain_combat_ship(
        &mut w,
        0,
        Vec2::new(600.0, 0.0),
        std::f32::consts::PI,
        Vec2::ZERO,
        target,
        brawler_stats,
    );
    let kiter = spawn_brain_combat_ship(
        &mut w,
        1,
        Vec2::new(-600.0, 0.0),
        std::f32::consts::PI,
        Vec2::ZERO,
        target,
        kiter_stats,
    );

    let mut dists: Vec<(f32, f32)> = Vec::with_capacity(TICKS as usize);
    for tick in 0..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        if tick == 0 {
            // `Changed<ShipStats>` fired on spawn → classified before the think.
            assert_eq!(brain_of(&w, brawler).archetype, FitArchetype::Brawler);
            assert_eq!(brain_of(&w, kiter).archetype, FitArchetype::Kiter);
        }
        dists.push((
            (pos_of(&w, brawler) - pos_of(&w, target)).length(),
            (pos_of(&w, kiter) - pos_of(&w, target)).length(),
        ));
    }
    for ship in [brawler, kiter] {
        assert_eq!(
            brain_of(&w, ship).behavior,
            Behavior::Engage,
            "both archetypes stay on the Engage task"
        );
    }

    let tail = &dists[dists.len() - WINDOW..];
    let n = tail.len() as f32;
    let (sum_b, sum_k) = tail
        .iter()
        .fold((0.0f32, 0.0f32), |(b, k), &(db, dk)| (b + db, k + dk));
    let (mean_b, mean_k) = (sum_b / n, sum_k / n);

    // RANGE-BAND OCCUPANCY: the brawler holds a strictly shorter ring…
    assert!(
        mean_b < mean_k,
        "the brawler fights closer than the kiter (mean {mean_b} vs {mean_k})"
    );
    // …and each sits within ±40% of ITS archetype's standoff ring.
    let standoff_b = standoff_distance(FitArchetype::Brawler, range);
    let standoff_k = standoff_distance(FitArchetype::Kiter, range);
    assert!(
        standoff_b < standoff_k,
        "the archetype cuts order the rings (brawler {standoff_b} < kiter {standoff_k})"
    );
    for (name, mean, standoff) in [
        ("brawler", mean_b, standoff_b),
        ("kiter", mean_k, standoff_k),
    ] {
        assert!(
            (0.6 * standoff..=1.4 * standoff).contains(&mean),
            "{name} holds its {standoff}-unit standoff band (mean distance {mean})"
        );
    }
    eprintln!("[t028] range bands: brawler {mean_b:.1} (ring {standoff_b}), kiter {mean_k:.1} (ring {standoff_k})");
}

// ---------------------------------------------------------------------------
// R96 Part C — combat stances (Charge parity + Orbit / Kite styles).
// ---------------------------------------------------------------------------

/// A combat ship like [`spawn_brain_combat_ship`] but flying a chosen
/// [`CombatStance`] (the brain otherwise identical: target set, Active tier,
/// phase bucket 0, no `Weapon` component so ballistics never confound motion).
#[allow(clippy::too_many_arguments)] // Mirrors `spawn_brain_combat_ship` + the stance.
fn spawn_stance_combat_ship(
    w: &mut World,
    id: u64,
    pos: Vec2,
    heading: f32,
    vel: Vec2,
    target: Entity,
    stats: sim::fitting::ShipStats,
    stance: CombatStance,
) -> Entity {
    let e = spawn_brain_combat_ship(w, id, pos, heading, vel, target, stats);
    // R96: pin the stance through the highest-precedence channel (`squad_stance`)
    // so the think-time resolution preserves it — `combat_stance` itself is a
    // RESOLVED field the think overwrites each tick from squad ← role ← archetype.
    w.get_mut::<AiBrain>(e).unwrap().squad_stance = Some(stance);
    e
}

/// Signed bearing angle (rad) of `ship` around `target` — `atan2` of the line
/// FROM the target TO the ship. Accumulating its UNWRAPPED delta over a window
/// measures signed angular progress (orbit circulation direction + amount).
fn bearing_around(target: Vec2, ship: Vec2) -> f32 {
    (ship - target).to_angle()
}

/// R96 Part C — an `Orbit` ship curves AROUND a (stationary) target: over a
/// settled window its bearing angle advances MONOTONICALLY in the orbit
/// direction (CCW positive, CW negative) while it holds near
/// `orbit_radius_frac · standoff`. The two circulation directions advance
/// OPPOSITE ways — the defining orbit property (not a kill).
#[test]
fn orbit_stance_produces_tangential_curving_motion_at_standoff() {
    const TICKS: u64 = 2400;
    const SETTLE: u64 = 300; // Let the ship reach its ring before measuring.

    // Run one circulation direction; return (signed angular progress over the
    // measured tail, mean radius over that tail).
    let run = |ccw: bool| -> (f32, f32) {
        let (mut w, mut s) = obj2_world();
        w.insert_resource(AiTuning {
            aoi_radius_active: 10_000.0,
            aoi_radius_mid: 20_000.0,
            ..AiTuning::default()
        });
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        let target_pos = Vec2::ZERO;
        let target = w
            .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
            .id();
        // A slow, glass, armed fit → Orbiter (top 30 < fast-cut 60): it circles
        // its weapon envelope. The orbit ring is built from its OWN archetype.
        let stats = combat_stats(40.0, 30.0, 0.0);
        let ai = *w.resource::<AiTuning>();
        let archetype = sim::ai::classify_archetype(&stats, &ai);
        assert_eq!(
            archetype,
            FitArchetype::Orbiter,
            "the orbit test fit is an Orbiter"
        );
        let range = weapon_range(Some(&stats)).expect("armed");
        let standoff = standoff_distance(archetype, range) * ai.orbit_radius_frac;
        // Start ON the ring, off the +X axis, facing tangentially.
        let start = Vec2::new(standoff, 0.0);
        let ship = spawn_stance_combat_ship(
            &mut w,
            0,
            start,
            std::f32::consts::FRAC_PI_2,
            Vec2::ZERO,
            target,
            stats,
            CombatStance::Orbit { ccw },
        );

        let mut accum = 0.0f32;
        let mut prev = bearing_around(target_pos, start);
        let mut radii: Vec<f32> = Vec::new();
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            assert_eq!(
                brain_of(&w, ship).behavior,
                Behavior::Engage,
                "the orbiter stays on the Engage task (tick {tick})"
            );
            let p = pos_of(&w, ship);
            let b = bearing_around(target_pos, p);
            if tick >= SETTLE {
                // Unwrap the per-tick bearing delta into a continuous accumulation.
                accum += wrap_angle(b - prev);
                radii.push((p - target_pos).length());
            }
            prev = b;
        }
        let mean_r = radii.iter().copied().sum::<f32>() / radii.len() as f32;
        (accum, mean_r)
    };

    let (ccw_prog, ccw_r) = run(true);
    let (cw_prog, cw_r) = run(false);

    // The orbit CURVES: clear signed angular progress, OPPOSITE signs for the
    // two circulation directions (the defining orbiting property). The Orbiter's
    // wide ring + modest speed make this a slow, steady sweep — a fraction of a
    // turn over the window is unambiguous monotone circulation.
    assert!(
        ccw_prog > 0.5,
        "CCW orbit advances the bearing counter-clockwise (progress {ccw_prog} rad)"
    );
    assert!(
        cw_prog < -0.5,
        "CW orbit advances the bearing clockwise (progress {cw_prog} rad)"
    );
    assert!(
        ccw_prog * cw_prog < 0.0,
        "the two circulation directions sweep OPPOSITE ways (ccw {ccw_prog}, cw {cw_prog})"
    );
    // And it stays NEAR its ring (a wide ±60% band — the point is curving, not radius precision).
    let standoff = standoff_distance(
        FitArchetype::Orbiter,
        weapon_range(Some(&combat_stats(40.0, 30.0, 0.0))).unwrap(),
    ) * AiTuning::default().orbit_radius_frac;
    for (name, r) in [("ccw", ccw_r), ("cw", cw_r)] {
        assert!(
            (0.4 * standoff..=1.6 * standoff).contains(&r),
            "{name} orbit holds near its {standoff}-unit ring (mean radius {r})"
        );
    }
    eprintln!(
        "[r96c] orbit: ccw progress {ccw_prog:.2} rad (r {ccw_r:.1}), cw {cw_prog:.2} rad (r {cw_r:.1}), ring {standoff:.1}"
    );
}

/// R96 Part C — a `Kite` ship with the target INSIDE its kite range opens the
/// distance (range increases over the window) while keeping the target roughly
/// AHEAD (it faces the target for fire as it backs off). Formalizes the kiter.
#[test]
fn kite_stance_opens_range_when_closed_and_faces_target() {
    const TICKS: u64 = 1800;
    const TAIL: u64 = 600; // The settled tail (ship at its kite ring, holding).
    let (mut w, mut s) = obj2_world();
    w.insert_resource(AiTuning {
        aoi_radius_active: 100_000.0,
        aoi_radius_mid: 200_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    let target_pos = Vec2::new(0.0, 0.0);
    let target = w
        .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
        .id();
    // A fast glass kiter fit; start WELL inside the kite ring so it must open.
    let stats = combat_stats(80.0, 30.0, 0.0);
    let range = weapon_range(Some(&stats)).expect("armed");
    let kite_ring = w.resource::<AiTuning>().kite_range_frac * range;
    let start = Vec2::new(kite_ring * 0.3, 0.0); // Deep inside → must open range.
                                                 // Start facing AWAY from the target (toward +X) — it has to flee first.
    let ship = spawn_stance_combat_ship(
        &mut w,
        0,
        start,
        0.0,
        Vec2::ZERO,
        target,
        stats,
        CombatStance::Kite,
    );

    let start_range = (start - target_pos).length();
    let mut max_range = start_range;
    // Alignment is only the KITER's "hold and shoot" property once it has reached
    // its ring; while fleeing inward→outward its nose points AWAY (running). So
    // measure facing only over the settled tail, after the range has opened.
    let mut tail_aligned = 0usize;
    let mut tail_samples = 0usize;
    for tick in 0..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        assert_eq!(brain_of(&w, ship).behavior, Behavior::Engage);
        let p = pos_of(&w, ship);
        let r = (p - target_pos).length();
        max_range = max_range.max(r);
        if tick >= TICKS - TAIL {
            let to_t = (target_pos - p).normalize_or_zero();
            let nose = Vec2::from_angle(w.get::<Heading>(ship).unwrap().0);
            tail_samples += 1;
            if nose.dot(to_t) > 0.5 {
                tail_aligned += 1;
            }
        }
    }
    let end_range = (pos_of(&w, ship) - target_pos).length();

    // It OPENED the range (started deep inside the kite ring).
    assert!(
        end_range > start_range + 1.0,
        "the kiter opened distance (start {start_range}, end {end_range})"
    );
    assert!(
        max_range > start_range,
        "range grew over the window (start {start_range}, peak {max_range})"
    );
    // And once HOLDING at the ring it faces the target to fire (the gun bears).
    let aligned_frac = tail_aligned as f32 / tail_samples.max(1) as f32;
    assert!(
        aligned_frac > 0.6,
        "the settled kiter faces the target to fire (aligned {aligned_frac:.2})"
    );
    eprintln!(
        "[r96c] kite: range {start_range:.1} → {end_range:.1} (peak {max_range:.1}), tail-aligned {aligned_frac:.2}, ring {kite_ring:.1}"
    );
}

/// R97 Phase 1 Stage C — THE EMERGENCE: a FIGHTING RETREAT (open range while
/// facing + firing on the pursuer) emerges from the independent MOVE/AIM/FIRE
/// channels alone — NO dedicated `FightingRetreat` behavior exists. An armed ship
/// pinned to `Retreat` against an in-range armed pursuer must, over a window:
/// (a) OPEN the range (distance to the pursuer grows), (b) FIRE on the aligned
/// ticks (the weapons-free rule: AIM on the hostile + aligned + gates pass),
/// (c) keep its NOSE on the pursuer (the AIM channel), and (d) stay `Retreat`
/// THROUGHOUT (proving emergence, not a new behavior). Run for BOTH a
/// forward-only hull (reverse-thrust retrograde) and a `can_strafe` hull (lateral
/// strafe).
#[test]
fn fighting_retreat_emerges_without_a_dedicated_behavior() {
    // (strafe, home) — forward-only flees directly away (reverse-drift); the
    // can_strafe hull flees PERPENDICULAR to the threat (toward `home`) so the
    // lateral channel — strafe, not reverse — does the opening while the gun bears.
    for (label, can_strafe, home) in [
        ("forward-only", false, None),
        ("can_strafe", true, Some(Vec2::new(0.0, 600.0))),
    ] {
        const TICKS: u64 = 240;
        let (mut w, mut s) = combat_world();
        w.spawn((PlayerShip, Position(Vec2::ZERO)));
        // An indestructible, NON-shooting pursuer in weapon range (range 1000):
        // it never damages the retreater, so no `DamageTaken` ever breaks the
        // pinned commit — the behavior stays `Retreat` for the whole window.
        let pursuer_pos = Vec2::new(100.0, 0.0);
        let pursuer = w
            .spawn((
                Target,
                TargetKind::Dummy,
                Position(pursuer_pos),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                CollisionRadius(1.2),
                Health(1.0e9),
                Faction::Blue,
            ))
            .id();
        let mut stats = combat_stats(80.0, 30.0, 0.0);
        stats.can_strafe = can_strafe;
        let retreater = spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, pursuer, stats);
        {
            // PIN Retreat (commit never expires; nothing damages it to re-think).
            let mut b = w.get_mut::<AiBrain>(retreater).unwrap();
            b.behavior = Behavior::Retreat;
            b.commit_until_tick = u64::MAX;
            b.home = home;
        }

        let start_range = (pos_of(&w, retreater) - pursuer_pos).length();
        let mut fired_ticks = 0usize;
        let mut aligned_ticks = 0usize;
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            // (d) Retreat THROUGHOUT — the emergence is channels, not a new behavior.
            assert_eq!(
                brain_of(&w, retreater).behavior,
                Behavior::Retreat,
                "[{label}] stays Retreat (no FightingRetreat variant) (tick {tick})"
            );
            // (c) the nose tracks the pursuer (the AIM channel).
            let p = pos_of(&w, retreater);
            let to_threat = (pursuer_pos - p).normalize_or_zero();
            let nose = Vec2::from_angle(w.get::<Heading>(retreater).unwrap().0);
            if nose.dot(to_threat) > 0.9 {
                aligned_ticks += 1;
                // (b) FIRE on the aligned ticks (weapons-free + gates pass).
                if fires(&w, retreater) {
                    fired_ticks += 1;
                }
            }
        }
        let end_range = (pos_of(&w, retreater) - pursuer_pos).length();

        // (a) the range OPENED.
        assert!(
            end_range > start_range + 15.0,
            "[{label}] fighting retreat OPENS the range ({start_range:.1} → {end_range:.1})"
        );
        // (c) the nose stayed on the pursuer for the bulk of the window.
        assert!(
            aligned_ticks as f32 / TICKS as f32 > 0.8,
            "[{label}] the nose tracks the threat ({aligned_ticks}/{TICKS} aligned)"
        );
        // (b) it FIRED while opening range (the fighting retreat, not a pure run).
        assert!(
            fired_ticks > 0,
            "[{label}] fires on the pursuer while withdrawing ({fired_ticks} fire-ticks)"
        );
        eprintln!(
            "[r97c] fighting-retreat ({label}): range {start_range:.1} → {end_range:.1}, \
             aligned {aligned_ticks}/{TICKS}, fired {fired_ticks}"
        );
    }
}

/// R98 HOTFIX F — Retreat reverse-thrust obstacle masking: a retreating ship
/// with a LARGE body squarely on its flee line (directly BEHIND it — it backs
/// toward the obstacle nose-on the pursuer) resolves its flee direction
/// through the danger-masked ContextMap and so STOPS SHORT / deviates instead
/// of reversing straight through the body. Pre-fix, the survival arms composed
/// via `compose_intent_aimed` with NO obstacle field — the ship plunged
/// blindly through the obstacle (no ship↔dummy collision response exists, so
/// penetration is the discriminating observable). The behavior must stay
/// `Retreat` throughout (the fix is a move-channel mask, not a re-selection).
#[test]
fn retreat_masks_reverse_thrust_around_obstacle_behind_it() {
    const TICKS: u64 = 400;
    let (mut w, mut s) = combat_world();
    w.insert_resource(ObstacleField::default()); // enable the Part-D build system.
                                                 // The move-arm detour knobs (the `ship_steers_around_obstacle…` pattern):
                                                 // wide clearance + long predictive lookahead so the reversing ship reacts
                                                 // well before its tail reaches the body.
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        obstacle_clearance_pad: 40.0,
        obstacle_query_radius: 320.0,
        obstacle_lookahead_s: 6.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    // An indestructible, NON-shooting pursuer ahead (+X): the flee line is -X.
    let pursuer_pos = Vec2::new(100.0, 0.0);
    let pursuer = w
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(pursuer_pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            CollisionRadius(1.2),
            Health(1.0e9),
            Faction::Blue,
        ))
        .id();
    // A LARGE body squarely on the flee line, BEHIND the retreater (surface at
    // x = -100): pre-fix the reverse-drifting ship backed straight through it.
    let obstacle_pos = Vec2::new(-150.0, 0.0);
    let obstacle_radius = 50.0;
    spawn_obstacle(&mut w, obstacle_pos, obstacle_radius);

    let stats = combat_stats(80.0, 30.0, 0.0);
    let retreater = spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, pursuer, stats);
    w.entity_mut(retreater).insert(CollisionRadius(4.0));
    {
        // PIN Retreat (commit never expires; nothing damages it to re-think);
        // no home → the flee vector is directly away from the pursuer (-X).
        let mut b = w.get_mut::<AiBrain>(retreater).unwrap();
        b.behavior = Behavior::Retreat;
        b.commit_until_tick = u64::MAX;
        b.home = None;
    }

    let mut min_x: f32 = 0.0;
    let mut min_surface_gap = f32::INFINITY;
    for tick in 0..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        assert_eq!(
            brain_of(&w, retreater).behavior,
            Behavior::Retreat,
            "the mask is a move-channel deflection — the behavior stays Retreat (tick {tick})"
        );
        let p = pos_of(&w, retreater);
        min_x = min_x.min(p.x);
        min_surface_gap = min_surface_gap.min((p - obstacle_pos).length() - obstacle_radius - 4.0);
    }

    // It REALLY retreated toward the body (the scenario is exercised, not a
    // vacuous stand-still) …
    assert!(
        min_x < -20.0,
        "the ship genuinely withdrew along the flee line (min x {min_x:.1})"
    );
    // … but NEVER reversed into/through it: the danger-masked flee keeps real
    // surface clearance (pre-fix the blind reverse plunged the gap negative).
    assert!(
        min_surface_gap > 0.0,
        "the retreating ship never backs into the obstacle \
         (min surface gap {min_surface_gap:.1})"
    );
    eprintln!("[r98f] retreat-mask: min x {min_x:.1}, min surface gap {min_surface_gap:.1}");
}

// ---------------------------------------------------------------------------
// R97 Phase 1 Stage D — collision-imminence move-drive + channel fusion
// determinism (closes Phase 1).
// ---------------------------------------------------------------------------

/// R97 Phase 1 Stage D — a `Kite`-stance ship with a target INSIDE its kite
/// range opens distance (Stage C kite-flee) AND fires on the aligned ticks (the
/// Stage-C weapons-free rule wired the kite arm's fire). With the kite ring set
/// INSIDE the weapon envelope, the settled kiter holds facing the target within
/// range, so its gun bears and `fire_decision` pulls the trigger.
#[test]
fn kite_while_firing() {
    const TICKS: u64 = 1800;
    const TAIL: u64 = 600; // The settled tail (kiter at its ring, holding + firing).
    let (mut w, mut s) = obj2_world();
    let stats = combat_stats(80.0, 30.0, 0.0); // Fast glass kiter (armed).
    let range = weapon_range(Some(&stats)).expect("armed");
    // Hold the kite ring WELL INSIDE the weapon envelope (0.6·range), so the
    // settled, target-facing kiter is in range and its gun bears → it FIRES.
    let kite_frac = 0.6;
    w.insert_resource(AiTuning {
        aoi_radius_active: 100_000.0,
        aoi_radius_mid: 200_000.0,
        kite_range_frac: kite_frac,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    let target_pos = Vec2::ZERO;
    let target = w
        .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
        .id();
    let kite_ring = kite_frac * range;
    // Start DEEP inside the ring → it must open distance to reach the ring.
    let start = Vec2::new(kite_ring * 0.25, 0.0);
    let ship = spawn_stance_combat_ship(
        &mut w,
        0,
        start,
        0.0, // facing +X — AWAY from the target at the origin (it flees first).
        Vec2::ZERO,
        target,
        stats,
        CombatStance::Kite,
    );

    let start_range = (start - target_pos).length();
    let mut max_range = start_range;
    let mut tail_fired = 0usize;
    let mut tail_samples = 0usize;
    for tick in 0..TICKS {
        mirror_tick_and_run(&mut w, &mut s, tick);
        assert_eq!(
            brain_of(&w, ship).behavior,
            Behavior::Engage,
            "the kiter holds the Engage task (tick {tick})"
        );
        let r = (pos_of(&w, ship) - target_pos).length();
        max_range = max_range.max(r);
        if tick >= TICKS - TAIL {
            tail_samples += 1;
            if fires(&w, ship) {
                tail_fired += 1;
            }
        }
    }
    let end_range = (pos_of(&w, ship) - target_pos).length();

    // (a) It OPENED the distance (started deep inside the kite ring).
    assert!(
        end_range > start_range + 1.0,
        "the kiter opened distance ({start_range:.1} → {end_range:.1})"
    );
    assert!(
        max_range > start_range,
        "range grew over the window (start {start_range:.1}, peak {max_range:.1})"
    );
    // (b) The settled kiter FIRES on the target it faces (Stage-C weapons-free,
    // in range since the ring sits inside the weapon envelope).
    assert!(
        tail_fired > 0,
        "the settled kiter fires on the target it holds ({tail_fired}/{tail_samples} fire-ticks)"
    );
    // The settled ring is inside weapon range (the precondition for firing).
    assert!(
        end_range <= range,
        "the settled kiter holds inside its weapon envelope ({end_range:.1} ≤ {range:.1})"
    );
    eprintln!(
        "[r97d] kite-fire: range {start_range:.1} → {end_range:.1} (peak {max_range:.1}), \
         tail-fired {tail_fired}/{tail_samples}, ring {kite_ring:.1}, weapon-range {range:.1}"
    );
}

/// R97 Phase 1 Stage D — THE TWO-LAYER SPLIT: an `Engage` ship charging a target
/// with a large obstacle on a near-term collision course BETWEEN them DEVIATES
/// (breaks off the straight charge) when the collision is imminent; the SAME
/// obstacle placed FAR off the charge line (the ship never closes on it →
/// non-imminent) leaves the charge essentially straight. Proves the higher
/// collision-preempt layer overrides the attack-run's move direction only when
/// imminent, while the always-on R96 reactive layer is unperturbed by a distant
/// body.
#[test]
fn collision_preempts_attack_run() {
    const TICKS: u64 = 600;
    let start = Vec2::new(0.0, 0.0);
    let target_pos = Vec2::new(400.0, 0.0);
    let obstacle_radius = 50.0;

    // One run: the obstacle at `obstacle_pos` (on-line → imminent; far off-axis
    // → non-imminent). Returns the max lateral deviation off the straight
    // ship→target (+X) line over the charge, plus the behavior staying Engage.
    let run = |obstacle_pos: Vec2, query_radius: f32| -> f32 {
        let (mut w, mut s) = obj2_world();
        w.insert_resource(ObstacleField::default());
        w.insert_resource(AiTuning {
            aoi_radius_active: 10_000.0,
            aoi_radius_mid: 20_000.0,
            obstacle_clearance_pad: 40.0,
            obstacle_query_radius: query_radius,
            ..AiTuning::default()
        });
        w.spawn((PlayerShip, Position(start)));
        spawn_obstacle(&mut w, obstacle_pos, obstacle_radius);
        let target = w
            .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
            .id();
        // A Brawler (close-ring) so the engage arm CLOSES toward the target —
        // it must transit the obstacle's line when the obstacle is on it.
        let stats = combat_stats(80.0, 30.0, 200.0);
        let ai = *w.resource::<AiTuning>();
        assert_eq!(
            sim::ai::classify_archetype(&stats, &ai),
            FitArchetype::Brawler,
            "the charging fighter is a close-ring Brawler"
        );
        let fighter = spawn_brain_combat_ship(&mut w, 0, start, 0.0, Vec2::ZERO, target, stats);
        w.entity_mut(fighter).insert(CollisionRadius(4.0));

        let mut max_lateral: f32 = 0.0;
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            // The MOVE channel breaks off, but the TASK stays Engage (the move
            // channel prioritizes not-crashing; selection is unchanged).
            assert_eq!(
                brain_of(&w, fighter).behavior,
                Behavior::Engage,
                "stays on the Engage task while the move breaks off (tick {tick})"
            );
            let p = pos_of(&w, fighter);
            max_lateral = max_lateral.max(p.y.abs());
            // Stop once it has clearly passed the obstacle's x toward the target.
            if p.x > 300.0 {
                break;
            }
        }
        max_lateral
    };

    // (a) IMMINENT: obstacle squarely on the charge line at the midpoint → the
    // ship is closing straight at it → the move-drive override deflects it well
    // off the line.
    let imminent_lateral = run(Vec2::new(200.0, 0.0), 400.0);
    // (b) FAR / NON-CLOSING: the SAME obstacle far off to the side, beyond the
    // query radius the whole charge → never in range → `imm == 0` → the empty-
    // field gate keeps the charge straight (the always-on reactive layer sees
    // nothing).
    let distant_lateral = run(Vec2::new(200.0, 5000.0), 200.0);

    assert!(
        imminent_lateral > 20.0,
        "an IMMINENT collision breaks off the straight charge \
         (max lateral {imminent_lateral:.1})"
    );
    // The distant / non-closing obstacle is OUT of query range the whole charge
    // → the empty-field gate keeps the run byte-identical to a no-obstacle
    // charge, so the only lateral is the Brawler's intrinsic range-band wobble
    // (small). The PROOF is the dominance: the imminent break-off is many times
    // larger than this residual.
    assert!(
        distant_lateral < 10.0,
        "a distant / non-closing obstacle leaves the charge ~straight \
         (only the intrinsic charge wobble — max lateral {distant_lateral:.1})"
    );
    assert!(
        imminent_lateral > distant_lateral * 3.0,
        "the imminent break-off DOMINATES the non-imminent charge \
         (imminent {imminent_lateral:.1} ≫ distant {distant_lateral:.1})"
    );
    eprintln!(
        "[r97d] collision-preempt: imminent lateral {imminent_lateral:.1}, \
         distant lateral {distant_lateral:.1}"
    );
}

/// R97 Phase 1 Stage D — channel-fusion DETERMINISM (mirrors
/// `identical_state_yields_identical_selection_and_intents`): two worlds built
/// identically — combat (an Engage fighter + a target) PLUS an obstacle in range
/// (exercising the MOVE collision-imminence override) and the AIM/FIRE channels
/// — stepped through the FULL fixed schedule, compared per tick at BIT level
/// (behavior, intent f32 bits + discrete fire/group fields, and the resulting
/// pos/vel/heading bits). The full MOVE/AIM/FIRE fusion + collision-imminence
/// path is bit-identical across fresh rebuilds (V-3/V-6, strict-f32).
#[test]
fn channel_fusion_is_deterministic() {
    const TICKS: u64 = 200;
    let start = Vec2::new(0.0, 0.0);
    let target_pos = Vec2::new(400.0, 0.0);
    let obstacle_pos = Vec2::new(180.0, 0.0); // On the charge line → imminent.
    let obstacle_radius = 50.0;

    let run = || -> Vec<(Behavior, [u32; 3], ShipIntent, [u32; 6])> {
        let (mut w, mut s) = obj2_world();
        w.insert_resource(ObstacleField::default());
        w.insert_resource(AiTuning {
            aoi_radius_active: 10_000.0,
            aoi_radius_mid: 20_000.0,
            obstacle_clearance_pad: 40.0,
            obstacle_query_radius: 400.0,
            ..AiTuning::default()
        });
        w.spawn((PlayerShip, Position(start)));
        spawn_obstacle(&mut w, obstacle_pos, obstacle_radius);
        // A moving target so the AIM lead + FIRE solve are non-trivial.
        let target = w
            .spawn((
                Position(target_pos),
                Velocity(Vec2::new(0.0, 12.0)),
                Heading(0.0),
            ))
            .id();
        let stats = combat_stats(80.0, 30.0, 200.0); // armed Brawler (fires).
        let fighter = spawn_brain_combat_ship(&mut w, 0, start, 0.0, Vec2::ZERO, target, stats);
        w.entity_mut(fighter).insert(CollisionRadius(4.0));

        let mut trace = Vec::with_capacity(TICKS as usize);
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            let intent = *w.get::<ShipIntent>(fighter).expect("ship has ShipIntent");
            trace.push((
                brain_of(&w, fighter).behavior,
                [
                    intent.forward.to_bits(),
                    intent.strafe.to_bits(),
                    intent.turn.to_bits(),
                ],
                intent, // PartialEq covers the discrete fire/group/assist fields.
                state_bits(&w, fighter),
            ));
        }
        trace
    };

    let trace_a = run();
    let trace_b = run();
    assert_eq!(
        trace_a, trace_b,
        "identical combat + threat + obstacle worlds fuse MOVE/AIM/FIRE + \
         collision-imminence to bit-identical per-tick state (R97 Stage D determinism)"
    );

    // The fusion is MEANINGFUL: the fighter engaged, broke off around the
    // obstacle (a lateral excursion off the straight line), and fired.
    assert!(
        trace_a.iter().any(|(b, ..)| *b == Behavior::Engage),
        "the fighter engaged the target"
    );
    assert!(
        trace_a.iter().any(|(.., intent, _)| intent.fire_primary),
        "the fighter fired during the run (FIRE channel exercised)"
    );
    // Lateral = the world Y of the fighter's position (state_bits index 1 is
    // `pos.y` bits): an excursion off the straight +X charge line proves the
    // obstacle move-drive deflected it.
    let max_lateral = trace_a
        .iter()
        .map(|(.., kin)| f32::from_bits(kin[1]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_lateral > 5.0,
        "the obstacle move-drive deflected the charge (max lateral {max_lateral:.1})"
    );
}

// ---------------------------------------------------------------------------
// T031 — OBJ5: perception + faction sensor network through the FULL fixed
// schedule (TR-013/TR-014, [COMPLETES TR-013]).
// VC1 unseen-never-targeted at any tier; VC2 fused share + jammed AND severed
// local-only fallbacks; newest-wins fusion dedupe across members.
// ---------------------------------------------------------------------------

use sim::ai::{faction_key, Contact, ContactList, LinkState, NetworkComponent, SensorNetworks};

/// The OBJ2 full-schedule world PLUS `SensorNetworks` (the gate
/// `sensor_network_system` needs — `obj2_world` alone skips the rebuild).
fn perception_world() -> (World, Schedule) {
    let (mut w, s) = obj2_world();
    w.insert_resource(SensorNetworks::default());
    (w, s)
}

/// A minimal perception scanner: brain + contact list + faction + position
/// (the exact `perception_scan_system` query), phase bucket 0 so scans land on
/// cadence multiples, plus an `AoiTier` for the classifier. Deliberately NOT a
/// `Ship`: it never moves, so every range/datalink geometry in these tests
/// stays pinned — and it is absent from the coarse index, so scanners never
/// sense each other (only the spawned hostile bodies).
fn spawn_perception_scanner(w: &mut World, id: u64, faction: Faction, pos: Vec2) -> Entity {
    w.spawn((
        AiStableId(id),
        AiBrain {
            think_tier: Tier::Active,
            phase_bucket: 0,
            ..AiBrain::default()
        },
        ContactList::default(),
        faction,
        Position(pos),
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ))
    .id()
}

/// A detectable hostile body: `Target` puts it in the coarse index (the scan's
/// candidate source), `TargetKind::Dummy` parks it in `seek_system` (zero
/// accel, zero velocity — it moves only when a test teleports it), `Faction`
/// makes it hostile-eligible, `CollisionRadius` is its v1 signature.
fn spawn_hostile_body(w: &mut World, faction: Faction, pos: Vec2, radius: f32) -> Entity {
    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        faction,
        CollisionRadius(radius),
    ))
    .id()
}

/// The scanner's contact entry for `target`, if it currently has one.
fn contact_for(w: &World, scanner: Entity, target: Entity) -> Option<Contact> {
    w.get::<ContactList>(scanner)
        .expect("scanner has ContactList")
        .contacts
        .iter()
        .copied()
        .find(|c| c.target == target)
}

fn contact_count(w: &World, scanner: Entity) -> usize {
    w.get::<ContactList>(scanner)
        .expect("scanner has ContactList")
        .contacts
        .len()
}

/// OBJ5-VC1 / TR-013: an enemy a ship has NOT perceived is never targeted, at
/// the Active AND the Dormant tier (the V-8 gate is identical at every tier —
/// only the cadence scales). Two ARMED Red scanners — one Active beside the
/// player, one Dormant 10k units out — each with a Blue hostile nearby:
/// (1) hostile OUTSIDE `base_sensor_range` → contact lists stay empty and
///     `brain.target` stays `None` across many scan cadences;
/// (2) hostile INSIDE range but BELOW `sig_threshold` → still never seen;
/// (3) threshold lowered below the signature → contact appears and the armed
///     idle brain acquires exactly that target.
#[test]
fn unseen_enemies_are_never_targeted() {
    let (mut w, mut s) = perception_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    // Active scanner inside aoi_radius_active (120) of the player; Dormant
    // scanner far beyond aoi_radius_mid (520). Both ARMED (Engage-eligible).
    let near = spawn_perception_scanner(&mut w, 0, Faction::Red, Vec2::new(10.0, 0.0));
    w.entity_mut(near).insert(stats_with_top_speed(80.0));
    let far = spawn_perception_scanner(&mut w, 1, Faction::Red, Vec2::new(10_000.0, 0.0));
    w.entity_mut(far)
        .insert((stats_with_top_speed(80.0), AoiTier::default())); // starts Dormant
    let scanners = [near, far];

    // Each scanner's own hostile, 400 units out — DOUBLE base_sensor_range.
    let h_near = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(410.0, 0.0), 3.0);
    let h_far = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(10_400.0, 0.0), 3.0);

    // Phase 1 — OUT OF RANGE: 201 ticks cover 13 Active scans (cadence 15)
    // and 3 Dormant scans (cadence 90, ticks 0/90/180). Never seen, never
    // targeted — checked EVERY tick.
    for t in 0..=200 {
        mirror_tick_and_run(&mut w, &mut s, t);
        for e in scanners {
            assert_eq!(
                contact_count(&w, e),
                0,
                "out-of-range hostile never becomes a contact (tick {t})"
            );
            assert_eq!(
                brain_of(&w, e).target,
                None,
                "an unseen enemy is never targeted (tick {t})"
            );
        }
    }
    // The two scanners really sat on DIFFERENT tiers the whole time.
    assert_eq!(w.get::<AoiTier>(near).unwrap().tier, Tier::Active);
    assert_eq!(w.get::<AoiTier>(far).unwrap().tier, Tier::Dormant);

    // Phase 2 — IN RANGE, BELOW THRESHOLD: both hostiles teleport to 100
    // units (inside the 200 sensor range) but the signature gate is raised
    // ABOVE their CollisionRadius (5.0 > 3.0). Still invisible at both tiers.
    w.resource_mut::<AiTuning>().sig_threshold = 5.0;
    w.get_mut::<Position>(h_near).unwrap().0 = Vec2::new(110.0, 0.0);
    w.get_mut::<Position>(h_far).unwrap().0 = Vec2::new(10_100.0, 0.0);
    for t in 201..=400 {
        mirror_tick_and_run(&mut w, &mut s, t);
        for e in scanners {
            assert_eq!(
                contact_count(&w, e),
                0,
                "below-threshold signature never becomes a contact (tick {t})"
            );
            assert_eq!(
                brain_of(&w, e).target,
                None,
                "a sub-signature enemy is never targeted (tick {t})"
            );
        }
    }

    // Phase 3 — the CONTROL: drop the threshold below the signature; the very
    // same geometry now produces a contact AND target acquisition at BOTH
    // tiers (proving phases 1–2 failed on perception, not on broken plumbing).
    w.resource_mut::<AiTuning>().sig_threshold = 1.0;
    for t in 401..=540 {
        mirror_tick_and_run(&mut w, &mut s, t); // Dormant scan lands at 450.
    }
    for (e, h, tier) in [(near, h_near, "Active"), (far, h_far, "Dormant")] {
        let c = contact_for(&w, e, h)
            .unwrap_or_else(|| panic!("{tier}-tier scanner sees the adequate signature"));
        assert_eq!(c.signature, 3.0, "signature = CollisionRadius");
        assert_eq!(
            brain_of(&w, e).target,
            Some(h),
            "armed idle brain acquires the perceived contact ({tier} tier)"
        );
    }
    assert_eq!(
        w.get::<AoiTier>(far).unwrap().tier,
        Tier::Dormant,
        "the far scanner detected + acquired while STILL Dormant (any-tier VC1)"
    );
}

/// Wide-AOI perception world (the `combat_world` AOI pattern): a player at the
/// origin plus 10k/20k radii keep every scanner Active-tier, so all scan
/// cadences stay at 15 ticks across the network geometry.
fn network_integration_world() -> (World, Schedule) {
    let (mut w, s) = perception_world();
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    (w, s)
}

/// The star fixture for the VC2 link tests: three Red scanners with A as the
/// hub (A–B 250, A–C 250 ≤ datalink 300; B–C ≈ 354 connects only THROUGH A)
/// and a Blue hostile at (-100, 0) that ONLY A can see (A 100 ≤ sensor 200;
/// B 350, C ≈ 269 out of range). Returns (a, b, c, hostile).
fn spawn_sensor_star(w: &mut World) -> (Entity, Entity, Entity, Entity) {
    let a = spawn_perception_scanner(w, 0, Faction::Red, Vec2::new(0.0, 0.0));
    let b = spawn_perception_scanner(w, 1, Faction::Red, Vec2::new(250.0, 0.0));
    let c = spawn_perception_scanner(w, 2, Faction::Red, Vec2::new(0.0, 250.0));
    let hostile = spawn_hostile_body(w, Faction::Blue, Vec2::new(-100.0, 0.0), 3.0);
    (a, b, c, hostile)
}

/// The Red faction's network components from the live resource.
fn red_components(w: &World) -> Vec<NetworkComponent> {
    w.resource::<SensorNetworks>()
        .by_faction
        .get(&faction_key(Faction::Red))
        .cloned()
        .unwrap_or_default()
}

/// OBJ5-VC2 / TR-014 (jam): on an intact network, B and C "see" the contact
/// only A detects (fused share). Jamming B excludes it RX **and** TX:
/// (1) B stops receiving — its entry freezes while A's fresh detections keep
///     propagating to C, and the stale share ages out of B's list by its own
///     staleness window (3 × cadence = 45 ticks) → B falls back to an OWN-
///     sensor picture;
/// (2) B's own LOCAL sensing still works while jammed — and what B sees never
///     enters the fused picture.
#[test]
fn network_shares_contacts_and_jam_falls_back_to_local() {
    let (mut w, mut s) = network_integration_world();
    let (a, b, c, hostile) = spawn_sensor_star(&mut w);
    let p0 = Vec2::new(-100.0, 0.0);

    // Phase 1 — INTACT SHARE: A scans at ticks 0/15, the network rebuilds the
    // same ticks (mid cadence 15) → B and C carry A's contact via write-back.
    for t in 0..=15 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    for (name, e) in [("A", a), ("B", b), ("C", c)] {
        let got = contact_for(&w, e, hostile)
            .unwrap_or_else(|| panic!("{name} carries the fused contact"));
        assert_eq!(
            (got.last_seen_tick, got.last_pos),
            (15, p0),
            "{name} sees A's freshest detection (fused share)"
        );
    }
    let comps = red_components(&w);
    assert_eq!(comps.len(), 1, "one intact component");
    assert_eq!(comps[0].members.len(), 3, "all three linked (star via A)");

    // Phase 2 — JAM B and move the target (still inside A's range only):
    // A's fresh contact propagates to C but NOT B; B's last_seen freezes.
    w.entity_mut(b).insert(LinkState {
        jammed: true,
        severed: false,
    });
    let p1 = Vec2::new(-150.0, 0.0);
    w.get_mut::<Position>(hostile).unwrap().0 = p1;
    for t in 16..=30 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    for (name, e) in [("A", a), ("C", c)] {
        let got = contact_for(&w, e, hostile).unwrap();
        assert_eq!(
            (got.last_seen_tick, got.last_pos),
            (30, p1),
            "{name} keeps receiving fresh updates after B is jammed"
        );
    }
    let frozen = contact_for(&w, b, hostile).expect("B still holds the stale share");
    assert_eq!(
        (frozen.last_seen_tick, frozen.last_pos),
        (15, p0),
        "jammed B receives NO new fused updates — last_seen stops advancing"
    );
    let comps = red_components(&w);
    assert_eq!(comps.len(), 1, "the remaining linked ships: one component");
    assert!(
        comps.iter().all(|nc| !nc.members.contains(&b)),
        "jammed B is in NO network component"
    );

    // Phase 3 — STALE-OUT: B's frozen entry (last_seen 15) ages past B's own
    // staleness window (3 × 15 = 45) at B's tick-75 scan, while C keeps
    // tracking the live target through the network.
    for t in 31..=75 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        contact_count(&w, b),
        0,
        "the stale share aged out of jammed B's list (own-picture fallback)"
    );
    let got = contact_for(&w, c, hostile).unwrap();
    assert_eq!(
        (got.last_seen_tick, got.last_pos),
        (75, p1),
        "C still tracks the live target via the network"
    );

    // Phase 4 — LOCAL FALLBACK: the target teleports to where ONLY jammed B
    // can see it (B 100; A ≈ 269, C ≈ 292 — both out of their sensor range).
    // B's OWN sensors pick it up; B's detection never enters fusion (TX
    // exclusion), so A keeps only its stale memory.
    let p2 = Vec2::new(250.0, 100.0);
    w.get_mut::<Position>(hostile).unwrap().0 = p2;
    for t in 76..=90 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    let local = contact_for(&w, b, hostile).expect("jammed B senses LOCALLY");
    assert_eq!(
        (local.last_seen_tick, local.last_pos),
        (90, p2),
        "jammed B's own sensors still work (local-only picture, TR-014)"
    );
    let a_got = contact_for(&w, a, hostile).expect("A holds its last own sighting");
    assert_eq!(
        (a_got.last_seen_tick, a_got.last_pos),
        (75, p1),
        "B's local detection never reaches the network (TX exclusion)"
    );
}

/// OBJ5-VC2 / TR-014 (severed): the OTHER `LinkState` flag, on a DIFFERENT
/// member, excludes by itself — same semantics as the jam (either flag alone).
/// Severing C freezes C's shared picture while A and B keep exchanging fresh
/// contacts, and C drops out of every network component.
#[test]
fn severed_flag_excludes_independently() {
    let (mut w, mut s) = network_integration_world();
    let (a, b, c, hostile) = spawn_sensor_star(&mut w);
    let p0 = Vec2::new(-100.0, 0.0);

    // Intact share first (the same star baseline as the jam test).
    for t in 0..=15 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    for e in [a, b, c] {
        assert_eq!(
            contact_for(&w, e, hostile).map(|g| (g.last_seen_tick, g.last_pos)),
            Some((15, p0)),
            "all three share A's detection while intact"
        );
    }

    // SEVER C (jammed stays false — this flag must exclude on its own).
    w.entity_mut(c).insert(LinkState {
        jammed: false,
        severed: true,
    });
    let p1 = Vec2::new(-150.0, 0.0);
    w.get_mut::<Position>(hostile).unwrap().0 = p1;
    for t in 16..=30 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    for (name, e) in [("A", a), ("B", b)] {
        assert_eq!(
            contact_for(&w, e, hostile).map(|g| (g.last_seen_tick, g.last_pos)),
            Some((30, p1)),
            "{name} keeps receiving fresh updates after C is severed"
        );
    }
    assert_eq!(
        contact_for(&w, c, hostile).map(|g| (g.last_seen_tick, g.last_pos)),
        Some((15, p0)),
        "severed C receives NO new fused updates (severed alone excludes)"
    );
    let comps = red_components(&w);
    assert_eq!(comps.len(), 1);
    assert!(
        comps.iter().all(|nc| !nc.members.contains(&c)),
        "severed C is in NO network component"
    );
}

/// OBJ5 fusion-dedupe at integration level (TR-014): two linked members hold
/// DIVERGENT entries for the same target — built by moving the target between
/// their scan PHASES (D scans on bucket 0 → tick 0; E on bucket 5 → tick 10) —
/// and the tick-15 rebuild dedupes newest-wins: afterwards EVERY member's
/// entry (and the stored fused picture) carries the newest last_seen_tick/pos.
#[test]
fn fusion_dedupes_newest_wins_across_members() {
    let (mut w, mut s) = network_integration_world();
    // D and E sit 250 apart (linked, ≤ 300); their sensor bubbles (200) are
    // disjoint enough that each target position is seen by exactly ONE.
    let d = spawn_perception_scanner(&mut w, 0, Faction::Red, Vec2::new(0.0, 0.0));
    let e = spawn_perception_scanner(&mut w, 1, Faction::Red, Vec2::new(250.0, 0.0));
    w.get_mut::<AiBrain>(e).unwrap().phase_bucket = 5; // scans at (t+5)%15==0
    let p_old = Vec2::new(-100.0, 0.0); // D 100 in range, E 350 out
    let p_new = Vec2::new(350.0, 0.0); // D 350 out, E 100 in range
    let hostile = spawn_hostile_body(&mut w, Faction::Blue, p_old, 3.0);

    // Tick 0: D scans (sees p_old @ 0); same-tick rebuild shares it to E.
    mirror_tick_and_run(&mut w, &mut s, 0);
    // The target moves into E's bubble; E's phase-offset scan lands at tick 10.
    w.get_mut::<Position>(hostile).unwrap().0 = p_new;
    for t in 1..=14 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    // Just before the rebuild the two members genuinely DISAGREE.
    let d_got = contact_for(&w, d, hostile).unwrap();
    assert_eq!(
        (d_got.last_seen_tick, d_got.last_pos),
        (0, p_old),
        "D still holds its own older sighting"
    );
    let e_got = contact_for(&w, e, hostile).unwrap();
    assert_eq!(
        (e_got.last_seen_tick, e_got.last_pos),
        (10, p_new),
        "E's phase-offset scan saw the target fresher, elsewhere"
    );

    // Tick 15: rebuild fuses the component → newest wins EVERYWHERE.
    mirror_tick_and_run(&mut w, &mut s, 15);
    let expected = Contact {
        target: hostile,
        last_pos: p_new,
        last_seen_tick: 10,
        signature: 3.0,
    };
    for (name, m) in [("D", d), ("E", e)] {
        assert_eq!(contact_count(&w, m), 1, "{name} has exactly one entry");
        assert_eq!(
            contact_for(&w, m, hostile),
            Some(expected),
            "{name} carries the NEWEST tick/pos after the rebuild (newest-wins)"
        );
    }
    let comps = red_components(&w);
    assert_eq!(comps.len(), 1);
    let mut members = vec![d, e];
    members.sort_by_key(|x| x.to_bits());
    assert_eq!(comps[0].members, members, "one component, bits-sorted");
    assert_eq!(
        comps[0].fused,
        vec![expected],
        "the stored fused picture is the deduped newest entry"
    );
}

// ---------------------------------------------------------------------------
// T034 — OBJ6: scenario roles / orchestration through the FULL fixed schedule
// (TR-015, [COMPLETES TR-015]).
// VC1 patrol-break-resume; VC2 ambush same-tick group transition;
// HoldFire/DefensiveOnly posture gates; derelict + no-target fallbacks.
// ---------------------------------------------------------------------------

use sim::ai::{Posture, RoleGoal, ScenarioRole, FIRED_UPON_WINDOW_TICKS};

/// The perception world widened so the whole play area stays Active-tier
/// around the player marker (the `combat_world` AOI pattern) — role tests are
/// about scripting, not tier cadences.
fn roles_world() -> (World, Schedule) {
    let (mut w, s) = perception_world();
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    (w, s)
}

/// A scenario-roled AI ship: the full flight + brain + perception stack the
/// T033 scenario spawner authors (`Ship`/intent/kinematics + `AiBrain` +
/// `AiStableId` + Active `AoiTier` + `ContactList` + `Faction` + the role).
/// `armed` adds a derived fighter `ShipStats` (autocannon → `can_fire`, so
/// perception target-acquisition is live).
fn spawn_roled_ship(
    w: &mut World,
    id: u64,
    faction: Faction,
    pos: Vec2,
    role: ScenarioRole,
    armed: bool,
) -> Entity {
    let e = w
        .spawn((
            Ship,
            ShipIntent::default(),
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            FlightAssist::On,
            AiStableId(id),
            AiBrain {
                think_tier: Tier::Active,
                phase_bucket: 0,
                ..AiBrain::default()
            },
            AoiTier {
                tier: Tier::Active,
                since_tick: 0,
            },
            ContactList::default(),
            faction,
            role,
        ))
        .id();
    if armed {
        w.entity_mut(e).insert(stats_with_top_speed(80.0));
    }
    e
}

fn role_of(w: &World, e: Entity) -> ScenarioRole {
    w.get::<ScenarioRole>(e)
        .expect("entity carries ScenarioRole")
        .clone()
}

fn fires(w: &World, e: Entity) -> bool {
    w.get::<ShipIntent>(e)
        .expect("entity carries ShipIntent")
        .fire_primary
}

/// OBJ6-VC1 / TR-015: a patrol-roled armed ship follows its route, BREAKS to
/// engage a hostile it perceives mid-route, and RESUMES the route (waypoint =
/// the route point, cursor progressing on arrival) once the threat is gone.
#[test]
fn patrol_breaks_to_engage_and_resumes() {
    let (mut w, mut s) = roles_world();
    let p0 = Vec2::new(60.0, 0.0);
    let p1 = Vec2::new(160.0, 0.0);
    let route = vec![p0, p1];
    let ship = spawn_roled_ship(
        &mut w,
        0,
        Faction::Red,
        p0, // ON the first route point: the cursor advances at the first think.
        ScenarioRole::new(RoleGoal::PatrolRoute(route.clone()), Posture::FreeEngage),
        true,
    );

    // Phase A — ON ROUTE: the role advances off the spawn point and the brain
    // flies the next leg as a Waypoint goal.
    mirror_tick_and_run(&mut w, &mut s, 0);
    let b = brain_of(&w, ship);
    assert_eq!(
        b.behavior,
        Behavior::Waypoint,
        "the script directs the route"
    );
    assert_eq!(b.waypoint, Some(p1), "next route leg asserted");
    assert_eq!(role_of(&w, ship).route_index, 1, "cursor advanced off p0");
    for t in 1..=30 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert_eq!(
            brain_of(&w, ship).behavior,
            Behavior::Waypoint,
            "no threat → the patrol stays on route (tick {t})"
        );
    }

    // Phase B — BREAK: a hostile appears inside sensor range; the next scan
    // perceives it, acquisition targets it, and Engage wins by bucket.
    let hostile = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(110.0, 60.0), 3.0);
    let mut engaged_at = None;
    for t in 31..=120 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, ship);
        if b.behavior == Behavior::Engage {
            assert_eq!(b.target, Some(hostile), "engages the perceived threat");
            engaged_at = Some(t);
            break;
        }
    }
    let engaged_at = engaged_at.expect("patrol breaks to engage the perceived threat (VC1)");
    for t in engaged_at + 1..=engaged_at + 30 {
        mirror_tick_and_run(&mut w, &mut s, t); // A short prosecution window.
    }

    // Phase C — RESUME: the threat despawns (V-1 sweep clears target +
    // contact) → the role re-asserts the route and the patrol resumes.
    w.despawn(hostile);
    let mut resumed_at = None;
    for t in engaged_at + 31..=engaged_at + 90 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, ship);
        if b.behavior == Behavior::Waypoint && b.target.is_none() {
            assert!(
                b.waypoint.is_some_and(|g| route.contains(&g)),
                "the resumed waypoint is a route point (got {:?})",
                b.waypoint
            );
            resumed_at = Some(t);
            break;
        }
    }
    let resumed_at = resumed_at.expect("patrol resumes the route after the threat is gone (VC1)");

    // Phase D — PROGRESS: the route keeps progressing after the resume — the
    // ship reaches p1 and the cursor wraps back to p0.
    let mut wrapped = false;
    for t in resumed_at + 1..=resumed_at + 900 {
        mirror_tick_and_run(&mut w, &mut s, t);
        if role_of(&w, ship).route_index == 0 {
            assert_eq!(
                brain_of(&w, ship).waypoint,
                Some(p0),
                "the wrapped cursor re-asserts the first route point"
            );
            wrapped = true;
            break;
        }
    }
    assert!(
        wrapped,
        "the route progresses (arrival at p1 wraps the cursor)"
    );
}

/// OBJ6-VC2 / TR-015: three ambush-roled ships with DIFFERENT phase buckets
/// (different scan/think cadence phases, two of them mid-commit-window) all
/// transition away from Hold the SAME tick the first member perceives a
/// hostile contact inside the trigger circle — one shared trigger evaluation,
/// coordinated target, fired-marker degrade.
#[test]
fn ambush_triggers_same_tick_for_all_assigned() {
    let (mut w, mut s) = roles_world();
    let center = Vec2::ZERO;
    let radius = 120.0;
    let goal = RoleGoal::Ambush {
        trigger_center: center,
        trigger_radius: radius,
    };
    let positions = [
        Vec2::new(-50.0, 40.0),
        Vec2::new(0.0, 55.0),
        Vec2::new(50.0, 40.0),
    ];
    // Distinct buckets: scans land on ticks ≡ −bucket (mod 15) → 12 / 8 / 4.
    let buckets = [3u16, 7, 11];
    let mut ships = Vec::new();
    for (i, (&pos, &bucket)) in positions.iter().zip(&buckets).enumerate() {
        let e = spawn_roled_ship(
            &mut w,
            i as u64,
            Faction::Red,
            pos,
            ScenarioRole::new(goal.clone(), Posture::FreeEngage),
            false,
        );
        w.get_mut::<AiBrain>(e).unwrap().phase_bucket = bucket;
        ships.push(e);
    }
    // A hostile inside EVERY member's sensor range (200) but OUTSIDE the
    // trigger circle: perceived, not triggering.
    let hostile = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(140.0, 0.0), 3.0);

    // Everyone has scanned at least once by tick 12 (4 / 8 / 12): the
    // out-of-circle contact exists, the trap stays dark.
    for t in 0..=15 {
        mirror_tick_and_run(&mut w, &mut s, t);
        for &e in &ships {
            let b = brain_of(&w, e);
            assert_eq!(b.behavior, Behavior::Hold, "ambush holds dark (tick {t})");
            assert_eq!(b.target, None, "no target while un-triggered (tick {t})");
        }
    }

    // The hostile steps INSIDE the circle. The trigger is perception-gated:
    // nothing happens until the next member scan refreshes a contact —
    // bucket 11 scans at tick 19, while the OTHER two are mid-commit (windows
    // armed at their ticks 12 / 8 run to 27 / 23): the same-tick transition
    // therefore proves the trigger's commit-clear + event think.
    w.get_mut::<Position>(hostile).unwrap().0 = Vec2::new(90.0, 0.0);
    for t in 16..=18 {
        mirror_tick_and_run(&mut w, &mut s, t);
        for &e in &ships {
            assert_eq!(
                brain_of(&w, e).behavior,
                Behavior::Hold,
                "stale (out-of-circle) contacts never trigger (tick {t})"
            );
        }
    }
    mirror_tick_and_run(&mut w, &mut s, 19);
    for &e in &ships {
        let b = brain_of(&w, e);
        assert_eq!(
            b.behavior,
            Behavior::Engage,
            "ALL assigned ships transition together (VC2)"
        );
        assert_eq!(b.target, Some(hostile), "one coordinated firing target");
        assert_eq!(
            b.last_think_tick, 19,
            "the transition happened ON the trigger tick (same-tick, VC2)"
        );
        match role_of(&w, e).goal {
            RoleGoal::Defend { anchor, radius: r } => {
                assert_eq!(anchor, center, "fired marker anchors the trap center");
                assert_eq!(r, radius);
            }
            other => panic!("fired ambush degrades to Defend, got {other:?}"),
        }
    }
}

/// TR-015 posture gates at BOTH seams (Engage candidacy + the fire overlay):
/// - HoldFire: pinned-Engage, in-range, aligned — NEVER fires over 300+ ticks
///   (the overlay gate, independent of selection);
/// - DefensiveOnly: never engages/fires until a `DamageTaken` re-think arrives,
///   then engages the SAME tick + fires within the `FIRED_UPON_WINDOW_TICKS`
///   window, and stops (fire + behavior) once the window expires;
/// - an unroled control ship in the same geometry DOES fire (the gate is the
///   differentiator, not broken plumbing).
#[test]
fn postures_gate_fire_and_engagement() {
    let (mut w, mut s) = combat_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    // An effectively indestructible flat-health dummy: the fights must outlive
    // the whole posture timeline.
    let spawn_dummy = |w: &mut World, pos: Vec2| {
        w.spawn((
            Target,
            TargetKind::Dummy,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            CollisionRadius(1.2),
            Health(1.0e9),
            Faction::Blue,
        ))
        .id()
    };

    // (a) HoldFire — behavior PINNED to Engage (commit never expires, no event
    // producers touch it) so the test isolates the fire-decision OVERLAY gate.
    let hold_target = spawn_dummy(&mut w, Vec2::new(60.0, 0.0));
    let hold_ship =
        spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, hold_target, brawler_shooter_stats());
    w.entity_mut(hold_ship).insert(ScenarioRole::new(
        RoleGoal::Defend {
            anchor: Vec2::ZERO,
            radius: 1.0e9,
        },
        Posture::HoldFire,
    ));
    {
        let mut b = w.get_mut::<AiBrain>(hold_ship).unwrap();
        b.behavior = Behavior::Engage;
        b.commit_until_tick = u64::MAX;
    }

    // (b) DefensiveOnly — free-running brain, target pre-set (the spawn
    // helper), anchored on its spawn point.
    let def_anchor = Vec2::new(5000.0, 0.0);
    let def_target = spawn_dummy(&mut w, def_anchor + Vec2::new(60.0, 0.0));
    let def_ship =
        spawn_armed_ai_fighter(&mut w, 1, def_anchor, def_target, brawler_shooter_stats());
    w.entity_mut(def_ship).insert(ScenarioRole::new(
        RoleGoal::Defend {
            anchor: def_anchor,
            radius: 1.0e9,
        },
        Posture::DefensiveOnly,
    ));

    // (c) The unroled CONTROL: same geometry, no posture gate.
    let ctl_target = spawn_dummy(&mut w, Vec2::new(60.0, 5000.0));
    let control = spawn_armed_ai_fighter(
        &mut w,
        2,
        Vec2::new(0.0, 5000.0),
        ctl_target,
        brawler_shooter_stats(),
    );

    // Phase 1 — UNGATED vs GATED: the control fires; HoldFire never; the
    // DefensiveOnly ship neither engages nor fires (no DamageTaken yet).
    let mut control_fired = false;
    for t in 0..60 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert!(!fires(&w, hold_ship), "HoldFire NEVER fires (tick {t})");
        assert!(
            !fires(&w, def_ship),
            "DefensiveOnly never fires before being fired upon (tick {t})"
        );
        assert_ne!(
            brain_of(&w, def_ship).behavior,
            Behavior::Engage,
            "DefensiveOnly never selects Engage before being fired upon (tick {t})"
        );
        control_fired |= fires(&w, control);
    }
    assert!(
        control_fired,
        "the unroled control ship fires in the same geometry (the gate is the difference)"
    );

    // Phase 2 — FIRED UPON: a DamageTaken re-think arms the window; the ship
    // engages the SAME tick and fires inside the window.
    w.resource_mut::<RethinkQueue>()
        .push(def_ship, AiEvent::DamageTaken);
    mirror_tick_and_run(&mut w, &mut s, 60);
    assert_eq!(
        brain_of(&w, def_ship).behavior,
        Behavior::Engage,
        "fired-upon DefensiveOnly engages the same tick"
    );
    assert_eq!(
        role_of(&w, def_ship).fired_upon_until,
        60 + FIRED_UPON_WINDOW_TICKS,
        "the weapons-free window is armed to now + the documented 300 ticks"
    );
    let window_end = 60 + FIRED_UPON_WINDOW_TICKS;
    let mut fired_in_window = fires(&w, def_ship);
    for t in 61..window_end {
        mirror_tick_and_run(&mut w, &mut s, t);
        fired_in_window |= fires(&w, def_ship);
        assert!(!fires(&w, hold_ship), "HoldFire stays silent (tick {t})");
    }
    assert!(
        fired_in_window,
        "DefensiveOnly fires while inside the fired-upon window"
    );

    // Phase 3 — EXPIRY: from the deadline tick on, the gate closes again —
    // no fire, and the behavior leaves Engage at the next think.
    for t in window_end..=window_end + 60 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert!(
            !fires(&w, def_ship),
            "the expired window never fires (tick {t})"
        );
        assert!(!fires(&w, hold_ship), "HoldFire stays silent (tick {t})");
    }
    assert_ne!(
        brain_of(&w, def_ship).behavior,
        Behavior::Engage,
        "DefensiveOnly disengages once the window expires"
    );
}

/// TR-015 fallbacks at schedule level: a patrol-roled ship with NO contacts
/// holds its route — it never selects Engage/Ram and never fires over 300
/// ticks (no engage thrash) — and the TR-001 derelict zero-intent pin holds
/// unchanged underneath a scenario role.
#[test]
fn derelict_and_no_target_fallbacks_hold() {
    let (mut w, mut s) = roles_world();
    let p0 = Vec2::new(60.0, 0.0);
    let p1 = Vec2::new(160.0, 0.0);
    let route = vec![p0, p1];
    let patrol = spawn_roled_ship(
        &mut w,
        0,
        Faction::Red,
        p0,
        ScenarioRole::new(RoleGoal::PatrolRoute(route.clone()), Posture::FreeEngage),
        true,
    );
    // The same script on a DERELICT hull (control fitted, no live control
    // source): the brain may select route behaviors, but the execute pin
    // keeps the intent at zero (TR-001 degrade under a role).
    let derelict = spawn_roled_ship(
        &mut w,
        1,
        Faction::Red,
        Vec2::new(60.0, 40.0),
        ScenarioRole::new(RoleGoal::PatrolRoute(route.clone()), Posture::FreeEngage),
        false,
    );
    let mut dead_stats = stats_with_top_speed(80.0);
    dead_stats.control_fitted = true;
    dead_stats.has_control = false;
    w.entity_mut(derelict).insert(dead_stats);

    for t in 0..300 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, patrol);
        assert!(
            !matches!(b.behavior, Behavior::Engage | Behavior::Ram),
            "no contacts → a roled patrol never selects Engage/Ram (tick {t})"
        );
        assert_eq!(b.target, None, "nothing to target (tick {t})");
        assert!(!fires(&w, patrol), "nothing to shoot (tick {t})");
        assert_eq!(
            *w.get::<ShipIntent>(derelict).unwrap(),
            ShipIntent::default(),
            "the derelict zero-intent pin holds under a ScenarioRole (tick {t})"
        );
    }
    // The script kept directing: the patrol's goal is still a route point.
    assert!(
        brain_of(&w, patrol)
            .waypoint
            .is_some_and(|g| route.contains(&g)),
        "the roled patrol holds its route"
    );
}

// ---------------------------------------------------------------------------
// T036 — OBJ7: scouting + search-and-destroy through the FULL fixed schedule
// (TR-021, [COMPLETES TR-021]).
// VC1 scout disengages from a superior threat + survives + reports the
// contact; VC2 ≥90% coarse-cell sweep coverage in budget + engage-on-
// perception + close (SC-007).
// ---------------------------------------------------------------------------

use std::collections::BTreeSet;

use sim::broadphase::{CoarseGrid, COARSE_CELL_SIZE};

/// OBJ7-VC1 / TR-021: an UNARMED scout with a `ScoutArea` role meets a
/// stronger ARMED hostile parked on its coverage route.
///
/// - The scout perceives it (its `ContactList` holds the threat, and the
///   detection is REPORTED into the faction's fused `SensorNetworks` picture —
///   TR-021's "report" mechanism is network fusion, no scout-specific code);
/// - the v1 superiority test (threat armed AND self unarmed) flips it to
///   `Evade`: the range to the threat OPENS over the window, it never selects
///   `Engage`/`Ram` (the scout combat veto), and it SURVIVES;
/// - despawning the hostile (V-1 sweep clears target + contact) resumes
///   coverage: behavior back to `Scout` and the route progresses.
#[test]
fn scout_disengages_from_superior_threat_and_survives() {
    let (mut w, mut s) = roles_world();
    let (min, max) = (Vec2::ZERO, Vec2::new(200.0, 100.0));
    let scout = spawn_roled_ship(
        &mut w,
        0,
        Faction::Red,
        Vec2::ZERO, // ON route[0]: the cursor advances at the first think.
        ScenarioRole::new(RoleGoal::ScoutArea { min, max }, Posture::FreeEngage),
        false, // UNARMED: weaker than any armed threat by the v1 superiority rule.
    );
    // A stronger ARMED hostile sitting on the first coverage leg, inside
    // base_sensor_range (200) of the spawn.
    let hostile = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(150.0, 0.0), 3.0);
    w.entity_mut(hostile).insert(stats_with_top_speed(80.0)); // armed (can_fire)

    // Phase A — DISENGAGE: the first scan perceives the threat; the
    // superiority test scores Evade (survival bucket) the same think.
    let mut evade_at = None;
    for t in 0..=30 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, scout);
        assert!(
            !matches!(b.behavior, Behavior::Engage | Behavior::Ram),
            "a scout never engages (tick {t})"
        );
        if b.behavior == Behavior::Evade {
            evade_at = Some(t);
            break;
        }
    }
    let evade_at = evade_at.expect("scout perceives the superior threat and evades (VC1)");
    assert_eq!(
        brain_of(&w, scout).target,
        Some(hostile),
        "the evade points away from the perceived threat"
    );
    assert!(
        contact_for(&w, scout, hostile).is_some(),
        "the threat is a maintained contact"
    );
    // TR-021 "report": the scout's detection entered the faction's fused
    // picture via ordinary sensor-network fusion (rebuilt at tick 0).
    let nets = w.resource::<SensorNetworks>();
    assert!(
        nets.by_faction[&faction_key(Faction::Red)]
            .iter()
            .any(|nc| nc.fused.iter().any(|c| c.target == hostile)),
        "the scout reports the contact into its faction picture"
    );

    // Phase B — SURVIVE: the range to the threat opens over the evade window
    // and the scout never turns to fight.
    let threat_pos = pos_of(&w, hostile);
    let d0 = (pos_of(&w, scout) - threat_pos).length();
    for t in evade_at + 1..=evade_at + 150 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, scout);
        assert!(
            !matches!(b.behavior, Behavior::Engage | Behavior::Ram),
            "the combat veto holds while evading (tick {t})"
        );
    }
    let d1 = (pos_of(&w, scout) - threat_pos).length();
    assert!(
        d1 > d0 + 15.0,
        "the scout opens the range ({d0:.1} → {d1:.1}) and survives"
    );
    assert!(w.get::<Position>(scout).is_some(), "the scout survives");

    // Phase C — RESUME: the threat despawns (V-1 sweep clears target +
    // contact) → no Evade candidate → the recon task wins again.
    w.despawn(hostile);
    let resume_from = evade_at + 151;
    let mut resumed_at = None;
    for t in resume_from..=resume_from + 60 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, scout);
        if b.behavior == Behavior::Scout {
            assert_eq!(b.target, None, "the dead threat is released");
            resumed_at = Some(t);
            break;
        }
    }
    let resumed_at = resumed_at.expect("scout resumes coverage after the threat is gone (VC1)");

    // Phase D — COVERAGE PROGRESSES: the route cursor advances (or the ship
    // closes on its asserted leg) after the resume.
    let idx0 = role_of(&w, scout).route_index;
    let wp0 = brain_of(&w, scout)
        .waypoint
        .expect("the role asserts a coverage leg");
    let d_wp0 = (pos_of(&w, scout) - wp0).length();
    for t in resumed_at + 1..=resumed_at + 600 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    let progressed = role_of(&w, scout).route_index != idx0
        || brain_of(&w, scout).waypoint != Some(wp0)
        || (pos_of(&w, scout) - wp0).length() + 10.0 < d_wp0;
    assert!(progressed, "the scout resumes route coverage (VC1 resume)");
}

/// OBJ7-VC2 / TR-021 / SC-007: an armed S&D ship with a `SweepRegion` role
/// over a 3×3 block of coarse interest-tier cells (64.0 each; sensor range
/// shrunk to 80 so the sweep needs real lanes) sensor-sweeps ≥ 90% of the
/// region's coarse cells within the budget — measured per tick as "cell
/// center within `base_sensor_range` of the ship" over `CoarseGrid::cell_of`
/// cells — then a target placed in the region is perceived once in sensor
/// range, the ship transitions to `Engage` (outranking the incumbent sweep),
/// and CLOSES onto its engage ring.
#[test]
fn sweep_covers_coarse_cells_and_engages_on_perception() {
    let (mut w, mut s) = roles_world();
    // Shrink the sensor so coverage genuinely requires flying the lanes
    // (region/budget tuning per the spec: the time budget is scenario-defined).
    w.resource_mut::<AiTuning>().base_sensor_range = 80.0;
    let (min, max) = (Vec2::ZERO, Vec2::new(160.0, 160.0));
    let ship = spawn_roled_ship(
        &mut w,
        0,
        Faction::Red,
        Vec2::ZERO,
        ScenarioRole::new(RoleGoal::SweepRegion { min, max }, Posture::FreeEngage),
        false,
    );
    // BRAWLER stats (armor over the cut, weapon reach pinned to 200): the
    // short standoff ring (0.3 × 200 = 60, band ±15) sits INSIDE the shrunk
    // sensor range, so "engage and close" is observable as ring convergence.
    let mut stats = stats_with_top_speed(80.0);
    stats.armor_value = 200.0;
    let mut weapon = stats.weapon.expect("fighter fit carries the autocannon");
    weapon.lifetime = 200.0 / weapon.muzzle_speed;
    stats.weapon = Some(weapon);
    w.entity_mut(ship).insert(stats);
    // R96: pin the COVERAGE pace to Cruise (the baseline coast) through the
    // precedence channel so the think keeps it — this test is about route
    // coverage, not pace style. A Brawler's archetype default is now Rush
    // (active braking onto each lane endpoint), which slows lane coverage; the
    // sweep VC is unchanged in intent, so hold the coast pace explicitly.
    w.get_mut::<AiBrain>(ship).unwrap().squad_profile = Some(MovementProfile::Cruise);

    // The region's coarse interest-tier cells (3×3 of 64.0).
    let (lo, hi) = (CoarseGrid::cell_of(min), CoarseGrid::cell_of(max));
    let mut region_cells: BTreeSet<(i32, i32)> = BTreeSet::new();
    for cy in lo.1..=hi.1 {
        for cx in lo.0..=hi.0 {
            region_cells.insert((cx, cy));
        }
    }
    assert_eq!(region_cells.len(), 9, "the fixture spans 3×3 coarse cells");

    // Phase A — COVERAGE: fly the sweep; per tick, mark region cells whose
    // center lies within the sensor radius of the ship as swept.
    let range = w.resource::<AiTuning>().base_sensor_range;
    let cell_center = |&(cx, cy): &(i32, i32)| {
        Vec2::new(
            (cx as f32 + 0.5) * COARSE_CELL_SIZE,
            (cy as f32 + 0.5) * COARSE_CELL_SIZE,
        )
    };
    const BUDGET: u64 = 6000;
    let mut swept: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut covered_at = None;
    for t in 0..BUDGET {
        mirror_tick_and_run(&mut w, &mut s, t);
        let p = pos_of(&w, ship);
        for cell in &region_cells {
            if (cell_center(cell) - p).length() <= range {
                swept.insert(*cell);
            }
        }
        // ≥ 90% of the region's coarse cells sensor-swept (VC2).
        if swept.len() * 10 >= region_cells.len() * 9 {
            covered_at = Some(t);
            break;
        }
        assert_eq!(
            brain_of(&w, ship).behavior,
            Behavior::Sweep,
            "no target → the ship keeps sweeping (tick {t})"
        );
    }
    let covered_at =
        covered_at.expect(">=90% of the region's coarse cells swept within the budget (VC2)");
    eprintln!(
        "[t036] sweep: {}/{} coarse cells at tick {covered_at}",
        swept.len(),
        region_cells.len()
    );

    // Phase B — PROSECUTE: a target appears in the region; once inside the
    // (shrunk) sensor range it is perceived, acquisition targets it, and
    // Engage outranks the incumbent sweep (the RECON baseline rule).
    let target = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(80.0, 80.0), 3.0);
    let mut engaged_at = None;
    for t in covered_at + 1..=covered_at + 2400 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, ship);
        if b.behavior == Behavior::Engage {
            assert_eq!(b.target, Some(target), "prosecutes the perceived target");
            engaged_at = Some(t);
            break;
        }
        assert_eq!(
            b.behavior,
            Behavior::Sweep,
            "until the target is perceived the sweep continues (tick {t})"
        );
    }
    let engaged_at = engaged_at.expect("the S&D ship engages once the target is perceived (VC2)");
    assert!(
        contact_for(&w, ship, target).is_some(),
        "the engage came from a real perception contact"
    );

    // Phase C — CLOSE: the ship converges onto its brawler engage ring
    // (standoff 60, band ≤ 75) around the target.
    let mut closed = (pos_of(&w, ship) - pos_of(&w, target)).length() <= 75.0;
    for t in engaged_at + 1..=engaged_at + 600 {
        if closed {
            break;
        }
        mirror_tick_and_run(&mut w, &mut s, t);
        assert_eq!(
            brain_of(&w, ship).behavior,
            Behavior::Engage,
            "prosecution holds while closing (tick {t})"
        );
        closed = (pos_of(&w, ship) - pos_of(&w, target)).length() <= 75.0;
    }
    assert!(closed, "the S&D ship closes onto its engage ring (SC-007)");
}

// ---------------------------------------------------------------------------
// T037 — TR-019: AI-populated per-tick-checksum determinism. A rich scenario
// world (brains + squads + perception/network + one aggregate collapse→expand
// round-trip) rebuilt FRESH from the same inputs in the same binary and re-run
// must be bit-identical, compared via a PER-TICK state checksum — catching
// drift at the FIRST divergent tick, not just at end state.
// ---------------------------------------------------------------------------

/// splitmix64-style avalanche fold — deterministic, dependency-free 64-bit
/// mixing (the TR-019 checksum needs bit-stability, not cryptography). Folding
/// is sequential, so field POSITION in the fold disambiguates equal values.
fn mix64(h: u64, v: u64) -> u64 {
    let mut z = (h ^ v).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Raw f32 bit-packing of a `Vec2` (`x` high word, `y` low) — checksum input.
fn vec2_bits(v: Vec2) -> u64 {
    (u64::from(v.x.to_bits()) << 32) | u64::from(v.y.to_bits())
}

/// The TR-019 per-tick state checksum, plus two read-only scenario probes.
///
/// EVERY entity with a `Position` is hashed in a STABLE order (sorted by
/// entity bits — fresh rebuilds spawn in the same order, so the bits match
/// across runs): kinematics at raw f32 bit level (`Position`/`Velocity`/
/// `Heading`/`AngularVelocity`), the `ShipIntent` (analog axes as bits +
/// the fire bools + `active_group`), and the AI behavior state — `AiBrain`
/// behavior discriminant + `thinks_total`, the `Squad` order discriminant,
/// the `Gliding` presence bit, and the `AoiTier` tier discriminant. Optional
/// components fold a presence tag so "absent" never aliases a zero value.
///
/// Returns `(checksum, gliding_member_count, any_contact_known)` — the probes
/// feed the scenario sanity assertions without a second world pass.
fn world_checksum(w: &mut World) -> (u64, usize, bool) {
    let mut q = w.query::<(
        Entity,
        &Position,
        Option<&Velocity>,
        Option<&Heading>,
        Option<&AngularVelocity>,
        Option<&ShipIntent>,
        Option<&AiBrain>,
        Option<&Squad>,
        Option<&Gliding>,
        Option<&AoiTier>,
        Option<&ContactList>,
    )>();
    let mut rows: Vec<(u64, u64)> = Vec::new();
    let mut gliding_count = 0usize;
    let mut any_contact = false;
    for (e, pos, vel, heading, omega, intent, brain, squad, glide, aoi, contacts) in q.iter(w) {
        let mut h = 0u64;
        h = mix64(h, vec2_bits(pos.0));
        match vel {
            Some(v) => {
                h = mix64(h, 1);
                h = mix64(h, vec2_bits(v.0));
            }
            None => h = mix64(h, 0),
        }
        match heading {
            Some(hd) => {
                h = mix64(h, 1);
                h = mix64(h, u64::from(hd.0.to_bits()));
            }
            None => h = mix64(h, 0),
        }
        match omega {
            Some(o) => {
                h = mix64(h, 1);
                h = mix64(h, u64::from(o.0.to_bits()));
            }
            None => h = mix64(h, 0),
        }
        match intent {
            Some(i) => {
                h = mix64(h, 1);
                h = mix64(h, u64::from(i.forward.to_bits()));
                h = mix64(h, u64::from(i.strafe.to_bits()));
                h = mix64(h, u64::from(i.turn.to_bits()));
                h = mix64(
                    h,
                    u64::from(i.fire_primary)
                        | (u64::from(i.fire_secondary) << 1)
                        | (u64::from(i.active_group) << 2),
                );
            }
            None => h = mix64(h, 0),
        }
        match brain {
            Some(b) => {
                h = mix64(h, 1);
                h = mix64(h, u64::from(b.behavior as u8));
                h = mix64(h, b.thinks_total);
            }
            None => h = mix64(h, 0),
        }
        match squad {
            Some(sq) => {
                let order = match sq.order {
                    SquadOrder::Hold => 0u64,
                    SquadOrder::MoveTo(_) => 1,
                    SquadOrder::Engage(_) => 2,
                    SquadOrder::FormUp => 3,
                    SquadOrder::Withdraw(_) => 4,
                };
                h = mix64(h, 1);
                h = mix64(h, order);
            }
            None => h = mix64(h, 0),
        }
        h = mix64(h, u64::from(glide.is_some()));
        match aoi {
            Some(a) => {
                h = mix64(h, 1);
                h = mix64(h, u64::from(a.tier as u8));
            }
            None => h = mix64(h, 0),
        }
        rows.push((e.to_bits(), h));

        gliding_count += usize::from(glide.is_some());
        any_contact |= contacts.is_some_and(|c| !c.contacts.is_empty());
    }
    rows.sort_unstable_by_key(|&(bits, _)| bits);
    let mut acc = mix64(0, rows.len() as u64); // Entity-count drift is drift too.
    for (bits, h) in rows {
        acc = mix64(acc, bits);
        acc = mix64(acc, h);
    }
    (acc, gliding_count, any_contact)
}

/// The TR-019 scenario world, built deterministically from fixed inputs (no
/// time, no rand): a player at the origin; a Red 3-fighter squad beside it
/// (Active tier — brains think on the fast cadence, members scan, the Red
/// sensor network fuses); and a Blue 3-fighter squad 700 units out — Dormant,
/// so it COLLAPSES to a cheap-glide aggregate at the hysteresis dwell, then
/// its `MoveTo` glide (anchor speed 80 u/s) carries it straight at the Red
/// squad: crossing the player's mid band / Red's sensor bubble promotes it
/// (proximity + far-hostile scan — mutual hostility), the aggregate EXPANDS,
/// and both squads' armed members acquire each other through perception. All
/// members are armed FITTED fighters (`can_fire` stats, no `Weapon` cooldown
/// component → fire intents rise but no projectile/damage path runs, so the
/// 600-tick population is stable). Default `AiTuning` radii throughout — the
/// collapse depends on them.
fn build_checksum_world() -> World {
    let (mut w, _schedule) = perception_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    // Red squad: inside aoi_radius_active (120) of the player, station-keeping.
    let red: Vec<Entity> = (0..3)
        .map(|i| spawn_squad_member(&mut w, Vec2::new(30.0 + i as f32 * 8.0, 0.0)))
        .collect();
    for &m in &red {
        w.entity_mut(m).insert((
            Faction::Red,
            ContactList::default(),
            stats_with_top_speed(80.0),
        ));
    }
    spawn_squad(&mut w, &red, FormationDef::wedge(3, 8.0), SquadOrder::Hold);

    // Blue squad: far beyond aoi_radius_mid (520) → settles Dormant + glides;
    // the MoveTo goal sits ON the Red squad, so the glide path closes the gap.
    let blue: Vec<Entity> = (0..3)
        .map(|i| spawn_squad_member(&mut w, Vec2::new(700.0 + i as f32 * 8.0, 0.0)))
        .collect();
    for &m in &blue {
        w.entity_mut(m).insert((
            Faction::Blue,
            ContactList::default(),
            stats_with_top_speed(80.0),
        ));
    }
    spawn_squad(
        &mut w,
        &blue,
        FormationDef::wedge(3, 8.0),
        SquadOrder::MoveTo(Vec2::new(30.0, 0.0)),
    );
    w
}

/// TR-019 [COMPLETES TR-019]: the scenario world above, rebuilt FRESH twice
/// and run 600 ticks through the FULL fixed schedule (`CurrentTick` mirrored,
/// the server's `step_sim` order), produces bit-identical per-tick checksums —
/// any mismatch reports the FIRST divergent tick. The run is then proven
/// meaningful: the Blue aggregate really collapsed (Gliding seen), was later
/// promoted + expanded (Gliding absent again), brains completed real thinks,
/// and perception contacts existed at some tick.
#[test]
fn ai_world_is_bit_identical_across_fresh_rebuilds() {
    const TICKS: u64 = 600;

    let run = || {
        let mut w = build_checksum_world();
        let mut s = Schedule::default();
        sim::add_fixed_step_systems(&mut s);
        let mut trace: Vec<u64> = Vec::with_capacity(TICKS as usize);
        let mut first_glide: Option<u64> = None;
        let mut expanded: Option<u64> = None;
        let mut saw_contact = false;
        for tick in 0..TICKS {
            mirror_tick_and_run(&mut w, &mut s, tick);
            let (checksum, gliding, contact) = world_checksum(&mut w);
            trace.push(checksum);
            if gliding > 0 && first_glide.is_none() {
                first_glide = Some(tick);
            }
            if gliding == 0 && first_glide.is_some() && expanded.is_none() {
                expanded = Some(tick);
            }
            saw_contact |= contact;
        }
        let max_thinks = w
            .query::<&AiBrain>()
            .iter(&w)
            .map(|b| b.thinks_total)
            .max()
            .unwrap_or(0);
        (trace, first_glide, expanded, saw_contact, max_thinks)
    };

    let (trace_a, glide_a, expand_a, contact_a, thinks_a) = run();
    let (trace_b, glide_b, expand_b, contact_b, thinks_b) = run();

    // The TR-019 comparison: bit identity per tick, first divergence named.
    if let Some(t) = trace_a.iter().zip(&trace_b).position(|(a, b)| a != b) {
        panic!(
            "TR-019 drift: fresh-rebuild AI worlds diverged at tick {t} of {TICKS} \
             (checksum {:#018x} vs {:#018x}) — caught at the FIRST divergent tick",
            trace_a[t], trace_b[t]
        );
    }
    assert_eq!(
        trace_a, trace_b,
        "per-tick checksum traces are equal across fresh rebuilds (TR-019)"
    );

    // The scenario actually exercised the machinery (no vacuous pass):
    let collapsed_at =
        glide_a.expect("the far Blue squad collapsed to a glide aggregate (Gliding seen)");
    let expanded_at = expand_a
        .expect("the glide was later promoted and EXPANDED (Gliding absent for those members)");
    assert!(
        collapsed_at < expanded_at,
        "collapse (tick {collapsed_at}) precedes expansion (tick {expanded_at}) — \
         a real aggregate round-trip"
    );
    assert!(
        contact_a,
        "perception + the sensor network produced real contacts at some tick"
    );
    assert!(thinks_a > 0, "brains completed real decision work");
    assert_eq!(
        (glide_a, expand_a, contact_a, thinks_a),
        (glide_b, expand_b, contact_b, thinks_b),
        "the scenario probes agree across the rebuilds too"
    );
    eprintln!(
        "[t037] checksum determinism: collapse @{collapsed_at}, expand @{expanded_at}, \
         max thinks {thinks_a}, contacts seen: {contact_a}"
    );
}

// ---------------------------------------------------------------------------
// R96 — style precedence chain (squad ← role ← archetype default).
// ---------------------------------------------------------------------------

use sim::ai::{default_combat_stance, default_movement_profile};

/// A Brawler-classified scenario AI ship for the precedence test: a fitted
/// `Ship` carrying `combat_stats(80, 30, 200)` (armed + tanky → Brawler) plus
/// the full brain stack at Active tier, phase bucket 0 (so it thinks at tick 0).
/// An optional `ScenarioRole` and `Faction` are attached when supplied.
fn spawn_precedence_ship(w: &mut World, id: u64, pos: Vec2, role: Option<ScenarioRole>) -> Entity {
    let mut e = w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        FlightAssist::On,
        combat_stats(80.0, 30.0, 200.0), // armed + tanky → Brawler.
        AiStableId(id),
        AiBrain {
            think_tier: Tier::Active,
            phase_bucket: 0,
            ..AiBrain::default()
        },
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ));
    if let Some(role) = role {
        e.insert((role, Faction::Red, ContactList::default()));
    }
    e.id()
}

/// R96 [resolves the precedence chain]: `ai_think_system` resolves each ship's
/// `movement_profile` / `combat_stance` ONCE per think via `squad ← role ←
/// archetype default`, each writer storing its `Option` LOCALLY (squad onto the
/// brain channel, role on the `ScenarioRole`, base from `default_*`). Four ships
/// exercise every precedence level through the FULL schedule:
///
/// - (a) a LONE Brawler → the archetype default (`Rush` / `Charge`);
/// - (b) a ROLED ship with `with_style(Some(Leisurely), Some(Orbit))` → the role
///   override wins over its (Brawler) archetype → `Leisurely` / `Orbit`;
/// - (c) a NON-roled squad member whose squad sets `Some(Rush)`/`Some(Kite)` →
///   the squad channel wins → `Rush` / `Kite`;
/// - (d) a ROLED squad member (squad-exempt) → the ROLE override beats the squad
///   style → the role's `Leisurely` / `Orbit`, NOT the squad's `Rush` / `Kite`.
#[test]
fn stance_and_profile_precedence() {
    let (mut w, mut s) = obj2_world();
    // A player marker keeps every ship Active (proximity); AOI is widened so the
    // spread-out ships all stay Active-tier and think on cadence.
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));

    // The role style override reused by (b) and (d): Leisurely pace + CCW orbit.
    let styled_role = || {
        ScenarioRole::new(RoleGoal::PatrolRoute(vec![Vec2::ZERO]), Posture::FreeEngage).with_style(
            Some(MovementProfile::Leisurely),
            Some(CombatStance::Orbit { ccw: true }),
        )
    };

    // (a) LONE Brawler — no role, no squad → the archetype default.
    let lone = spawn_precedence_ship(&mut w, 0, Vec2::new(0.0, 0.0), None);

    // (b) ROLED lone ship — the role style overrides its Brawler archetype.
    let roled = spawn_precedence_ship(&mut w, 1, Vec2::new(100.0, 0.0), Some(styled_role()));

    // (c) NON-roled squad member — the squad style overrides the archetype.
    let sc0 = spawn_precedence_ship(&mut w, 2, Vec2::new(0.0, 200.0), None);
    let sc1 = spawn_precedence_ship(&mut w, 3, Vec2::new(15.0, 200.0), None);
    // (d) ROLED squad member — squad-exempt, so its role style wins over the squad.
    let sd_roled = spawn_precedence_ship(&mut w, 4, Vec2::new(30.0, 200.0), Some(styled_role()));
    let squad = spawn_squad(
        &mut w,
        &[sc0, sc1, sd_roled],
        FormationDef::wedge(3, 12.0),
        SquadOrder::Hold,
    );
    // Impose the squad style override (spawn_squad leaves it None): Rush / Kite.
    {
        let mut sq = w.get_mut::<Squad>(squad).expect("squad entity");
        sq.movement_profile = Some(MovementProfile::Rush);
        sq.combat_stance = Some(CombatStance::Kite);
    }

    // Step enough ticks that the squad assignment + every brain think resolves.
    for tick in 0..=2 {
        mirror_tick_and_run(&mut w, &mut s, tick);
    }

    // All four ships classified as Brawler (the archetype the defaults key off).
    for ship in [lone, roled, sc0, sd_roled] {
        assert_eq!(
            brain_of(&w, ship).archetype,
            FitArchetype::Brawler,
            "the fit classifies as Brawler (so the archetype default is Rush/Charge)"
        );
    }

    // (a) LONE → the archetype default: Brawler = Rush / Charge.
    let a = brain_of(&w, lone);
    assert_eq!(
        (a.movement_profile, a.combat_stance),
        (
            default_movement_profile(FitArchetype::Brawler),
            default_combat_stance(FitArchetype::Brawler)
        ),
        "lone ship resolves to its archetype default"
    );
    assert_eq!(
        (a.movement_profile, a.combat_stance),
        (MovementProfile::Rush, CombatStance::Charge),
        "Brawler archetype default is Rush / Charge"
    );

    // (b) ROLED lone → the role override beats the archetype default.
    let b = brain_of(&w, roled);
    assert_eq!(
        (b.movement_profile, b.combat_stance),
        (
            MovementProfile::Leisurely,
            CombatStance::Orbit { ccw: true }
        ),
        "the role's with_style override wins over the Brawler archetype default"
    );

    // (c) NON-roled squad member → the squad channel beats the archetype.
    for member in [sc0, sc1] {
        let c = brain_of(&w, member);
        assert_eq!(
            (c.squad_profile, c.squad_stance),
            (Some(MovementProfile::Rush), Some(CombatStance::Kite)),
            "the squad wrote its style onto the non-roled member's channel"
        );
        assert_eq!(
            (c.movement_profile, c.combat_stance),
            (MovementProfile::Rush, CombatStance::Kite),
            "the squad style override wins over the archetype default"
        );
    }

    // (d) ROLED squad member → squad-exempt, so the ROLE override wins (the
    // squad never wrote its channel for this member).
    let d = brain_of(&w, sd_roled);
    assert_eq!(
        (d.squad_profile, d.squad_stance),
        (None, None),
        "a roled member is squad-exempt — the squad channel stays None"
    );
    assert_eq!(
        (d.movement_profile, d.combat_stance),
        (
            MovementProfile::Leisurely,
            CombatStance::Orbit { ccw: true }
        ),
        "the role override wins over the squad style for a roled member"
    );
}

// ---------------------------------------------------------------------------
// R97 Phase 2 Stage E — STRATEGIC objective/planner tier (HTN) through the
// FULL fixed schedule. The planner sets `squad.order` from the squad faction's
// fused contact picture; `squad_think_system` (next in the set) translates it
// to member brains. These tests drive the picture through REAL perception +
// fusion (members carry a `Faction` + `ContactList`, hostiles are real bodies
// in the coarse index), so the whole strategic→squad→member chain is exercised.
// ---------------------------------------------------------------------------

use sim::ai::{strategic_plan_system, wing_plan_system, Objective, SquadObjective, WingObjective};

/// A wide-AOI full-schedule world with `SensorNetworks` (the
/// `network_integration_world` pattern): a player at the origin and 10k/20k
/// AOI radii keep every squad/member Active-tier, so scans + the network
/// rebuild both run at the 15-tick mid cadence. `SquadObjective`-carrying
/// squads in this world drive `strategic_plan_system`.
fn strategy_world() -> (World, Schedule) {
    let (mut w, s) = perception_world();
    w.insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    w.spawn((PlayerShip, Position(Vec2::ZERO)));
    (w, s)
}

/// A squad member that PERCEIVES: the standard squad-member stack plus a
/// `Faction` (so the squad has a faction + the member feeds fusion) and a
/// `ContactList` (so `perception_scan_system` scans real hostile bodies into
/// it and `sensor_network_system` fuses them into the faction picture the
/// planner reads). Active `AoiTier` so it scans every 15 ticks.
fn spawn_perceiving_member(w: &mut World, faction: Faction, pos: Vec2) -> Entity {
    w.init_resource::<AiIdAllocator>();
    let id = w.resource_mut::<AiIdAllocator>().allocate();
    let buckets = w.resource::<AiTuning>().fallback_bucket_count;
    w.spawn((
        Ship,
        ShipIntent::default(),
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        FlightAssist::On,
        id,
        AiBrain::new(id, buckets),
        ContactList::default(),
        faction,
        AoiTier {
            tier: Tier::Active,
            since_tick: 0,
        },
    ))
    .id()
}

/// Attach a `SquadObjective` to a squad entity (the planner's input).
fn set_objective(w: &mut World, se: Entity, goal: Objective) {
    w.entity_mut(se).insert(SquadObjective::new(goal));
}

fn objective_of(w: &World, se: Entity) -> SquadObjective {
    w.get::<SquadObjective>(se)
        .expect("squad carries a SquadObjective")
        .clone()
}

/// OBJ (Stage E): a `DestroyTarget` squad clears a perceived ESCORT first, then
/// switches to the target itself once the escort is gone. Real perception:
/// the squad members scan the target + its escort (both real Blue bodies near
/// the squad), fusion builds the Red picture, and the planner — seeing an
/// escort within the screening ring of the target — engages the escort; once
/// the escort despawns, the picture holds only the target, so the planner
/// engages the target.
#[test]
fn destroy_target_plans_escorts_then_target() {
    let (mut w, mut s) = strategy_world();

    // A small Red squad — strong enough NOT to be outnumbered by two bodies.
    let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(12.0, 0.0));
    let m2 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(24.0, 0.0));
    // Give each member a healthy hull so own_strength is solidly above the
    // perceived two-body enemy strength (no outnumbered flip here).
    for m in [m0, m1, m2] {
        w.entity_mut(m).insert(Health(100.0));
    }
    let se = spawn_squad(
        &mut w,
        &[m0, m1, m2],
        FormationDef::wedge(3, 12.0),
        SquadOrder::Hold,
    );

    // The target + a screening escort within sensor range of the squad, close
    // together (the escort sits inside the target's escort-screening ring,
    // default defend_arrive_radius 50). Both signature 3 (CollisionRadius).
    let target = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(150.0, 0.0), 3.0);
    let escort = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(140.0, 20.0), 3.0);
    set_objective(&mut w, se, Objective::DestroyTarget(target));

    // Step past the first plan tick (tick 0: scan + fuse + plan all align on
    // the %90/%15 multiple at phase-bucket 0; other buckets re-plan by tick 90).
    for t in 0..=90 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }

    // The escort is perceived screening the target → engage the ESCORT first.
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::Engage(escort),
        "a perceived escort near the target is cleared first"
    );

    // Remove the escort: the picture now holds only the target → engage it.
    w.despawn(escort);
    for t in 91..=181 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::Engage(target),
        "escort gone → engage the target itself"
    );
}

/// OBJ (Stage E): a SMALL `DestroyTarget` squad facing a much LARGER perceived
/// enemy force flips its objective to `Withdraw` (order = `Withdraw`), and on
/// arrival at the withdraw point regroups (`FormUp`, objective `Regroup`).
#[test]
fn outnumbered_squad_withdraws_then_regroups() {
    let (mut w, mut s) = strategy_world();

    // A lone weak member (own strength ~ 0.5 from a half-health hull).
    let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    w.entity_mut(m0).insert(Health(50.0));
    let se = spawn_squad(
        &mut w,
        &[m0],
        FormationDef::wedge(1, 12.0),
        SquadOrder::Hold,
    );

    // The target PLUS a wall of escorts — a big perceived force (≥ 1.5× the
    // squad's ~0.5 own strength is trivially met by several signature-3 bodies).
    let target = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(160.0, 0.0), 3.0);
    for i in 0..6 {
        spawn_hostile_body(
            &mut w,
            Faction::Blue,
            Vec2::new(150.0 + i as f32 * 5.0, 15.0),
            3.0,
        );
    }
    set_objective(&mut w, se, Objective::DestroyTarget(target));

    for t in 0..=90 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }

    // Outnumbered → the objective flipped to Withdraw and the order is Withdraw.
    assert!(
        matches!(objective_of(&w, se).goal, Objective::Withdraw(_)),
        "an outnumbered DestroyTarget squad flips to Withdraw"
    );
    assert!(
        matches!(squad_of(&w, se).order, SquadOrder::Withdraw(_)),
        "the squad order is Withdraw"
    );

    // The squad-of-1 degrade flies the member to the withdraw point (a
    // Waypoint, since Withdraw degrades to a solo waypoint). Drive it home by
    // teleporting the member onto the rally so the arrival test trips, then let
    // the next plan tick regroup. Capture the withdraw point first.
    let rally = match objective_of(&w, se).goal {
        Objective::Withdraw(p) => p,
        _ => unreachable!("just asserted Withdraw"),
    };
    w.get_mut::<Position>(m0).unwrap().0 = rally;
    // Also clear the perceived threat so Regroup can later complete; despawn the
    // hostiles (the picture ages out / prunes via the V-1 sweep + staleness).
    let blues: Vec<Entity> = {
        let mut q = w.query::<(Entity, &Faction)>();
        q.iter(&w)
            .filter(|(_, f)| **f == Faction::Blue)
            .map(|(e, _)| e)
            .collect()
    };
    for b in blues {
        w.despawn(b);
    }
    // Step until the objective first becomes Regroup — the arrival flip. The
    // squad re-plans on its slow cadence (~every 90 ticks), so we look for the
    // Regroup state in a bounded window AFTER the withdraw flip and BEFORE the
    // subsequent Regroup-complete plan tick collapses it to Hold (a squad-of-1
    // is trivially cohered, so Regroup auto-completes one cadence later).
    let mut saw_regroup_formup = false;
    for t in 91..=200 {
        mirror_tick_and_run(&mut w, &mut s, t);
        if matches!(objective_of(&w, se).goal, Objective::Regroup { .. }) {
            // On the arrival flip the order is FormUp (re-form at the rally).
            assert_eq!(
                squad_of(&w, se).order,
                SquadOrder::FormUp,
                "the squad re-forms (FormUp) at the rally on the Withdraw→Regroup flip"
            );
            saw_regroup_formup = true;
            break;
        }
    }
    assert!(
        saw_regroup_formup,
        "arriving at the withdraw point flips the objective to Regroup (FormUp order)"
    );
}

/// OBJ (Stage E): a `DefendZone` squad engages an intruder that enters the
/// defended ring, then returns to holding station at the anchor once the
/// intruder leaves (despawns).
///
/// R98 HOTFIX D: an intruder that withdraws to HOVER between the acquisition
/// ring (`radius`) and the release ring (`radius × defend_release_factor`)
/// does NOT flap the order — the engaged squad KEEPS engaging it across
/// several plan cadences (the engage-release hysteresis); the return to
/// station requires leaving the RELEASE ring or despawning (the despawn path
/// kept below).
#[test]
fn defend_zone_engages_intruder_then_returns() {
    let (mut w, mut s) = strategy_world();

    let anchor = Vec2::new(0.0, 0.0);
    let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(12.0, 0.0));
    for m in [m0, m1] {
        w.entity_mut(m).insert(Health(100.0));
    }
    let se = spawn_squad(
        &mut w,
        &[m0, m1],
        FormationDef::line_abreast(2, 12.0),
        SquadOrder::Hold,
    );
    set_objective(
        &mut w,
        se,
        Objective::DefendZone {
            anchor,
            radius: 120.0,
        },
    );

    // No intruder yet → the planner holds station at the anchor (MoveTo).
    for t in 0..=90 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::MoveTo(anchor),
        "no intruder → hold station at the anchor"
    );

    // An intruder enters the ring (80 < 120 radius) within sensor range.
    let intruder = spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(80.0, 0.0), 3.0);
    for t in 91..=181 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::Engage(intruder),
        "an intruder inside the ring is engaged"
    );

    // R98 HOTFIX D — the HOVER case: the intruder withdraws to 140 u — OUTSIDE
    // the 120 acquisition ring but INSIDE the 150 release ring (120 × 1.25).
    // Pre-fix this exact geometry flapped Engage↔MoveTo every plan tick; the
    // hysteresis must keep the order PINNED on Engage at every tick across
    // several plan cadences (90 ticks each).
    w.get_mut::<Position>(intruder).unwrap().0 = Vec2::new(140.0, 0.0);
    for t in 182..=455 {
        mirror_tick_and_run(&mut w, &mut s, t);
        assert_eq!(
            squad_of(&w, se).order,
            SquadOrder::Engage(intruder),
            "an engaged intruder hovering between the acquisition and release \
             rings never flaps the order (tick {t})"
        );
    }

    // The intruder leaves (despawns): the picture clears, the squad returns to
    // holding station at the anchor (the release path the hysteresis allows).
    w.despawn(intruder);
    for t in 456..=636 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::MoveTo(anchor),
        "intruder gone → return to holding station at the anchor"
    );
}

/// OBJ (Stage E): the planner re-plans only at the SLOW `strategic_plan_ticks`
/// cadence (not every tick), and two identical worlds produce identical
/// `squad.order` sequences (determinism).
#[test]
fn strategic_planner_runs_at_slow_cadence_and_is_deterministic() {
    let build = || {
        let (mut w, s) = strategy_world();
        let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
        let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(12.0, 0.0));
        for m in [m0, m1] {
            w.entity_mut(m).insert(Health(100.0));
        }
        let route = vec![Vec2::new(300.0, 0.0), Vec2::new(300.0, 300.0)];
        let se = spawn_squad(
            &mut w,
            &[m0, m1],
            FormationDef::wedge(2, 12.0),
            SquadOrder::Hold,
        );
        set_objective(&mut w, se, Objective::PatrolRoute(route));
        (w, s, se)
    };

    // Run A: record the order + last_plan_tick each tick over a window that
    // spans several plan cadences (90 ticks each).
    let run = || {
        let (mut w, mut s, se) = build();
        let mut orders = Vec::new();
        let mut plan_ticks = Vec::new();
        for t in 0..=200 {
            mirror_tick_and_run(&mut w, &mut s, t);
            orders.push(squad_of(&w, se).order);
            plan_ticks.push(objective_of(&w, se).last_plan_tick);
        }
        (orders, plan_ticks)
    };

    let (orders_a, plan_ticks_a) = run();
    let (orders_b, plan_ticks_b) = run();

    // Determinism: identical order + plan-tick sequences across two runs.
    assert_eq!(
        orders_a, orders_b,
        "two identical worlds yield identical squad.order sequences"
    );
    assert_eq!(plan_ticks_a, plan_ticks_b, "identical plan-tick sequences");

    // Slow cadence: `last_plan_tick` advances only when the planner actually
    // re-plans — NOT every tick. Detect the real re-plan EVENTS as the ticks
    // where the recorded value CHANGES from the previous tick's value (the
    // `SquadObjective::new` default seeds it to 0 before the first real plan,
    // so we track changes rather than raw distinct values). Each such event is
    // a genuine slow-cadence re-plan.
    let plan_cadence = u64::from(AiTuning::default().strategic_plan_ticks);
    let mut replans: Vec<u64> = Vec::new();
    let mut prev = 0u64;
    for &pt in &plan_ticks_a {
        // A change in the recorded `last_plan_tick` is a real re-plan event (the
        // `SquadObjective::new` default seeds it to 0 before the first plan).
        if pt != prev {
            replans.push(pt);
            prev = pt;
        }
    }
    // The planner re-planned at least twice across the 200-tick window (the
    // cadence really fired) but FAR fewer than 200 times (it is slow).
    assert!(
        replans.len() >= 2,
        "the planner re-planned across the window on the slow cadence (got {replans:?})"
    );
    assert!(
        replans.len() <= 4,
        "the planner re-plans only on the slow cadence, not every tick (got {replans:?})"
    );
    // Consecutive real re-plans are exactly one plan cadence apart.
    for w2 in replans.windows(2) {
        assert_eq!(
            w2[1] - w2[0],
            plan_cadence,
            "consecutive re-plans are exactly one plan cadence apart (events: {replans:?})"
        );
    }
}

/// A squad WITHOUT a `SquadObjective` is unaffected by the planner: its order
/// stays whatever was authored (the Phase-1 behavior), proving the strategic
/// tier is strictly additive and gated on the component.
#[test]
fn squad_without_objective_is_unaffected_by_planner() {
    let (mut w, mut s) = strategy_world();
    let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(12.0, 0.0));
    let goal = Vec2::new(400.0, 0.0);
    let se = spawn_squad(
        &mut w,
        &[m0, m1],
        FormationDef::wedge(2, 12.0),
        SquadOrder::MoveTo(goal),
    );
    // A hostile is present and would trip a DefendZone/DestroyTarget planner —
    // but with NO objective the planner skips this squad entirely.
    spawn_hostile_body(&mut w, Faction::Blue, Vec2::new(60.0, 0.0), 3.0);

    for t in 0..=180 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::MoveTo(goal),
        "an objective-less squad keeps its authored order — the planner ignores it"
    );
    assert!(
        w.get::<SquadObjective>(se).is_none(),
        "no SquadObjective was ever attached"
    );
    // The planner system is genuinely registered (it ran on the schedule), but
    // touched nothing — confirm via a quick direct-system smoke too.
    let _ = strategic_plan_system; // referenced so the import is load-bearing.
}

/// R98 HOTFIX B1: the strategic planner SKIPS a squad whose ALIVE members ALL
/// carry a `ScenarioRole`. Roled members are squad-order-exempt at the brain
/// level (script > squad), so an order planned here could reach nothing but
/// the dormant cheap-glide — which is exactly the playtest oscillation bug.
/// The squad's authored order must never change and zero planning work must
/// be done (`last_plan_tick` never stamped), even with an objective attached
/// that would otherwise rewrite the order every plan tick.
#[test]
fn planner_skips_squad_of_all_roled_members() {
    let (mut w, mut s) = strategy_world();
    let anchor = Vec2::new(300.0, 0.0);

    let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(12.0, 0.0));
    for m in [m0, m1] {
        w.entity_mut(m).insert((
            Health(100.0),
            // EVERY member carries a role → the squad is uncommandable.
            ScenarioRole::new(
                RoleGoal::PatrolRoute(vec![Vec2::new(0.0, 200.0)]),
                Posture::FreeEngage,
            ),
        ));
    }
    let se = spawn_squad(
        &mut w,
        &[m0, m1],
        FormationDef::wedge(2, 12.0),
        SquadOrder::Hold,
    );
    // A DefendZone objective that — on a COMMANDABLE squad — rewrites the order
    // to MoveTo(anchor) on the very first plan tick (proven by
    // `defend_zone_engages_intruder_then_returns`).
    set_objective(
        &mut w,
        se,
        Objective::DefendZone {
            anchor,
            radius: 120.0,
        },
    );

    for t in 0..=180 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }

    assert_eq!(
        squad_of(&w, se).order,
        SquadOrder::Hold,
        "an all-roled squad is invisible to the planner — the authored order \
         never changes (no order can reach its role-exempt members)"
    );
    assert_eq!(
        objective_of(&w, se).last_plan_tick,
        0,
        "zero planning work: last_plan_tick is never stamped for a skipped squad"
    );
}

// ---------------------------------------------------------------------------
// R97 Phase 2 Stage F — the THIN WING tier (wing_plan_system)
// A wing groups role-coherent squads (Squad.wing == Some(wing)) under one brain;
// wing_plan_system decomposes a WingObjective into each member squad's
// SquadObjective at the slow strategic cadence. These tests step the FULL fixed
// schedule (wing_plan runs BEFORE strategic_plan, which runs before squad_think).
// ---------------------------------------------------------------------------

/// Spawn a bare WING entity carrying a stable id + a `WingObjective` — the
/// authoring shape the scenario uses (a wing has no body of its own).
fn spawn_wing(w: &mut World, goal: Objective) -> Entity {
    w.init_resource::<AiIdAllocator>();
    let id = w.resource_mut::<AiIdAllocator>().allocate();
    w.spawn((id, WingObjective::new(goal))).id()
}

/// Link an existing squad under `wing` (the `Squad.wing` hierarchy seam).
fn set_wing(w: &mut World, squad: Entity, wing: Entity) {
    w.get_mut::<Squad>(squad).expect("squad entity").wing = Some(wing);
}

/// OBJ (Stage F): a wing with TWO member squads + a `WingObjective::DefendZone`
/// decomposes — after a wing-plan tick — into each member squad's
/// `SquadObjective`: the LEAD squad (lowest stable id) defends the zone
/// directly, the OTHER screens the perimeter (a `PatrolRoute` ring). A squad
/// NOT in the wing keeps its own authored objective, untouched.
#[test]
fn wing_objective_assigns_squad_objectives() {
    let (mut w, mut s) = strategy_world();
    let anchor = Vec2::new(500.0, 0.0);
    let radius = 400.0;

    // Two RED member squads (each one perceiving member with a healthy hull),
    // spawned lead-first so the lead squad has the lowest stable id.
    let lead_m = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
    w.entity_mut(lead_m).insert(Health(100.0));
    let lead_sq = spawn_squad(
        &mut w,
        &[lead_m],
        FormationDef::wedge(1, 12.0),
        SquadOrder::Hold,
    );
    let scr_m = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(40.0, 0.0));
    w.entity_mut(scr_m).insert(Health(100.0));
    let screen_sq = spawn_squad(
        &mut w,
        &[scr_m],
        FormationDef::wedge(1, 12.0),
        SquadOrder::Hold,
    );

    // Both member squads enroll in the strategic tier with a placeholder
    // objective (Hold) — the wing RE-TARGETS them. They join the wing.
    set_objective(&mut w, lead_sq, Objective::Hold);
    set_objective(&mut w, screen_sq, Objective::Hold);
    let wing = spawn_wing(&mut w, Objective::DefendZone { anchor, radius });
    set_wing(&mut w, lead_sq, wing);
    set_wing(&mut w, screen_sq, wing);

    // An INDEPENDENT squad (no wing) with its own DefendZone elsewhere — the
    // wing must never touch it.
    let free_m = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 600.0));
    w.entity_mut(free_m).insert(Health(100.0));
    let free_sq = spawn_squad(
        &mut w,
        &[free_m],
        FormationDef::wedge(1, 12.0),
        SquadOrder::Hold,
    );
    let free_goal = Objective::DefendZone {
        anchor: Vec2::new(0.0, 600.0),
        radius: 50.0,
    };
    set_objective(&mut w, free_sq, free_goal.clone());

    // Step past the first slow plan tick (wing re-plans on the bare
    // strategic-cadence multiple; tick 0 is one such multiple).
    for t in 0..=90 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }

    // The wing decomposed onto its members: lead anchors the zone, the screen
    // squad patrols the perimeter ring.
    assert_eq!(
        objective_of(&w, lead_sq).goal,
        Objective::DefendZone { anchor, radius },
        "the LEAD squad (lowest stable id) defends the zone directly"
    );
    assert!(
        matches!(
            objective_of(&w, screen_sq).goal,
            Objective::PatrolRoute(ref route) if route.len() == 4
                && route.iter().all(|p| ((*p - anchor).length() - radius).abs() < 1e-3)
        ),
        "the non-lead member squad screens the perimeter (a ring patrol at `radius`): got {:?}",
        objective_of(&w, screen_sq).goal
    );

    // The independent squad's objective is untouched by the wing.
    assert_eq!(
        objective_of(&w, free_sq).goal,
        free_goal,
        "a squad NOT in the wing keeps its own authored objective"
    );

    // The wing stamped its last_plan_tick on the slow cadence — by tick 90
    // (one strategic cadence past the tick-0 plan) it re-planned at tick 90.
    assert_eq!(
        w.get::<WingObjective>(wing)
            .expect("wing carries a WingObjective")
            .last_plan_tick,
        90,
        "the wing re-planned on the slow cadence (last at tick 90)"
    );
}

/// OBJ (Stage F): the wing planner is deterministic — two identical worlds yield
/// identical member-squad objective sequences across the full schedule.
#[test]
fn wing_plan_is_deterministic() {
    let build = || {
        let (mut w, s) = strategy_world();
        let m0 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(0.0, 0.0));
        w.entity_mut(m0).insert(Health(100.0));
        let sq0 = spawn_squad(
            &mut w,
            &[m0],
            FormationDef::wedge(1, 12.0),
            SquadOrder::Hold,
        );
        let m1 = spawn_perceiving_member(&mut w, Faction::Red, Vec2::new(40.0, 0.0));
        w.entity_mut(m1).insert(Health(100.0));
        let sq1 = spawn_squad(
            &mut w,
            &[m1],
            FormationDef::wedge(1, 12.0),
            SquadOrder::Hold,
        );
        set_objective(&mut w, sq0, Objective::Hold);
        set_objective(&mut w, sq1, Objective::Hold);
        let wing = spawn_wing(
            &mut w,
            Objective::DefendZone {
                anchor: Vec2::new(500.0, 0.0),
                radius: 400.0,
            },
        );
        set_wing(&mut w, sq0, wing);
        set_wing(&mut w, sq1, wing);
        (w, s, sq0, sq1)
    };
    let run = || {
        let (mut w, mut s, sq0, sq1) = build();
        let mut trace = Vec::new();
        for t in 0..=200 {
            mirror_tick_and_run(&mut w, &mut s, t);
            trace.push((objective_of(&w, sq0).goal, objective_of(&w, sq1).goal));
        }
        trace
    };
    assert_eq!(
        run(),
        run(),
        "the wing planner is deterministic across identical worlds"
    );
    // The import is load-bearing (the system is registered in the schedule).
    let _ = wing_plan_system;
}

// ---------------------------------------------------------------------------
// R99 Phase A — PlayerOrder: a USER command override at HIGHEST precedence
// (player > squad > role > archetype), sticking through the think loop. These
// run the FULL fixed schedule so the precedence integration in `ai_think_system`
// + the squad-exempt / planner-skip / V-1 prune wiring are all exercised live.
// ---------------------------------------------------------------------------

use sim::ai::{OrderKind, PlayerOrder};

/// R99 Phase A: a `PlayerOrder::move_to(P)` sticks ABOVE both a conflicting
/// `ScenarioRole` (a PatrolRoute pointing one way) AND a squad `MoveTo` order
/// pointing another way — every think the brain's waypoint is P, the ship flies
/// to P, and neither the role nor the squad ever diverts it.
#[test]
fn player_order_moveto_sticks_over_role_and_squad() {
    let (mut w, mut s) = roles_world();
    let player_goal = Vec2::new(300.0, 0.0);
    let route_decoy = Vec2::new(-400.0, 0.0); // The role would patrol the OTHER way.
    let squad_decoy = Vec2::new(0.0, -500.0); // The squad order points elsewhere.

    // A ship with a conflicting PatrolRoute role AND a PlayerOrder::move_to(P).
    let ship = spawn_roled_ship(
        &mut w,
        0,
        Faction::Red,
        Vec2::ZERO,
        ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![route_decoy, Vec2::new(-200.0, 0.0)]),
            Posture::FreeEngage,
        ),
        true,
    );
    w.entity_mut(ship).insert(PlayerOrder::move_to(player_goal));

    // Put it in a squad whose order points yet another way (the squad must NOT
    // command a player-ordered member — it is squad-exempt).
    let squad = spawn_squad(
        &mut w,
        &[ship],
        FormationDef::wedge(1, 12.0),
        SquadOrder::MoveTo(squad_decoy),
    );
    let _ = squad;

    // Every think: the player's waypoint wins (not the role leg, not the squad).
    let mut reached = false;
    for t in 0..=2000 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, ship);
        assert_eq!(
            b.waypoint,
            Some(player_goal),
            "the player MoveTo waypoint wins every think (tick {t})"
        );
        assert_ne!(b.waypoint, Some(route_decoy), "the role never diverts it");
        assert_ne!(b.waypoint, Some(squad_decoy), "the squad never diverts it");
        if (pos_of(&w, ship) - player_goal).length() <= 12.0 {
            reached = true;
            break;
        }
    }
    assert!(reached, "the commanded ship flies to the player's point P");

    // The squad never wrote a goal onto the exempt member (its brain leader/slot
    // stay clear — the squad assignment was skipped).
    let b = brain_of(&w, ship);
    assert_eq!(b.leader, None, "no squad leader assigned (squad-exempt)");
    assert_eq!(
        b.formation_slot, None,
        "no squad slot assigned (squad-exempt)"
    );
}

/// R99 Phase A: `PlayerOrder::attack(t)` makes the ship select `Engage` with
/// `target == t` and close on it; despawning `t` makes the V-1 sweep CLEAR the
/// Attack command (kind → None, settings-only) so the ship stops engaging the
/// ghost (degrades out of Engage at its next think).
#[test]
fn player_order_attack_engages_and_autoclears() {
    let (mut w, mut s) = combat_world();
    w.spawn((PlayerShip, Position(Vec2::ZERO))); // Keeps the bubble Active.

    let target = spawn_weak_fitted_target(&mut w, Vec2::new(150.0, 0.0));
    let stats = brawler_shooter_stats();
    // An armed fighter with a DEFAULT brain (no target set) — the PlayerOrder is
    // the ONLY thing that makes it engage.
    let fighter = spawn_armed_ai_fighter(&mut w, 0, Vec2::ZERO, target, stats);
    // Reset the brain target the helper pre-set, so only the PlayerOrder drives.
    w.get_mut::<AiBrain>(fighter).unwrap().target = None;
    w.entity_mut(fighter).insert(PlayerOrder::attack(target));

    let start_dist = (pos_of(&w, fighter) - pos_of(&w, target)).length();

    // Phase A — the command makes it Engage the target and close.
    let mut engaged = false;
    for t in 0..600 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, fighter);
        if b.behavior == Behavior::Engage {
            assert_eq!(b.target, Some(target), "engages the COMMANDED target");
            engaged = true;
        }
        if engaged && t > 30 {
            break;
        }
    }
    assert!(engaged, "PlayerOrder::attack(t) selects Engage on t");
    let closed_dist = (pos_of(&w, fighter) - pos_of(&w, target)).length();
    assert!(
        closed_dist < start_dist,
        "the commanded ship CLOSED on the target ({closed_dist} < {start_dist})"
    );

    // Phase B — despawn the target: the sweep clears the dangling Attack to
    // settings-only (kind None), and the ship stops engaging the ghost.
    w.despawn(target);
    let mut stopped = false;
    for t in 600..900 {
        mirror_tick_and_run(&mut w, &mut s, t);
        // The sweep cleared the dangling Attack command to settings-only.
        let order = w
            .get::<PlayerOrder>(fighter)
            .expect("order kept (style survives)");
        assert_eq!(
            order.kind, None,
            "the dangling Attack(t) cleared to settings-only (tick {t})"
        );
        let b = brain_of(&w, fighter);
        if b.behavior != Behavior::Engage {
            assert_eq!(b.target, None, "no ghost target");
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the ship stops engaging once the target despawns");
}

/// R99 Phase A: a roled ship whose role sets stance X is overridden by a
/// `PlayerOrder` carrying stance Y — the resolved `combat_stance` is Y (the
/// player wins the style chain over the role, which already wins over the
/// archetype default).
#[test]
fn player_order_style_overrides_role_and_archetype() {
    let (mut w, mut s) = roles_world();

    // The role pins stance X = Charge (over the armed-fighter archetype default).
    let role = ScenarioRole::new(
        RoleGoal::PatrolRoute(vec![Vec2::new(200.0, 0.0)]),
        Posture::FreeEngage,
    )
    .with_style(None, Some(CombatStance::Charge));
    let ship = spawn_roled_ship(&mut w, 0, Faction::Red, Vec2::ZERO, role, true);

    // The player overrides the STANCE to Y = Kite (and the profile to Rush, to
    // show the profile channel wins too).
    w.entity_mut(ship).insert(
        PlayerOrder::settings_only()
            .with_stance(CombatStance::Kite)
            .with_profile(MovementProfile::Rush),
    );

    for t in 0..=2 {
        mirror_tick_and_run(&mut w, &mut s, t);
    }
    let b = brain_of(&w, ship);
    assert_eq!(
        b.combat_stance,
        CombatStance::Kite,
        "the player stance Y wins over the role stance X and the archetype default"
    );
    assert_eq!(
        b.movement_profile,
        MovementProfile::Rush,
        "the player profile wins the chain too"
    );
}

/// R99 Phase A: a squad member under a `PlayerOrder` is squad-exempt — the squad
/// order never moves it (its waypoint is the player's, not the squad's) — and
/// the strategic planner SKIPS a squad whose members are all commanded/roled.
#[test]
fn player_order_ship_is_squad_exempt() {
    let (mut w, mut s) = strategy_world();

    let squad_goal = Vec2::new(0.0, 600.0); // Where the squad order would send it.
    let player_goal = Vec2::new(400.0, 0.0); // Where the player commands it.

    // A commandable squad member, commanded by the player to a DIFFERENT point.
    let commanded = spawn_perceiving_member(&mut w, Faction::Red, Vec2::ZERO);
    w.entity_mut(commanded).insert(Health(100.0));
    w.entity_mut(commanded)
        .insert(PlayerOrder::move_to(player_goal));
    let squad = spawn_squad(
        &mut w,
        &[commanded],
        FormationDef::wedge(1, 12.0),
        SquadOrder::MoveTo(squad_goal),
    );

    // The planner objective would re-target this squad each plan tick — but the
    // squad is all-commanded, so the planner must SKIP it (no last_plan_tick
    // stamp), and the squad order never reaches the exempt member.
    set_objective(&mut w, squad, Objective::PatrolRoute(vec![squad_goal]));

    let mut saw_player_goal = false;
    for t in 0..=200 {
        mirror_tick_and_run(&mut w, &mut s, t);
        let b = brain_of(&w, commanded);
        // Before the first cadence-due think the waypoint is still unset; once
        // set it is ALWAYS the player goal and NEVER the squad's.
        assert_ne!(
            b.waypoint,
            Some(squad_goal),
            "the squad order never reaches the commanded member (tick {t})"
        );
        if b.waypoint == Some(player_goal) {
            saw_player_goal = true;
        }
    }
    assert!(
        saw_player_goal,
        "the commanded member flies the PLAYER goal (squad-exempt)"
    );

    // The planner skipped the all-commanded squad: its objective was never
    // planned (last_plan_tick stays at the never-planned 0 sentinel).
    assert_eq!(
        objective_of(&w, squad).last_plan_tick,
        0,
        "the planner skips an all-commanded squad (no plan stamp)"
    );

    // And the exempt member carries no squad leader/slot assignment.
    let b = brain_of(&w, commanded);
    assert_eq!((b.leader, b.formation_slot), (None, None), "squad-exempt");
    let _ = OrderKind::MoveTo(player_goal); // OrderKind import is load-bearing.
}
