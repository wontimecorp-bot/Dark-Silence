//! The networked client plugin (T045, OBJ4) — wires the netcode lifecycle into
//! the Bevy schedule and makes `cargo run -p client` a runnable single-player
//! experience over the in-memory loopback transport (Principle VII).
//!
//! [`NetClientPlugin`] adds, per the OBJ3/OBJ4 lifecycle:
//! - **FixedUpdate** ([`net_fixed_update`]): build + send the numbered
//!   [`protocol::ClientInput`] for this tick ([`crate::input::build_client_input`]),
//!   step the embedded server (loopback solo play), drain received messages,
//!   [`reconcile`](crate::prediction::Predictor::reconcile) the local ship against
//!   the newest snapshot, and push remote snapshots into the [`SnapshotBuffer`].
//! - **Update** ([`net_update`]): interpolate every remote entity from the
//!   snapshot buffer ([`SnapshotBuffer::interpolate_remotes`]) into its rendered
//!   `Transform`, and apply the smoothed reconciliation correction
//!   ([`RenderSmoother`]) to the **local** ship's rendered pose.
//!
//! The LOCAL ship renders from the predicted state + smoothed correction; REMOTE
//! entities render from interpolation (AD-005). The E002 gunsight pip and follow
//! camera continue to track the local ship's rendered `Transform`, so their feel
//! is intact.
//!
//! **Transport seam:** the plugin holds its transport + embedded server behind a
//! [`NonSend`] resource ([`LoopbackHost`]) — loopback is single-threaded
//! (`Rc`-backed), so it is a non-send resource by construction. Defaulting to an
//! embedded [`server::ServerApp::loopback`] is the solo-play path; the same
//! FixedUpdate systems drive a renet-backed transport once it is swapped in
//! (Phase 4) — only the host resource changes, not the lifecycle systems.

use bevy::prelude::*;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, EntityKind, LoopbackTransport, Message,
    NetTransport, Snapshot, SnapshotAck, CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::components::{TargetKind, Velocity};
use sim::ShipIntent;

use crate::input::{build_client_input, InputSequencer};
use crate::interpolation::{DeltaReconstructor, SnapshotBuffer};
use crate::prediction::{InputBuffer, NumberedInput, Predictor, RenderSmoother, ShipInit};
use crate::render_sync::RemoteEntity;
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

/// The client-side netcode state, held together so the FixedUpdate/Update systems
/// can advance the whole lifecycle. A `NonSend` resource so it can live alongside
/// the `Rc`-backed loopback host without a `Send + Sync` bound (the predicted
/// `World` itself is `Send`, but co-locating keeps the solo-play wiring simple).
pub struct NetClientState {
    /// The local-ship predicted simulation (predict + reconcile, OBJ3).
    pub predictor: Predictor,
    /// Unacknowledged numbered inputs awaiting reconciliation (TR-007/027).
    pub input_buffer: InputBuffer,
    /// Monotonic input numbering + redundant tail for the wire (TR-007).
    pub sequencer: InputSequencer,
    /// Smooths the rendered local ship toward each reconciled state (TR-033).
    pub smoother: RenderSmoother,
    /// Received-snapshot ring feeding remote-entity interpolation (TR-010/027).
    /// Stores **reconstructed full** snapshots (delta-applied), so interpolation
    /// and reconciliation consume full state unchanged (T063).
    pub snapshots: SnapshotBuffer,
    /// Client-side delta reconstruction (T063): folds each received delta onto the
    /// running acked baseline (or a keyframe re-baselines), producing the full
    /// snapshot the buffer + reconcile consume and the id to ack.
    pub reconstructor: DeltaReconstructor,
    /// This client's authoritative ship id, learned at handshake (so the local
    /// ship is reconciled and excluded from interpolation, AD-005).
    pub local_id: EntityId,
    /// Server tick rate (Hz) for the predicted fixed step + interp timeline.
    pub tick_rate_hz: u16,
    /// Interpolation delay (ms) the client renders remotes behind real time
    /// (TR-010/044).
    pub interp_delay_ms: f64,
    /// The interpolation-timeline clock in milliseconds, advanced one snapshot-rate
    /// step's worth of real time per FixedUpdate. Drives the render time
    /// `now_ms − interp_delay_ms`.
    pub now_ms: f64,
}

