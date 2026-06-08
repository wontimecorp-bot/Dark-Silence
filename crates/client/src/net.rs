//! The windowed solo-client plugin (T045, OBJ4) — wires the embedded
//! authoritative server into the Bevy schedule and makes `cargo run -p client` a
//! runnable single-player experience.
//!
//! **Why this path renders from the server world directly (not from the netcode).**
//! For solo loopback there is *zero* real latency, so the predict/interpolate
//! netcode adds nothing and actively hurts feel: a predicted-in-isolation local
//! ship flies *through* asteroids then rubber-bands, and remotes rendered ~100 ms
//! in the past make hits look disconnected. The embedded [`ServerApp`] already IS
//! a full authoritative simulation running in-process — collision, weapon, AI,
//! and destruction all step there each tick. So the windowed client renders the
//! window **directly from the embedded server's world** at full `f32` precision
//! ([`ServerApp::render_state`]), using E002's smooth fixed-step interpolation
//! (the [`RenderInterp`] + [`interpolate_transforms`] seam). This restores E002's
//! flight feel AND gives crisp, in-sync collision + real-time hits.
//!
//! **The netcode modules are intact and unchanged.** [`crate::prediction`]
//! (`Predictor`, reconcile, `RenderSmoother`…), [`crate::interpolation`]
//! (`SnapshotBuffer`, `DeltaReconstructor`, `interpolate_remotes`), and the
//! `protocol`/`server` netcode are exercised by the integration tests under
//! `crates/*/tests/` and remain the path real *remote* multiplayer uses. Only this
//! windowed render path stopped consuming them.
//!
//! Lifecycle this plugin adds (per FixedUpdate, the authoritative-tick cadence):
//! - [`net_fixed_update`]: read the local ship's [`ShipIntent`] (written by
//!   [`crate::input::read_input`] in PreUpdate), build + send the numbered
//!   [`protocol::ClientInput`] so the server pilots the ship, then
//!   [`ServerApp::tick`] (applies the input + steps the full sim incl.
//!   collision/weapon/AI/destruction this tick), then **drain and discard** the
//!   loopback inbox so its queue can't grow unbounded over a long session.
//! - [`capture_render_state`]: read [`ServerApp::render_state`] and reconcile the
//!   rendered Bevy entities with it, keyed by [`EntityId`] — roll each entity's
//!   [`RenderInterp`] prev→curr and set curr = the server pose, find-or-spawn
//!   newly-appeared entities (mesh/material by kind+flags), and despawn rendered
//!   entities whose id is gone.
//!
//! Then in **Update**, E002's [`interpolate_transforms`] blends every rendered
//! entity's `RenderInterp` into its `Transform` by the `Time<Fixed>` overstep —
//! buttery 60 fps motion of the 30 Hz sim, exactly like E002, for the local ship,
//! targets, and projectiles through one shared path.
//!
//! **Transport seam:** the plugin holds its transport + embedded server behind a
//! [`NonSend`] resource ([`LoopbackHost`]) — loopback is single-threaded
//! (`Rc`-backed), so it is a non-send resource by construction. The same
//! FixedUpdate systems drive a renet-backed transport once it is swapped in
//! (Phase 4) — only the host resource changes, not the lifecycle systems.

use std::collections::HashMap;

use bevy::prelude::*;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, EntityKind, LoopbackTransport, Message,
    NetTransport, CLIENT_TOKEN_BYTES,
};
use server::{RenderEntity, Scenario, ServerApp, PROTOCOL_VERSION};
use sim::components::{
    Afterburner, ArmorHp, AuthoredCells, CollisionRadius, Destructible, Energy, Heat, Position,
    TargetKind, Velocity,
};
use sim::damage::seed_defense_layers;
use sim::fitting::{
    build_layout, derive_ship_stats, hull_collision_radius, seed_catalogs, Fit, SlotId,
    HULL_FIGHTER, MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_SHIELD_BASIC,
    MODULE_THRUSTER_BASIC,
};
use sim::{FactionSpawns, FixedDt, HitFeedback, ShipIntent};

use crate::input::{build_client_input, InputSequencer};
use crate::render_sync::{
    interpolate_transforms, EngineFlame, HullTile, RemoteEntity, RenderInterp, ShieldBubble,
    ShieldChild, ShipHull, ShipThrottle,
};
use crate::scene::{
    build_hull_mesh, build_hull_mesh_contour, build_module_overlay_mesh, build_ship_fixtures,
    FixtureRole, RenderAssets, CELL_SIZE,
};

/// The loopback solo-play host: the embedded authoritative [`ServerApp`] plus the
/// client end of its [`LoopbackTransport`] and the established connection. A
/// `NonSend` Bevy resource because the loopback transport is `Rc`-backed
/// (single-threaded) — exactly the constraint loopback solo play already lives
/// under. Swapping in a renet transport (Phase 4) replaces this resource with a
/// real-socket host; the FixedUpdate/Update systems are unchanged.
pub struct LoopbackHost {
    /// The embedded authoritative server, stepped once per FixedUpdate so solo
    /// play runs the real server loop (no authority/validation bypass, TR-018).
    pub server: ServerApp,
    /// The client end of the loopback pair.
    pub transport: LoopbackTransport,
    /// This client's connection handle.
    pub conn: ConnectionId,
}

/// The windowed-client netcode state for the solo render path. A `NonSend`
/// resource so it can live alongside the `Rc`-backed loopback host without a
/// `Send + Sync` bound.
///
/// Trimmed to exactly what the windowed path needs: the input sequencer (to build
/// the numbered [`ClientInput`] the server pilots the ship from) and this client's
/// authoritative ship id (so the local ship's rendered entity is reconciled
/// against the right [`RenderEntity`]). The predictor / smoother / snapshot buffer
/// / delta reconstructor are deliberately NOT held here — they live on in
/// [`crate::prediction`] / [`crate::interpolation`] (and their tests) and are the
/// path real *remote* multiplayer uses; the windowed solo path renders from the
/// server world directly, so it does not run them.
pub struct NetClientState {
    /// Monotonic input numbering + redundant tail for the wire (TR-007), so the
    /// server receives well-formed numbered input and pilots the local ship.
    pub sequencer: InputSequencer,
    /// This client's authoritative ship id, learned at handshake — the
    /// [`RenderEntity`] id the local (pre-spawned) ship is reconciled against.
    pub local_id: EntityId,
}

/// Maps each authoritative wire [`EntityId`] to the Bevy entity that renders it,
/// so [`capture_render_state`] can find-or-spawn and despawn rendered entities by
/// stable id across ticks. The pre-spawned [`LocalShip`] is registered here under
/// the client's `local_id` at startup; every other rendered entity is a
/// [`RemoteEntity`] spawned on first sight.
#[derive(Resource, Default)]
pub struct NetRenderMap {
    map: HashMap<EntityId, Entity>,
}

/// Marker for the local player's rendered ship entity in the Bevy world. The local
/// ship is now just another [`RenderInterp`] entity (no longer special-cased): its
/// pose is captured from the server's render state by [`capture_render_state`] and
/// smoothed by [`interpolate_transforms`], like every other rendered entity. The
/// follow camera, gunsight pip, and HUD still find it by this tag.
#[derive(Component)]
pub struct LocalShip;

/// Extra radius, in sim units, the shield-impact crescent sits OUTSIDE the hull
/// silhouette (FIX 0a polish). The per-ship shield radius is derived from the hull's
/// footprint (see [`shield_radius_for`]) plus this margin, so the glowing band hugs the
/// hull edge — close enough to read as the ship's deflector surface, far enough not to
/// clip into the plate. Tunable for feel.
pub const SHIELD_MARGIN: f32 = 0.3;

/// Per-ship shield radius (sim units) for the impact crescent — derived from the fitted
/// ship's footprint so the normalized arc mesh ([`crate::scene::build_arc_band_mesh`],
/// outer radius `1.0`) can be **scaled** to hug ANY hull (FIX 0a polish).
///
/// Takes the larger grid dimension (the ship's longest extent in cells), converts it to a
/// half-extent in sim units (`max_dim · CELL_SIZE · 0.5`, the distance from the ship centre
/// to the far edge of the silhouette), and adds [`SHIELD_MARGIN`] so the band sits just
/// OUTSIDE the plate. Examples (with [`CELL_SIZE`] `0.32`, [`SHIELD_MARGIN`] `0.3`):
/// fighter `9×11` → `11·0.32·0.5 + 0.3 ≈ 2.06`; corvette `13×15` → `15·0.32·0.5 + 0.3 ≈ 2.7`.
/// Falls back to a sane default for a degenerate `(0, 0)` (a non-fitted entity never gets a
/// visible flash, so this is only defensive).
fn shield_radius_for(grid_dims: (u16, u16)) -> f32 {
    let max_dim = grid_dims.0.max(grid_dims.1) as f32;
    if max_dim <= 0.0 {
        return 1.0 + SHIELD_MARGIN;
    }
    max_dim * CELL_SIZE * 0.5 + SHIELD_MARGIN
}

/// Phase 1B voxel LOD gate (sim units): a fitted ship within this camera-relative
/// distance is **near** (Tier-0) and rendered as its dense cell-grid (a cell-box child
/// per hull cell); beyond it the ship renders as the single coarse [`RenderAssets::ship_mesh`]
/// box. This caps the fine-rendered cell count to the few ships you are actually fighting
/// (the perf lever the plan designs in from Phase 1). Distance is measured from the LOCAL
/// player ship's world position (which the follow-camera centres on) to each ship. For
/// the demo the handful of ships are always near, so the voxel path runs; the box path
/// exists + switches cleanly when a ship crosses the threshold. Tunable for feel.
pub const SHIP_VOXEL_LOD_DIST: f32 = 60.0;

