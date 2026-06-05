//! Scenario selection — which authoritative world [`ServerApp::spawn_scenario`] builds.
//!
//! [`Scenario::Sandbox`] reproduces the original demo composition byte-for-byte (the practice
//! dummies + drifting asteroids + seeker from [`ServerApp::spawn_demo_world`] plus the two fitted
//! E007 demo enemies); [`Scenario::MiningSkirmish`] is the 2-faction asteroid-mining game mode
//! (filled out in later phases). The windowed client picks one and calls `spawn_scenario` once,
//! before the handshake. The headless determinism / botkit / unit-test worlds NEVER call this, so
//! `spawn_demo_world` and the exact entity sets those tests assert are untouched.

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::With;
use glam::Vec2;
use sim::components::{
    CollisionRadius, Faction, Heading, Health, Position, Ship, Target, TargetKind, Velocity,
};
use sim::{
    Cargo, FactionSpawns, MiningState, MiningTransport, RefinedResources, Turret, TurretSpec,
};

use crate::ServerApp;

/// Mining-skirmish arena layout (camera/sim units; tunable). The central asteroid sits north of the
/// origin (where unfitted client ships spawn until Phase 5's faction spawn), so the arena is in
/// clear view. Red is the left flank, Blue the right; each faction's transport sits between its
/// outpost and the shared asteroid.
const ARENA_Y: f32 = 25.0;
const MINE_NODE_POS: Vec2 = Vec2::new(0.0, ARENA_Y);
const RED_OUTPOST_POS: Vec2 = Vec2::new(-34.0, ARENA_Y);
const BLUE_OUTPOST_POS: Vec2 = Vec2::new(34.0, ARENA_Y);
const RED_TRANSPORT_POS: Vec2 = Vec2::new(-22.0, ARENA_Y);
const BLUE_TRANSPORT_POS: Vec2 = Vec2::new(22.0, ARENA_Y);
// Player spawn points (Phase 5): in open space just outside each faction's home outpost, facing the
// contested asteroid.
const RED_SPAWN_POS: Vec2 = Vec2::new(-34.0, 16.0);
const BLUE_SPAWN_POS: Vec2 = Vec2::new(34.0, 16.0);
// Role sizes / toughness (HP). The asteroid is effectively permanent; the outpost is far beefier
// than the transport (but not a "battle outpost").
const MINE_NODE_RADIUS: f32 = 5.0;
const MINE_NODE_HEALTH: f32 = 1_000_000.0;
const OUTPOST_RADIUS: f32 = 3.0;
const OUTPOST_HEALTH: f32 = 800.0;
const TRANSPORT_RADIUS: f32 = 1.6;
const TRANSPORT_HEALTH: f32 = 200.0;
// Mining-loop tunables (Phase 3): cruise speed, arrival tolerance (clears the asteroid/outpost
// radius + the transport's own), cargo capacity, and load/unload rates (~4 s to fill / 2 s to empty).
const TRANSPORT_NAV_SPEED: f32 = 18.0;
const TRANSPORT_ARRIVE_RADIUS: f32 = 7.0;
const CARGO_CAPACITY: f32 = 100.0;
const LOAD_RATE: f32 = 25.0;
const UNLOAD_RATE: f32 = 50.0;

/// Which authoritative world the embedded server populates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scenario {
    /// The original sandbox/demo world: [`ServerApp::spawn_demo_world`] (2 dummies + 2 drifting
    /// asteroids + 1 seeker) plus the two fitted E007 demo enemies the player can shoot apart.
    #[default]
    Sandbox,
    /// The first real game mode — a 2-faction asteroid-mining skirmish (built out in Phase 1+).
    MiningSkirmish,
}

impl ServerApp {
    /// Populate the authoritative world for `scenario`. Called once by the windowed client before
    /// the handshake; NEVER by the headless tests, so `spawn_demo_world` and the test entity sets
    /// stay untouched. Inserts [`sim::ScenarioActive`] so the scenario-gameplay systems
    /// (`run_if(resource_exists::<ScenarioActive>)`) run only in a live scenario world.
    pub fn spawn_scenario(&mut self, scenario: Scenario) {
        self.world.insert_resource(sim::ScenarioActive);
        // The per-faction refined-resources score (Phase 3). Inserted for both scenarios so the
        // gated `mining_transport_system` has its `ResMut<RefinedResources>` whenever it runs; it is
        // a harmless zeroed tally in Sandbox (no transports update it).
        self.world.insert_resource(RefinedResources::default());
        match scenario {
            Scenario::Sandbox => {
                // The original demo composition (byte-for-byte): the demo targets + the two fitted
                // enemies the windowed client used to spawn inline in `setup_loopback_host`.
                self.spawn_demo_world();
                self.spawn_fitted_enemy(Vec2::new(14.0, 0.0));
                self.spawn_fitted_enemy(Vec2::new(18.0, 6.0));
            }
            Scenario::MiningSkirmish => self.spawn_mining_skirmish(),
        }
    }

