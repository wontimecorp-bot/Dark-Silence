//! Regression guard for the E007 live-demo wiring: the fitted enemies spawned for
//! the windowed client must actually surface in `render_state` (what the client
//! draws from) AND render as a **distinct `Ship`** — not as a practice-dummy cube
//! indistinguishable from `spawn_demo_world`'s targets.

use glam::Vec2;
use protocol::EntityKind;
use server::{Scenario, ServerApp};

/// Phase 0 lock: `spawn_scenario(Sandbox)` reproduces the original inline demo composition (the
/// `spawn_demo_world` targets + the two fitted enemies). The refactor that moved the windowed
/// client's spawn calls behind a `Scenario` dispatcher is behaviour-preserving.
#[test]
fn sandbox_scenario_reproduces_the_original_demo_composition() {
    let (mut server, _t) = ServerApp::loopback();
    server.spawn_scenario(Scenario::Sandbox);
    server.tick();
    let rs = server.render_state();

    // The 5 plain practice targets (2 dummies + 2 asteroids + 1 seeker) render as Targets.
    let targets = rs.iter().filter(|e| e.kind == EntityKind::Target).count();
    assert_eq!(
        targets, 5,
        "Sandbox keeps the 5 spawn_demo_world targets (got {targets})"
    );
    // The 2 fitted enemies render as Ships at their fixed positions.
    let ships: Vec<_> = rs.iter().filter(|e| e.kind == EntityKind::Ship).collect();
    assert_eq!(
        ships.len(),
        2,
        "Sandbox keeps the 2 fitted enemies as Ships (got {})",
        ships.len()
    );
    assert!(ships
        .iter()
        .any(|e| (e.pos - Vec2::new(14.0, 0.0)).length() < 0.6));
    assert!(ships
        .iter()
        .any(|e| (e.pos - Vec2::new(18.0, 6.0)).length() < 0.6));
}

/// Phase 1 lock: `spawn_scenario(MiningSkirmish)` builds the static arena — the central asteroid
/// mine node + each faction's outpost + transport, all `Target`s with the right sub-kind flags.
#[test]
fn mining_skirmish_scenario_spawns_the_static_arena() {
    use sim::components::TargetKind;

    let (mut server, _t) = ServerApp::loopback();
    server.spawn_scenario(Scenario::MiningSkirmish);
    server.tick();
    let rs = server.render_state();

    let count = |k: TargetKind| {
        rs.iter()
            .filter(|e| e.kind == EntityKind::Target && e.flags == k.as_u8())
            .count()
    };
    assert_eq!(count(TargetKind::MineNode), 1, "one central mine node");
    assert_eq!(
        count(TargetKind::Outpost),
        2,
        "two refinery outposts (Red + Blue)"
    );
    assert_eq!(
        count(TargetKind::Transport),
        2,
        "two mining transports (Red + Blue)"
    );
    // No leftover demo targets bleed into the skirmish arena.
    assert_eq!(
        count(TargetKind::Dummy),
        0,
        "no demo dummies in the skirmish"
    );
}

#[test]
fn fitted_enemies_render_as_distinct_ships() {
    let (mut server, _t) = ServerApp::loopback();
    server.spawn_demo_world();
    server.spawn_fitted_enemy(Vec2::new(14.0, 0.0));
    server.spawn_fitted_enemy(Vec2::new(18.0, 6.0));
    // Mirror the client: step once before the first render read.
    server.tick();

    let rs = server.render_state();

    // Both enemies are present...
    let e14 = rs
        .iter()
        .find(|e| (e.pos - Vec2::new(14.0, 0.0)).length() < 0.6)
        .expect("enemy at (14,0) missing from render_state");
    let e18 = rs
        .iter()
        .find(|e| (e.pos - Vec2::new(18.0, 6.0)).length() < 0.6)
        .expect("enemy at (18,6) missing from render_state");

    // ...and render as Ship (distinct from the practice-dummy cubes), so the player
    // can see them and know what to shoot.
    assert_eq!(
        e14.kind,
        EntityKind::Ship,
        "fitted enemy must render as a Ship"
    );
    assert_eq!(
        e18.kind,
        EntityKind::Ship,
        "fitted enemy must render as a Ship"
    );

    // A LIVING fitted enemy carries a charged shield, so the client draws a shield
    // bubble around it (E007 live-demo, Deliverable 2).
    assert!(
        e14.shield_frac > 0.0 && e18.shield_frac > 0.0,
        "a living fitted enemy reports shield_frac > 0 (got {} / {})",
        e14.shield_frac,
        e18.shield_frac
    );

    // The plain practice dummies are still Targets (not turned into ships) and carry
    // NO shield bubble (shield_frac == 0).
    let plain: Vec<_> = rs.iter().filter(|e| e.kind == EntityKind::Target).collect();
    assert!(
        plain.len() >= 5,
        "the 5 spawn_demo_world targets still render as Targets (got {})",
        plain.len()
    );
    assert!(
        plain.iter().all(|e| e.shield_frac == 0.0),
        "plain practice targets carry no shield bubble (shield_frac == 0)"
    );

    // Phase 1B (client-only voxel payload): a living fitted enemy carries a NON-EMPTY
    // per-cell payload (the revise-A dense fighter silhouette — 51 cells on a 9×11) so
    // the client can render it as a colored cell-grid body, with the fighter's grid_dims.
    // A plain practice target carries no cells (it stays a single cube).
    assert!(
        e14.cells.len() == 51 && e14.grid_dims == (9, 11),
        "a fitted enemy carries its dense fighter cell-grid (got {} cells, dims {:?})",
        e14.cells.len(),
        e14.grid_dims
    );
    assert!(
        !e18.cells.is_empty(),
        "a fitted enemy carries a non-empty voxel cell payload"
    );
    // The payload encodes module kinds: the fighter fit (reactor/thruster/weapon/armor)
    // plus structural plating, so it carries both structural (kind 0) and module (kind
    // 1..=6) cells.
    assert!(
        e14.cells.iter().any(|c| c.kind == 0),
        "the fitted enemy has structural (hull-tint) cells"
    );
    assert!(
        e14.cells.iter().any(|c| (1..=6).contains(&c.kind)),
        "the fitted enemy has module-colored cells"
    );
    assert!(
        plain
            .iter()
            .all(|e| e.cells.is_empty() && e.grid_dims == (0, 0)),
        "plain practice targets carry no voxel cell payload"
    );
}

