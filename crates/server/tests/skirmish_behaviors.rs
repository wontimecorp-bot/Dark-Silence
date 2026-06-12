//! R102 Part B — the headless SCENARIO-LEVEL proof that the new `Disposition`
//! personalities actually change AI behavior in the real server world.
//!
//! Each test builds a focused, deterministic world (a hand-built 2-ship world at
//! the server level where the full skirmish is overkill, or the whole
//! `MiningSkirmish` for the integration smoke), scripts a player / drives a tick
//! loop, and asserts a property the disposition is supposed to produce:
//!
//! - [`hunter_engages_a_passing_player`] — a `hunter()` ACQUIRES + engages a
//!   passing hostile (the "enemies now react" proof).
//! - [`sentry_ignores_a_passing_player_until_fired_upon`] — a `sentry()` ignores
//!   a passer until fired upon, then engages (the defensive-guard proof).
//! - [`low_tenacity_guard_drops_stale_target_and_resumes_patrol`] — a low-tenacity
//!   guard drops a target that left sensor range and resumes its route (the
//!   "idle in Engage" fix).
//! - [`leashed_sentry_breaks_off_and_returns`] — a short-leash `sentry()` clears a
//!   target it has been lured past its leash from `home`, while a long-leash
//!   `hunter()` in the same spot keeps chasing (the leash proof).
//! - [`brawler_kills_stationary_outpost`] — a brawler (hunter disposition) destroys
//!   a fully stationary fitted target within budget (the S5 aim-at-core proof in a
//!   real scenario world).
//! - [`skirmish_runs_clean_with_dispositions`] — the whole authored `MiningSkirmish`
//!   plus a player runs ~2000 ticks without a panic and the disposition'd ships
//!   behave (the authoring-didn't-break-the-scenario smoke).
//!
//! Determinism: every world is built from fixed inputs (no RNG); the AI reads the
//! pinned `AiTuning`. The hand-built worlds WIDEN the AOI radii so the whole
//! engagement stays Active-tier around the player marker (pure scenario tuning, no
//! logic change) — the behavior under test is the disposition, not the LOD band.

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::With;
use glam::Vec2;
use server::{Scenario, ServerApp};
use sim::ai::{
    AiBrain, AiStableId, AiTuning, AoiTier, Behavior, Disposition, PlayerShip, Posture, RoleGoal,
    ScenarioRole, Tier, FIRED_UPON_WINDOW_TICKS,
};
use sim::components::{
    AngularVelocity, Faction, FlightAssist, Heading, Position, Projectile, Ship, Velocity, Weapon,
};
use sim::damage::Wreck;
use sim::fitting::{
    build_layout, derive_ship_stats, seed_catalogs, Fit, FitLayout, ShipStats, SlotId,
    HULL_FIGHTER, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
};
use sim::{FactionSpawns, FixedDt, RefinedResources, ScenarioActive, ShipIntent, SimTuning};

/// Prepare a BARE server world for a hand-built AI behavior test: insert the
/// gated-AI prerequisites (`ScenarioActive` + the `RefinedResources` the gated
/// mining system reads) and WIDEN the AOI radii so every ship near the player is
/// Active-tier (thinks/scans ≈ every think) — so the test measures the
/// disposition, not the LOD band. `cell_hp` sets `SimTuning::struct_cell_hp` (use
/// a low value to make the brawler-kill land inside budget; the default is fine
/// otherwise). No scenario arena is spawned — the caller hand-builds the ships.
fn behavior_server(cell_hp: Option<f32>) -> ServerApp {
    let (mut server, _t) = ServerApp::loopback();
    // The gated AI set runs only with `ScenarioActive`; `mining_transport_system`
    // (also `ScenarioActive`-gated) reads `RefinedResources`, so both must exist.
    server.world_mut().insert_resource(ScenarioActive);
    server
        .world_mut()
        .insert_resource(RefinedResources::default());
    // Keep the WHOLE bubble Active so cadence/LOD never gates the proof; all other
    // AI knobs (sensor range, leash/grace bases) stay at their pinned defaults.
    server.world_mut().insert_resource(AiTuning {
        aoi_radius_active: 10_000.0,
        aoi_radius_mid: 20_000.0,
        ..AiTuning::default()
    });
    if let Some(hp) = cell_hp {
        server.world_mut().insert_resource(SimTuning {
            struct_cell_hp: hp,
            ..SimTuning::default()
        });
    }
    // Leak the loopback transport for the test's lifetime (the loop never drives
    // a client; only `server.tick()` advances the world).
    std::mem::forget(_t);
    server
}

