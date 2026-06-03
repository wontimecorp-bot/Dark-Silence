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

    // The plain practice dummies are still Targets (not turned into ships).
    let plain_targets = rs.iter().filter(|e| e.kind == EntityKind::Target).count();
    assert!(
        plain_targets >= 5,
        "the 5 spawn_demo_world targets still render as Targets (got {plain_targets})"
    );
}
