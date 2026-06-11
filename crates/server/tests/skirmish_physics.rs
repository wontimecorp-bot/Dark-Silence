//! R98 Fix G — the scenario-level physics regression test for the MiningSkirmish
//! glide tug-of-war (the playtest bug): with a player parked at the Red spawn,
//! NO ship near the player may ever change velocity faster than real physics
//! allows, teleport, or sit there running dormant cheap-glide kinematics.
//!
//! Pre-fix, the Red patrol (members carrying `ScenarioRole`s while their squad
//! ALSO carried a `SquadObjective` — two masters per ship) oscillated ~244 u
//! from the player with near-full-speed instantaneous velocity flips at the
//! dormant boundary (~80–160 u/s per-tick jumps): this test's `MAX_TICK_DV`
//! bound would have tripped within the first few hundred ticks. Post-fix
//! (R98 B–F sim guards + the Fix-A one-controller authoring rework) every
//! near-player ship obeys thrust-bounded, continuous Newtonian motion.

use std::collections::BTreeMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::With;
use glam::Vec2;
use server::{Scenario, ServerApp};
use sim::ai::{AiBrain, Gliding, PlayerShip};
use sim::components::{Faction, Position, Ship, Velocity};
use sim::{FactionSpawns, ShipIntent};

/// Max allowed |Δv| (world units/s) for a near-player ship between consecutive
/// ticks. Legit physics at the 30 Hz fixed step: the fitted fighter's
/// `thrust_force / total_mass · dt` is ≈ 0.1–2 u/s per tick (and a low-speed
/// resting bounce off a structure adds at most a couple more), while the
/// glide-flip bug produced ~80–160 u/s instantaneous jumps. 5.0 is generous
/// headroom over everything legitimate and ~20–30× below the bug's signature.
const MAX_TICK_DV: f32 = 5.0;

/// Position-continuity slack (world units): `pos(t) − pos(t−1) − v(t)·dt` must
/// stay within this (integrator-order slack + the collision system's small
/// separation push-outs). A glide expand/collapse pop or any teleport is far
/// larger.
const MAX_POS_RESIDUAL: f32 = 2.0;

/// The "near the player" assertion radius (world units) — comfortably beyond
/// `aoi_radius_mid` (520), so the whole Active/Mid neighborhood is covered.
const NEAR_RADIUS: f32 = 700.0;

/// No `Gliding` (dormant cheap-glide) ship may come this close to the player:
/// with `aoi_radius_mid = 520` plus promotion-is-immediate classification, a
/// gliding ship inside 600 u means the dormant kinematics leaked on screen.
const GLIDE_EXCLUSION_RADIUS: f32 = 600.0;

/// How long to run (ticks): 3000 = 100 s at 30 Hz — several strategic plan
/// cycles, patrol legs, a full transport mining round-trip, and the ambush
/// trigger near the rock.
const TICKS: u64 = 3000;

