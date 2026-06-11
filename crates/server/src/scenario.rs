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
use sim::ai::{
    spawn_squad, AiBrain, AiIdAllocator, AiTuning, AoiTier, ContactList, FormationDef, Posture,
    RoleGoal, ScenarioRole, SquadOrder,
};
use sim::components::{
    AngularVelocity, BoxCollider, CollisionRadius, Faction, Heading, Health, Movable, Position,
    RamMass, RenderScale, Ship, Target, TargetKind, Velocity,
};
use sim::fitting::{
    disc_hull, hull_collision_radius, station_hull, HullCatalog, HullId, CELL_WORLD_SIZE,
    HULL_MINENODE, HULL_OUTPOST, HULL_TRANSPORT,
};
use sim::ShipIntent;
use sim::{
    Cargo, FactionSpawns, MiningState, MiningTransport, MiningTuning, RefinedResources, Turret,
    TurretSpec, VoxelizeOnHit,
};

use crate::ServerApp;

/// The mining-skirmish data, authored in `assets/content/scenario.ron` (Refinement 4): structure
/// sizes / collision / HP, the transport's [`MiningTuning`], the turret loadouts, and the arena
/// layout. Hot-editable like `ships.ron`/`modules.ron` — see [`load_scenario_content`].
#[derive(Debug, Clone, Deserialize)]
struct ScenarioContent {
    arena: ArenaSpec,
    mine_node: StructureSpec,
    outpost: VoxelStructureSpec,
    transport: VoxelStructureSpec,
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

/// A flat (non-carveable) structure — render size + collision + durability. Used for the asteroid.
#[derive(Debug, Clone, Deserialize)]
struct StructureSpec {
    /// Render mesh extent `(x, y, z)` — scales the client's UNIT mesh ([`RenderScale`]).
    render_size: (f32, f32, f32),
    /// Collision circle radius (combat hitbox).
    collision_radius: f32,
    health: f32,
    /// Refinement 11 — per-cell HP if this body is **carveable**: when present (`Some`), the body
    /// lazy-voxelizes on first hit into a round [`disc_hull`](sim::fitting::disc_hull) (diameter =
    /// `collision_radius · 2 / CELL_WORLD_SIZE`) and can be dug into. `None`/absent → a permanent
    /// flat-HP landmark (the old behaviour). High HP = a slow dig.
    #[serde(default)]
    carve_cell_hp: Option<f32>,
}

/// A carveable structure (Refinement 5): a `(cols, rows)` voxel hull grid + plating thickness +
/// per-cell HP. Spawns as a cheap flat box marked [`VoxelizeOnHit`]; the first hit converts it to the
/// cell hull (size = `grid · CELL_WORLD_SIZE`, so render + collision derive from the grid).
#[derive(Debug, Clone, Deserialize)]
struct VoxelStructureSpec {
    /// Hull grid `(cols, rows)` — world size is `grid · CELL_WORLD_SIZE` (≈0.32 u/cell). The hull is a
    /// solid filled block of this grid (Refinement 7).
    grid: (u16, u16),
    /// HP per structural cell (toughness of carving; deeper carve to the centre core = destroyed).
    cell_hp: f32,
    /// Refinement 10 — inertial mass for ship↔structure rams (`RamMass`). A heavier station is barely
    /// nudged. Defaults large (effectively immovable) when omitted.
    #[serde(default = "default_ram_mass")]
    mass: f32,
    /// Refinement 10 — whether a ram can SHOVE this station (it drifts, with drag) vs being an
    /// immovable wall the craft just bounces off. Defaults `false`.
    #[serde(default)]
    movable: bool,
}

/// Default `RamMass` for a structure with no `mass` in RON — large enough that a ram barely moves it.
fn default_ram_mass() -> f32 {
    5000.0
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

        // Refinement 5: inject the procedural station hulls into the catalog so the lazy-voxelize
        // conversion (`voxelize_pending_system`) + render can resolve them by id. Windowed-only.
        {
            let (tc, tr) = content.transport.grid;
            let (oc, or) = content.outpost.grid;
            let t_hull = station_hull(HULL_TRANSPORT, "Transport", tc, tr);
            let o_hull = station_hull(HULL_OUTPOST, "Outpost", oc, or);
            // Refinement 11: the carveable central rock's ROUND disc hull (only if it has carve HP).
            // Diameter (cells) = world diameter / cell size, so the voxel disc ≈ the rendered sphere.
            let mine_disc = content.mine_node.carve_cell_hp.map(|_| {
                let diameter =
                    (content.mine_node.collision_radius * 2.0 / CELL_WORLD_SIZE).round() as u16;
                disc_hull(HULL_MINENODE, "MineNode", diameter)
            });
            if let Some(mut hulls) = self.world.get_resource_mut::<HullCatalog>() {
                hulls.hulls.insert(HULL_TRANSPORT, t_hull);
                hulls.hulls.insert(HULL_OUTPOST, o_hull);
                if let Some(d) = mine_disc {
                    hulls.hulls.insert(HULL_MINENODE, d);
                }
            }
        }

        let hw = content.arena.half_width;
        let start = content.arena.transport_start_offset;
        let (sx, sy) = content.arena.spawn_offset;
        // Phase 5: where an auto-joining human spawns (just outside their home outpost, arena-facing).
        self.world.insert_resource(FactionSpawns {
            red: Vec2::new(-hw + sx, sy),
            blue: Vec2::new(hw - sx, sy),
        });

        let mine = self.spawn_structure(TargetKind::MineNode, Vec2::ZERO, &content.mine_node, None);
        // Refinement 11: if the rock has carve HP, mark it for lazy voxelization — the first shot/ram
        // converts the cheap flat sphere into the carveable disc hull. It stays immovable (no
        // `Movable`/`RamMass`), so rams bounce + hurt the craft while fire digs into the rock.
        if let Some(cell_hp) = content.mine_node.carve_cell_hp {
            self.world.entity_mut(mine).insert(VoxelizeOnHit {
                hull: HULL_MINENODE,
                cell_hp,
            });
        }
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

        // 00008-ship-ai T033 (TR-015): the scenario's authored AI content —
        // per-faction patrol squads + ambush pairs around the asteroid.
        self.spawn_skirmish_ai(hw);
    }

