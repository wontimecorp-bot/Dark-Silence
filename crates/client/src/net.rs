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
use sim::damage::seed_defense_layers;
use sim::fitting::{
    build_layout, derive_ship_stats, seed_catalogs, Fit, SlotId, HULL_FIGHTER, MODULE_AUTOCANNON,
    MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
};
use sim::{FixedDt, HitFeedback, ShipIntent};

use crate::input::{build_client_input, InputSequencer};
use crate::render_sync::{
    interpolate_transforms, RemoteEntity, RenderInterp, ShieldBubble, ShieldChild,
};
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
        entity.insert((fit, stats, layout, shields, section_armor, hull_structure));
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
    dt: Res<FixedDt>,
    mut render_map: ResMut<NetRenderMap>,
    mut interp_q: Query<&mut RenderInterp>,
    mut vel_q: Query<&mut Velocity, With<LocalShip>>,
    shield_child_q: Query<&ShieldChild>,
    mut bubble_q: Query<(&mut Visibility, &mut Transform)>,
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
            );
        } else {
            // Newly-appeared entity (never the local ship — it is pre-registered):
            // spawn a rendered remote with the right look. Its interp `prev` is
            // seeded one tick back (`pos − vel·dt`) so it renders FROM where it was
            // a tick ago — for a fresh projectile that is the muzzle, so the bullet
            // visibly travels out of the ship instead of popping in ~a tick ahead
            // (≈ the reticle distance for a fast round).
            let spawned = spawn_render_entity(&mut commands, &assets, e, dt.0);
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

    // FIX 0b: a debris fragment gets a deterministic, id-derived base rotation (so
    // fragments don't all align) and a scale from the per-chunk size hint in `flags`
    // (residual cell-count). `interpolate_transforms` then drives its drift + spin from
    // the inherited COM momentum + heading. Non-debris entities keep the existing
    // heading-only orientation + unit scale (byte-identical to before).
    let transform = if e.kind == EntityKind::Debris {
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

/// Distance from the ship centre to the shield surface where the localized impact
/// flash sits, in sim units (≈ the deflector radius). FIX 0a.
const SHIELD_FLASH_RADIUS: f32 = 1.5;

/// Show + fade a rendered ship's **localized shield-impact flash** at the bullet
/// impact point (FIX 0a) — REPLACES the old full-ship deflector bubble.
///
/// There is NO persistent bubble, NO whole-ship bloom, and NO pulsing: a SMALL bright
/// cyan disc/sphere sits on the shield surface AT the point the shot struck the
/// still-up shield, appearing ONLY for the split-second of the hit and fading out over
/// the flash window. Its alpha is `shield_flash` (1.0 at impact → 0.0 as the timer
/// bleeds out). The impact point is derived from the world `hit_dir` (centre→impact,
/// world space): rotating it into the ship-local frame by `-heading` (the child
/// inherits the ship's rotation) and placing the child at `local_dir *
/// SHIELD_FLASH_RADIUS`. When `hit_dir == Vec2::ZERO` (no recent shield hit) the flash
/// is hidden regardless of `shield_flash`.
///
/// - **No child yet**: lazily spawn the small sphere ONCE as a CHILD of `parent` (so it
///   follows + inherits the ship's rotation), with its OWN cloned material (so its
///   alpha can fade independently of other ships'), hidden. It is despawned
///   automatically with its parent (Bevy despawns children recursively), so a destroyed
///   ship's flash vanishes with the hulk. Plain practice targets / unshielded entities
///   never get hit on the shield, so their child simply stays hidden (alpha 0).
/// - **Child exists**: toggle [`Visibility`] — visible while `shield_flash > 0` AND
///   `hit_dir != 0`, hidden otherwise — set its local translation to the impact point
///   on the shield surface, and set the cloned material's `base_color` alpha to
///   `shield_flash` so the flash fades over the ~0.25 s window.
// One Bevy helper threading exactly the data the flash needs (commands, assets, the
// material store for the per-flash alpha fade, the child link + the child's
// visibility/transform query, the parent, and the flash/dir/heading inputs).
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
) {
    let flash = shield_flash.clamp(0.0, 1.0);
    // Visible only while flashing AND there is a real impact direction.
    let show = flash > 0.0 && hit_dir != Vec2::ZERO;
    // World→ship-local: the child inherits the ship's rotation, so undo the heading.
    let local_dir = Vec2::from_angle(-heading).rotate(hit_dir);
    let local_pos = local_dir * SHIELD_FLASH_RADIUS;

    match shield_child_q.get(parent) {
        Ok(child) => {
            // Existing flash child: toggle visibility, move it to the impact point, and
            // fade its alpha to the flash.
            if let Ok((mut vis, mut tf)) = bubble_q.get_mut(child.entity) {
                *vis = if show {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                };
                tf.translation = Vec3::new(local_pos.x, local_pos.y, 0.0);
            }
            if let Some(material) = materials.get_mut(&child.material) {
                // Bright cyan spark; alpha follows the flash so the impact glow fades.
                material.base_color = Color::srgba(0.4, 0.75, 1.0, flash);
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
                        base_color: Color::srgba(0.4, 0.75, 1.0, 0.0),
                        emissive: LinearRgba::rgb(0.2, 0.7, 1.2),
                        alpha_mode: AlphaMode::Blend,
                        ..default()
                    })
                });
            let bubble = commands
                .spawn((
                    ShieldBubble,
                    Mesh3d(assets.shield_impact_mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    // Local (child) transform: seeded at the impact point on the shield
                    // surface (updated each tick once the child exists).
                    Transform::from_translation(Vec3::new(local_pos.x, local_pos.y, 0.0)),
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