/// Spawn a dispositioned AI enemy fighter (no scenario role) at `pos`, facing
/// `heading`, on `faction`. Uses the public `spawn_test_enemy` API — the same
/// armed fitted `Ship` + full AI stack + the chosen `Disposition` the dev panel's
/// "spawn test enemy" button uses.
fn spawn_enemy(
    server: &mut ServerApp,
    pos: Vec2,
    heading: f32,
    faction: Faction,
    d: Disposition,
) -> Entity {
    server.spawn_test_enemy(pos, heading, faction, d)
}

/// Spawn the scripted human: a fitted `Ship` at `pos` on `faction`, marked
/// `PlayerShip` (the AOI anchor) with its trigger held (it observes, never fires
/// unless the test arms it). Returns the entity.
fn spawn_player(server: &mut ServerApp, pos: Vec2, faction: Faction) -> Entity {
    let player = server.spawn_fitted_ship(pos, 0.0, faction);
    server.world_mut().entity_mut(player).insert(PlayerShip);
    if let Some(mut intent) = server.world_mut().get_mut::<ShipIntent>(player) {
        intent.fire_primary = false;
    }
    player
}

/// A short-range BRAWLER shooter's `ShipStats` (mirrors
/// `sim/tests/ai.rs::brawler_shooter_stats`): the real starter-fighter derivation
/// with two scenario pins — a tight ≈120 u weapon envelope (so heading-aligned fire
/// reliably lands on the target's hull at close range) and `armor_value = 200`, so
/// `classify_archetype` reads it as a BRAWLER (the close-standoff archetype whose
/// Charge stance WEAVES — the rake that disconnects the core). The shooter carries
/// NO `Fit`/`FitLayout`, so `recompute_ship_stats_system` never overwrites these pins.
fn brawler_shooter_stats() -> ShipStats {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(2), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
    let layout = build_layout(hull, &fit, &modules);
    let mut stats = derive_ship_stats(hull, &fit, &modules, &layout);
    let mut weapon = stats.weapon.expect("autocannon fitted");
    weapon.lifetime = 120.0 / weapon.muzzle_speed; // ≈120 u reach.
    stats.weapon = Some(weapon);
    stats.armor_value = 200.0; // armed + tanky → Brawler archetype.
    stats
}

/// Spawn a hand-built ARMED brawler shooter at the server level (mirrors
/// `sim/tests/ai.rs::spawn_armed_ai_fighter`): a fitted-less `Ship` whose real fire
/// path (`weapon_fire_system` — `ShipStats.weapon` + the `Weapon` cooldown) is
/// driven ONLY by the intent its brain emits (`ai_execute_system`'s `fire_decision`).
/// Pre-targeted at `target`, Active-tier, carrying the supplied `Disposition`.
fn spawn_brawler(
    server: &mut ServerApp,
    id: u64,
    pos: Vec2,
    target: Entity,
    d: Disposition,
) -> Entity {
    let stats = brawler_shooter_stats();
    let world = server.world_mut();
    let tpos = world.get::<Position>(target).map_or(Vec2::ZERO, |p| p.0);
    let heading = (tpos - pos).to_angle();
    let weapon = Weapon {
        cooldown: 0.0,
        fire_rate: stats.weapon.map(|p| p.fire_rate).unwrap_or(5.0),
        muzzle_speed: stats.weapon.map(|p| p.muzzle_speed).unwrap_or(200.0),
        spool: 1.0,
        shot_counter: 0,
    };
    world
        .spawn((
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
            d,
        ))
        .id()
}