/// Refinement 6: a fitted entity whose hull footprint (`max(grid_dims) · CELL_SIZE`) exceeds this is
/// a big STRUCTURE, not a ship. Its far-LOD placeholder is the unit [`RenderAssets::lod_box_mesh`]
/// scaled to the footprint (so it reads at the right size), where a ship just uses `ship_mesh` at
/// scale ONE. ~7.0 sits between the corvette's ~4.8 u and the transport's ~10.2 u.
pub const STRUCTURE_LOD_FOOTPRINT: f32 = 7.0;

/// R49 — minimum hull footprint (`max(grid_dims) · CELL_SIZE`) for a ship to carry nav/running lights.
/// Excludes the fighter (~3.5) + corvette (~4.8) so only bigger ships get them. Tunable.
pub const NAV_MIN_FOOTPRINT: f32 = 6.0;

/// Runtime hull-render style toggle (Fix #11 M2), flipped in-game by
/// [`crate::input::toggle_hull_render`] (the `V` key). `false` (default) = the blocky per-cell
/// voxel mesh ([`build_hull_mesh`]); `true` = the smoothed rounded contour
/// ([`build_hull_mesh_contour`]). Purely cosmetic — the sim's ricochet/carve is unaffected — so
/// it can be flipped freely to A/B the look. [`sync_ship_hull`] rebuilds a ship's hull mesh when
/// this differs from the style it was last built in ([`ShipHull::built_contour`]).
#[derive(Resource, Default, Clone, Copy)]
pub struct HullRenderMode {
    pub contour: bool,
}

/// Runtime **module-color** toggle (Fix #11 M3), flipped in-game by
/// [`crate::input::toggle_module_color`] (the `C` key). `false` (default) = the uniform hull
/// look; `true` = cells are tinted by module type ([`crate::scene::module_palette`]). In the
/// VOXEL look this is per-cell vertex coloring (paired with a white-base hull material so the
/// hues show); in the CONTOUR look it adds a thin module-marker overlay child over the smooth
/// hull. Purely cosmetic. [`sync_ship_hull`] rebuilds a ship's hull when this differs from the
/// state it was last built in ([`ShipHull::built_module_color`]).
#[derive(Resource, Default, Clone, Copy)]
pub struct ModuleColorMode {
    pub on: bool,
}

/// The windowed solo-client plugin (T045). Adds the embedded-server lifecycle and
/// the render-from-server-world path, making the client runnable solo
/// (Principle VII). Add it after [`DefaultPlugins`] and the fixed-step clock.
pub struct NetClientPlugin;

impl Plugin for NetClientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetRenderMap>()
            // `setup_loopback_host` queries the pre-spawned `LocalShip` to register
            // it in the `NetRenderMap`, so it MUST run after the scene spawns it.
            .add_systems(
                Startup,
                setup_loopback_host.after(crate::scene::setup_scene),
            )
            // FixedUpdate (authoritative-tick cadence): pilot + step the embedded
            // server, then capture its world into the rendered entities' interp
            // snapshots. `capture_render_state` runs AFTER `net_fixed_update` so it
            // reads the world the server JUST stepped this tick.
            .add_systems(
                FixedUpdate,
                (net_fixed_update, capture_render_state).chain(),
            )
            // Update (per render frame): blend every rendered entity's RenderInterp
            // into its Transform by the fixed-step overstep — E002's smooth motion.
            .add_systems(Update, interpolate_transforms);
    }
}

/// `Startup`: stand up the embedded loopback server, populate its authoritative
/// world with the demo targets, connect the client, run the handshake to learn
/// this client's ship id, seed the trimmed [`NetClientState`], and register the
/// pre-spawned [`LocalShip`] in the [`NetRenderMap`] under that id.
///
/// The scene (`scene::setup_scene`) owns the [`LocalShip`] tag and its
/// [`RenderInterp`] — it spawns the local ship deterministically, so this system
/// never races scene setup. We map `local_id → that entity` here so
/// [`capture_render_state`] drives the existing local ship rather than spawning a
/// duplicate.
/// Which scenario the windowed client populates the embedded server with. The mining skirmish is
/// the new default game mode; `Scenario::Sandbox` reproduces the original demo (kept selectable).
const SELECTED_SCENARIO: Scenario = Scenario::MiningSkirmish;

fn setup_loopback_host(world: &mut World) {
    let (mut server, mut transport) = ServerApp::loopback();

    // Refinement 39/41: apply the WINDOWED-ONLY sim-tuning override (the dev panel's saved
    // `render_tuning.ron`) to the embedded server BEFORE spawning, so spawns build at the dev tuning
    // (e.g. struct-cell HP/mass) and every tick reads the dev flight/damage configs. This is loaded
    // here — NOT in `ServerApp::new` — so the headless determinism/botkit/demo worlds (which never run
    // this windowed setup) keep their `Default` tuning and stay bit-identical. Module/hull DESIGNS are
    // NOT overridden here: R41 writes design edits back to the canonical `modules.ron`/`ships.ron`,
    // which `ServerApp::loopback()` → `load_content_or_default` already loaded into the catalog.
    let dev = crate::tuning_io::load_dev_settings();
    {
        let w = server.world_mut();
        w.insert_resource(dev.tuning);
        w.insert_resource(dev.sim_tuning);
        w.insert_resource(dev.penetration);
        w.insert_resource(dev.shield);
        w.insert_resource(dev.salvage);
        w.insert_resource(dev.stat_scaling);
        w.insert_resource(dev.resistance);
    }

    // Populate the authoritative world with the selected scenario BEFORE the handshake
    // tick below, so its entities exist the first time `capture_render_state` reads the
    // server world. `Scenario::Sandbox` reproduces the original demo (practice dummies +
    // drifting asteroids + seeker + the two fitted E007 enemies); `MiningSkirmish` is the
    // new game mode. The headless tests never call `spawn_scenario`, so `spawn_demo_world`
    // + the test entity sets are untouched.
    server.spawn_scenario(SELECTED_SCENARIO);
    // `spawn_scenario(MiningSkirmish)` inserts `MiningTuning` from `scenario.ron`, so apply the dev
    // override AFTERWARDS (else it would be clobbered). Sandbox also inserts a default MiningTuning.
    server.world_mut().insert_resource(dev.mining);
    let conn = NetTransport::connect(&mut transport, loopback_addr());

    // Handshake: send Connect, tick the server once so it accepts + replies, then
    // read the ConnectAccepted to learn our ship id.
    transport.send_reliable(
        conn,
        &Message::Connect(Connect {
            protocol_version: PROTOCOL_VERSION,
            client_token: [0u8; CLIENT_TOKEN_BYTES],
        }),
    );
    server.tick();

    let mut local_id = None;
    for msg in transport.recv(conn) {
        if let Message::ConnectAccepted(accepted) = msg {
            local_id = Some(accepted.client_id);
        }
    }
    let local_id = local_id.expect("loopback handshake yields a ConnectAccepted");

    // T033 (FR-014): give the embedded server's player ship a fit so the windowed
    // ship flies fit-driven. The handshake tick above spawned this client's
    // authoritative ship; attach a starter fighter fit + its derived `ShipStats` +
    // `FitLayout` directly on the SERVER-side entity, because the windowed window
    // renders from (and the flight is computed in) the embedded server's world. The
    // flight system's override path then reads this `ShipStats` instead of the
    // global `Tuning` — "fit drives the ship". `spawn_demo_world` targets are
    // untouched (only this one ship entity is fitted).
    attach_starter_fit(&mut server, local_id);

    // Phase 5: auto-join a side + spawn at that faction's base (mining skirmish only).
    // `attach_starter_fit` stays the pure reusable fit; this sets the player's team + spawn position
    // around it, so the player's shots are factioned (enemy structures/turrets hostile, friendlies
    // not) and they start near their refinery. Sandbox has no `FactionSpawns` → the player stays
    // unfactioned at the origin (the original free-for-all behaviour).
    if let Some(spawns) = server.world().get_resource::<FactionSpawns>().copied() {
        let faction = server.assign_faction();
        if let Some(ship) = server.ship_entity_for(local_id) {
            let mut entity = server.world_mut().entity_mut(ship);
            entity.insert(faction);
            entity.insert(Position(spawns.for_faction(faction)));
        }
    }

    // Register the pre-spawned local ship under its authoritative id so the capture
    // system updates it in place (the scene spawns exactly one `LocalShip`).
    let mut local_ship_q = world.query_filtered::<Entity, With<LocalShip>>();
    let local_ship = local_ship_q
        .single(world)
        .expect("scene::setup_scene spawns exactly one LocalShip before the net plugin's Startup");
    world
        .resource_mut::<NetRenderMap>()
        .map
        .insert(local_id, local_ship);

    world.insert_non_send_resource(LoopbackHost {
        server,
        transport,
        conn,
    });
    world.insert_non_send_resource(NetClientState {
        sequencer: InputSequencer::new(),
        local_id,
    });
}