/// After a fitted enemy is destroyed, the death-strip removes its `Target`/`CollisionRadius`
/// and tags it `Wreck` so `render_state` no longer emits it as a pristine `Ship` — it
/// renders as drifting wreckage (an `EntityKind::Debris` entity). The hulk now KEEPS its
/// residual `FitLayout`, so its Debris entry carries a real cell payload (it renders as the
/// remaining carved cells, not a generic box); the enemy is no longer a live damageable
/// target (the `Wreck` tag excludes it from `fitted_damage_system`), so the repeated-"KILL"
/// loop ends. This is the server-side proof of the visible, clean death (E007, Deliverable 1).
#[test]
fn destroyed_fitted_enemy_renders_as_debris_not_a_pristine_ship() {
    use sim::damage::shatter_ship;

    let (mut server, _t) = ServerApp::loopback();
    let enemy = server.spawn_fitted_enemy(Vec2::new(14.0, 0.0));
    server.tick();

    // Pre-death: it renders as a Ship.
    let before = server.render_state();
    let ship_before = before
        .iter()
        .find(|e| (e.pos - Vec2::new(14.0, 0.0)).length() < 0.6)
        .expect("enemy present before death");
    assert_eq!(
        ship_before.kind,
        EntityKind::Ship,
        "a living fitted enemy renders as a Ship"
    );

    // Shatter it directly (the live-combat death the hull-depletion trigger drives).
    shatter_ship(server.world_mut(), enemy);

    // Post-death: it is no longer a Target (cannot be re-killed) but KEEPS its residual
    // FitLayout, so the hulk renders as its real carved cells (not a box). It now renders
    // as drifting Debris, not a Ship.
    use sim::components::Target;
    use sim::fitting::FitLayout;
    assert!(
        server.world().get::<Target>(enemy).is_none(),
        "the destroyed enemy is no longer a Target (no repeated KILL)"
    );
    assert!(
        server.world().get::<FitLayout>(enemy).is_some(),
        "the destroyed enemy KEEPS its residual FitLayout so the hulk renders as its real \
         carved cells (the `Wreck` tag, not FitLayout removal, ends re-carving)"
    );

    let after = server.render_state();
    // The destroyed enemy (its body persists) now reads as wreckage, and severed chunks
    // drift around it — all as `EntityKind::Debris`, NOT grey `Asteroid` spheres.
    let debris: Vec<_> = after
        .iter()
        .filter(|e| e.kind == EntityKind::Debris)
        .collect();
    assert!(
        !debris.is_empty(),
        "the destroyed enemy + its severed chunks render as drifting ship-fragment Debris"
    );
    // The severed-chunk render fix: a Debris entity that carries a residual FitLayout now
    // emits its REAL cells (so the client draws it as a hull mesh of the exact cells that
    // broke off, not a generic box). At least one debris piece here (the hulk and/or the
    // severed chunks) therefore carries a non-empty cell payload with real grid_dims.
    assert!(
        debris
            .iter()
            .any(|e| !e.cells.is_empty() && e.grid_dims != (0, 0)),
        "wreckage with a residual FitLayout emits its real severed cells (so it renders as \
         its actual shape, not a placeholder box)"
    );
    // The size hint (residual cell-count) rides in `flags` and is always ≥ 1 so the
    // client never scales a layout-less fragment to zero.
    assert!(
        debris.iter().all(|e| e.flags >= 1),
        "each debris chunk carries a non-zero cell-count size hint in flags"
    );
    // No destroyed-ship debris leaks onto the path as a grey Asteroid target.
    assert!(
        !after.iter().any(|e| e.kind == EntityKind::Target
            && sim::components::TargetKind::from_u8(e.flags)
                == Some(sim::components::TargetKind::Asteroid)),
        "destroyed-ship wreckage no longer renders as grey Asteroid spheres"
    );
    // No fitted enemy still renders as a Ship near (14,0) — it became a wreck.
    let still_ship = after
        .iter()
        .any(|e| e.kind == EntityKind::Ship && (e.pos - Vec2::new(14.0, 0.0)).length() < 0.6);
    assert!(
        !still_ship,
        "the destroyed enemy no longer renders as a pristine Ship"
    );
}