/// Spawn a WEAK, fully-STATIONARY fitted target at `pos` (mirrors
/// `sim/tests/ai.rs::spawn_weak_fitted_target`): an EMPTY-fit fighter hull, so every
/// cell — including the deepest `core_cell` — is a weak (1-HP, via the world's
/// `struct_cell_hp`) STRUCTURAL cell and the carve-to-core kill lands well inside
/// the budget (a FULLY-fitted ship would keep TOUGH module/armor cells that
/// `struct_cell_hp` does not weaken — the carve would stall on the module blob).
/// Shields stripped so the kill is about the CARVE, not the pool. Marked
/// `PlayerShip` so the Active bubble anchors on it.
fn spawn_weak_target(server: &mut ServerApp, pos: Vec2) -> Entity {
    use sim::components::{CollisionRadius, Destructible, Target, TargetKind};
    use sim::damage::seed_defense_layers;
    use sim::fitting::hull_collision_radius;
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
    let fit = Fit::new(HULL_FIGHTER); // EMPTY → every cell is a weak struct cell.
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    let (mut shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);
    shields.current = 0.0; // strip the shield: the kill is the carve.
    server
        .world_mut()
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            CollisionRadius(hull_collision_radius(hull.grid_dims)),
            Destructible,
            PlayerShip,
            fit,
            layout,
            stats,
            shields,
            section_armor,
            hull_structure,
        ))
        .id()
}

/// Read a ship's brain (panics if absent — every AI ship carries one).
fn brain(server: &ServerApp, e: Entity) -> AiBrain {
    *server
        .world()
        .get::<AiBrain>(e)
        .expect("AI ship carries an AiBrain")
}

/// Pin a ship's authoritative Position/Velocity for this tick (the test owns this
/// body's motion — e.g. the scripted player, or a held-in-place lure target).
fn pin(server: &mut ServerApp, e: Entity, pos: Vec2, vel: Vec2) {
    let world = server.world_mut();
    if let Some(mut p) = world.get_mut::<Position>(e) {
        p.0 = pos;
    }
    if let Some(mut v) = world.get_mut::<Velocity>(e) {
        v.0 = vel;
    }
}

/// A `hunter()` enemy REACTS to a hostile that merely flies past within sensor
/// range WITHOUT firing: its `FreeEngage` effective posture opens the acquisition
/// gate on sight, so the perception scan sets `brain.target = player`, the brain
/// selects Engage, and the hunter opens FIRE on the passer. The "enemies now react"
/// proof. (A `sentry()` in the same flyby would ignore the passer — proven below.)
#[test]
fn hunter_engages_a_passing_player() {
    let mut server = behavior_server(None);
    // The hunter sits at the origin, on Blue; the human is Red (hostile).
    let hunter = spawn_enemy(
        &mut server,
        Vec2::ZERO,
        0.0,
        Faction::Blue,
        Disposition::hunter(),
    );
    // The player flies IN from 260 to 100 u and HOLDS — passing through sensor
    // range (< 200) and never firing (its trigger is held).
    let player = spawn_player(&mut server, Vec2::new(260.0, 0.0), Faction::Red);

    let dt = server.world().resource::<FixedDt>().0;
    let approach_speed = 40.0; // u/s inbound — a plausible flyby, not a teleport.
    let hold_x = 100.0;

    let mut acquired_at = None;
    let mut engaged_at = None;
    let mut fired_at = None;
    for tick in 1..=400u64 {
        let x = (260.0 - approach_speed * dt * tick as f32).max(hold_x);
        let player_pos = Vec2::new(x, 0.0);
        let player_vel = if x > hold_x {
            Vec2::new(-approach_speed, 0.0)
        } else {
            Vec2::ZERO
        };
        pin(&mut server, player, player_pos, player_vel);
        server.tick();
        let b = brain(&server, hunter);
        if acquired_at.is_none() && b.target == Some(player) {
            acquired_at = Some(tick);
        }
        if acquired_at.is_some() && engaged_at.is_none() && b.behavior == Behavior::Engage {
            engaged_at = Some(tick);
        }
        // The hunter actually OPENED FIRE on the passer (a projectile exists).
        if fired_at.is_none() {
            let world = server.world_mut();
            let shots = world
                .query_filtered::<(), With<Projectile>>()
                .iter(world)
                .count();
            if shots > 0 {
                fired_at = Some(tick);
                break;
            }
        }
    }

    let acquired = acquired_at.expect(
        "the hunter must ACQUIRE the passing player (brain.target == player) — FreeEngage \
         acquisition gate should fire on sight",
    );
    let engaged = engaged_at.expect("the hunter must select Engage once it has the target");
    let fired =
        fired_at.expect("the hunter must OPEN FIRE on the acquired passer (enemies now react)");
    assert!(
        acquired <= engaged && engaged <= fired,
        "the reaction is ordered: acquire @{acquired} -> engage @{engaged} -> fire @{fired}",
    );
    eprintln!("[hunter] acquired @{acquired}, engaged @{engaged}, opened fire @{fired}");
}