/// Attach a starter fighter fit + its derived [`ShipStats`] + [`FitLayout`] to the
/// embedded server's player ship (T033, FR-014/019).
///
/// The windowed ship's flight is computed in the embedded server's `sim` schedule,
/// which reads a ship's [`ShipStats`] component when present (the Phase 4 rewire) —
/// so to make fitting drive flight, the fit + derived stats must live on the
/// SERVER-side ship entity. This resolves that entity by the wire [`EntityId`] the
/// client learned at handshake ([`ServerApp::ship_entity_for`]) and inserts:
/// - a starter [`Fit`] on [`HULL_FIGHTER`] (reactor + two thrusters + one
///   autocannon — a valid, within-budget loadout from the seed catalog);
/// - the [`derive_ship_stats`] result (so flight reads fit-derived thrust/mass/turn
///   and the ship can fire — the autocannon makes `can_fire == true`);
/// - the [`build_layout`] hit/armor map (the E007 surface, kept in lock-step);
/// - the three E007 defense layers from [`seed_defense_layers`]
///   ([`Shields`](sim::damage::Shields)/[`SectionArmor`](sim::damage::SectionArmor)/
///   [`HullStructure`](sim::damage::HullStructure)) so the **player** ship is also
///   damageable — symmetric with the demo enemy (nothing shoots the player yet, but
///   the pipeline is complete; enemy fire is a follow-on).
///
/// **Flight sanity:** the two seed thrusters sum to the E002 thrust/torque/strafe
/// magnitudes (30/12/18), so emergent top speed (`thrust/linear_drag = 80`) and max
/// turn rate (`turn_torque/angular_drag = 3.0 rad/s`) match the old `Tuning`
/// baseline; the fighter's heavier total mass (hull base + modules) makes
/// acceleration more deliberate than the unit-mass `Tuning` ship — the intended
/// "the feel now reflects the fighter's fit" payoff, still bounded and playable.
///
/// `spawn_demo_world` targets are unaffected — only this one ship entity is fitted.
/// If the ship entity cannot be resolved (it always can after the handshake), this
/// is a no-op rather than a panic (defensive).
fn attach_starter_fit(server: &mut ServerApp, local_id: EntityId) {
    let (modules, hulls) = seed_catalogs();
    let Some(hull) = hulls.get(HULL_FIGHTER).cloned() else {
        return;
    };

    // Build the starter loadout on the fighter via the validate-then-apply install
    // (so the fit is guaranteed legal / within budget). Slot layout:
    // 0 Reactor, 1+2 Thruster, 3 Weapon, 5 Armor, 6 Shield (Refinement 10: slot 6 is now a SHIELD
    // hardpoint — a real, carveable shield generator at cell (4,2) so shields follow the generator's
    // health and zero out when it's shot off; mass/cpu/power all stay within the fighter's budget:
    // cpu 5→7 ≤ 8, power_draw 9→15 ≤ 30 supply).
    let mut fit = Fit::new(HULL_FIGHTER);
    let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules);
    let _ = fit.install_module(SlotId(5), MODULE_ARMOR_PLATE, &hull, &modules);
    let _ = fit.install_module(SlotId(6), MODULE_SHIELD_BASIC, &hull, &modules);

    // Build the full-health hit/armor map first, then derive stats against it
    // (E007 BREAKING-CHANGE: derive_ship_stats now reads per-cell health). At full
    // health every module's health-factor is 1.0, so stats match the pre-E007 derive.
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    // Refinement 10: record the authored (full-fit) cell count as the hull-integrity baseline the
    // HUD hull bar divides by, so the bar depletes as the player is carved apart.
    let authored_cells = AuthoredCells(layout.cells.len() as u32);
    // Seed the player ship's E007 defense layers from the same shared helper the demo
    // enemy uses (Principle II) — so the player is damageable on identical rules. With the
    // Refinement-10 shield generator fitted (slot 4), the shield pool is seeded from that
    // module (60 hp / 5 regen), and `recompute_ship_stats_system` keeps `Shields.max`/`regen_rate`
    // synced to the generator's live health — shooting it off zeroes the shields.
    let (shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);

    let Some(ship) = server.ship_entity_for(local_id) else {
        return;
    };
    if let Ok(mut entity) = server.world_mut().get_entity_mut(ship) {
        // FIX (carve location): once the ship is fitted, grow its collision circle to
        // the VISIBLE hull footprint (`hull_collision_radius`, ≈1.76 for the 9×11
        // fighter) — overriding the unfitted `spawn_client_ship` `0.8` — so a shot that
        // visually clips the fitted hull registers, matching what the player sees. (Side
        // effect: the player ship's ram circle vs asteroids is now the same larger
        // footprint, which is consistent with the rendered ship; combat/determinism
        // tests still pass.)
        entity.insert((
            fit,
            stats,
            layout,
            shields,
            section_armor,
            hull_structure,
            CollisionRadius(hull_collision_radius(hull.grid_dims)),
            // Carve-targetable: a live ship is `FitLayout` + `CollisionRadius` +
            // `Destructible`. The flag carries over to the hulk + severed chunks so the
            // player's wreckage stays destructible too (a per-entity toggle for later).
            Destructible,
            // Phase E: the dynamic combat pools — Energy full, Heat cold. `energy_system`
            // re-derives `max`/`regen`/`dissipation` from the live tuning each tick; these are
            // just the spawn seeds (full charge, no heat). Only live ships carry them.
            Energy::seed(stats.power_supply),
            Heat::seed(),
            // Phase F: the afterburner pool (full) + the depleting armor-HP layer (full at
            // `armor_value`). `apply_damage` soaks penetrating hits into ArmorHp before carving
            // the hull, so the hull is protected while armor holds.
            Afterburner::seed(),
            ArmorHp::seed(stats.armor_value),
            authored_cells,
        ));
    }
}

/// `FixedUpdate`: pilot + step the embedded authoritative server one tick.
///
/// Reads the local ship's current `ShipIntent` (written by `input::read_input` in
/// `PreUpdate`), numbers + sends it so the server pilots the ship, steps the
/// embedded server (which applies the input and steps the full sim — motion,
/// collision, weapon, AI, destruction — THIS tick), then drains and discards the
/// loopback inbox so its queue can't grow unbounded (we render from the server
/// world directly in [`capture_render_state`], not from snapshots). No
/// predictor/reconcile/smoother/snapshot-buffer runs on this path.
fn net_fixed_update(
    mut host: NonSendMut<LoopbackHost>,
    mut state: NonSendMut<NetClientState>,
    mut feedback: ResMut<HitFeedback>,
    ship_q: Query<&ShipIntent, With<LocalShip>>,
) {
    // The intent the player is holding this tick (PreUpdate wrote it). Default to
    // neutral if the local ship isn't present yet.
    let intent = ship_q.single().copied().unwrap_or_default();

    // Build + send the numbered client input (TR-007) so the server pilots the
    // local ship through its identical validate-and-apply path (no bypass).
    let server_tick = host.server.server_tick();
    let input: ClientInput = build_client_input(&mut state.sequencer, server_tick, intent);
    let conn = host.conn;
    host.transport
        .send_unreliable(conn, &Message::ClientInput(input));

    // Step the embedded authoritative server: this applies the input and steps the
    // full shared sim (motion + collision + weapon + AI + destruction) one tick, so
    // collision and hits resolve THIS tick and surface in `render_state` below.
    host.server.tick();

    // E007 live-demo feedback surfacing: combat resolves in the embedded SERVER's
    // world, so `fitted_damage_system` sets the SERVER's `HitFeedback` — but the HUD
    // (`hud::update_hud`) reads THIS client app's own `HitFeedback` resource, which
    // the server never updates. Copy the server's feedback into the client resource
    // each tick (`HitFeedback` is `Copy`) so the SHIELD/PEN/RICOCHET/OVERPEN/MISS +
    // HIT/KILL cues actually show when the player shoots the enemy (FR-024).
    *feedback = host.server.hit_feedback();

    // Drain and discard the loopback inbox. We render from the server world
    // directly (`capture_render_state`), so snapshots are unused here; draining
    // keeps the loopback queue from growing unbounded over a long session.
    let _ = host.transport.recv(conn);
}

