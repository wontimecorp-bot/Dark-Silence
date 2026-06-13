//! R103 (extends R102 Part A) — the MOVING-player flythrough regression for the
//! dormant-GLIDE LOD leak (allied/enemy ships visibly "sliding across the screen
//! without physics" — and, the THIRD report, "sliding" as blips ON THE RADAR).
//!
//! `skirmish_physics.rs` parks the player at the Red spawn; this test instead
//! SCRIPTS the player FLYING a path through the arena (out to the patrol lanes
//! near the origin and back, SWEEPING in Y through the formation band) over the
//! run, then asserts the SAME invariants at every tick against the moving
//! anchor:
//!   (a) NO entity carrying the `Gliding` marker is within `RADAR_VISIBLE_RADIUS`
//!       of the player — a gliding ship that close means dormant cheap-glide
//!       kinematics leaked into a surface the player can PERCEIVE (the bug). R103
//!       raises this radius from the old 600u (which covered only the ~375u main
//!       view) to 750u = the client's 700u `radar::RADAR_RANGE` + 50u handoff
//!       margin, because the WIDEST perceivable surface is the radar, not the
//!       main view — gliding ships at 600–700u still slid as radar blips, and
//!   (b) per-tick |Δv| ≤ `MAX_TICK_DV` for every non-gliding ship within
//!       `RADAR_VISIBLE_RADIUS` (the R98 thrust-bounded-motion invariant, now
//!       measured against a MOVING viewer) — so the glide→physics handoff at the
//!       750u floor is invisible (settled physics before the 700u radar).
//!
//! It runs the sweep under TWO `AiTuning` regimes:
//!   - `AiTuning::default()` (`aoi_radius_mid = 520`) — the headless/golden
//!     pinned tuning, and
//!   - a SMALL-radius tuning (`aoi_radius_mid = 150`, `aoi_radius_active = 60`)
//!     — the suspected live dev-panel regime (the "AOI radius Mid" slider goes
//!     down to 20), where a dormant glide collapses well inside the camera's
//!     view band.
//!
//! Pre-R102 the small-radius regime tripped (a) within a few hundred ticks the
//! first time a role-driven Blue patrol fighter glided inside 600 u of the flying
//! player; pre-R103 a ship gliding at 600–700u still slid on the radar. Post-fix
//! (the `glide_min_radius` floor raised to 750u, decoupling the Dormant/glide
//! cutoff from the tunable tier radius), neither regime trips at the radar radius.
//!
//! R103 also adds `faction_swap_midflight_does_not_mass_glide`: mid-flight it
//! RE-INSERTS the player's `Faction`+`Position` (mirroring the dev panel's Team
//! button) and briefly removes/re-adds the `PlayerShip` marker (the transient
//! empty-player state), asserting that transient never mass-collapses squads into
//! a near-player glide (the latent mass-glide hardening).

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

/// R103 — the PERCEIVABLE-space radius (world units): the band around the player
/// a glide must never enter and inside which physics continuity is asserted. 750
/// = the client's 700u `radar::RADAR_RANGE` (the WIDEST surface the player can
/// perceive — wider than the ~375u main view R102's 600u covered) + a 50u
/// glide→physics handoff margin, so a ship is settled physics before it's
/// perceptible on the radar. This is the sim-side mirror of
/// `AiTuning::glide_min_radius` (also 750). Keep in sync if `RADAR_RANGE` changes.
const RADAR_VISIBLE_RADIUS: f32 = 750.0;

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