/// Marker for the local player's rendered ship entity in the Bevy world. The
/// local ship renders from the predicted state + smoothed correction (AD-005), so
/// it is treated distinctly from interpolated remotes.
#[derive(Component)]
pub struct LocalShip;

/// The networked client plugin (T045). Adds the netcode lifecycle systems and, by
/// default, embeds a loopback server so the client is runnable solo
/// (Principle VII). Add it after [`DefaultPlugins`] and the fixed-step clock.
pub struct NetClientPlugin;

impl Plugin for NetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_loopback_host)
            // The netcode lifecycle: build+send input, step server, recv,
            // reconcile, buffer remote snapshots — all in the fixed step so it is
            // tied to the authoritative tick, not the render frame.
            .add_systems(FixedUpdate, net_fixed_update)
            // Per-frame: interpolate remotes + apply the smoothed local correction.
            .add_systems(Update, net_update);
    }
}

/// `Startup`: stand up the embedded loopback server, populate its authoritative
/// world with the demo targets, connect the client, run the handshake to learn
/// this client's ship id and the announced rates, and seed the client netcode
/// state.
///
/// The scene (`scene::setup_scene`) owns the [`LocalShip`] tag now — it spawns the
/// local ship with the tag deterministically, so this system no longer tags it
/// (the old `With<Ship>` tagging here raced scene setup in `Startup` and, if it
/// ran first, never tagged the ship → frozen local ship, dead controls).
fn setup_loopback_host(world: &mut World) {
    let (mut server, mut transport) = ServerApp::loopback();
    // Populate the authoritative world with the demo targets BEFORE the handshake
    // tick below, so the very first snapshot the client receives already carries
    // them (they then render as interpolated remotes via `net_update`).
    server.spawn_demo_world();
    let conn = NetTransport::connect(&mut transport, loopback_addr());

    // Handshake: send Connect, tick the server once so it accepts + replies, then
    // read the ConnectAccepted to learn our ship id and the session rates.
    transport.send_reliable(
        conn,
        &Message::Connect(Connect {
            protocol_version: PROTOCOL_VERSION,
            client_token: [0u8; CLIENT_TOKEN_BYTES],
        }),
    );
    server.tick();

    let mut local_id = None;
    let mut tick_rate_hz = server.rates().tick_rate_hz;
    let mut interp_delay_ms = server.rates().interp_delay_ms as f64;
    for msg in transport.recv(conn) {
        if let Message::ConnectAccepted(accepted) = msg {
            local_id = Some(accepted.client_id);
            tick_rate_hz = accepted.tick_rate_hz;
            interp_delay_ms = accepted.interp_delay_ms as f64;
        }
    }
    let local_id = local_id.expect("loopback handshake yields a ConnectAccepted");

    let dt = 1.0 / tick_rate_hz as f32;
    let state = NetClientState {
        predictor: Predictor::new(ShipInit::default(), dt),
        input_buffer: InputBuffer::new(),
        sequencer: InputSequencer::new(),
        smoother: RenderSmoother::new(),
        snapshots: SnapshotBuffer::new(tick_rate_hz),
        reconstructor: DeltaReconstructor::new(),
        local_id,
        tick_rate_hz,
        interp_delay_ms,
        now_ms: 0.0,
    };

    world.insert_non_send_resource(LoopbackHost {
        server,
        transport,
        conn,
    });
    world.insert_non_send_resource(state);
}