    /// Build the static mining-skirmish arena (Phase 1): the central asteroid + each faction's
    /// refinery outpost + mining transport. All are stationary `Health`-based destructibles (no
    /// `Ship`/`FitLayout` → immobile + shot via the flat-`Health` path). Phase 3 makes the
    /// transports mobile + adds the mining loop; Phase 4 mounts turrets.
    fn spawn_mining_skirmish(&mut self) {
        // Phase 5: where an auto-joining human spawns (near their home outpost).
        self.world.insert_resource(FactionSpawns {
            red: RED_SPAWN_POS,
            blue: BLUE_SPAWN_POS,
        });
        let mine = self.spawn_structure(
            TargetKind::MineNode,
            MINE_NODE_POS,
            MINE_NODE_RADIUS,
            MINE_NODE_HEALTH,
            None,
        );
        let red_outpost = self.spawn_outpost(RED_OUTPOST_POS, Faction::Red);
        let blue_outpost = self.spawn_outpost(BLUE_OUTPOST_POS, Faction::Blue);
        self.spawn_transport(RED_TRANSPORT_POS, Faction::Red, red_outpost, mine);
        self.spawn_transport(BLUE_TRANSPORT_POS, Faction::Blue, blue_outpost, mine);
    }

    /// Spawn a faction's refinery outpost (Phase 4): the beefy `Health` structure + 3 mounted
    /// **heavy** turrets (the better-aim [`TurretSpec::outpost_preset`]). Returns the outpost entity.
    fn spawn_outpost(&mut self, pos: Vec2, faction: Faction) -> Entity {
        let outpost = self.spawn_structure(
            TargetKind::Outpost,
            pos,
            OUTPOST_RADIUS,
            OUTPOST_HEALTH,
            Some(faction),
        );
        for offset in [
            Vec2::new(2.2, 0.0),
            Vec2::new(-1.6, 1.9),
            Vec2::new(-1.6, -1.9),
        ] {
            self.mount_turret(
                faction,
                Turret::heavy(outpost, offset),
                TurretSpec::outpost_preset(),
            );
        }
        outpost
    }

    /// Spawn one turret entity (no `Position` — its muzzle is computed from the host each tick;
    /// carries the host's [`Faction`] + a [`Heading`] aim). `turret_system` drives aim + fire.
    fn mount_turret(&mut self, faction: Faction, turret: Turret, spec: TurretSpec) {
        self.world.spawn((turret, spec, faction, Heading(0.0)));
    }

    /// Auto-join (Phase 5): the side with FEWER human ships (Red on a tie). A method the future
    /// multi-human path reuses per connection; for the solo windowed client it returns `Red`.
    pub fn assign_faction(&mut self) -> Faction {
        let (mut red, mut blue) = (0u32, 0u32);
        let mut q = self.world.query_filtered::<&Faction, With<Ship>>();
        for f in q.iter(&self.world) {
            match f {
                Faction::Red => red += 1,
                Faction::Blue => blue += 1,
            }
        }
        if red <= blue {
            Faction::Red
        } else {
            Faction::Blue
        }
    }

    /// Spawn a faction's mining transport (Phase 3): a `Health`-based [`TargetKind::Transport`]
    /// structure plus the mining-loop components ([`MiningTransport`] endpoints + tunables,
    /// [`Cargo`], [`MiningState`]). `mining_transport_system` then runs its navigate→load→return→
    /// unload cycle, growing [`RefinedResources`] on each unload.
    fn spawn_transport(
        &mut self,
        pos: Vec2,
        faction: Faction,
        home_outpost: Entity,
        mine_node: Entity,
    ) -> Entity {
        let t = self.spawn_structure(
            TargetKind::Transport,
            pos,
            TRANSPORT_RADIUS,
            TRANSPORT_HEALTH,
            Some(faction),
        );
        self.world.entity_mut(t).insert((
            MiningTransport {
                home_outpost,
                mine_node,
                load_rate: LOAD_RATE,
                unload_rate: UNLOAD_RATE,
                nav_speed: TRANSPORT_NAV_SPEED,
                arrive_radius: TRANSPORT_ARRIVE_RADIUS,
            },
            Cargo {
                current: 0.0,
                capacity: CARGO_CAPACITY,
            },
            MiningState::default(),
        ));
        // 2 mounted LIGHT turrets (the weaker-aim transport preset) for self-defense.
        for offset in [Vec2::new(0.0, 1.1), Vec2::new(0.0, -1.1)] {
            self.mount_turret(
                faction,
                Turret::light(t, offset),
                TurretSpec::transport_preset(),
            );
        }
        t
    }

    /// Spawn one stationary `Health`-based scenario structure (asteroid / outpost / transport). A
    /// non-`Ship`, non-`FitLayout` entity: `ship_motion_system` never moves it (immobile), and the
    /// flat `collision_detect_system` path applies damage on a hit. An optional [`Faction`] tags its
    /// side (the asteroid is neutral). Returns the entity (Phase 3/4 attach mining + turrets).
    fn spawn_structure(
        &mut self,
        kind: TargetKind,
        pos: Vec2,
        radius: f32,
        health: f32,
        faction: Option<Faction>,
    ) -> Entity {
        let mut e = self.world.spawn((
            Target,
            kind,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            CollisionRadius(radius),
            Health(health),
        ));
        if let Some(f) = faction {
            e.insert(f);
        }
        e.id()
    }
}