/// Script the moving player's position this tick: a back-and-forth sweep in X
/// across the arena half-span, with a deterministic Y oscillation that SWEEPS
/// THROUGH the patrol formation band instead of riding its y = 150 edge.
///
/// R103 — the old path held y = 150 exactly, which is the LOWER edge of the
/// Blue role patrol's route (waypoints at y ∈ [150, 250], formation offsets
/// pushing members up to ~250+); a glide collapsing on the far (north) side of
/// the band could sit just outside the old 600u check while still inside the
/// 700u radar. Oscillating Y across [0, 300] drives the player straight THROUGH
/// the formation band (and the y = 0 transport lane) from both sides, so a
/// gliding member anywhere in the band passes within the 750u radar radius and
/// is checked. The Y wave is a pure (deterministic) function of tick — a slow
/// triangle wave whose period is coprime-ish with the X period so the (x, y)
/// path traces a dense lattice across the arena rather than a single line.
fn player_path(tick: u64, dt: f32) -> Vec2 {
    // A triangle wave in x over the arena half-span.
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
    // A SLOWER triangle wave in y over [0, 300], sweeping through the patrol
    // formation band (route y ∈ [150, 250]) and the y = 0 lane from both edges.
    // Period chosen ≠ the X period so the path covers a 2-D lattice, not a line.
    let y_span = 300.0_f32; // [0, 300]
    let y_period = 1700.0_f32; // distinct from `period` (4400) → non-repeating sweep
    let y_phase = dist % y_period;
    let y = if y_phase <= 0.5 * y_period {
        y_phase / (0.5 * y_period) * y_span // 0 → 300
    } else {
        y_span - (y_phase - 0.5 * y_period) / (0.5 * y_period) * y_span // 300 → 0
    };
    Vec2::new(x, y)
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
            if d > RADAR_VISIBLE_RADIUS {
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
            if d <= RADAR_VISIBLE_RADIUS && out.glide_leak.is_none() {
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
        "default: no AI ship ever entered the {RADAR_VISIBLE_RADIUS} u play space in {TICKS} \
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
        "small-radius: no AI ship ever entered the {RADAR_VISIBLE_RADIUS} u play space in {TICKS} \
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

/// R103 Task 3 — the FACTION-SWAP mass-glide regression. The windowed client's
/// dev-panel "Team" button re-factions the player ship in place
/// (`client/net.rs`: `entity.insert(Faction::X); entity.insert(Position(...))`),
/// and a respawn/handshake re-attaches the `PlayerShip` AOI-anchor marker. If the
/// player set goes momentarily EMPTY during such a swap — or the classifier mass-
/// demotes everything to Dormant the instant the marker is gone — the whole fleet
/// could collapse to the no-physics glide right where the re-appearing player
/// then perceives it.
///
/// This test flies the standard sweep, and partway through RE-INSERTS the
/// player's `Faction` + `Position` (the Team-button swap Red→Blue→Red, a teleport
/// to the other spawn — exactly `net.rs`'s `insert(Faction); insert(Position)`).
/// It asserts NO `Gliding` ship is ever within `RADAR_VISIBLE_RADIUS` of the
/// player on ANY tick — INCLUDING the swap tick, where the player teleports onto
/// a neighborhood whose squads were gliding while it was in the other half: the
/// `glide_motion_system` floor-breach must EXPAND them the SAME tick the anchor
/// arrives, never leaving a one-tick near-player glide. It does NOT assert |Δv|
/// (the swap teleports the player by design).
///
/// The `PlayerShip` AOI-anchor marker is NOT removed across the swap: the real
/// client (`net.rs`) only ever re-INSERTS it (idempotent), never drops it, so the
/// player set never empties on a swap. A true one-tick marker removal would
/// create a window with NO anchor to expand an already-gliding squad against
/// (expansion is by definition impossible with zero players) — an unrealistic
/// state the client never produces. The empty-player COLLAPSE hardening (Task 2b,
/// `glide_collapse_system` skips collapse on an empty set) is what protects a
/// genuine transient; it is unit-tested in `sim::ai::lod`. Here we exercise the
/// realistic swap and prove the teleport-onto-glide expands cleanly.
#[test]
fn faction_swap_midflight_does_not_mass_glide() {
    let (mut server, _t) = ServerApp::loopback();
    server.spawn_scenario(Scenario::MiningSkirmish);

    let spawns = *server
        .world()
        .get_resource::<FactionSpawns>()
        .expect("MiningSkirmish inserts FactionSpawns");
    let red_spawn = spawns.red;
    let player = server.spawn_fitted_ship(red_spawn, 0.0, Faction::Red);
    server.world_mut().entity_mut(player).insert(PlayerShip);
    if let Some(mut intent) = server.world_mut().get_mut::<ShipIntent>(player) {
        intent.fire_primary = false;
    }

    let dt = server.world().resource::<FixedDt>().0;
    let mut prev_player = red_spawn;

    // Swap the player's faction at three mid-flight beats (Red→Blue→Red→Blue),
    // each a re-faction + teleport to that side's spawn (the Team-button path).
    const SWAP_TICKS: [u64; 3] = [800, 1500, 2200];
    let mut first_leak: Option<(u64, f32, Faction)> = None;
    let mut swapped_faction = Faction::Red;

    for tick in 1..=TICKS {
        // A real faction swap at the scripted beats: re-faction + re-position the
        // player (mirror `net.rs`'s `insert(Faction); insert(Position)`), a
        // teleport onto the other spawn's neighborhood — where squads may already
        // be gliding (the player was in the far half). The `PlayerShip` marker is
        // left intact (the client never drops it), so the floor-breach must expand
        // those squads the SAME tick the anchor lands.
        let swapping = SWAP_TICKS.contains(&tick);
        if swapping {
            swapped_faction = match swapped_faction {
                Faction::Red => Faction::Blue,
                Faction::Blue => Faction::Red,
            };
            let spawn = spawns.for_faction(swapped_faction);
            let world = server.world_mut();
            let mut e = world.entity_mut(player);
            e.insert(swapped_faction);
            e.insert(Position(spawn));
            prev_player = spawn;
        } else {
            // Normal scripted flight along the sweep (the path is symmetric, so
            // reuse it directly for whichever half the player is currently in).
            let target = player_path(tick, dt);
            let world = server.world_mut();
            if let Some(mut p) = world.get_mut::<Position>(player) {
                p.0 = target;
            }
            let v = (target - prev_player) / dt;
            if let Some(mut vel) = world.get_mut::<Velocity>(player) {
                vel.0 = v;
            }
            prev_player = target;
        }

        server.tick();

        // Assert NO gliding ship is within the radar radius of the player on ANY
        // tick — including the transient empty-player tick and the ticks right
        // after a swap teleports the anchor into a fresh neighborhood.
        let world = server.world_mut();
        let player_pos = world
            .get::<Position>(player)
            .expect("player ship persists")
            .0;
        let mut glide_q = world.query_filtered::<(Entity, &Position), With<Gliding>>();
        for (_entity, pos) in glide_q.iter(world) {
            let d = (pos.0 - player_pos).length();
            if d <= RADAR_VISIBLE_RADIUS && first_leak.is_none() {
                first_leak = Some((tick, d, swapped_faction));
            }
        }
    }

    // Non-vacuity: the AI actually ran (brains thought) so the assertion was not
    // trivially satisfied by a dead world.
    let world = server.world_mut();
    let mut brains = world.query::<&AiBrain>();
    let total_thinks: u64 = brains.iter(world).map(|b| b.thinks_total).sum();
    assert!(
        total_thinks > 0,
        "faction-swap: no AiBrain ever thought — the AI never ran (vacuous)"
    );
    assert!(
        first_leak.is_none(),
        "faction-swap: a GLIDING ship leaked within {RADAR_VISIBLE_RADIUS} u of the player at \
         {first_leak:?} (tick, distance, faction-after-swap) — a transient empty-player state \
         (Team-button re-faction / marker drop) mass-collapsed squads into a near-player glide",
    );
}