/// `FixedUpdate`, after [`net_fixed_update`]: reconcile the rendered Bevy entities
/// with the authoritative [`ServerApp::render_state`], keyed by [`EntityId`].
///
/// For each [`RenderEntity`]: roll the matching Bevy entity's [`RenderInterp`]
/// prev→curr and set curr to the server pose. The local ship (mapped under
/// `local_id`) is updated in place — its `Velocity` is set too so the HUD SPD
/// reads the authoritative speed. Any other id is find-or-spawned with the
/// mesh/material for its `kind`+`flags` and tagged [`RemoteEntity`]. Rendered
/// entities whose id is no longer in `render_state` (destroyed targets, expired
/// projectiles) are despawned, so they vanish immediately.
///
/// E007 live-demo visuals: a lazily-spawned cyan **shield deflector flash** child is
/// shown for any rendered ship for the split-second its shield absorbs a hit
/// (`RenderEntity::shield_flash > 0`), its alpha fading with the flash — there is NO
/// persistent bubble and NO hull scale-pulse.
// A Bevy system with the params it genuinely needs (transport host, net state, render
// assets + the material store for the per-bubble alpha fade, fixed dt, the id→entity
// map, and the interp/velocity/shield queries); the count is inherent to the system.
#[allow(clippy::too_many_arguments)]
fn capture_render_state(
    mut commands: Commands,
    mut host: NonSendMut<LoopbackHost>,
    state: NonSend<NetClientState>,
    assets: Res<RenderAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    dt: Res<FixedDt>,
    mut render_map: ResMut<NetRenderMap>,
    mut interp_q: Query<&mut RenderInterp>,
    mut vel_q: Query<&mut Velocity, With<LocalShip>>,
    shield_child_q: Query<&ShieldChild>,
    mut bubble_q: Query<(&mut Visibility, &mut Transform)>,
    mut ship_hull_q: Query<&mut ShipHull>,
    hull_mode: Res<HullRenderMode>,
    module_mode: Res<ModuleColorMode>,
) {
    let local_id = state.local_id;
    let contour = hull_mode.contour;
    let module_color = module_mode.on;
    let entities = host.server.render_state();

    // Phase 1B LOD origin: the local player ship's world position (the follow-camera
    // centres on it). Voxelize ships within `SHIP_VOXEL_LOD_DIST` of it; box the rest.
    // Falls back to the origin if the local ship isn't in this render set yet (startup).
    let lod_origin = entities
        .iter()
        .find(|e| e.id == local_id)
        .map(|e| e.pos)
        .unwrap_or(Vec2::ZERO);

    // Track which ids are present this tick so we can despawn the rest.
    let mut present: std::collections::HashSet<EntityId> = std::collections::HashSet::new();

    for e in &entities {
        present.insert(e.id);

        if let Some(&bevy_entity) = render_map.map.get(&e.id) {
            // Existing rendered entity: roll its interp snapshot prev→curr.
            if let Ok(mut interp) = interp_q.get_mut(bevy_entity) {
                interp.prev_pos = interp.curr_pos;
                interp.prev_heading = interp.curr_heading;
                interp.curr_pos = e.pos;
                interp.curr_heading = e.heading;
            }
            // The local ship feeds the HUD SPD from the authoritative velocity.
            if e.id == local_id {
                if let Ok(mut vel) = vel_q.get_mut(bevy_entity) {
                    vel.0 = e.vel;
                }
            }
            // Refinement 5/6: keep the rendered parent scale right each tick, LOD-aware. The parent
            // transform is shared by the coarse far-LOD placeholder mesh AND (when near) the hull
            // child (`Transform::IDENTITY`, which inherits it), so the correct scale differs by state:
            //   * intact (no cells) → its `RenderScale` box (`e.scale`).
            //   * fitted + NEAR → ONE, so the hull child renders at natural cell size.
            //   * fitted + FAR + big footprint (a STRUCTURE) → the hull footprint, so the unit
            //     placeholder box (restored by `sync_ship_hull` / the original unit structure mesh)
            //     reads at the right ~30-u size instead of a tiny 1-u cube.
            //   * fitted + FAR + small (a ship) → ONE (its `ship_mesh` placeholder is already sized).
            // Skip `Debris` — a fragment owns its per-chunk Transform scale (set at spawn).
            if e.kind != EntityKind::Debris {
                if let Ok((_, mut tf)) = bubble_q.get_mut(bevy_entity) {
                    let footprint =
                        e.grid_dims.0.max(e.grid_dims.1) as f32 * crate::scene::CELL_SIZE;
                    let near = e.pos.distance(lod_origin) <= SHIP_VOXEL_LOD_DIST;
                    tf.scale = if e.cells.is_empty() {
                        e.scale
                    } else if !near && footprint > STRUCTURE_LOD_FOOTPRINT {
                        Vec3::new(
                            e.grid_dims.0 as f32 * crate::scene::CELL_SIZE,
                            e.grid_dims.1 as f32 * crate::scene::CELL_SIZE,
                            2.0,
                        )
                    } else {
                        Vec3::ONE
                    };
                }
            }
            // Revise-B: seamless hull-surface LOD for a fitted ship (non-empty cell payload).
            sync_ship_hull(
                &mut commands,
                &assets,
                &mut meshes,
                &mut ship_hull_q,
                bevy_entity,
                e,
                lod_origin,
                contour,
                module_color,
            );
            // Show + fade (or lazily spawn) this entity's LOCALIZED shield-impact flash
            // from the hit-driven `shield_flash`, placed at the impact (`hit_dir`).
            update_shield_bubble(
                &mut commands,
                &assets,
                &mut materials,
                &shield_child_q,
                &mut bubble_q,
                bevy_entity,
                e.shield_flash,
                e.hit_dir,
                e.heading,
                e.grid_dims,
            );
            // R49 — stamp the ship's throttle so `update_engine_flames` can size its exhaust flames.
            if let Ok(mut ec) = commands.get_entity(bevy_entity) {
                ec.insert(ShipThrottle(e.throttle));
            }
        } else {
            // Newly-appeared entity (never the local ship — it is pre-registered):
            // spawn a rendered remote with the right look. Its interp `prev` is
            // seeded one tick back (`pos − vel·dt`) so it renders FROM where it was
            // a tick ago — for a fresh projectile that is the muzzle, so the bullet
            // visibly travels out of the ship instead of popping in ~a tick ahead
            // (≈ the reticle distance for a fast round).
            let spawned = spawn_render_entity(&mut commands, &assets, e, dt.0);
            // Revise-B: build the merged hull surface immediately if this newly-seen fitted
            // ship is near, so its first rendered frame already shows the solid hull plate.
            sync_ship_hull(
                &mut commands,
                &assets,
                &mut meshes,
                &mut ship_hull_q,
                spawned,
                e,
                lod_origin,
                contour,
                module_color,
            );
            // Seed (if its shield is being hit) its localized impact flash immediately
            // so the first rendered frame already shows it at the impact point.
            update_shield_bubble(
                &mut commands,
                &assets,
                &mut materials,
                &shield_child_q,
                &mut bubble_q,
                spawned,
                e.shield_flash,
                e.hit_dir,
                e.heading,
                e.grid_dims,
            );
            if let Ok(mut ec) = commands.get_entity(spawned) {
                ec.insert(ShipThrottle(e.throttle));
            }
            render_map.map.insert(e.id, spawned);
        }
    }

    // Despawn rendered entities whose id is gone from the authoritative world
    // (destroyed targets, expired projectiles) so they vanish immediately. The
    // local ship is never in this set while it lives, so it is never despawned
    // here; if it ever were destroyed server-side it would correctly disappear.
    render_map.map.retain(|id, entity| {
        if present.contains(id) {
            true
        } else {
            // Free a fitted ship's merged hull `Mesh` handle before despawning it, so the
            // mesh store does not leak a mesh per destroyed ship over a long session (the
            // hull-surface child itself is despawned recursively with the parent below).
            if let Ok(hull) = ship_hull_q.get(*entity) {
                if let Some(mesh) = &hull.mesh {
                    meshes.remove(mesh);
                }
            }
            if let Ok(mut ec) = commands.get_entity(*entity) {
                ec.despawn();
            }
            false
        }
    });
}