/// A `sentry()` enemy IGNORES a hostile that merely passes within sensor range
/// (its `DefensiveOnly` effective posture keeps the acquisition gate shut), then
/// ENGAGES once it has been fired upon — modeled by arming its role's
/// `fired_upon_until` (the trigger pass does exactly this on a `DamageTaken`).
/// The defensive-guard proof.
#[test]
fn sentry_ignores_a_passing_player_until_fired_upon() {
    let mut server = behavior_server(None);
    let sentry = spawn_enemy(
        &mut server,
        Vec2::ZERO,
        0.0,
        Faction::Blue,
        Disposition::sentry(),
    );
    // A `DefensiveOnly` ship needs a ROLE to hold the fired-upon window. A
    // PatrolRoute role never self-acquires (only the gated perception scan does),
    // so acquisition is driven purely by the disposition's posture gate.
    server
        .world_mut()
        .entity_mut(sentry)
        .insert(ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![Vec2::ZERO, Vec2::new(0.0, 30.0)]),
            Posture::DefensiveOnly,
        ));
    let player = spawn_player(&mut server, Vec2::new(120.0, 0.0), Faction::Red);

    // PHASE 1 — the player loiters WELL within sensor range (120 < 200), not
    // firing, for long enough that several scans run. The sentry must NOT acquire.
    for tick in 1..=120u64 {
        pin(&mut server, player, Vec2::new(120.0, 0.0), Vec2::ZERO);
        server.tick();
        assert_eq!(
            brain(&server, sentry).target,
            None,
            "tick {tick}: a sentry must IGNORE a passing hostile until fired upon \
             (DefensiveOnly acquisition gate stays shut)",
        );
    }

    // PHASE 2 — the player "fires on it": arm the sentry role's fired-upon window
    // (what `role_trigger_system` does on a pending `DamageTaken`). Now the
    // DefensiveOnly gate opens and the next scan acquires + the brain engages.
    let now = server.world().resource::<sim::CurrentTick>().0;
    if let Some(mut role) = server.world_mut().get_mut::<ScenarioRole>(sentry) {
        role.fired_upon_until = now + FIRED_UPON_WINDOW_TICKS;
    }
    let mut acquired_at = None;
    for tick in 1..=60u64 {
        pin(&mut server, player, Vec2::new(120.0, 0.0), Vec2::ZERO);
        server.tick();
        if brain(&server, sentry).target == Some(player) {
            acquired_at = Some(tick);
            break;
        }
    }
    let acquired = acquired_at.expect(
        "once fired upon, the sentry's DefensiveOnly gate opens and it ACQUIRES the player",
    );
    eprintln!("[sentry] ignored 120 ticks, then acquired {acquired} ticks after being fired upon");
}

