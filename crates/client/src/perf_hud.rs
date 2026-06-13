//! R104 — frame-time telemetry + an F9 HUD overlay.
//!
//! The lag the player feels during heavy combat is CLIENT-render-side (hull mesh
//! rebuilds on carve, the embedded sim sharing the frame, particles), NOT the AI
//! or sim baseline (the `fleet_stress` bench measures the headless sim tick only).
//! This module exposes where the frame time actually goes — sim-ms vs render-ms,
//! the fixed-step catch-up sub-step count, and hull-mesh-rebuilds per frame — so
//! the cause is visible instead of guessed. It also owns the fixed-step catch-up
//! CLAMP that bounds the "skippy lag that gets worse" death spiral.
//!
//! Non-dev-gated: compiles in both feature configs (the dev panel adds a richer
//! "Performance" section on top of this).

use crate::fonts::FontAssets;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

/// Per-frame performance numbers, filled each frame and shown by the overlay /
/// the dev-panel "Performance" section. `sim_ms`, `fixed_substeps`, and
/// `mesh_rebuilds` ACCUMULATE across any fixed-step catch-up sub-steps within a
/// render frame; [`reset_frame_counters`] (in `First`) zeroes them each frame.
#[derive(Resource, Default)]
pub struct PerfStats {
    /// Wall-time spent inside the embedded `host.server.tick()` this frame (ms),
    /// summed over every fixed sub-step that ran.
    pub sim_ms: f32,
    /// Smoothed total frame time (ms), from `FrameTimeDiagnosticsPlugin`.
    pub frame_ms: f32,
    /// `frame_ms - sim_ms` (clamped ≥0) — roughly the render/UI share.
    pub render_ms: f32,
    /// How many fixed-step sub-steps ran this render frame (catch-up count).
    pub fixed_substeps: u32,
    /// How many ship hull meshes were actually rebuilt this frame (carve cost).
    pub mesh_rebuilds: u32,
    /// Live particle count (engine trails + damage smoke/sparks).
    pub live_particles: usize,
    /// Smoothed FPS.
    pub fps: f32,
}

/// Whether the F9 perf overlay is visible (default off).
#[derive(Resource, Default)]
pub struct ShowPerf(pub bool);

/// Marker for the perf-overlay text node.
#[derive(Component)]
pub struct PerfText;

/// `Startup` — bound the fixed-step catch-up so a slow combat frame degrades to
/// slight slow-mo instead of a death spiral. When a render frame runs long, Bevy
/// otherwise advances `Time<Virtual>` by the full elapsed wall time (default cap
/// 250 ms ≈ 7 fixed sub-steps at 30 Hz), each re-running the sim + hull-mesh
/// rebuilds → slower → more catch-up steps → worse. Clamping the virtual delta to
/// ~100 ms (≈3 sub-steps) bounds that. This affects only single-player/loopback
/// real-time PACING under load; the fixed step itself + determinism are unchanged.
pub fn clamp_fixed_catchup(mut vtime: ResMut<Time<Virtual>>) {
    vtime.set_max_delta(std::time::Duration::from_millis(100));
}

/// `Startup` — spawn the perf overlay text node (bottom-left, hidden until F9).
pub fn setup_perf_hud(mut commands: Commands, fonts: Res<FontAssets>) {
    commands.spawn((
        Text::new(String::new()),
        TextFont {
            font: fonts.mono.clone(),
            font_size: 14.0,
            ..default()
        },
        TextColor(Color::srgb(0.6, 1.0, 0.6)),
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
        Visibility::Hidden,
        PerfText,
    ));
}

/// `First` — zero the per-frame accumulators before the frame's fixed sub-steps
/// run (so `sim_ms`/`fixed_substeps`/`mesh_rebuilds` reflect THIS frame).
pub fn reset_frame_counters(mut perf: ResMut<PerfStats>) {
    perf.sim_ms = 0.0;
    perf.fixed_substeps = 0;
    perf.mesh_rebuilds = 0;
}

/// `Update` — read FPS/frame-time from diagnostics + the live particle count.
pub fn update_perf_stats(
    diag: Res<DiagnosticsStore>,
    count: Res<crate::particles::ParticleCount>,
    mut perf: ResMut<PerfStats>,
) {
    if let Some(fps) = diag
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
    {
        perf.fps = fps as f32;
    }
    if let Some(ft) = diag
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.smoothed())
    {
        perf.frame_ms = ft as f32;
    }
    perf.render_ms = (perf.frame_ms - perf.sim_ms).max(0.0);
    perf.live_particles = count.0;
}

/// `Update` — F9 toggles the overlay.
pub fn toggle_perf(keys: Res<ButtonInput<KeyCode>>, mut show: ResMut<ShowPerf>) {
    if keys.just_pressed(KeyCode::F9) {
        show.0 = !show.0;
    }
}

/// `Update` — show/hide + refresh the overlay text.
pub fn update_perf_hud(
    show: Res<ShowPerf>,
    perf: Res<PerfStats>,
    mut q: Query<(&mut Text, &mut Visibility), With<PerfText>>,
) {
    let Ok((mut text, mut vis)) = q.single_mut() else {
        return;
    };
    *vis = if show.0 {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if show.0 {
        text.0 = format!(
            "FPS {:>3.0}  frame {:>5.1}ms  sim {:>4.1}  render {:>4.1}  substeps {}  rebuilds {}  parts {}",
            perf.fps,
            perf.frame_ms,
            perf.sim_ms,
            perf.render_ms,
            perf.fixed_substeps,
            perf.mesh_rebuilds,
            perf.live_particles,
        );
    }
}