/// Spawn a rendered Bevy entity for a newly-appeared [`RenderEntity`], picking the
/// mesh/material by `kind` (+ `flags` for the target sub-kind) from the shared
/// [`RenderAssets`] — the same look the old `net_update` produced. It is tagged
/// [`RemoteEntity`] and given a [`RenderInterp`] whose `prev` is one tick back
/// (`pos − vel·dt`) and `curr` is the current pose, so its first rendered frame
/// interpolates FROM where it was a tick ago. For a freshly-fired projectile that
/// previous position is the muzzle, so the bullet visibly emerges from the ship
/// rather than appearing a tick's travel ahead (≈ the reticle for a fast round).
fn spawn_render_entity(
    commands: &mut Commands,
    assets: &RenderAssets,
    e: &RenderEntity,
    dt: f32,
) -> Entity {
    let (mesh, material) = match e.kind {
        EntityKind::Ship => (assets.ship_mesh.clone(), assets.ship_material.clone()),
        // The wire `EntityKind` only says "Target"; the sub-kind rides in `flags`
        // (set from `TargetKind::as_u8` server-side) so we restore the distinct
        // E002 looks: grey asteroid sphere, green seeker dart, reddish dummy cube
        // (the fallback for an unknown tag).
        EntityKind::Target => match TargetKind::from_u8(e.flags) {
            Some(TargetKind::Asteroid) => (
                assets.asteroid_mesh.clone(),
                assets.asteroid_material.clone(),
            ),
            Some(TargetKind::Seeker) => {
                (assets.seeker_mesh.clone(), assets.seeker_material.clone())
            }
            // Mining-skirmish structures (Phase 1): outpost / transport / central asteroid.
            Some(TargetKind::Outpost) => {
                (assets.outpost_mesh.clone(), assets.outpost_material.clone())
            }
            Some(TargetKind::Transport) => (
                assets.transport_mesh.clone(),
                assets.transport_material.clone(),
            ),
            Some(TargetKind::MineNode) => (
                assets.minenode_mesh.clone(),
                assets.minenode_material.clone(),
            ),
            _ => (assets.dummy_mesh.clone(), assets.dummy_material.clone()),
        },
        EntityKind::Projectile => (
            assets.projectile_mesh.clone(),
            assets.projectile_material.clone(),
        ),
        // FIX 0b: a destroyed ship's severed chunks + hulk render as tinted, tumbling
        // ship-fragment boxes (not grey asteroid spheres).
        EntityKind::Debris => (assets.debris_mesh.clone(), assets.debris_material.clone()),
    };
    // Phase 2 (mining skirmish): override a factioned simple-mesh entity's material with its team
    // colour (red/blue) so friend/foe reads at a glance — the mesh shape still conveys role. `0` =
    // neutral (keeps the base look). Fitted ships render via the voxel hull path, not this material,
    // so they are unaffected (ship faction tint is a later follow-up).
    let material = match e.faction {
        1 => assets.faction_red_material.clone(),
        2 => assets.faction_blue_material.clone(),
        _ => material,
    };

    // A LAYOUT-LESS debris fragment (no cell payload) gets a deterministic, id-derived
    // base rotation (so fragments don't all align) and a scale from the per-chunk size
    // hint in `flags` (residual cell-count) — the generic box fallback. A debris chunk
    // that DOES carry real cells (a severed piece / dead hulk) is rendered as its actual
    // hull mesh by `sync_ship_hull`, so it takes the SAME heading-only, unit-scale
    // transform a ship does (the cells are already correctly shaped/scaled around the
    // chunk `Position` = cell-COM world) — no tumble/scale distortion. Non-debris entities
    // keep the existing heading-only orientation + unit scale (byte-identical to before).
    let transform = if e.kind == EntityKind::Debris && e.cells.is_empty() {
        let (extra_rot, scale) = debris_transform(e.id, e.flags);
        Transform {
            translation: Vec3::new(e.pos.x, e.pos.y, 0.0),
            rotation: Quat::from_rotation_z(e.heading) * extra_rot,
            scale: Vec3::splat(scale),
        }
    } else {
        // `e.scale` is `Vec3::ONE` for everything except the mining structures (transport / outpost /
        // asteroid), which carry a `RenderScale` from `scenario.ron` → their unit mesh is scaled to
        // the authored size. `interpolate_transforms` only rewrites translation+rotation each frame,
        // so this spawn-time scale persists.
        Transform::from_rotation(Quat::from_rotation_z(e.heading))
            .with_translation(Vec3::new(e.pos.x, e.pos.y, 0.0))
            .with_scale(e.scale)
    };

    commands
        .spawn((
            RemoteEntity {
                id: e.id,
                kind: e.kind,
            },
            RenderInterp {
                // A fresh PROJECTILE is captured at its swept-segment tail (`PrevPosition` = the gun
                // muzzle on the spawn tick, R17). Seed its `prev` one tick of the SHOOTER'S velocity
                // behind the muzzle (`inherited_vel`, R19) so its first overstep LAG-MATCHES the
                // (interpolated, one-tick-lagging) ship: it stays glued to the gun for that frame
                // (`bullet − ship_centre = gun_offset`, constant) and then launches straight forward,
                // with no perpendicular "L" slide at speed. (Seeding plain `e.pos` left a `drift·dt`
                // sideways jog; `e.pos − e.vel·dt` drew a phantom a full muzzle-tick behind.) Other
                // kinds keep the one-tick back-seed so a newly-seen mover eases in.
                prev_pos: if e.kind == EntityKind::Projectile {
                    e.pos - e.inherited_vel * dt
                } else {
                    e.pos - e.vel * dt
                },
                curr_pos: e.pos,
                prev_heading: e.heading,
                curr_heading: e.heading,
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            transform,
        ))
        .id()
}

/// Revise-B seamless-hull LOD + rebuild-on-change for one rendered ship `parent`.
///
/// Decides the ship's LOD from its distance to `lod_origin` (the local player ship):
/// **near** (`<= `[`SHIP_VOXEL_LOD_DIST`]) → render as ONE merged seamless hull-surface
/// mesh ([`build_hull_mesh`]) under a single child, wearing the single uniform
/// [`RenderAssets::hull_material`] — so the ship reads as a solid steel plate with NO
/// visible cells; **far** → render as the single coarse [`RenderAssets::ship_mesh`] box
/// (the parent's own mesh). The parent transform, `LocalShip`/`RemoteEntity` markers,
/// camera-follow, gunsight, and HUD all live on the parent and are untouched — only its
/// VISUAL (own box mesh vs. hull-surface child) changes.
///
/// A non-fitted entity (empty `cells`) is a no-op — it keeps its single mesh (asteroids,
/// projectiles, a layout-less wreck, an unfitted ship are never given a hull surface).
///
/// **Wreckage path (severed chunk / dead hulk).** A [`EntityKind::Debris`] entity that
/// carries cells is wreckage: it flows through this SAME path so it renders as its REAL
/// severed cells (correct shape/size/scale, reusing [`build_hull_mesh`]) instead of a
/// generic box. Two things differ from a live ship: (1) the merged surface wears the
/// darkened "dead metal" [`RenderAssets::wreck_hull_material`] so it reads as debris, and
/// (2) the cells are centred on the wreck's **cell-COM** (via [`hull_mesh_center`]) — the
/// chunk's `Position` is that COM in world — not the grid centre, so the cells sit exactly
/// where the piece broke off and drifted to. Its LOD-far fallback is the tinted debris box.
///
/// LOD switch is clean both ways: near → drop the parent's own `Mesh3d`/`MeshMaterial3d`
/// (so it stops drawing the box) and spawn the hull-surface child; far → despawn the child,
/// FREE its `Mesh` handle from [`Assets<Mesh>`] (no leak), and restore the box mesh
/// (ship box for a ship, tinted debris box for a wreck).
///
/// **Rebuild-on-cell-set-change hook (Phase-2 erosion seam):** while near, the present cell
/// set's cheap hash ([`cells_hash`]) is compared to the one the current mesh was built from
/// ([`ShipHull::cells_hash`]); the merged mesh is rebuilt ONLY when it changes. In revise-B
/// (no destruction) the set never changes, so the mesh builds **once on first sight** and
/// the per-tick check is a no-op. In Phase 2 a carved cell drops from `FitLayout.cells` → it
/// drops from `e.cells` → the hash changes → the mesh is rebuilt with the hole (and new
/// breach-edge walls) and the old `Mesh` handle is freed. That rebuild is also where Phase 2
/// will pass a real `exposed` predicate into [`build_hull_mesh`] so a breach-exposed module
/// cell can be tinted (today modules are hidden under the uniform hull color).
#[allow(clippy::too_many_arguments)]
fn sync_ship_hull(
    commands: &mut Commands,
    assets: &RenderAssets,
    meshes: &mut Assets<Mesh>,
    ship_hull_q: &mut Query<&mut ShipHull>,
    parent: Entity,
    e: &RenderEntity,
    lod_origin: Vec2,
    contour: bool,
    module_color: bool,
) {
    // Only a fitted ship OR a severed chunk / dead hulk carries a cell payload; everything
    // else (projectiles, plain targets) keeps its single mesh and is handled by the
    // empty-cells bail below (after the hull-state is resolved). A FITTED entity carved to
    // ZERO cells — a hard ram obliterates the whole hull, so `destroy_ship` keeps an empty
    // residual `FitLayout` and `render_state` re-emits it as `Debris` with no cells — is NOT
    // bailed here: it still has a built voxel hull child that must be TORN DOWN so the
    // destruction actually renders (otherwise the last intact mesh stays frozen on screen,
    // the R16 bug). The `!has_cells && !voxelized` bail below skips only the never-built case.
    let has_cells = !e.cells.is_empty();

    // A `Debris` entity with cells is WRECKAGE (a severed chunk or a destroyed-ship hulk):
    // render its real cells with the darkened "dead metal" wreck tint and centre the cells
    // on their own cell-COM (the chunk's `Position` is that COM in world). A live ship
    // renders with the live hull material, centred on the GRID CENTRE (its `Position` sits
    // there) — byte-identical to before. The LOD-far box fallback also differs: a wreck
    // falls back to the tumbling debris box, a ship to its coarse ship box.
    let is_wreck = e.kind == EntityKind::Debris;
    // Module coloring in the VOXEL look paints each cell's quads with its module hue as a vertex
    // color, which only shows under a WHITE-base material (StandardMaterial multiplies vertex ×
    // base_color). The CONTOUR look keeps the normal tinted hull material and shows modules via a
    // separate overlay child instead, so it always uses the plain material.
    let voxel_colored = module_color && !contour;
    let hull_mat = match (is_wreck, voxel_colored) {
        (true, true) => assets.wreck_hull_material_white.clone(),
        (true, false) => assets.wreck_hull_material.clone(),
        (false, true) => assets.hull_material_white.clone(),
        // Plain voxel look: tint a live factioned hull by its team (Refinement 5) so the carveable
        // structures + ships read red/blue; `0` (neutral / no faction) keeps the default steel. The
        // module-colour view + wrecks keep their own materials above.
        (false, false) => match e.faction {
            1 => assets.faction_red_hull_material.clone(),
            2 => assets.faction_blue_hull_material.clone(),
            _ => assets.hull_material.clone(),
        },
    };
    let center = hull_mesh_center(e);

    let near = e.pos.distance(lod_origin) <= SHIP_VOXEL_LOD_DIST;
    // A big STRUCTURE (its hull footprint exceeds the ship range) renders its near hull as a CHUNKED
    // mesh — split into tiles so a carve rebuilds only the touched tile, not its whole ~8k-cell hull.
    // Ships + wreck chunks keep the single merged-mesh path below.
    let footprint = e.grid_dims.0.max(e.grid_dims.1) as f32 * CELL_SIZE;
    let big_structure = !is_wreck && footprint > STRUCTURE_LOD_FOOTPRINT;

    // Resolve (or initialize) this parent's hull tracking. A freshly-spawned parent has no
    // `ShipHull` yet (commands are deferred), so seed a default and attach it at the end.
    let mut new_state = None;
    let mut current = match ship_hull_q.get_mut(parent) {
        Ok(c) => HullView::Existing(c),
        Err(_) => {
            new_state = Some(ShipHull::default());
            HullView::New(new_state.as_mut().unwrap())
        }
    };

    // Nothing to draw AND nothing built to tear down (a projectile / plain target / a fresh
    // fitted entity that never voxelized) — bail exactly as the old top-of-fn early-return
    // did. The synthesized `new_state` (if any) is dropped, never attached, so a cell-less
    // entity never gains a `ShipHull` (byte-identical to before).
    if !has_cells && !current.voxelized() {
        return;
    }

    // A fitted entity with cells (and near) builds/rebuilds its hull. An OBLITERATED one
    // (empty cells but a previously-built voxel hull) skips this and falls through to the
    // `else if current.voxelized()` teardown branch below — so its destruction renders (the
    // frozen hull child is despawned + the wreck box restored) instead of freezing intact.
    if near && has_cells {
        if !current.voxelized() {
            // Far→near switch: stop drawing the parent's coarse box so only the hull
            // surface shows (the parent keeps its transform + markers).
            if let Ok(mut ec) = commands.get_entity(parent) {
                ec.remove::<(Mesh3d, MeshMaterial3d<StandardMaterial>)>();
            }
            current.set_voxelized(true);
        }

        if big_structure {
            // Chunked path: rebuild only the tiles a carve touched. Structures are all-structural →
            // always the plain faction-tinted voxel look (no module-colour / contour variants).
            let struct_mat = match e.faction {
                1 => assets.faction_red_hull_material.clone(),
                2 => assets.faction_blue_hull_material.clone(),
                _ => assets.hull_material.clone(),
            };
            chunked_hull_update(
                commands,
                meshes,
                parent,
                e,
                center,
                &struct_mat,
                current.tiles_mut(),
            );
        } else {
            // --- ship / wreck single-merged-mesh path (unchanged) ---
            // Cheap order-independent hash of the present `(col, row, kind)` set — the rebuild trigger.
            let hash = cells_hash(e);

            // Build on first sight, REBUILD when the cell set changed (Phase-2 erosion), when the
            // render-style toggle flipped (voxel ↔ contour), OR when the module-color toggle flipped
            // (same cells, new look) — all "same parent, new mesh" cases.
            let needs_build = current.child().is_none()
                || current.cells_hash() != hash
                || current.built_contour() != contour
                || current.built_module_color() != module_color;
            if needs_build {
                // Free the previous mesh + child (rebuild path) so meshes/entities don't leak.
                if let Some(old) = current.take_mesh() {
                    meshes.remove(&old);
                }
                if let Some(old_child) = current.take_child() {
                    if let Ok(mut ec) = commands.get_entity(old_child) {
                        ec.despawn();
                    }
                }
                // The contour module-marker overlay is rebuilt with the hull (its inputs — cells,
                // style, color — are exactly the rebuild triggers), so always tear the old one down.
                if let Some(old) = current.take_module_overlay_mesh() {
                    meshes.remove(&old);
                }
                if let Some(old_child) = current.take_module_overlay_child() {
                    if let Ok(mut ec) = commands.get_entity(old_child) {
                        ec.despawn();
                    }
                }
                // R47 — the hard-surface fixtures rebuild with the hull (same cells/style triggers),
                // so tear the old ones down first.
                free_fixtures(commands, meshes, current.hull_mut());

                // Merge the present cells into ONE seamless hull surface + add it to the mesh store,
                // centred on `center` (grid centre for a ship, cell-COM for a wreck) so the cells sit
                // around the parent `Position`.
                let cell_tuples: Vec<(u16, u16, u8)> =
                    e.cells.iter().map(|c| (c.col, c.row, c.kind)).collect();
                // The runtime toggles pick the look: smoothed rounded contour (Fix #11 M2) vs the
                // blocky per-cell voxel mesh. Both share the same `center`-relative local frame, so
                // the child sits identically under the parent transform either way. In the voxel look,
                // `module_color` paints per-cell vertex colors (shown by the white-base material above).
                let raw_mesh = if contour {
                    build_hull_mesh_contour(&cell_tuples, CELL_SIZE, center)
                } else {
                    build_hull_mesh(&cell_tuples, CELL_SIZE, center, module_color)
                };
                let mesh = meshes.add(raw_mesh);

                // R48/R49 — the COMBAT look (a live ship, plain voxel mesh) wears the cinematic
                // ExtendedMaterial hull (fresnel rim + panels + grime), faction-tinted. Wrecks, the
                // module-colour inspection view, and the contour mesh keep plain StandardMaterial. The
                // child is fully despawned+rebuilt (never component-swapped), so the differing material
                // component types never collide on one entity.
                let use_ext = !is_wreck && !module_color && !contour;
                let child = if use_ext {
                    let ext = match e.faction {
                        1 => assets.hull_ext_red.clone(),
                        2 => assets.hull_ext_blue.clone(),
                        _ => assets.hull_ext_neutral.clone(),
                    };
                    commands
                        .spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(ext),
                            Transform::IDENTITY,
                        ))
                        .id()
                } else {
                    commands
                        .spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(hull_mat.clone()),
                            Transform::IDENTITY,
                        ))
                        .id()
                };
                if let Ok(mut ec) = commands.get_entity(parent) {
                    ec.add_child(child);
                }
                current.set_child(Some(child));
                current.set_mesh(Some(mesh));
                current.set_cells_hash(hash);
                current.set_built_contour(contour);
                current.set_built_module_color(module_color);

                // Module-color OVERLAY (the "second layer", contour look only): thin colored markers
                // on module cells over the smooth hull. `None` when the chunk has no module cells.
                if contour && module_color {
                    if let Some(overlay_raw) =
                        build_module_overlay_mesh(&cell_tuples, CELL_SIZE, center)
                    {
                        let overlay_mesh = meshes.add(overlay_raw);
                        let overlay_child = commands
                            .spawn((
                                Mesh3d(overlay_mesh.clone()),
                                MeshMaterial3d(assets.module_overlay_material.clone()),
                                Transform::IDENTITY,
                            ))
                            .id();
                        if let Ok(mut ec) = commands.get_entity(parent) {
                            ec.add_child(overlay_child);
                        }
                        current.set_module_overlay_child(Some(overlay_child));
                        current.set_module_overlay_mesh(Some(overlay_mesh));
                    }
                }

                // R47/R48 — hard-surface FIXTURES (the 3D ship parts): built for the normal COMBAT
                // look (a live ship, not the module-colour inspection overlay). Each role-tagged mesh
                // (gunmetal greebles / warm glow / nav lights / faction accent) is one child wearing
                // the matching shared material; the Accent role is tinted by the ship's faction. A
                // wreck stays bare debris; the module-colour debug view keeps the raw cell look.
                if !is_wreck && !module_color {
                    // R49 — nav/running lights only on BIGGER ships (transports etc.), not nimble
                    // fighters: the player fighter (footprint ~3.5) + corvette (~4.8) are excluded;
                    // `NAV_MIN_FOOTPRINT` (~6.0) is the cut. (Transports render via the chunked path
                    // and don't get fixtures yet — a follow-up.)
                    let nav_ok = footprint >= NAV_MIN_FOOTPRINT;
                    let (fixtures, thrusters) =
                        build_ship_fixtures(&cell_tuples, CELL_SIZE, center);
                    for (raw, role) in fixtures {
                        if matches!(
                            role,
                            FixtureRole::NavRed | FixtureRole::NavGreen | FixtureRole::NavWhite
                        ) && !nav_ok
                        {
                            continue;
                        }
                        let mat = match role {
                            FixtureRole::Metal => assets.fixture_metal_material.clone(),
                            FixtureRole::Glow => assets.fixture_glow_material.clone(),
                            FixtureRole::NavRed => assets.nav_red_material.clone(),
                            FixtureRole::NavGreen => assets.nav_green_material.clone(),
                            FixtureRole::NavWhite => assets.nav_white_material.clone(),
                            FixtureRole::Accent => match e.faction {
                                1 => assets.accent_red_material.clone(),
                                2 => assets.accent_blue_material.clone(),
                                _ => assets.accent_neutral_material.clone(),
                            },
                        };
                        let mh = meshes.add(raw);
                        let child = commands
                            .spawn((Mesh3d(mh.clone()), MeshMaterial3d(mat), Transform::IDENTITY))
                            .id();
                        if let Ok(mut ec) = commands.get_entity(parent) {
                            ec.add_child(child);
                        }
                        let h = current.hull_mut();
                        h.fixture_children.push(child);
                        h.fixture_meshes.push(mh);
                    }
                    // R49 — one throttle-reactive engine FLAME child per thruster (shared cone mesh, so
                    // NOT pushed to `fixture_meshes`; hidden until `update_engine_flames` shows it). It
                    // rides in `fixture_children` so a carved-off thruster drops its flame on rebuild.
                    for origin in thrusters {
                        let flame = commands
                            .spawn((
                                EngineFlame { origin },
                                Mesh3d(assets.engine_flame_mesh.clone()),
                                MeshMaterial3d(assets.engine_flame_material.clone()),
                                Transform::from_translation(origin),
                                Visibility::Hidden,
                            ))
                            .id();
                        if let Ok(mut ec) = commands.get_entity(parent) {
                            ec.add_child(flame);
                        }
                        current.hull_mut().fixture_children.push(flame);
                    }
                }
            }
        }
    } else if current.voxelized() {
        // Near→far switch (or a big structure that went far): despawn the hull child(ren) + overlay +
        // any chunk tiles, free their meshes, restore the coarse placeholder box.
        for (_, tile) in std::mem::take(current.tiles_mut()) {
            free_hull_tile(commands, meshes, tile);
        }
        if let Some(old) = current.take_mesh() {
            meshes.remove(&old);
        }
        if let Some(old_child) = current.take_child() {
            if let Ok(mut ec) = commands.get_entity(old_child) {
                ec.despawn();
            }
        }
        if let Some(old) = current.take_module_overlay_mesh() {
            meshes.remove(&old);
        }
        if let Some(old_child) = current.take_module_overlay_child() {
            if let Ok(mut ec) = commands.get_entity(old_child) {
                ec.despawn();
            }
        }
        // R47 — drop the hard-surface fixture children too (near→far / destruction).
        free_fixtures(commands, meshes, current.hull_mut());
        if let Ok(mut ec) = commands.get_entity(parent) {
            // Restore the coarse far-LOD placeholder: a wreck → the tinted debris box; a big STRUCTURE
            // → the UNIT `lod_box_mesh` (the parent transform scales it to the hull footprint, so it
            // reads at the right ~30-u size — Refinement 6) wearing the faction-tinted hull material;
            // a ship → its coarse `ship_mesh` (already ship-sized at scale ONE). (Rarely hit — the
            // demo keeps combatants near.)
            let footprint = e.grid_dims.0.max(e.grid_dims.1) as f32 * CELL_SIZE;
            if is_wreck {
                ec.insert((
                    Mesh3d(assets.debris_mesh.clone()),
                    MeshMaterial3d(assets.debris_material.clone()),
                ));
            } else if footprint > STRUCTURE_LOD_FOOTPRINT {
                // Refinement 12: the ROUND rock uses its sphere placeholder (the parent transform
                // scales it to a flat ~60-u disc ≈ the near voxel hull) so it stays round at any
                // zoom; the square outpost/transport keep the box. Same colour (`hull_mat`) either
                // way, so the near↔far swap is seamless.
                let placeholder =
                    if matches!(TargetKind::from_u8(e.flags), Some(TargetKind::MineNode)) {
                        assets.minenode_mesh.clone()
                    } else {
                        assets.lod_box_mesh.clone()
                    };
                ec.insert((Mesh3d(placeholder), MeshMaterial3d(hull_mat.clone())));
            } else {
                ec.insert((
                    Mesh3d(assets.ship_mesh.clone()),
                    MeshMaterial3d(assets.ship_material.clone()),
                ));
            }
        }
        current.set_voxelized(false);
    }

    // If we built a fresh `ShipHull` for a newly-spawned parent, attach it now.
    if let Some(state) = new_state {
        if let Ok(mut ec) = commands.get_entity(parent) {
            ec.insert(state);
        }
    }
}