/// A low-tenacity guard (`sentry()`, tenacity 0.2) that has ACQUIRED a target
/// then loses sight of it (the target leaves sensor range) CLEARS the stale
/// target within its grace and RESUMES its role behavior (a patrol waypoint),
/// instead of sitting frozen in Engage on a ghost — the "idle in Engage" fix.
#[test]
fn low_tenacity_guard_drops_stale_target_and_resumes_patrol() {
    let mut server = behavior_server(None);
    let guard = spawn_enemy(
        &mut server,
        Vec2::ZERO,
        0.0,
        Faction::Blue,
        Disposition::sentry(),
    );
    // A DEFEND-zone role: it anchors `home`/`waypoint` to the post but, unlike a
    // PatrolRoute, it NEVER clears a lost target on its own (it holds an acquired
    // target until despawn — the "idle in Engage" trap). So the ONLY thing that
    // releases the stale target here is the disposition's tenacity grace. A small
    // zone radius keeps the role from re-acquiring the fled lure.
    let post = Vec2::ZERO;
    server
        .world_mut()
        .entity_mut(guard)
        .insert(ScenarioRole::new(
            RoleGoal::Defend {
                anchor: post,
                radius: 60.0,
            },
            Posture::FreeEngage,
        ));
    // The lure is a hostile fitted ship; the test directly hands the guard this
    // target + a fresh "seen" stamp (it had just acquired + engaged it). The lure
    // is the PlayerShip Active-anchor.
    let lure = spawn_player(&mut server, Vec2::new(40.0, 0.0), Faction::Red);
    let now = server.world().resource::<sim::CurrentTick>().0;
    {
        let mut b = server.world_mut().get_mut::<AiBrain>(guard).unwrap();
        b.target = Some(lure);
        b.target_seen_tick = now;
        b.behavior = Behavior::Engage;
    }

    // The lure FLEES far out of sensor range (and out of the guard's contact
    // list). Within the sentry's short grace (≈ 81 ticks) the disposition clears
    // the stale target; the Defend role then drives the guard back to its post.
    let mut dropped_at = None;
    for tick in 1..=200u64 {
        pin(&mut server, lure, Vec2::new(5000.0, 0.0), Vec2::ZERO);
        server.tick();
        if brain(&server, guard).target.is_none() {
            dropped_at = Some(tick);
            break;
        }
    }
    let dropped = dropped_at.expect(
        "a low-tenacity guard must DROP a target gone out of contact (within its grace) — \
         not stay frozen in Engage on a ghost",
    );
    // It RESUMED its task: a waypoint (the post) is set and it is no longer Engaging.
    let b = brain(&server, guard);
    assert_ne!(
        b.behavior,
        Behavior::Engage,
        "after dropping the stale target the guard must leave Engage"
    );
    assert!(
        b.waypoint.is_some(),
        "the guard resumes its post (a defend waypoint is restored)"
    );
    eprintln!("[low-tenacity] dropped the stale target @{dropped} ticks, resumed its post");
}

