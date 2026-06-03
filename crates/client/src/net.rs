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
use server::{RenderEntity, ServerApp, PROTOCOL_VERSION};
use sim::components::{CollisionRadius, Destructible, TargetKind, Velocity};
use sim::damage::seed_defense_layers;
use sim::fitting::{
    build_layout, derive_ship_stats, hull_collision_radius, seed_catalogs, Fit, SlotId,
    HULL_FIGHTER, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
};
use sim::{FixedDt, HitFeedback, ShipIntent};

use crate::input::{build_client_input, InputSequencer};
use crate::render_sync::{
    interpolate_transforms, RemoteEntity, RenderInterp, ShieldBubble, ShieldChild, ShipHull,
};
use crate::scene::{build_hull_mesh, RenderAssets, CELL_SIZE};

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
fn setup_loopback_host(world: &mut World) {
    let (mut server, mut transport) = ServerApp::loopback();
    // Populate the authoritative world with the demo targets BEFORE the handshake
    // tick below, so they exist the first time `capture_render_state` reads the
    // server world.
    server.spawn_demo_world();
    // E007 live-demo: spawn fitted enemies the player can shoot apart so the whole
    // damage pipeline (typed hits → module degrade → section sever → drifting chunks
    // → wreck) is visible live. One is placed directly ahead of the player (which
    // starts at the origin facing +x), a second offset for variety. Both are
    // stationary, slowly-spinning, fully-defended `Target`+`FitLayout` entities the
    // E007 `fitted_damage_system` resolves hits against (see
    // `ServerApp::spawn_fitted_enemy`). They are NOT in `spawn_demo_world` (tests
    // depend on its contents) — they are demo-only.
    server.spawn_fitted_enemy(Vec2::new(14.0, 0.0));
    server.spawn_fitted_enemy(Vec2::new(18.0, 6.0));
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
    // 0 Reactor, 1+2 Thruster, 3 Weapon.
    let mut fit = Fit::new(HULL_FIGHTER);
    let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules);

    // Build the full-health hit/armor map first, then derive stats against it
    // (E007 BREAKING-CHANGE: derive_ship_stats now reads per-cell health). At full
    // health every module's health-factor is 1.0, so stats match the pre-E007 derive.
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    // Seed the player ship's E007 defense layers from the same shared helper the demo
    // enemy uses (Principle II) — so the player is damageable on identical rules. The
    // fighter has no Shield hardpoint, so the shield is the default pool from
    // `seed_defense_layers`. Nothing fires at the player yet (enemy AI fire is a
    // follow-on), but the layer state is now complete and live.
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
) {
    let local_id = state.local_id;
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
            // Revise-B: seamless hull-surface LOD for a fitted ship (non-empty cell payload).
            sync_ship_hull(
                &mut commands,
                &assets,
                &mut meshes,
                &mut ship_hull_q,
                bevy_entity,
                e,
                lod_origin,
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
        Transform::from_rotation(Quat::from_rotation_z(e.heading))
            .with_translation(Vec3::new(e.pos.x, e.pos.y, 0.0))
    };

    commands
        .spawn((
            RemoteEntity {
                id: e.id,
                kind: e.kind,
            },
            RenderInterp {
                // `prev` one tick back (the muzzle for a fresh projectile) so the
                // first rendered frame interpolates out from the ship.
                prev_pos: e.pos - e.vel * dt,
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
fn sync_ship_hull(
    commands: &mut Commands,
    assets: &RenderAssets,
    meshes: &mut Assets<Mesh>,
    ship_hull_q: &mut Query<&mut ShipHull>,
    parent: Entity,
    e: &RenderEntity,
    lod_origin: Vec2,
) {
    // Only a fitted ship OR a severed chunk / dead hulk carries a cell payload; everything
    // else (projectiles, plain targets, a layout-less wreck) keeps its single mesh.
    if e.cells.is_empty() {
        return;
    }

    // A `Debris` entity with cells is WRECKAGE (a severed chunk or a destroyed-ship hulk):
    // render its real cells with the darkened "dead metal" wreck tint and centre the cells
    // on their own cell-COM (the chunk's `Position` is that COM in world). A live ship
    // renders with the live hull material, centred on the GRID CENTRE (its `Position` sits
    // there) — byte-identical to before. The LOD-far box fallback also differs: a wreck
    // falls back to the tumbling debris box, a ship to its coarse ship box.
    let is_wreck = e.kind == EntityKind::Debris;
    let hull_mat = if is_wreck {
        assets.wreck_hull_material.clone()
    } else {
        assets.hull_material.clone()
    };
    let center = hull_mesh_center(e);

    let near = e.pos.distance(lod_origin) <= SHIP_VOXEL_LOD_DIST;

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

    if near {
        // Cheap order-independent hash of the present `(col, row, kind)` set — the
        // rebuild trigger.
        let hash = cells_hash(e);

        if !current.voxelized() {
            // Far→near switch: stop drawing the parent's coarse box so only the hull
            // surface shows (the parent keeps its transform + markers).
            if let Ok(mut ec) = commands.get_entity(parent) {
                ec.remove::<(Mesh3d, MeshMaterial3d<StandardMaterial>)>();
            }
            current.set_voxelized(true);
        }

        // Build on first sight, or REBUILD when the cell set changed (Phase-2 erosion).
        let needs_build = current.child().is_none() || current.cells_hash() != hash;
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

            // Merge the present cells into ONE seamless hull surface + add it to the mesh
            // store, centred on `center` (grid centre for a ship, cell-COM for a wreck) so
            // the cells sit around the parent `Position`. Modules stay hidden:
            // `build_hull_mesh` uses one uniform material (Phase 2 will pass an `exposed`
            // predicate to reveal breach cells). A wreck wears the dead-metal tint.
            let cell_tuples: Vec<(u16, u16, u8)> =
                e.cells.iter().map(|c| (c.col, c.row, c.kind)).collect();
            let mesh = meshes.add(build_hull_mesh(&cell_tuples, CELL_SIZE, center));

            let child = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(hull_mat.clone()),
                    Transform::IDENTITY,
                ))
                .id();
            if let Ok(mut ec) = commands.get_entity(parent) {
                ec.add_child(child);
            }
            current.set_child(Some(child));
            current.set_mesh(Some(mesh));
            current.set_cells_hash(hash);
        }
    } else if current.voxelized() {
        // Near→far switch: despawn the hull child, free its mesh, restore the coarse box.
        if let Some(old) = current.take_mesh() {
            meshes.remove(&old);
        }
        if let Some(old_child) = current.take_child() {
            if let Ok(mut ec) = commands.get_entity(old_child) {
                ec.despawn();
            }
        }
        if let Ok(mut ec) = commands.get_entity(parent) {
            // Restore the coarse far-LOD mesh: a wreck → the tinted debris box, a ship →
            // the coarse ship box. (The demo keeps combatants near, so this path is rare.)
            if is_wreck {
                ec.insert((
                    Mesh3d(assets.debris_mesh.clone()),
                    MeshMaterial3d(assets.debris_material.clone()),
                ));
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
    if e.kind == EntityKind::Debris {
        // Mean cell centre of just this wreck's cells — matches the sim's `local_com` so the
        // cells render around the chunk `Position` (= their world COM).
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
        // Pack (col, row, kind) and scramble; XOR-fold so order does not matter.
        let key = ((c.col as u64) << 24) ^ ((c.row as u64) << 8) ^ (c.kind as u64);
        acc ^= key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    acc
}

/// Holds either the live `&mut ShipHull` from the query or a freshly-built one for a parent
/// spawned THIS tick (whose component is still a deferred command). Lets [`sync_ship_hull`]
/// treat both uniformly, then attach the new one at the end.
enum HullView<'a> {
    Existing(Mut<'a, ShipHull>),
    New(&'a mut ShipHull),
}

impl HullView<'_> {
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

/// The loopback registry address (any address works for loopback — it is a
/// switch key, not a real socket).
fn loopback_addr() -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}
