//! Regression guard for the E007 live-demo wiring: the fitted enemies spawned for
//! the windowed client must actually surface in `render_state` (what the client
//! draws from) AND render as a **distinct `Ship`** — not as a practice-dummy cube
//! indistinguishable from `spawn_demo_world`'s targets.

use glam::Vec2;
use protocol::EntityKind;
use server::ServerApp;

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
}

/// After a fitted enemy is destroyed, the death-strip removes its `Target`/`FitLayout`
/// so `render_state` no longer emits it as a pristine `Ship` — it renders as drifting
/// debris (a `Target`+`Asteroid` wreck) instead, and the enemy is no longer a live
/// damageable target (so the repeated-"KILL" loop ends). This is the server-side proof
/// of the visible, clean death (E007, Deliverable 1).
#[test]
fn destroyed_fitted_enemy_renders_as_debris_not_a_pristine_ship() {
    use sim::components::TargetKind;
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

    // Post-death: it is no longer a Target / has no FitLayout (cannot be re-killed),
    // and it now renders as drifting debris (a Target+Asteroid wreck), not a Ship.
    use sim::components::Target;
    use sim::fitting::FitLayout;
    assert!(
        server.world().get::<Target>(enemy).is_none(),
        "the destroyed enemy is no longer a Target (no repeated KILL)"
    );
    assert!(
        server.world().get::<FitLayout>(enemy).is_none(),
        "the destroyed enemy lost its FitLayout (no longer a pristine ship)"
    );

    let after = server.render_state();
    // The destroyed enemy (its body persists) now reads as debris, and severed chunks
    // drift around it.
    let debris: Vec<_> = after
        .iter()
        .filter(|e| {
            e.kind == EntityKind::Target
                && TargetKind::from_u8(e.flags) == Some(TargetKind::Asteroid)
        })
        .collect();
    assert!(
        !debris.is_empty(),
        "the destroyed enemy + its severed chunks render as drifting Asteroid debris"
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