/// Per near-player ship, per tick: velocity changes are thrust-bounded (no
/// instantaneous glide flips), positions are continuous (no pops), and no
/// dormant cheap-glide runs near the player. Non-vacuous: asserts AI ships
/// actually entered the near radius and AI brains actually thought.
#[test]
fn mining_skirmish_physics_sanity() {
    let (mut server, _t) = ServerApp::loopback();
    server.spawn_scenario(Scenario::MiningSkirmish);

    // The PLAYER: mirror the windowed client's auto-join server-side — a
    // fitted Red ship at the Red faction spawn (just outside the Red outpost,
    // ≈ (-1188, -8)), marked `PlayerShip` so the AOI classifier anchors the
    // Active/Mid rings where a real player sits (client/net.rs does exactly
    // this insert after its handshake).
    let red_spawn = server
        .world()
        .get_resource::<FactionSpawns>()
        .expect("MiningSkirmish inserts FactionSpawns")
        .red;
    let player = server.spawn_fitted_ship(red_spawn, 0.0, Faction::Red);
    server.world_mut().entity_mut(player).insert(PlayerShip);
    // A human's trigger is their own; the helper spawns with `fire_primary`
    // pinned (it's a bench/test combatant helper). The parked player observes.
    if let Some(mut intent) = server.world_mut().get_mut::<ShipIntent>(player) {
        intent.fire_primary = false;
    }

    let dt = server.world().resource::<sim::FixedDt>().0;

    // Per-entity previous state, keyed by `Entity` (Ord — deterministic
    // iteration). Recorded for EVERY ship every tick so an entity re-entering
    // the near radius is always compared against its true t−1 state.
    let mut prev: BTreeMap<Entity, (Vec2, Vec2)> = BTreeMap::new();
    // Non-vacuity evidence (reported in the panic-free path too).
    let mut ai_near_ticks = 0u64;
    let mut max_ai_near_at_once = 0usize;

    for tick in 1..=TICKS {
        server.tick();
        let world = server.world_mut();
        let player_pos = world
            .get::<Position>(player)
            .expect("player ship persists")
            .0;

        // Sweep every (Ship, Position, Velocity): assert the near-player ones,
        // record t-state for all.
        let mut current: BTreeMap<Entity, (Vec2, Vec2)> = BTreeMap::new();
        {
            let mut q = world.query_filtered::<(Entity, &Position, &Velocity), With<Ship>>();
            for (entity, pos, vel) in q.iter(world) {
                current.insert(entity, (pos.0, vel.0));
            }
        }
        let mut ai_near_now = 0usize;
        for (&entity, &(pos, vel)) in &current {
            if (pos - player_pos).length() > NEAR_RADIUS {
                continue;
            }
            if entity != player {
                ai_near_now += 1;
            }
            let Some(&(prev_pos, prev_vel)) = prev.get(&entity) else {
                continue; // First tick this entity exists — no t−1 state yet.
            };
            let dv = (vel - prev_vel).length();
            assert!(
                dv <= MAX_TICK_DV,
                "tick {tick}: ship {entity:?} near the player ({:.1} u away) jumped \
                 |Δv| = {dv:.2} u/s in one tick (> {MAX_TICK_DV}): v {prev_vel:?} -> {vel:?} \
                 — the glide-flip signature",
                (pos - player_pos).length(),
            );
            let residual = (pos - prev_pos - vel * dt).length();
            assert!(
                residual <= MAX_POS_RESIDUAL,
                "tick {tick}: ship {entity:?} near the player ({:.1} u away) teleported: \
                 pos residual {residual:.2} u (> {MAX_POS_RESIDUAL}): {prev_pos:?} -> {pos:?} \
                 with v {vel:?}",
                (pos - player_pos).length(),
            );
        }
        if ai_near_now > 0 {
            ai_near_ticks += 1;
            max_ai_near_at_once = max_ai_near_at_once.max(ai_near_now);
        }
        prev = current;

        // Dormant cheap-glide must NEVER run near the player (aoi_radius_mid =
        // 520 + immediate promotion ⇒ anything inside 600 u is awake).
        let mut glide_q = world.query_filtered::<(Entity, &Position), With<Gliding>>();
        for (entity, pos) in glide_q.iter(world) {
            let d = (pos.0 - player_pos).length();
            assert!(
                d > GLIDE_EXCLUSION_RADIUS,
                "tick {tick}: GLIDING ship {entity:?} only {d:.1} u from the player \
                 (≤ {GLIDE_EXCLUSION_RADIUS}) — dormant kinematics leaked into the play space",
            );
        }
    }

    // Non-vacuity: the systems under test actually ran against this player.
    assert!(
        ai_near_ticks > 0,
        "no AI ship ever came within {NEAR_RADIUS} u of the player in {TICKS} ticks — \
         the test exercised nothing"
    );
    let world = server.world_mut();
    let mut brains = world.query::<&AiBrain>();
    let total_thinks: u64 = brains.iter(world).map(|b| b.thinks_total).sum();
    assert!(
        total_thinks > 0,
        "no AiBrain ever completed a think in {TICKS} ticks — the AI never ran"
    );
    // Evidence for the report (visible with `--nocapture`).
    println!(
        "non-vacuity: AI ships within {NEAR_RADIUS} u on {ai_near_ticks}/{TICKS} ticks \
         (max {max_ai_near_at_once} at once); total AI thinks = {total_thinks}"
    );
}