/// The cell-space layout origin for [`build_hull_mesh`] — the point the entity's cells are
/// drawn around so the silhouette sits on the parent `Position`.
///
/// - A **ship / fitted target** (`EntityKind::Ship`) sits at its hull GRID CENTRE
///   `(cols·0.5, rows·0.5)` — its `Position` is the grid centre — so passing the grid
///   centre reproduces the classic grid-centred ship layout exactly (byte-identical).
/// - A **severed chunk / dead hulk** (`EntityKind::Debris` with cells) sits at its
///   **cell-COM** — the sim set its `Position` to the world COM of just these cells at the
///   instant of severing ([`sim::damage::sever_chunk`]). So passing the mean of its received
///   cells' centres (`mean(col+0.5, row+0.5)`, matching the sim's `local_com`) lays the
///   cells out around that `Position`, i.e. exactly where they were on the hull, now
///   drifting. (A dead hulk keeps the grid centre via this same mean only if it still owns
///   the whole grid; in practice its `Position` is the ship's last grid-centre position and
///   its residual cells are the full remaining silhouette, so the mean ≈ grid centre and it
///   reads correctly either way.)
fn hull_mesh_center(e: &RenderEntity) -> Vec2 {
    // Fix #6: a wreck carries a FROZEN cell-space anchor (captured at sever / death). Lay its
    // cells out around THAT fixed point so carving a cell does NOT recompute the centre and
    // visibly shift the whole piece. Absent (a live ship, or a degenerate layout-less wreck)
    // → the classic reference below.
    if let Some(anchor) = e.mesh_anchor {
        return anchor;
    }
    if e.kind == EntityKind::Debris {
        // Mean cell centre of just this wreck's cells — matches the sim's `local_com` so the
        // cells render around the chunk `Position` (= their world COM). (Fallback only; real
        // wreckage carries a frozen `mesh_anchor` above so it does not drift as it erodes.)
        let n = e.cells.len().max(1) as f32;
        let sum = e.cells.iter().fold(Vec2::ZERO, |acc, c| {
            acc + Vec2::new(c.col as f32 + 0.5, c.row as f32 + 0.5)
        });
        sum / n
    } else {
        // Ship: the hull grid centre (its `Position` sits here) — classic layout.
        Vec2::new(e.grid_dims.0 as f32 * 0.5, e.grid_dims.1 as f32 * 0.5)
    }
}

