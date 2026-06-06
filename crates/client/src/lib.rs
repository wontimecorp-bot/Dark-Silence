//! Dark Silence game client library — E002 single-player flight & combat slice,
//! extended in E003 with the netcode lifecycle (prediction, reconciliation,
//! interpolation, snapshots) for *remote* multiplayer (OBJ3, AD-005).
//!
//! A **thin Bevy shell** over the shared `sim` crate (ADR-0013): rendering,
//! keyboard input, the HUD, and scene setup only. All gameplay state and logic
//! (motion, collision, weapon, combat, AI) live in `sim` as headless `bevy_ecs`
//! systems.
//!
//! The **windowed solo client** (the runnable `cargo run -p client` shell) embeds
//! an authoritative `server::ServerApp` and renders the window directly from that
//! server's world each tick, interpolating the rendered `Transform`s between fixed
//! steps (FR-004, Principle II/VII): for solo loopback there is zero real latency,
//! so the predict/interpolate netcode is pure overhead and a feel regression, and
//! reading the embedded authoritative sim directly gives crisp, in-sync collision
//! and hits. See [`net`].
//!
//! The crate is a **library + thin binary**: the gameplay-adjacent logic
//! (input numbering, prediction, reconciliation, interpolation) lives in the
//! library so the integration tests under `tests/` can drive it headlessly (no
//! window) — those modules are the path real *remote* multiplayer uses and are
//! intact — while `main.rs` is a one-line shell that calls [`run`].

// Bevy systems use tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

pub mod camera;
#[cfg(feature = "dev_panel")]
pub mod dev_panel;
pub mod fitting_ui;
pub mod fonts;
pub mod hud;
pub mod hud_bars;
pub mod input;
pub mod interpolation;
pub mod module_bars;
pub mod net;
pub mod prediction;
pub mod radar;
pub mod render_sync;
pub mod scene;
pub mod starfield;
pub mod tuning_io;

use bevy::prelude::*;
use fitting_ui::{FittingScreenState, FittingUiPlugin};
use net::NetClientPlugin;
use sim::{FixedDt, HitFeedback, Tuning};

/// The Bevy `FixedUpdate` cadence the embedded-server lifecycle runs at. It MUST
/// match the server-announced authoritative tick rate (TR-044, default **30 Hz**):
/// [`net::net_fixed_update`] steps the embedded server **once per FixedUpdate**,
/// advancing one server tick's worth of sim, so a mismatched (e.g. 60 Hz) cadence
/// would step the 30 Hz sim twice per real tick — 2× game speed and a doubled
/// input rate. Rendering is decoupled and runs per-frame in `Update`
/// ([`render_sync::interpolate_transforms`] blends the captured fixed-step poses)
/// at the display rate.
pub const TICK_HZ: f64 = 30.0;