/// `FixedUpdate`: advance the full netcode lifecycle one authoritative tick.
///
/// Reads the local ship's current `ShipIntent` (written by `input::read_input` in
/// `PreUpdate`), numbers + sends it, steps the embedded server, drains the inbox,
/// reconciles the local ship against the newest received snapshot, and buffers
/// every remote snapshot for interpolation. Loopback solo play steps the server
/// inline here; a renet host would instead rely on a remote server's ticks.
fn net_fixed_update(
    mut host: NonSendMut<LoopbackHost>,
    mut state: NonSendMut<NetClientState>,
    ship_q: Query<&ShipIntent, With<LocalShip>>,
) {
    // The intent the player is holding this tick (PreUpdate wrote it). Default to
    // neutral if the local ship isn't present yet.
    let intent = ship_q.single().copied().unwrap_or_default();

    // --- Build + send the numbered client input (TR-007). --------------------
    let server_tick = host.server.server_tick();
    let input: ClientInput = build_client_input(&mut state.sequencer, server_tick, intent);
    let seq = input.seq;
    let newest_intent = input.inputs.first().copied();
    let conn = host.conn;
    host.transport
        .send_unreliable(conn, &Message::ClientInput(input));

    // --- Predict the local ship immediately (no round-trip, SC-001). ---------
    if let Some(qi) = newest_intent {
        // Destructure so the predictor and its input buffer can be borrowed
        // mutably at once (`predict` records the input into the buffer, TR-007).
        let st = &mut *state;
        st.predictor
            .predict(&mut st.input_buffer, NumberedInput { seq, intent: qi });
    }

    // --- Step the embedded authoritative server (loopback solo play). --------
    host.server.tick();

    // --- Drain the inbox: reconstruct deltas, reconcile local, buffer remotes. -
    let local_id = state.local_id;
    let messages = host.transport.recv(conn);
    let mut newest_snapshot: Option<Snapshot> = None;
    for msg in messages {
        if let Message::Snapshot(delta) = msg {
            // T063: reconstruct the FULL state from baseline + delta before it
            // feeds interpolation / reconciliation. An unreconstructable delta
            // (server deltaed against a baseline we don't hold) is dropped; the
            // server keyframes / re-deltas until an ack catches up.
            let Some(reconstructed) = state.reconstructor.reconstruct(&delta) else {
                continue;
            };
            // Ack the reconstructed snapshot so the server advances its per-client
            // delta baseline to the state we now hold (lost-ack → server keyframes,
            // T064). The ack is unreliable (it may itself be lost — harmless, the
            // server just keeps deltaing against the prior baseline).
            host.transport.send_unreliable(
                conn,
                &Message::SnapshotAck(SnapshotAck {
                    last_snapshot_id: reconstructed.ack_id,
                }),
            );
            // Buffer the FULL snapshot for remote interpolation (stale/dup gated,
            // TR-037).
            let full = reconstructed.full;
            let applied = state.snapshots.push(full.clone());
            if applied {
                // Track the newest applied snapshot to reconcile the local ship
                // against (the authoritative anchor, TR-009).
                newest_snapshot = Some(full);
            }
        }
    }
    if let Some(snapshot) = newest_snapshot {
        // Where the local ship is currently rendered (predicted + residual offset),
        // captured before reconcile so the smoother can blend from it.
        let previously_rendered = state.predictor.ship_state().pos + state.smoother.offset();
        // Destructure so the predictor and its input buffer borrow independently.
        let NetClientState {
            predictor,
            input_buffer,
            smoother,
            ..
        } = &mut *state;
        predictor.reconcile(&snapshot, local_id, input_buffer);
        let reconciled = predictor.ship_state().pos;
        smoother.observe_correction(previously_rendered, reconciled);
    }

    // Advance the interpolation clock by one tick's worth of real time. Remotes
    // are rendered `interp_delay_ms` behind this.
    let tick_ms = 1000.0 / state.tick_rate_hz as f64;
    state.now_ms += tick_ms;
}