/// Cheap order-independent hash of a fitted ship's present `(col, row, kind)` cell set —
/// the rebuild-on-change trigger for [`sync_ship_hull`]. XOR-folds a per-cell scramble so
/// the result is independent of the payload's iteration order (it already arrives row-major,
/// but XOR makes the check robust). Equal hash ⇒ same hull ⇒ no rebuild; in revise-B it is
/// constant per ship (no carving), so the merged mesh is built exactly once.
fn cells_hash(e: &RenderEntity) -> u64 {
    let mut acc: u64 = e.cells.len() as u64;
    for c in &e.cells {
        acc ^= hash_one_cell(c.col, c.row, c.kind);
    }
    acc
}

/// Scramble one `(col, row, kind)` cell key — XOR-folded by the callers so cell order doesn't matter.
fn hash_one_cell(col: u16, row: u16, kind: u8) -> u64 {
    let key = ((col as u64) << 24) ^ ((row as u64) << 8) ^ (kind as u64);
    key.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Order-independent hash of a `(col, row, kind)` tuple slice (one chunk tile's cells).
fn tile_cells_hash(cells: &[(u16, u16, u8)]) -> u64 {
    let mut acc: u64 = cells.len() as u64;
    for &(col, row, kind) in cells {
        acc ^= hash_one_cell(col, row, kind);
    }
    acc
}

/// Chunked-mesh tile edge, in cells. A big structure's hull is split into `HULL_TILE × HULL_TILE`
/// tiles so a carve rebuilds only the tile(s) it touched (≤ `HULL_TILE²` cells), not the whole hull.
const HULL_TILE: u16 = 16;

/// Free a chunk tile's mesh handle + despawn its child entity (no leak).
fn free_hull_tile(commands: &mut Commands, meshes: &mut Assets<Mesh>, tile: HullTile) {
    if let Some(mesh) = tile.mesh {
        meshes.remove(&mesh);
    }
    if let Some(child) = tile.child {
        if let Ok(mut ec) = commands.get_entity(child) {
            ec.despawn();
        }
    }
}

/// R47/R48 — despawn + free ALL the hard-surface fixture children (greebles, glow, nav lights,
/// accents) tracked on a [`ShipHull`]. Called on a hull rebuild (so a carve refreshes the parts) and
/// on near→far / teardown.
fn free_fixtures(commands: &mut Commands, meshes: &mut Assets<Mesh>, h: &mut ShipHull) {
    for m in h.fixture_meshes.drain(..) {
        meshes.remove(&m);
    }
    for c in h.fixture_children.drain(..) {
        if let Ok(mut ec) = commands.get_entity(c) {
            ec.despawn();
        }
    }
}

/// **Chunked hull rebuild** (the chunked-mesh optimization, for a big STRUCTURE only): split the
/// present cells into `HULL_TILE`-cell tiles and rebuild ONLY the tiles whose cell set changed since
/// last tick (plus drop tiles fully carved away), instead of rebuilding the whole ~8k-cell hull every
/// carve. Each tile is one [`build_hull_mesh`] child under `parent` at `Transform::IDENTITY` (so it
/// inherits the parent pose), sharing `center` so the tiles line up into the seamless hull. Material
/// is the structure's faction tint (structures are all-structural → no module-colour/contour look).
fn chunked_hull_update(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    parent: Entity,
    e: &RenderEntity,
    center: Vec2,
    material: &Handle<StandardMaterial>,
    tiles: &mut std::collections::BTreeMap<(u16, u16), HullTile>,
) {
    // Group the present cells into tiles.
    let mut by_tile: std::collections::BTreeMap<(u16, u16), Vec<(u16, u16, u8)>> =
        std::collections::BTreeMap::new();
    for c in &e.cells {
        by_tile
            .entry((c.col / HULL_TILE, c.row / HULL_TILE))
            .or_default()
            .push((c.col, c.row, c.kind));
    }
    // Drop tiles that no longer have any cells (fully carved away).
    let stale: Vec<(u16, u16)> = tiles
        .keys()
        .copied()
        .filter(|k| !by_tile.contains_key(k))
        .collect();
    for key in stale {
        if let Some(tile) = tiles.remove(&key) {
            free_hull_tile(commands, meshes, tile);
        }
    }
    // Build/rebuild only the tiles whose cell set changed (or are new).
    for (key, cells) in by_tile {
        let hash = tile_cells_hash(&cells);
        if tiles
            .get(&key)
            .is_some_and(|t| t.hash == hash && t.child.is_some())
        {
            continue; // unchanged — keep the existing tile mesh
        }
        if let Some(old) = tiles.remove(&key) {
            free_hull_tile(commands, meshes, old);
        }
        let mesh = meshes.add(build_hull_mesh(&cells, CELL_SIZE, center, false));
        let child = commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
            ))
            .id();
        if let Ok(mut ec) = commands.get_entity(parent) {
            ec.add_child(child);
        }
        tiles.insert(
            key,
            HullTile {
                hash,
                mesh: Some(mesh),
                child: Some(child),
            },
        );
    }
}

/// Holds either the live `&mut ShipHull` from the query or a freshly-built one for a parent
/// spawned THIS tick (whose component is still a deferred command). Lets [`sync_ship_hull`]
/// treat both uniformly, then attach the new one at the end.
enum HullView<'a> {
    Existing(Mut<'a, ShipHull>),
    New(&'a mut ShipHull),
}

impl HullView<'_> {
    /// R47 — direct `&mut ShipHull` for the fixture fields (avoids a getter/setter per field).
    fn hull_mut(&mut self) -> &mut ShipHull {
        match self {
            HullView::Existing(c) => c,
            HullView::New(c) => c,
        }
    }
    fn voxelized(&self) -> bool {
        match self {
            HullView::Existing(c) => c.voxelized,
            HullView::New(c) => c.voxelized,
        }
    }
    fn set_voxelized(&mut self, v: bool) {
        match self {
            HullView::Existing(c) => c.voxelized = v,
            HullView::New(c) => c.voxelized = v,
        }
    }
    fn child(&self) -> Option<Entity> {
        match self {
            HullView::Existing(c) => c.child,
            HullView::New(c) => c.child,
        }
    }
    fn set_child(&mut self, v: Option<Entity>) {
        match self {
            HullView::Existing(c) => c.child = v,
            HullView::New(c) => c.child = v,
        }
    }
    fn take_child(&mut self) -> Option<Entity> {
        match self {
            HullView::Existing(c) => c.child.take(),
            HullView::New(c) => c.child.take(),
        }
    }
    /// The per-tile chunked-hull map (big structures only; empty for ships).
    fn tiles_mut(&mut self) -> &mut std::collections::BTreeMap<(u16, u16), HullTile> {
        match self {
            HullView::Existing(c) => &mut c.tiles,
            HullView::New(c) => &mut c.tiles,
        }
    }
    fn set_mesh(&mut self, v: Option<Handle<Mesh>>) {
        match self {
            HullView::Existing(c) => c.mesh = v,
            HullView::New(c) => c.mesh = v,
        }
    }
    fn take_mesh(&mut self) -> Option<Handle<Mesh>> {
        match self {
            HullView::Existing(c) => c.mesh.take(),
            HullView::New(c) => c.mesh.take(),
        }
    }
    fn cells_hash(&self) -> u64 {
        match self {
            HullView::Existing(c) => c.cells_hash,
            HullView::New(c) => c.cells_hash,
        }
    }
    fn set_cells_hash(&mut self, v: u64) {
        match self {
            HullView::Existing(c) => c.cells_hash = v,
            HullView::New(c) => c.cells_hash = v,
        }
    }
    fn built_contour(&self) -> bool {
        match self {
            HullView::Existing(c) => c.built_contour,
            HullView::New(c) => c.built_contour,
        }
    }
    fn set_built_contour(&mut self, v: bool) {
        match self {
            HullView::Existing(c) => c.built_contour = v,
            HullView::New(c) => c.built_contour = v,
        }
    }
    fn built_module_color(&self) -> bool {
        match self {
            HullView::Existing(c) => c.built_module_color,
            HullView::New(c) => c.built_module_color,
        }
    }
    fn set_built_module_color(&mut self, v: bool) {
        match self {
            HullView::Existing(c) => c.built_module_color = v,
            HullView::New(c) => c.built_module_color = v,
        }
    }
    fn set_module_overlay_child(&mut self, v: Option<Entity>) {
        match self {
            HullView::Existing(c) => c.module_overlay_child = v,
            HullView::New(c) => c.module_overlay_child = v,
        }
    }
    fn take_module_overlay_child(&mut self) -> Option<Entity> {
        match self {
            HullView::Existing(c) => c.module_overlay_child.take(),
            HullView::New(c) => c.module_overlay_child.take(),
        }
    }
    fn set_module_overlay_mesh(&mut self, v: Option<Handle<Mesh>>) {
        match self {
            HullView::Existing(c) => c.module_overlay_mesh = v,
            HullView::New(c) => c.module_overlay_mesh = v,
        }
    }
    fn take_module_overlay_mesh(&mut self) -> Option<Handle<Mesh>> {
        match self {
            HullView::Existing(c) => c.module_overlay_mesh.take(),
            HullView::New(c) => c.module_overlay_mesh.take(),
        }
    }
}