    /// T033 (TR-015) — author the mining skirmish's AI ships: per faction one
    /// 3-fighter PATROL squad sweeping its own half (route north of the
    /// mining lane, so the new fighters never collide with the transport
    /// economy) and one 2-fighter AMBUSH pair lying dark beside the central
    /// asteroid (trigger circle radius 120 around the rock — the contested
    /// ground; an enemy transport docking at the mine, or an enemy patrol leg
    /// crossing the middle, springs it: a living skirmish, by design).
    ///
    /// Posture examples (so postures are exercised in-game): the BLUE patrol
    /// is `DefensiveOnly` (it returns fire only while fired-upon — see
    /// `FIRED_UPON_WINDOW_TICKS`); everything else is `FreeEngage`.
    ///
    /// 10 AI ships total — playtest content, not the bench. Additive only:
    /// `Scenario::Sandbox` and every golden world are untouched, and the
    /// Phase-1 arena lock (`mining_skirmish_scenario_spawns_the_static_arena`)
    /// counts `Target` sub-kinds, which fitted `Ship`s never carry.
    fn spawn_skirmish_ai(&mut self, hw: f32) {
        use std::f32::consts::PI;
        for (faction, sign, heading, patrol_posture) in [
            (Faction::Red, -1.0_f32, 0.0_f32, Posture::FreeEngage),
            (Faction::Blue, 1.0, PI, Posture::DefensiveOnly),
        ] {
            // PATROL: a 3-4 point loop over the faction's half, offset north
            // (+Y) of the y = 0 transport lane. Mirrored per faction.
            let route = vec![
                Vec2::new(sign * (hw - 200.0), 150.0),
                Vec2::new(sign * hw * 0.5, 250.0),
                Vec2::new(sign * 200.0, 150.0),
            ];
            let mut members = Vec::with_capacity(3);
            for i in 0..3 {
                // Trail the spawn cluster behind route[0] so ships don't
                // overlap (wedge-ish offsets; the brains sort themselves out).
                let offset = Vec2::new(
                    -sign * 15.0 * i as f32,
                    if i == 2 { -12.0 } else { 12.0 * i as f32 },
                );
                let role = ScenarioRole::new(RoleGoal::PatrolRoute(route.clone()), patrol_posture);
                members.push(self.spawn_ai_fighter(route[0] + offset, heading, faction, role));
            }
            spawn_squad(
                &mut self.world,
                &members,
                FormationDef::wedge(members.len(), 12.0),
                SquadOrder::Hold,
            );

            // AMBUSH: a dark pair flanking the asteroid (south of the lane),
            // sprung by any hostile contact inside the 120-radius circle
            // around the rock.
            let ambush = RoleGoal::Ambush {
                trigger_center: Vec2::ZERO,
                trigger_radius: 120.0,
            };
            for j in 0..2 {
                let pos = Vec2::new(sign * (80.0 + 15.0 * j as f32), -100.0 - 10.0 * j as f32);
                let role = ScenarioRole::new(ambush.clone(), Posture::FreeEngage);
                self.spawn_ai_fighter(pos, heading, faction, role);
            }
        }
    }