/// `Update` (per render frame): drive the rendered transforms from the netcode.
///
/// - The **local** ship renders from the predicted state with the smoothed
///   reconciliation correction applied ([`RenderSmoother::step`]), so a
///   correction never teleports it (TR-033). The follow camera + gunsight pip
///   (E002) read this same `Transform`, so their behavior is intact.
/// - Every **remote** entity renders from [`SnapshotBuffer::interpolate_remotes`]
///   at render time `now_ms − interp_delay_ms` (TR-010). Remote visuals are
///   spawned/despawned to match the interpolated set.
pub fn net_update(
    mut commands: Commands,
    mut state: NonSendMut<NetClientState>,
    assets: Res<RenderAssets>,
    mut local_q: Query<(&mut Transform, &mut Velocity), (With<LocalShip>, Without<RemoteEntity>)>,
    mut remote_q: Query<(Entity, &RemoteEntity, &mut Transform), Without<LocalShip>>,
) {
    // --- Local ship: predicted pose + smoothed correction (AD-005). ----------
    let predicted = state.predictor.ship_state();
    let rendered = state.smoother.step(predicted.pos);
    if let Ok((mut tf, mut vel)) = local_q.single_mut() {
        tf.translation.x = rendered.x;
        tf.translation.y = rendered.y;
        tf.rotation = Quat::from_rotation_z(predicted.heading);
        // Feed the HUD: the predicted velocity drives the SPD readout (the scene
        // ship has no local sim stepping its `Velocity`, so prediction is the
        // authoritative-for-render source).
        vel.0 = predicted.vel;
    }

    // --- Remote entities: interpolate from the snapshot buffer (TR-010). -----
    let now_ms = state.now_ms;
    let interp_delay_ms = state.interp_delay_ms;
    let local_id = state.local_id;
    let remotes = state
        .snapshots
        .interpolate_remotes(now_ms, interp_delay_ms, local_id);

    // Update existing remote visuals; track which ids were updated this frame.
    let mut updated: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (entity, remote, mut tf) in &mut remote_q {
        if let Some(interp) = remotes.iter().find(|r| r.id == remote.id) {
            tf.translation.x = interp.pos.x;
            tf.translation.y = interp.pos.y;
            tf.rotation = Quat::from_rotation_z(interp.heading);
            updated.insert(remote.id.0);
        } else {
            // No longer in the interpolated set → despawn cleanly (TR-010 clean
            // disappear).
            commands.entity(entity).despawn();
        }
    }

    // Spawn a mesh-bearing visual for any newly-appeared remote not yet
    // represented, picking the mesh/material by `EntityKind` from the shared
    // render assets so remote ships/targets/projectiles are actually VISIBLE
    // (the previous meshless marker spawn rendered nothing). Subsequent frames
    // update the existing remote's `Transform` above and despawn it when it
    // leaves the interpolated set.
    for e in &remotes {
        if updated.contains(&e.id.0) {
            continue;
        }
        let (mesh, material) = match e.kind {
            EntityKind::Ship => (assets.ship_mesh.clone(), assets.ship_material.clone()),
            // The wire `EntityKind` only says "Target"; the sub-kind rides in
            // `flags` (set from `TargetKind::as_u8` server-side) so we restore the
            // distinct E002 looks: grey asteroid sphere, green seeker dart, reddish
            // dummy cube (the fallback for an unknown tag).
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
        commands.spawn((
            RemoteEntity {
                id: e.id,
                kind: e.kind,
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::from_rotation(Quat::from_rotation_z(e.heading))
                .with_translation(Vec3::new(e.pos.x, e.pos.y, 0.0)),
        ));
    }
}

/// The loopback registry address (any address works for loopback — it is a
/// switch key, not a real socket).
fn loopback_addr() -> std::net::SocketAddr {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}
