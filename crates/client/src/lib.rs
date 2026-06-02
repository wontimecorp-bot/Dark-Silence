//! Dark Silence game client library — E002 single-player flight & combat slice,
//! extended in E003 with client-side prediction & reconciliation (OBJ3, AD-005).
//!
//! A **thin Bevy shell** over the shared `sim` crate (ADR-0013): rendering,
//! keyboard input, the HUD, and scene setup only. All gameplay state and logic
//! (motion, collision, weapon, combat, AI) live in `sim` as headless
//! `bevy_ecs` systems; this client advances them in `FixedUpdate`
//! (`Time<Fixed>`, 60 Hz) decoupled from rendering, and interpolates the
//! rendered `Transform`s between fixed steps (FR-004, Principle II/VII).
//!
//! The crate is a **library + thin binary**: the gameplay-adjacent logic
//! (input numbering, prediction, reconciliation) lives in the library so the
//! integration tests under `tests/` can drive it headlessly (no window), while
//! `main.rs` is a one-line shell that calls [`run`].

// Bevy systems use tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

pub mod camera;
pub mod hud;
pub mod input;
pub mod interpolation;
pub mod net;
pub mod prediction;
pub mod render_sync;
pub mod scene;

use bevy::prelude::*;
use net::NetClientPlugin;
use sim::{FixedDt, HitFeedback, Tuning};

/// The Bevy `FixedUpdate` cadence the netcode lifecycle runs at. It MUST match
/// the server-announced authoritative tick rate (TR-044, default **30 Hz**):
/// [`net::net_fixed_update`] steps the embedded server and the predictor **once
/// per FixedUpdate**, each advancing one server tick's worth of sim, so a
/// mismatched (e.g. 60 Hz) cadence would step the 30 Hz sim twice per real tick —
/// 2× game speed and a doubled input rate that desyncs prediction. Rendering is
/// decoupled and runs per-frame in `Update` at the display rate.
pub const TICK_HZ: f64 = 30.0;

/// Build and run the Bevy client app (the windowed single-player shell, T045).
///
/// E003 OBJ4 wires the netcode into the live render path via
/// [`net::NetClientPlugin`]: it embeds a loopback [`server::ServerApp`] and steps
/// it each FixedUpdate, so this is a runnable single-player experience over the
/// in-memory transport (Principle VII). The LOCAL ship renders from client-side
/// prediction + the smoothed reconciliation correction; REMOTE entities render
/// from snapshot interpolation. Keyboard input (E002) still writes the local
/// ship's `ShipIntent` each frame; the plugin numbers + sends it and predicts.
pub fn run() -> AppExit {
    App::new()
        .add_plugins(DefaultPlugins)
        // Fixed-step clock the netcode lifecycle systems run on, and the matching
        // dt the shared sim reads (the embedded server uses its announced rate).
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(FixedDt((1.0 / TICK_HZ) as f32))
        .insert_resource(Tuning::default())
        .insert_resource(HitFeedback::default())
        .add_systems(
            Startup,
            (scene::setup_scene, camera::setup_camera, hud::setup_hud),
        )
        // Input runs before the fixed step so intents apply the same frame; the
        // net plugin reads the local ship's `ShipIntent` in FixedUpdate.
        .add_systems(PreUpdate, (input::read_input, input::toggle_assist))
        // The netcode lifecycle (build+send input, step server, recv, reconcile,
        // buffer remote snapshots in FixedUpdate; interpolate + smooth in Update).
        .add_plugins(NetClientPlugin)
        // Per-frame rendering: attach projectile visuals, then the camera/pip read
        // the local ship's (predicted+smoothed) transform that `net_update` wrote.
        .add_systems(
            Update,
            (
                (
                    render_sync::add_projectile_visuals,
                    render_sync::update_aim_pip,
                    camera::follow_camera,
                )
                    .chain()
                    .after(net::net_update),
                camera::zoom_camera,
                hud::update_hud,
            ),
        )
        .run()
}