/// A short-leash `sentry()` lured PAST its leash from `home` CLEARS the target and
/// turns back, while a long-leash `hunter()` in the IDENTICAL setup KEEPS chasing
/// — the leash proof. Both ships are held at the same distance from the same home
/// with the same perceived target; only the disposition differs.
#[test]
fn leashed_sentry_breaks_off_and_returns() {
    let mut server = behavior_server(None);
    let home = Vec2::ZERO;
    // 150 u from home: PAST the sentry leash (≈ 83 u) but INSIDE the hunter leash
    // (≈ 288 u) at the pinned `disposition_leash_base = 300`.
    let chase_pos = Vec2::new(150.0, 0.0);
    let sentry = spawn_enemy(
        &mut server,
        chase_pos,
        0.0,
        Faction::Blue,
        Disposition::sentry(),
    );
    let hunter = spawn_enemy(
        &mut server,
        chase_pos,
        0.0,
        Faction::Blue,
        Disposition::hunter(),
    );
    // Both anchored to the same home (what a Defend role / HoldAt order would set).
    for e in [sentry, hunter] {
        server.world_mut().get_mut::<AiBrain>(e).unwrap().home = Some(home);
    }
    // A real hostile right by the chasers, pinned in their sensor range so it
    // stays in CONTACT — the brain re-stamps `target_seen_tick` every think, so the
    // tenacity grace never fires and the LEASH is the only release under test.
    let target = spawn_player(&mut server, Vec2::new(165.0, 0.0), Faction::Red);

    // Hand BOTH chasers the same target directly (a sentry's DefensiveOnly gate
    // would otherwise refuse to acquire — but a ship lured this far has already
    // been chasing). Fresh "seen" stamp so the grace path stays dormant.
    let now = server.world().resource::<sim::CurrentTick>().0;
    for e in [sentry, hunter] {
        let mut b = server.world_mut().get_mut::<AiBrain>(e).unwrap();
        b.target = Some(target);
        b.target_seen_tick = now;
    }

    // Hold them PAST the sentry leash for several thinks; re-pin home + the target
    // contact each tick. The short-leash sentry breaks off (target cleared once it
    // is past its ≈83 u leash from home); the long-leash hunter (≈288 u) holds it.
    let mut sentry_broke = None;
    for tick in 1..=80u64 {
        pin(&mut server, sentry, chase_pos, Vec2::ZERO);
        pin(&mut server, hunter, chase_pos, Vec2::ZERO);
        pin(&mut server, target, Vec2::new(165.0, 0.0), Vec2::ZERO);
        for e in [sentry, hunter] {
            server.world_mut().get_mut::<AiBrain>(e).unwrap().home = Some(home);
        }
        server.tick();
        if sentry_broke.is_none() && brain(&server, sentry).target.is_none() {
            sentry_broke = Some(tick);
        }
    }
    let broke = sentry_broke.expect(
        "the short-leash sentry must BREAK OFF (clear the target) once held past its leash \
         radius from home",
    );
    assert_eq!(
        brain(&server, hunter).target,
        Some(target),
        "the long-leash hunter, at the SAME distance from home, must KEEP chasing the target",
    );
    eprintln!("[leash] sentry broke off @{broke}; hunter still chasing at 150 u from home");
}

