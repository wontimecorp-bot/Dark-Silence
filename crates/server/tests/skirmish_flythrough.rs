//! R102 Part A — the MOVING-player flythrough regression for the dormant-GLIDE
//! LOD leak (allied/enemy ships visibly "sliding across the screen without
//! physics").
//!
//! `skirmish_physics.rs` parks the player at the Red spawn; this test instead
//! SCRIPTS the player FLYING a path through the arena (out to the patrol lanes
//! near the origin and back) over the run, then asserts the SAME invariants at
//! every tick against the moving anchor:
//!   (a) NO entity carrying the `Gliding` marker is within `PLAY_SPACE_RADIUS`
//!       of the player — a gliding ship that close means dormant cheap-glide
//!       kinematics leaked into the play space (the bug), and
//!   (b) per-tick |Δv| ≤ `MAX_TICK_DV` for every non-gliding ship within
//!       `PLAY_SPACE_RADIUS` (the R98 thrust-bounded-motion invariant, now
//!       measured against a MOVING viewer).
//!
//! It runs the sweep under TWO `AiTuning` regimes:
//!   - `AiTuning::default()` (`aoi_radius_mid = 520`) — the headless/golden
//!     pinned tuning, and
//!   - a SMALL-radius tuning (`aoi_radius_mid = 150`, `aoi_radius_active = 60`)
//!     — the suspected live dev-panel regime (the "AOI radius Mid" slider goes
//!     down to 20), where a dormant glide collapses well inside the camera's
//!     view band.
//!
//! Pre-fix, the small-radius regime trips (a) within a few hundred ticks the
//! first time a role-driven Blue patrol fighter glides inside 600 u of the
//! flying player. Post-fix (the `glide_min_radius` floor decoupling the
//! Dormant/glide cutoff from the tunable tier radius), neither regime trips.

use std::collections::BTreeMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::With;
use glam::Vec2;
use server::{Scenario, ServerApp};
use sim::ai::{AiBrain, AiTuning, Gliding, PlayerShip};
use sim::components::{Faction, Position, Ship, Velocity};
use sim::{FactionSpawns, FixedDt, ShipIntent};

/// Max allowed |Δv| (world units/s) for a play-space ship between consecutive
/// ticks — the R98 thrust-bounded bound (legit ≈ 0.1–2 u/s/tick; the glide-flip
/// bug produced ~80–160 u/s instantaneous jumps). Matches `skirmish_physics`.
const MAX_TICK_DV: f32 = 5.0;

/// The play-space radius (world units): the band around the player a glide must
/// never enter and inside which physics continuity is asserted. 600 matches
/// `skirmish_physics`'s `GLIDE_EXCLUSION_RADIUS` (≥ the max camera view corner).
const PLAY_SPACE_RADIUS: f32 = 600.0;

/// How long to fly (ticks): 3000 = 100 s at 30 Hz — long enough for the player
/// to cross the arena to the patrol lanes and back several times while squads
/// collapse/expand around the moving viewer.
const TICKS: u64 = 3000;

/// Cruise speed (world units/s) the scripted player flies at — a moderate pace
/// inside the fitted fighter's ~80 u/s envelope, so the flythrough is a
/// plausible human path, not a teleport.
const PLAYER_SPEED: f32 = 70.0;

/// Outcome of one flythrough sweep: the first `Gliding`-in-play-space violation
/// and the first |Δv| violation (each `Some((tick, distance))`), or `None` if
/// the invariant held for the whole run.
#[derive(Debug, Default)]
struct Outcome {
    glide_leak: Option<(u64, f32)>,
    dv_spike: Option<(u64, f32)>,
    ai_near_ticks: u64,
    total_thinks: u64,
}

/// Script the moving player's position this tick: a back-and-forth sweep along
/// the y = 150 patrol lane, from the Red half across the origin into the Blue
/// half and back. The asteroid sits at the origin and the Blue role-driven
/// patrol loops the +x half around y ∈ [150, 250]; flying this line drives the
/// player straight through every squad's collapse/expand neighborhood.
fn player_path(tick: u64, dt: f32) -> Vec2 {
    // A triangle wave in x over the arena half-span, holding y on the lane.
    let dist = PLAYER_SPEED * dt * tick as f32;
    // Sweep x within ±1100 (just inside the ±1200 outposts). period = 2·span.
    let span = 1100.0_f32;
    let period = 4.0 * span; // out (2·span) + back (2·span)
    let phase = dist % period;
    let x = if phase <= 2.0 * span {
        -span + phase // -span → +span
    } else {
        span - (phase - 2.0 * span) // +span → -span
    };
    Vec2::new(x, 150.0)
}

