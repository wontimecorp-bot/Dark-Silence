//! R99 Phase C — the faction auto-join load-balancer counts HUMANS, not AI.
//!
//! `ServerApp::assign_faction` puts a joining human on the smaller team. Before
//! the fix it counted EVERY `Faction`-tagged ship including AI fighters, so the
//! authored AI fleet composition decided the human's side: an R98 edit that
//! added 2 Red AI escorts (Red 7 vs Blue 5) flipped the solo auto-join from Red
//! (on the intended 0-0 human tie) to Blue. The balancer must only count
//! `PlayerShip`-marked ships (the human AOI anchor), leaving AI out.

use bevy_ecs::entity::Entity;
use server::ServerApp;
use sim::ai::PlayerShip;
use sim::components::Faction;

/// AI ships — even heavily skewed to one side — must NEVER move the human's
/// auto-join: with 0 humans on each side it is always a tie → Red. Then once a
/// human Red ship exists, the next join balances onto Blue (humans 1 Red vs 0
/// Blue), proving the count is over `PlayerShip` and not `Faction` alone.
#[test]
fn assign_faction_counts_humans_not_ai() {
    let (mut server, _t) = ServerApp::loopback();

    // Heavily skew the AI fleet toward Red (AI-only: no `PlayerShip` marker).
    // The old all-`Faction` count would push the human to Blue here.
    let red_ai: [Entity; 5] = std::array::from_fn(|i| {
        server.spawn_fitted_ship(glam::Vec2::new(i as f32, 0.0), 0.0, Faction::Red)
    });
    let _blue_ai: [Entity; 1] = std::array::from_fn(|i| {
        server.spawn_fitted_ship(glam::Vec2::new(i as f32, 10.0), 0.0, Faction::Blue)
    });
    // Sanity: these are AI ships (no `PlayerShip`), so they must not count.
    for e in red_ai {
        assert!(
            server.world().get::<PlayerShip>(e).is_none(),
            "AI ship should carry no PlayerShip marker"
        );
    }

    // 0 humans on each side → tie → Red, regardless of the 5-vs-1 AI skew.
    assert_eq!(
        server.assign_faction(),
        Faction::Red,
        "solo auto-join must be Red on a 0-0 human tie, ignoring AI fleet size"
    );

    // Now a HUMAN joins Red (mirror client/net.rs: faction + PlayerShip marker).
    let human = server.spawn_fitted_ship(glam::Vec2::new(0.0, -10.0), 0.0, Faction::Red);
    server.world_mut().entity_mut(human).insert(PlayerShip);

    // Humans are now 1 Red vs 0 Blue → the next join balances onto Blue, even
    // though the Red AI fleet still outnumbers Blue overall.
    assert_eq!(
        server.assign_faction(),
        Faction::Blue,
        "with 1 human on Red and 0 on Blue, the next human auto-joins Blue"
    );
}