/// A brawler (hunter disposition — Charge stance, Rush pace) destroys a FULLY
/// STATIONARY fitted target within budget — the R101 S5 aim-at-core proof, now in
/// a real server scenario world. The target is a weak fitted enemy (1-HP cells,
/// shields stripped) so the carve-to-core kill lands inside the budget at the
/// brawler's engage range.
#[test]
fn brawler_kills_stationary_outpost() {
    const BUDGET: u64 = 3600;
    // Weak structural cells so the carve-to-core kill lands well inside budget. The
    // aggressive hunter weaves wider + occasionally Rams a near-dead hull (the
    // disposition's personality), so a soft per-cell HP keeps the kill comfortably
    // in budget — the proof is the AIM-AT-CORE death, not the carve rate.
    let mut server = behavior_server(Some(0.4));

    // The STATIONARY weak fitted target (the standin "outpost" core the brawler
    // must disconnect) — an empty-fit hull so every cell is a weak struct cell. It
    // is naturally stationary (zero velocity, no forces — no per-tick pin needed).
    let target = spawn_weak_target(&mut server, Vec2::new(150.0, 0.0));
    let cells_at_start = server
        .world()
        .get::<FitLayout>(target)
        .expect("the fitted target has a hull layout")
        .cells
        .len();
    assert!(cells_at_start > 0, "the target starts with hull cells");

    // The brawler: the proven S5 short-range BRAWLER shooter (armed + tanky →
    // Brawler archetype, ≈120 u reach), FREE TO MOVE so its own controller closes to
    // the brawler standoff ring and runs the on-band WEAVE that rakes the gun across
    // the hull and disconnects the core. It carries the hunter DISPOSITION (the
    // aggressive personality this scenario proof exercises — `FreeEngage` opens the
    // engage gate, Charge+Rush close in); the brain owns the aim (at the target's
    // `core_cell`) + the trigger. (The target was pre-locked at spawn — left to a
    // bare brain the per-tick `get_mut` re-pinning would churn change-detection and
    // throttle the carve, so the lock is set ONCE.)
    let brawler = spawn_brawler(&mut server, 0, Vec2::ZERO, target, Disposition::hunter());
    assert_eq!(
        brain(&server, brawler).target,
        Some(target),
        "the brawler starts already locked on its target",
    );

    let mut destroyed_at = None;
    let mut carved = false;
    for tick in 1..=BUDGET {
        server.tick();
        if let Some(layout) = server.world().get::<FitLayout>(target) {
            carved |= layout.cells.len() < cells_at_start;
        }
        if server.world().get::<Wreck>(target).is_some()
            || server.world().get_entity(target).is_err()
        {
            destroyed_at = Some(tick);
            break;
        }
    }

    let destroyed = destroyed_at.unwrap_or_else(|| {
        panic!(
            "the brawler (hunter disposition) must DESTROY the stationary target within {BUDGET} \
             ticks — cells {} of {cells_at_start} left",
            server
                .world()
                .get::<FitLayout>(target)
                .map_or(0, |l| l.cells.len()),
        )
    });
    assert!(carved, "the target's hull was carved before death");
    assert!(
        server.world().get::<Wreck>(target).is_some() || server.world().get_entity(target).is_err(),
        "the stationary target is destroyed (Wreck / gone)",
    );
    eprintln!("[brawler] killed the stationary target @{destroyed} ticks");
}

/// The whole authored `MiningSkirmish` (with the R102 disposition mix) + a player
/// runs ~2000 ticks without a panic, the AI thinks, and the dispositioned ships
/// are present + behaving (a fleeing/roaming/hunting personality is reachable) —
/// the sanity smoke that the disposition authoring didn't break the scenario.
#[test]
fn skirmish_runs_clean_with_dispositions() {
    let (mut server, _t) = ServerApp::loopback();
    server.spawn_scenario(Scenario::MiningSkirmish);

    // A player at the Red spawn (mirrors the windowed auto-join), observing.
    let red_spawn = server
        .world()
        .get_resource::<FactionSpawns>()
        .expect("MiningSkirmish inserts FactionSpawns")
        .red;
    let player = spawn_player(&mut server, red_spawn, Faction::Red);

    // The authoring attached dispositions to the escort/patrol/ambush groups.
    let dispo_count = {
        let world = server.world_mut();
        let mut q = world.query_filtered::<Entity, (With<Disposition>, With<Ship>)>();
        q.iter(world).count()
    };
    assert!(
        dispo_count >= 6,
        "the skirmish authoring must attach dispositions to several AI ships \
         (escort 2 + Blue patrol 3 + ambush 4 = 9 expected); found {dispo_count}",
    );

    for _ in 0..2000u64 {
        server.tick();
        // The player just observes from its spawn (no panic is the main assertion).
        let _ = server.world().get::<Position>(player);
    }

    // The AI actually ran, and the dispositioned ships are still present + sane.
    let world = server.world_mut();
    let total_thinks: u64 = {
        let mut q = world.query::<&AiBrain>();
        q.iter(world).map(|b| b.thinks_total).sum()
    };
    assert!(
        total_thinks > 0,
        "the skirmish AI never thought in 2000 ticks"
    );
    let dispo_alive = {
        let mut q = world.query_filtered::<Entity, (With<Disposition>, With<Ship>)>();
        q.iter(world).count()
    };
    assert!(
        dispo_alive > 0,
        "at least one dispositioned ship survives the smoke run",
    );
    eprintln!(
        "[skirmish smoke] {dispo_count} dispositioned ships spawned, {dispo_alive} alive after \
         2000 ticks; total AI thinks = {total_thinks}",
    );
}