/// Run one full flythrough sweep under `tuning`, scripting the player's
/// Position/Velocity each tick, and report the first violations + non-vacuity.
fn fly_through(tuning: AiTuning) -> Outcome {
    let (mut server, _t) = ServerApp::loopback();
    // Override the pinned default with the regime under test BEFORE the scenario
    // spawns (mirrors the windowed client: `net.rs` inserts `dev.ai` before
    // `spawn_scenario`, so squad spawns read this tuning).
    server.world_mut().insert_resource(tuning);
    server.spawn_scenario(Scenario::MiningSkirmish);

    // The PLAYER: a fitted Red ship at the Red spawn, marked `PlayerShip` so the
    // AOI classifier anchors its rings on the (about-to-move) viewer.
    let red_spawn = server
        .world()
        .get_resource::<FactionSpawns>()
        .expect("MiningSkirmish inserts FactionSpawns")
        .red;
    let player = server.spawn_fitted_ship(red_spawn, 0.0, Faction::Red);
    server.world_mut().entity_mut(player).insert(PlayerShip);
    if let Some(mut intent) = server.world_mut().get_mut::<ShipIntent>(player) {
        intent.fire_primary = false;
    }

    let dt = server.world().resource::<FixedDt>().0;

    let mut prev: BTreeMap<Entity, (Vec2, Vec2)> = BTreeMap::new();
    let mut out = Outcome::default();
    let mut prev_player = red_spawn;

    for tick in 1..=TICKS {
        // SCRIPT the player's flight: drive its authoritative Position/Velocity
        // directly (the test owns the human input here). Velocity is the actual
        // per-tick displacement / dt so the AOI classifier + any near-player
        // physics see a consistent moving body.
        let target = player_path(tick, dt);
        {
            let world = server.world_mut();
            if let Some(mut p) = world.get_mut::<Position>(player) {
                p.0 = target;
            }
            let v = (target - prev_player) / dt;
            if let Some(mut vel) = world.get_mut::<Velocity>(player) {
                vel.0 = v;
            }
        }
        prev_player = target;

        server.tick();

        // Re-read the player position (the tick may have integrated/collided it,
        // but we forced Position pre-tick; read it back as the anchor).
        let world = server.world_mut();
        let player_pos = world
            .get::<Position>(player)
            .expect("player ship persists")
            .0;

        // Sweep every (Ship, Position, Velocity): record t-state for all, assert
        // the play-space ones' |Δv|.
        let mut current: BTreeMap<Entity, (Vec2, Vec2)> = BTreeMap::new();
        {
            let mut q = world.query_filtered::<(Entity, &Position, &Velocity), With<Ship>>();
            for (entity, pos, vel) in q.iter(world) {
                current.insert(entity, (pos.0, vel.0));
            }
        }
        let mut ai_near_now = 0usize;
        for (&entity, &(pos, vel)) in &current {
            if entity == player {
                continue;
            }
            let d = (pos - player_pos).length();
            if d > PLAY_SPACE_RADIUS {
                continue;
            }
            ai_near_now += 1;
            if let Some(&(_, prev_vel)) = prev.get(&entity) {
                let dv = (vel - prev_vel).length();
                if dv > MAX_TICK_DV && out.dv_spike.is_none() {
                    out.dv_spike = Some((tick, d));
                }
            }
        }
        if ai_near_now > 0 {
            out.ai_near_ticks += 1;
        }
        prev = current;

        // (a) Dormant cheap-glide must NEVER run inside the play space.
        let mut glide_q = world.query_filtered::<(Entity, &Position), With<Gliding>>();
        for (_entity, pos) in glide_q.iter(world) {
            let d = (pos.0 - player_pos).length();
            if d <= PLAY_SPACE_RADIUS && out.glide_leak.is_none() {
                out.glide_leak = Some((tick, d));
            }
        }
    }

    let world = server.world_mut();
    let mut brains = world.query::<&AiBrain>();
    out.total_thinks = brains.iter(world).map(|b| b.thinks_total).sum();
    out
}

/// DEFAULT tuning (`aoi_radius_mid = 520`): the flying player never sees a glide
/// and every play-space ship moves under thrust-bounded physics.
#[test]
fn flythrough_default_tuning_no_visible_glide() {
    let out = fly_through(AiTuning::default());
    assert!(
        out.ai_near_ticks > 0,
        "default: no AI ship ever entered the {PLAY_SPACE_RADIUS} u play space in {TICKS} \
         ticks — the flythrough exercised nothing"
    );
    assert!(
        out.total_thinks > 0,
        "default: no AiBrain ever thought — the AI never ran"
    );
    assert!(
        out.glide_leak.is_none(),
        "default tuning: a GLIDING ship leaked into the play space at {:?} (tick, distance) — \
         dormant kinematics visible to the moving player",
        out.glide_leak
    );
    assert!(
        out.dv_spike.is_none(),
        "default tuning: a play-space ship jumped |Δv| > {MAX_TICK_DV} at {:?} (tick, distance) \
         — the glide-flip signature against the moving viewer",
        out.dv_spike
    );
}

/// SMALL-radius tuning (`aoi_radius_mid = 150`, `aoi_radius_active = 60`): the
/// suspected live dev-panel regime. Even with the tier radius dragged well under
/// the camera view, NO ship the player can see may glide, and play-space motion
/// stays thrust-bounded. (Pre-fix this trips the glide-leak assertion.)
#[test]
fn flythrough_small_radius_no_visible_glide() {
    let tuning = AiTuning {
        aoi_radius_active: 60.0,
        aoi_radius_mid: 150.0,
        ..AiTuning::default()
    };
    let out = fly_through(tuning);
    assert!(
        out.ai_near_ticks > 0,
        "small-radius: no AI ship ever entered the {PLAY_SPACE_RADIUS} u play space in {TICKS} \
         ticks — the flythrough exercised nothing"
    );
    assert!(
        out.total_thinks > 0,
        "small-radius: no AiBrain ever thought — the AI never ran"
    );
    assert!(
        out.glide_leak.is_none(),
        "SMALL-RADIUS tuning: a GLIDING ship leaked into the play space at {:?} (tick, distance) \
         — dormant kinematics visible to the moving player even though aoi_radius_mid is small. \
         The glide boundary must be FLOORED to the play space, not the tunable tier radius",
        out.glide_leak
    );
    assert!(
        out.dv_spike.is_none(),
        "SMALL-RADIUS tuning: a play-space ship jumped |Δv| > {MAX_TICK_DV} at {:?} \
         (tick, distance) — the glide-flip signature against the moving viewer",
        out.dv_spike
    );
}