    /// One scenario AI fighter: the armed fitted `Ship` (`spawn_fitted_ship`,
    /// the R56 combatant fit) plus the full AI stack — `AiBrain` (phase bucket
    /// from a freshly allocated `AiStableId`, V-4), `AoiTier` (Dormant until
    /// the classifier promotes it near the player), `ContactList` (perception)
    /// and its `ScenarioRole`. The helper's always-firing `ShipIntent` is
    /// reset: an AI ship's trigger belongs to its brain's `fire_decision`
    /// (TR-011), not a spawn-time pin.
    fn spawn_ai_fighter(
        &mut self,
        pos: Vec2,
        heading: f32,
        faction: Faction,
        role: ScenarioRole,
    ) -> Entity {
        let entity = self.spawn_fitted_ship(pos, heading, faction);
        let bucket_count = self.world.get_resource::<AiTuning>().map_or_else(
            || AiTuning::default().fallback_bucket_count,
            |t| t.fallback_bucket_count,
        );
        let id = self.world.resource_mut::<AiIdAllocator>().allocate();
        self.world.entity_mut(entity).insert((
            AiBrain::new(id, bucket_count),
            id,
            AoiTier::default(),
            ContactList::default(),
            role,
        ));
        if let Some(mut intent) = self.world.get_mut::<ShipIntent>(entity) {
            intent.fire_primary = false; // The brain owns the trigger (TR-011).
        }
        entity
    }

    /// Spawn a faction's refinery outpost (Phase 4): the beefy `Health` structure + its turret
    /// battery (the better-aim outpost loadout from `scenario.ron`). Returns the outpost entity.
    fn spawn_outpost(&mut self, pos: Vec2, faction: Faction, content: &ScenarioContent) -> Entity {
        let outpost = self.spawn_voxel_structure(
            TargetKind::Outpost,
            pos,
            faction,
            &content.outpost,
            HULL_OUTPOST,
        );
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
        let t = self.spawn_voxel_structure(
            TargetKind::Transport,
            pos,
            faction,
            &content.transport,
            HULL_TRANSPORT,
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

    /// Spawn a **carveable** structure (Refinement 5 — outpost / transport): a cheap flat box NOW
    /// (`Target` + flat `Health` + a `CollisionRadius` sized to the eventual voxel footprint + a
    /// `RenderScale` box) carrying a [`VoxelizeOnHit`] marker. It stays out of the carve/voxel-render
    /// path until its first hit, when `voxelize_pending_system` swaps in `hull`'s cell layout (size =
    /// `grid · CELL_WORLD_SIZE`). `faction` tags its side. Returns the entity.
    fn spawn_voxel_structure(
        &mut self,
        kind: TargetKind,
        pos: Vec2,
        faction: Faction,
        spec: &VoxelStructureSpec,
        hull: HullId,
    ) -> Entity {
        let (cols, rows) = spec.grid;
        // The pre-conversion box matches the eventual voxel hull's footprint (grid · cell size), so
        // the shape doesn't jump on first hit; a modest fixed depth for thickness.
        let render = Vec3::new(
            cols as f32 * CELL_WORLD_SIZE,
            rows as f32 * CELL_WORLD_SIZE,
            4.0,
        );
        let entity = self
            .world
            .spawn((
                Target,
                kind,
                Position(pos),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                // Hitbox = the eventual voxel footprint (right both before + after conversion).
                CollisionRadius(hull_collision_radius(spec.grid)),
                // Placeholder flat HP: never reduced (the voxelize path TAGS instead of damaging) and
                // removed on conversion, so it sits high enough to ignore any stray flat damage.
                Health(1.0e6),
                RenderScale(render),
                faction,
                VoxelizeOnHit {
                    hull,
                    cell_hp: spec.cell_hp,
                },
                // Refinement 10: inertial mass for ship↔structure rams (a craft that slams it bounces
                // off + gets carved; a `Movable` station is shoved, a heavy/fixed one barely moves).
                RamMass(spec.mass),
                // Refinement 11: an oriented-box hitbox (half-extents = grid · cell · 0.5) so a square
                // block collides as a TIGHT box, not an under-covering inscribed circle (no deep sink).
                BoxCollider(Vec2::new(
                    cols as f32 * CELL_WORLD_SIZE * 0.5,
                    rows as f32 * CELL_WORLD_SIZE * 0.5,
                )),
            ))
            .id();
        if spec.movable {
            self.world.entity_mut(entity).insert(Movable);
        }
        entity
    }

    /// Spawn one stationary flat-`Health` structure (the asteroid) from its [`StructureSpec`]. A
    /// non-`Ship`, non-`FitLayout` entity: `ship_motion_system` never moves it, and the flat
    /// `collision_detect_system` path applies damage on a hit. Carries a [`RenderScale`] (the authored
    /// render size) so the client scales its unit mesh. An optional [`Faction`] tags its side (the
    /// asteroid is neutral). Returns the entity.
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