/// Deterministic per-fragment orientation + scale for a [`EntityKind::Debris`] chunk
/// (FIX 0b). The base rotation is derived from the chunk's stable wire `id` so each
/// fragment is angled differently (they don't all align), and the scale grows with the
/// `size_hint` (residual cell-count packed into `flags` server-side, clamped to
/// `0.7..=1.6` so a single-cell sliver and a multi-cell wing both read sensibly). Pure
/// + deterministic: the same id/hint always yields the same look.
fn debris_transform(id: EntityId, size_hint: u8) -> (Quat, f32) {
    // Hash the id into a stable angle in `[0, 2π)` — a cheap integer scramble, not RNG,
    // so the orientation is reproducible and frame-stable.
    let scrambled = id.0.wrapping_mul(2_654_435_761);
    let angle = (scrambled as f32 / u32::MAX as f32) * std::f32::consts::TAU;
    // Vary the tumble axis a little by id too, so fragments don't all spin about Z.
    let tilt = ((id.0.wrapping_mul(40_503) & 0xFF) as f32 / 255.0 - 0.5) * 0.6;
    let rot = Quat::from_rotation_z(angle) * Quat::from_rotation_x(tilt);
    // Size hint ≥ 1; map to a modest scale band so fragments differ without ballooning.
    let cells = size_hint.max(1) as f32;
    let scale = (0.6 + 0.12 * cells).clamp(0.7, 1.6);
    (rot, scale)
}

/// Show + fade a rendered ship's **localized shield-impact flash** as a sleek glowing
/// energy crescent of the shield ring, centred on the bullet impact bearing and SCALED to
/// hug the ship's hull (FIX 0a polish) — REPLACES the earlier impact-point sphere and the
/// old full-ship deflector bubble.
///
/// There is NO persistent ring, NO whole-ship bloom, and NO pulsing: a soft cyan **energy
/// crescent** (the normalized annular sliver from [`crate::scene::build_arc_band_mesh`],
/// tapered to a white-hot core by its vertex colors, blended ADDITIVELY) lights the slice
/// facing the point the shot struck the still-up shield, appearing ONLY for the
/// split-second of the hit and fading out over the flash window. Its overall brightness is
/// `shield_flash` (1.0 at impact → 0.0 as the timer bleeds out), applied as the material
/// `base_color` alpha that multiplies the per-vertex gradient.
///
/// **Sized to the hull.** The mesh is normalized (outer radius `1.0`), so the child gets a
/// uniform **scale** of [`shield_radius_for`]`(grid_dims)` — derived from the ship's
/// footprint plus [`SHIELD_MARGIN`] — making the band hug any hull (fighter 9×11, corvette
/// 13×15) just outside the silhouette. The impact bearing is derived from the world
/// `hit_dir` (centre→impact, world space): rotating it into the ship-local frame by
/// `-heading` (the child inherits the ship's rotation) and taking that vector's angle. The
/// child stays **centred on the ship** (local translation `Vec3::ZERO`), is **rotated about
/// Z** to that bearing, and is **scaled** to the shield radius. When `hit_dir == Vec2::ZERO`
/// (no recent shield hit) the flash is hidden regardless of `shield_flash`.
///
/// - **No child yet**: lazily spawn the crescent ONCE as a CHILD of `parent` (so it
///   follows + inherits the ship's rotation), with its OWN cloned material (so its alpha
///   can fade independently of other ships'), hidden. It is despawned automatically with
///   its parent (Bevy despawns children recursively), so a destroyed ship's flash vanishes
///   with the hulk. Plain practice targets / unshielded entities never get hit on the
///   shield, so their child simply stays hidden (alpha 0).
/// - **Child exists**: toggle [`Visibility`] — visible while `shield_flash > 0` AND
///   `hit_dir != 0`, hidden otherwise — set its local rotation to the impact bearing and
///   its scale to the per-ship shield radius, and set the cloned material's `base_color`
///   alpha to `shield_flash` so the crescent fades over the ~0.25 s window.
// One Bevy helper threading exactly the data the flash needs (commands, assets, the
// material store for the per-flash alpha fade, the child link + the child's
// visibility/transform query, the parent, and the flash/dir/heading/grid-dims inputs).
#[allow(clippy::too_many_arguments)]
fn update_shield_bubble(
    commands: &mut Commands,
    assets: &RenderAssets,
    materials: &mut Assets<StandardMaterial>,
    shield_child_q: &Query<&ShieldChild>,
    bubble_q: &mut Query<(&mut Visibility, &mut Transform)>,
    parent: Entity,
    shield_flash: f32,
    hit_dir: Vec2,
    heading: f32,
    grid_dims: (u16, u16),
) {
    let flash = shield_flash.clamp(0.0, 1.0);
    // Visible only while flashing AND there is a real impact direction.
    let show = flash > 0.0 && hit_dir != Vec2::ZERO;
    // World→ship-local: the child inherits the ship's rotation, so undo the heading.
    // The crescent (centred on +X local) is rotated about Z to this bearing so its lit
    // slice faces the impact; its radii live in the mesh, so no translation is needed.
    let local_dir = Vec2::from_angle(-heading).rotate(hit_dir);
    let bearing = local_dir.to_angle();
    // Per-ship scale of the normalized (outer-radius-1.0) crescent so it hugs the hull
    // (z-scale is irrelevant — the band is flat in XY).
    let radius = shield_radius_for(grid_dims);
    let local_tf = Transform::from_rotation(Quat::from_rotation_z(bearing))
        .with_scale(Vec3::new(radius, radius, 1.0));

    match shield_child_q.get(parent) {
        Ok(child) => {
            // Existing flash child: toggle visibility, orient + scale it to the hull, and
            // fade its alpha to the flash.
            if let Ok((mut vis, mut tf)) = bubble_q.get_mut(child.entity) {
                *vis = if show {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                *tf = local_tf;
            }
            if let Some(material) = materials.get_mut(&child.material) {
                // Moderate cyan base; alpha follows the flash so the additive glow fades.
                // The per-vertex colors carry the white-hot core / blue rim gradient.
                material.base_color = Color::srgba(0.45, 0.8, 1.0, flash);
            }
        }
        Err(_) => {
            // No child yet: lazily spawn one (hidden) with its OWN material instance —
            // cloned from the shared prototype (`assets.shield_material`, the single
            // source of the cyan impact look) so its alpha can fade independently of
            // other ships'. Spawned once for any rendered ship; it stays hidden (alpha
            // 0) until that ship's shield is actually hit.
            let material = materials
                .get(&assets.shield_material)
                .cloned()
                .map(|m| materials.add(m))
                .unwrap_or_else(|| {
                    materials.add(StandardMaterial {
                        base_color: Color::srgba(0.45, 0.8, 1.0, 0.0),
                        emissive: LinearRgba::rgb(0.2, 0.7, 1.2),
                        alpha_mode: AlphaMode::Add,
                        cull_mode: None,
                        double_sided: true,
                        ..default()
                    })
                });
            let bubble = commands
                .spawn((
                    ShieldBubble,
                    Mesh3d(assets.shield_arc_mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    // Local (child) transform: centred on the ship (ZERO translation),
                    // rotated about Z to face the impact bearing, and scaled to the
                    // per-ship shield radius (updated each tick once the child exists).
                    local_tf,
                    if show {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    },
                ))
                .id();
            if let Ok(mut ec) = commands.get_entity(parent) {
                ec.add_child(bubble);
                ec.insert(ShieldChild {
                    entity: bubble,
                    material,
                });
            }
        }
    }
}

/// R49 — drive every per-thruster engine FLAME each frame from its parent ship's [`ShipThrottle`]: the
/// flame (a shared additive cone child, anchored at the thruster's nozzle-mouth `origin`) is oriented
/// aft (`-X`, base at the nozzle) and scaled in length by throttle, hidden when idle. Replaces the old
/// single rear-centre velocity-driven exhaust. Tunable scale via [`ShipVisualTuning`].
pub fn update_engine_flames(
    tuning: Res<crate::ShipVisualTuning>,
    ships: Query<&ShipThrottle>,
    mut flames: Query<(&EngineFlame, &ChildOf, &mut Transform, &mut Visibility)>,
) {
    for (flame, parent, mut tf, mut vis) in &mut flames {
        let throttle = ships.get(parent.parent()).map(|t| t.0).unwrap_or(0.0);
        let show = throttle > 0.03;
        *vis = if show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if !show {
            continue;
        }
        // Length grows with throttle; the cone (Bevy `Cone` axis +Y) is rotated +Y→-X so it trails aft,
        // base at the nozzle `origin`, tip `length` further aft.
        let length = CELL_SIZE * tuning.flame_length * (0.35 + throttle);
        let width = CELL_SIZE * tuning.flame_width;
        *tf = Transform::from_translation(flame.origin - Vec3::new(length * 0.5, 0.0, 0.0))
            .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::new(width, length, width));
    }
}

/// The loopback registry address (any address works for loopback — it is a
/// switch key, not a real socket).
fn loopback_addr() -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}
