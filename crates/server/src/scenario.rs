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
use glam::{Vec2, Vec3};
use serde::Deserialize;
use sim::components::{
    AngularVelocity, CollisionRadius, Faction, Heading, Health, Position, RenderScale, Ship,
    Target, TargetKind, Velocity,
};
use sim::{
    Cargo, FactionSpawns, MiningState, MiningTransport, MiningTuning, RefinedResources, Turret,
    TurretSpec,
};

use crate::ServerApp;

/// The mining-skirmish data, authored in `assets/content/scenario.ron` (Refinement 4): structure
/// sizes / collision / HP, the transport's [`MiningTuning`], the turret loadouts, and the arena
/// layout. Hot-editable like `ships.ron`/`modules.ron` — see [`load_scenario_content`].
#[derive(Debug, Clone, Deserialize)]
struct ScenarioContent {
    arena: ArenaSpec,
    mine_node: StructureSpec,
    outpost: StructureSpec,
    transport: StructureSpec,
    mining: MiningTuning,
    transport_turret: TurretLoadout,
    outpost_turret: TurretLoadout,
}

/// Arena layout: the east-west span + where the transports start + where players spawn.
#[derive(Debug, Clone, Deserialize)]
struct ArenaSpec {
    /// Half the span: outposts at `(±half_width, 0)`, the asteroid at the origin.
    half_width: f32,
    /// How far toward the asteroid each transport starts from its outpost.
    transport_start_offset: f32,
    /// Player spawn offset from the home outpost, arena-facing (mirrored per faction).
    spawn_offset: (f32, f32),
}

/// One structure's render size + collision + durability.
#[derive(Debug, Clone, Deserialize)]
struct StructureSpec {
    /// Render mesh extent `(x, y, z)` — scales the client's UNIT mesh ([`RenderScale`]).
    render_size: (f32, f32, f32),
    /// Collision circle radius (combat hitbox).
    collision_radius: f32,
    health: f32,
}

/// A host's turret battery: the shared aim spec + (kinetic) weapon stats + per-turret mount offsets.
#[derive(Debug, Clone, Deserialize)]
struct TurretLoadout {
    spec: TurretSpec,
    weapon: TurretWeapon,
    mounts: Vec<(f32, f32)>,
}

/// Kinetic weapon stats for a turret (maps to [`Turret::mounted`]).
#[derive(Debug, Clone, Deserialize)]
struct TurretWeapon {
    damage: f32,
    muzzle_speed: f32,
    fire_rate: f32,
    projectile_mass: f32,
}

/// The mining-scenario content baked into the binary as a fallback (mirrors the external file).
const EMBEDDED_SCENARIO_RON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/content/scenario.ron"
));

