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
use sim::components::{TargetKind, Velocity};
use sim::ShipIntent;

use crate::input::{build_client_input, InputSequencer};
use crate::render_sync::{interpolate_transforms, RemoteEntity, RenderInterp};
use crate::scene::RenderAssets;

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
fn capture_render_state(
    mut commands: Commands,
    mut host: NonSendMut<LoopbackHost>,
    state: NonSend<NetClientState>,
    assets: Res<RenderAssets>,
    mut render_map: ResMut<NetRenderMap>,
    mut interp_q: Query<&mut RenderInterp>,
    mut vel_q: Query<&mut Velocity, With<LocalShip>>,
) {
    let local_id = state.local_id;
    let entities = host.server.render_state();

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
        } else {
            // Newly-appeared entity (never the local ship — it is pre-registered):
            // spawn a rendered remote with the right look, snapped to its pose.
            let spawned = spawn_render_entity(&mut commands, &assets, e);
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
/// [`RemoteEntity`] and given a [`RenderInterp`] snapped to its current pose (no
/// interpolation on its first frame), so [`interpolate_transforms`] drives it.
fn spawn_render_entity(commands: &mut Commands, assets: &RenderAssets, e: &RenderEntity) -> Entity {
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
    };
    commands
        .spawn((
            RemoteEntity {
                id: e.id,
                kind: e.kind,
            },
            RenderInterp::snapped(e.pos, e.heading),
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_rotation(Quat::from_rotation_z(e.heading))
                .with_translation(Vec3::new(e.pos.x, e.pos.y, 0.0)),
        ))
        .id()
}

/// The loopback registry address (any address works for loopback — it is a
/// switch key, not a real socket).
fn loopback_addr() -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}