/// Build and run the Bevy client app (the windowed single-player shell, T045).
///
/// E003 OBJ4 wires the embedded authoritative server into the live render path
/// via [`net::NetClientPlugin`]: it embeds a loopback [`server::ServerApp`] and
/// steps it each FixedUpdate, so this is a runnable single-player experience over
/// the in-memory transport (Principle VII). The windowed solo client renders
/// **directly from the embedded server's world** at full `f32` precision — zero
/// loopback latency makes the predict/interpolate netcode pure overhead and a feel
/// regression, so this path uses E002's smooth fixed-step interpolation instead:
/// every rendered entity (the local ship, targets, projectiles) carries a
/// `RenderInterp` captured from the server each tick by `net::capture_render_state`
/// and blended into its `Transform` each frame by `render_sync::interpolate_transforms`.
/// Keyboard input (E002) still writes the local ship's `ShipIntent` each frame; the
/// plugin numbers + sends it so the server pilots the ship through its identical
/// authoritative path. The netcode modules (`prediction`/`interpolation`/the
/// `protocol`/`server` netcode) are unchanged and remain the path real *remote*
/// multiplayer uses.
pub fn run() -> AppExit {
    // Bevy resolves `assets/` relative to CARGO_MANIFEST_DIR (= crates/client) under `cargo run`,
    // or the exe's directory for a directly-launched binary — but Dark Silence keeps its assets at
    // the WORKSPACE ROOT (alongside the content RON). Without this, the HUD fonts + icons (the
    // project's first + only AssetServer loads) fail with `AssetNotFound` and render blank. Point
    // Bevy's asset root at the workspace root so they load regardless of how the game is launched;
    // an explicit `BEVY_ASSET_ROOT` (e.g. a packaged build) still wins. `env!("CARGO_MANIFEST_DIR")`
    // is the compile-time crate dir, so `/../..` is the workspace root.
    if std::env::var_os("BEVY_ASSET_ROOT").is_none() {
        std::env::set_var(
            "BEVY_ASSET_ROOT",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../.."),
        );
    }

    // Refinement 27: load the persisted Starfield + HUD tuning from `render_tuning.ron` (code
    // defaults if absent/unparseable) and insert both resources below.
    let render_tuning = tuning_io::load_render_tuning();

    let mut app = App::new();
    app.add_plugins(DefaultPlugins)
        // Refinement 25: the procedural starfield background material (custom WGSL shader).
        .add_plugins(MaterialPlugin::<starfield::StarfieldMaterial>::default())
        // Fixed-step clock the embedded-server lifecycle runs on, and the matching
        // dt the shared sim reads (the embedded server uses its announced rate).
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(FixedDt((1.0 / TICK_HZ) as f32))
        .insert_resource(Tuning::default())
        .insert_resource(HitFeedback::default())
        // Hull render style toggle (Fix #11 M2): default voxel; `V` flips to the
        // smoothed rounded contour at runtime. Purely cosmetic — the sim's
        // ricochet/carve is unaffected — so it can be A/B'd freely mid-playtest.
        .init_resource::<net::HullRenderMode>()
        // Module-color toggle (Fix #11 M3): default off; `C` tints cells by module
        // type (voxel = per-cell vertex colors, contour = a marker overlay). Cosmetic.
        .init_resource::<net::ModuleColorMode>()
        // Refinement 24: live-tunable HUD bar/readout layout (the dev panel edits it; default = the
        // hardcoded positions). Present even without the dev panel → the HUD sits at its defaults.
        // Refinement 24/25/27: the live HUD + starfield tuning, loaded from `render_tuning.ron`
        // (the dev panel edits them; its Save button writes them back). Code defaults if no file.
        .insert_resource(render_tuning.hud)
        .insert_resource(render_tuning.starfield)
        // Refinement 21/22: load the shared HUD fonts (label + mono) + icon images into
        // `FontAssets`/`IconAssets` BEFORE the Startup HUD setups, which clone the handles.
        .add_systems(PreStartup, fonts::load_hud_assets)
        .add_systems(
            Startup,
            (
                scene::setup_scene,
                camera::setup_camera,
                hud::setup_hud,
                hud::setup_energy_bars,
                hud::setup_score_hud,
                // Refinement 14: segmented per-module-type condition bars (right side) — plain Bevy
                // UI nodes, no camera dependency.
                module_bars::setup_module_bars,
                // Camera-anchored trapezoid bars (afterburner ramp + heat double-ramp) —
                // parented to the camera, so it must exist first.
                hud_bars::setup_trapezoid_bars.after(camera::setup_camera),
                // Camera-anchored ranged sensor radar (top-right) — also a camera child.
                radar::setup_radar.after(camera::setup_camera),
                // Refinement 25: the starfield background quad — a camera child, so the camera first.
                starfield::setup_starfield.after(camera::setup_camera),
            ),
        )
        // Input runs before the fixed step so intents apply the same frame; the
        // net plugin reads the local ship's `ShipIntent` in FixedUpdate.
        .add_systems(
            PreUpdate,
            (
                input::read_input,
                input::toggle_assist,
                input::toggle_hull_render,
                input::toggle_module_color,
            ),
        )
        // The interactive fitting screen (E006 US5): the plugin registers the
        // `FittingScreenState` app-state + the screen's build/teardown + the
        // place/remove/budget/preview/preset systems. A key (Tab) toggles between
        // the flying view and the fitting screen (T034, FR-012).
        .add_plugins(FittingUiPlugin)
        .add_systems(Update, toggle_fitting_screen)
        // The embedded-server lifecycle: pilot + step the server and capture its
        // world into every rendered entity's `RenderInterp` in FixedUpdate;
        // interpolate `RenderInterp` → `Transform` in Update.
        .add_plugins(NetClientPlugin)
        // Per-frame rendering: the camera and gunsight pip read the local ship's
        // `Transform`, which `render_sync::interpolate_transforms` (added by the net
        // plugin in Update) wrote this frame — so they run after it.
        .add_systems(
            Update,
            (
                (render_sync::update_aim_pip, camera::follow_camera)
                    .chain()
                    .after(render_sync::interpolate_transforms),
                camera::zoom_camera,
                hud::update_hud,
                hud::update_energy_hud,
                hud::update_score_hud,
                hud_bars::update_trapezoid_bars,
                module_bars::update_module_bars,
                radar::update_radar,
                // Refinement 24: apply the live HUD layout (dev panel) to the bars + the readout.
                hud_bars::apply_bar_layout,
                hud::apply_readout_layout,
                // Refinement 25: feed camera uniforms + live tuning to the starfield + bloom.
                starfield::update_starfield.after(camera::follow_camera),
            ),
        );

    // Live DEV tuning panel (Phase M6) — an egui overlay (backtick to toggle) bound to the
    // embedded server's tuning resources. Default-on `dev_panel` feature; compiled out by
    // `--no-default-features` (then this is absent and the egui dep is dropped).
    #[cfg(feature = "dev_panel")]
    app.add_plugins(dev_panel::DevPanelPlugin);

    app.run()
}

/// Toggle between the flying view and the interactive fitting screen on a fresh
/// `Tab` press (T034, FR-012). Flips the [`FittingScreenState`] app-state, which
/// the [`FittingUiPlugin`] watches to build/tear down the screen and gate its
/// place/remove/budget/preview/preset systems. Flight input keeps running so the
/// player can still see the ship behind the panel; the fitting edits never touch a
/// running ship until committed (client-only sandbox).
fn toggle_fitting_screen(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<FittingScreenState>>,
    mut next: ResMut<NextState<FittingScreenState>>,
) {
    if keys.just_pressed(KeyCode::Tab) {
        next.set(match state.get() {
            FittingScreenState::Flying => FittingScreenState::Fitting,
            FittingScreenState::Fitting => FittingScreenState::Flying,
        });
    }
}