/// Load `scenario.ron` — external file (under `$DARK_SILENCE_CONTENT` or `assets/content`) if present
/// and valid, else the embedded default (logged on a parse error so a bad edit never breaks startup).
/// Mirrors [`crate::load_content_or_default`]; only ever called on the windowed `MiningSkirmish` path.
fn load_scenario_content() -> ScenarioContent {
    let dir = std::env::var_os("DARK_SILENCE_CONTENT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("assets/content"));
    if let Ok(s) = std::fs::read_to_string(dir.join("scenario.ron")) {
        match ron::from_str::<ScenarioContent>(&s) {
            Ok(c) => {
                eprintln!(
                    "[content] loaded external scenario.ron from {}",
                    dir.display()
                );
                return c;
            }
            Err(e) => {
                eprintln!("[content] external scenario.ron invalid ({e}); using embedded default")
            }
        }
    }
    ron::from_str(EMBEDDED_SCENARIO_RON).expect("embedded scenario.ron must parse")
}

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
                // Harmless zeroed tuning so the gated `mining_transport_system` has its
                // `Res<MiningTuning>` (no transports read it in Sandbox).
                self.world.insert_resource(MiningTuning::default());
                // The original demo composition (byte-for-byte): the demo targets + the two fitted
                // enemies the windowed client used to spawn inline in `setup_loopback_host`.
                self.spawn_demo_world();
                self.spawn_fitted_enemy(Vec2::new(14.0, 0.0));
                self.spawn_fitted_enemy(Vec2::new(18.0, 6.0));
            }
            Scenario::MiningSkirmish => self.spawn_mining_skirmish(),
        }
    }

    /// Build the static mining-skirmish arena (Phase 1) from [`ScenarioContent`] (`scenario.ron`):
    /// the central asteroid + each faction's refinery outpost + mining transport. All are stationary
    /// `Health`-based destructibles (no `Ship`/`FitLayout` → immobile + shot via the flat-`Health`
    /// path), carrying a [`RenderScale`] so the client renders them at the authored size.
    fn spawn_mining_skirmish(&mut self) {
        let content = load_scenario_content();
        // The transport's live movement/economy tuning comes from the content (Refinement 3/4).
        self.world.insert_resource(content.mining);

        let hw = content.arena.half_width;
        let start = content.arena.transport_start_offset;
        let (sx, sy) = content.arena.spawn_offset;
        // Phase 5: where an auto-joining human spawns (just outside their home outpost, arena-facing).
        self.world.insert_resource(FactionSpawns {
            red: Vec2::new(-hw + sx, sy),
            blue: Vec2::new(hw - sx, sy),
        });

        let mine = self.spawn_structure(TargetKind::MineNode, Vec2::ZERO, &content.mine_node, None);
        let red_outpost = self.spawn_outpost(Vec2::new(-hw, 0.0), Faction::Red, &content);
        let blue_outpost = self.spawn_outpost(Vec2::new(hw, 0.0), Faction::Blue, &content);
        self.spawn_transport(
            Vec2::new(-hw + start, 0.0),
            Faction::Red,
            red_outpost,
            mine,
            &content,
        );
        self.spawn_transport(
            Vec2::new(hw - start, 0.0),
            Faction::Blue,
            blue_outpost,
            mine,
            &content,
        );
    }

    /// Spawn a faction's refinery outpost (Phase 4): the beefy `Health` structure + its turret
    /// battery (the better-aim outpost loadout from `scenario.ron`). Returns the outpost entity.
    fn spawn_outpost(&mut self, pos: Vec2, faction: Faction, content: &ScenarioContent) -> Entity {
        let outpost =
            self.spawn_structure(TargetKind::Outpost, pos, &content.outpost, Some(faction));
        self.mount_turrets(outpost, faction, &content.outpost_turret);
        outpost
    }

    /// Mount a host's turret battery from a [`TurretLoadout`]: one turret entity per mount offset, all
    /// sharing the loadout's aim `spec` + (kinetic) weapon stats. Each turret has no `Position` (its
    /// muzzle is `host.Position + mount_offset` each tick), carries the host's [`Faction`] + a
    /// [`Heading`] aim; `turret_system` drives aim + fire.
    fn mount_turrets(&mut self, host: Entity, faction: Faction, loadout: &TurretLoadout) {
        let w = &loadout.weapon;
        for &(mx, my) in &loadout.mounts {
            let turret = Turret::mounted(
                host,
                Vec2::new(mx, my),
                w.damage,
                w.muzzle_speed,
                w.fire_rate,
                w.projectile_mass,
            );
            self.world
                .spawn((turret, loadout.spec, faction, Heading(0.0)));
        }
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
        content: &ScenarioContent,
    ) -> Entity {
        let t = self.spawn_structure(
            TargetKind::Transport,
            pos,
            &content.transport,
            Some(faction),
        );
        self.world.entity_mut(t).insert((
            MiningTransport {
                home_outpost,
                mine_node,
            },
            Cargo { current: 0.0 },
            MiningState::default(),
            // Angular-velocity state for the Newtonian turn model (Refinement 3).
            AngularVelocity(0.0),
        ));
        self.mount_turrets(t, faction, &content.transport_turret);
        t
    }

    /// Spawn one stationary `Health`-based scenario structure (asteroid / outpost / transport) from
    /// its [`StructureSpec`]. A non-`Ship`, non-`FitLayout` entity: `ship_motion_system` never moves
    /// it (immobile), and the flat `collision_detect_system` path applies damage on a hit. Carries a
    /// [`RenderScale`] (the authored render size) so the client scales its unit mesh. An optional
    /// [`Faction`] tags its side (the asteroid is neutral). Returns the entity.
    fn spawn_structure(
        &mut self,
        kind: TargetKind,
        pos: Vec2,
        spec: &StructureSpec,
        faction: Option<Faction>,
    ) -> Entity {
        let (rx, ry, rz) = spec.render_size;
        let mut e = self.world.spawn((
            Target,
            kind,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            CollisionRadius(spec.collision_radius),
            Health(spec.health),
            RenderScale(Vec3::new(rx, ry, rz)),
        ));
        if let Some(f) = faction {
            e.insert(f);
        }
        e.id()
    }
}
