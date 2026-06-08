//! R49 — live-tunable ship visual effects (glow / engine flame / nav lights / faction accent / fresnel
//! rim + panels + grime / fill light / bloom). Mirrors [`crate::starfield::StarfieldTuning`]: a
//! `Serialize`/`Deserialize` resource persisted in `render_tuning.ron`, edited in the dev panel, and
//! applied each frame by [`apply_ship_visuals`]. All client render → determinism-neutral.

use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::camera::MainCamera;
use crate::scene::{FillLight, RenderAssets};

/// All live-tunable ship-visual knobs. Defaults give the intended look; the dev panel edits them and
/// they persist via `render_tuning.ron`.
#[derive(Resource, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ShipVisualTuning {
    /// Engine-nozzle + reactor GLOW emissive: `glow_color × glow_intensity` (HDR → blooms).
    pub glow_intensity: f32,
    pub glow_color: [f32; 3],
    /// Throttle exhaust FLAME size (× `CELL_SIZE`): length (at full throttle) + width.
    pub flame_length: f32,
    pub flame_width: f32,
    /// Nav/running-light emissive scale (0 = off).
    pub nav_intensity: f32,
    /// Faction accent emissive scale (0 = off).
    pub accent_intensity: f32,
    /// Cool fill `DirectionalLight` illuminance (0 = off). Subtle top-down.
    pub fill_intensity: f32,
    /// Camera bloom strength.
    pub bloom_intensity: f32,
    /// Cinematic hull shader: fresnel rim strength + power; panel spacing + line width; grime.
    pub rim_strength: f32,
    pub rim_power: f32,
    pub panel_scale: f32,
    pub panel_width: f32,
    pub grime: f32,
    /// R50 — engine ION-TRAIL: on/off, spawn rate (particles/sec at full throttle), size, life (s).
    #[serde(default = "default_true")]
    pub trail_on: bool,
    #[serde(default = "default_trail_rate")]
    pub trail_rate: f32,
    #[serde(default = "default_trail_size")]
    pub trail_size: f32,
    #[serde(default = "default_trail_life")]
    pub trail_life: f32,
    /// R50 — DAMAGE smoke (puffs per carve) + sparks on hit, each on/off.
    #[serde(default = "default_true")]
    pub smoke_on: bool,
    #[serde(default = "default_smoke_amount")]
    pub smoke_amount: f32,
    #[serde(default = "default_true")]
    pub spark_on: bool,
}

fn default_true() -> bool {
    true
}
fn default_trail_rate() -> f32 {
    60.0
}
fn default_trail_size() -> f32 {
    0.10
}
fn default_trail_life() -> f32 {
    0.45
}
fn default_smoke_amount() -> f32 {
    8.0
}

impl Default for ShipVisualTuning {
    fn default() -> Self {
        Self {
            glow_intensity: 6.0,
            glow_color: [1.0, 0.45, 0.12],
            flame_length: 3.0,
            flame_width: 0.6,
            nav_intensity: 3.0,
            accent_intensity: 2.2,
            fill_intensity: 2200.0,
            bloom_intensity: 0.20,
            rim_strength: 1.0,
            rim_power: 2.6,
            panel_scale: 0.45,
            panel_width: 0.045,
            grime: 1.0,
            trail_on: true,
            trail_rate: default_trail_rate(),
            trail_size: default_trail_size(),
            trail_life: default_trail_life(),
            smoke_on: true,
            smoke_amount: default_smoke_amount(),
            spark_on: true,
        }
    }
}

/// Apply [`ShipVisualTuning`] to the shared materials, the hull shader, the camera bloom, and the fill
/// light — only when it changes (the dev panel mutates it; the first frame applies the defaults).
#[allow(clippy::too_many_arguments)]
pub fn apply_ship_visuals(
    tuning: Res<ShipVisualTuning>,
    assets: Option<Res<RenderAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut hull_mats: ResMut<Assets<crate::hull_shader::HullMaterial>>,
    mut bloom_q: Query<&mut Bloom, With<MainCamera>>,
    mut fill_q: Query<&mut DirectionalLight, With<FillLight>>,
) {
    if !tuning.is_changed() {
        return;
    }
    let Some(assets) = assets else { return };

    // Engine/reactor GLOW: warm hue × intensity (HDR → bloom halo).
    let glow = LinearRgba::rgb(
        tuning.glow_color[0] * tuning.glow_intensity,
        tuning.glow_color[1] * tuning.glow_intensity,
        tuning.glow_color[2] * tuning.glow_intensity,
    );
    if let Some(m) = materials.get_mut(&assets.fixture_glow_material) {
        m.emissive = glow;
    }

    // Nav lights (port red / starboard green / white spine) × nav_intensity.
    let n = tuning.nav_intensity;
    set_emissive(
        &mut materials,
        &assets.nav_red_material,
        [1.0, 0.02, 0.02],
        n,
    );
    set_emissive(
        &mut materials,
        &assets.nav_green_material,
        [0.02, 1.0, 0.04],
        n,
    );
    set_emissive(
        &mut materials,
        &assets.nav_white_material,
        [0.85, 0.85, 1.0],
        n,
    );

    // Faction accents (neutral cool / team red / team blue) × accent_intensity.
    let a = tuning.accent_intensity;
    set_emissive(
        &mut materials,
        &assets.accent_neutral_material,
        [0.25, 0.55, 1.0],
        a,
    );
    set_emissive(
        &mut materials,
        &assets.accent_red_material,
        [1.0, 0.18, 0.14],
        a,
    );
    set_emissive(
        &mut materials,
        &assets.accent_blue_material,
        [0.14, 0.42, 1.0],
        a,
    );

    // Cinematic hull shader: panel/grime/rim params on all 3 faction handles (rgb tint stays baked).
    let params = Vec4::new(
        tuning.panel_scale,
        tuning.panel_width,
        tuning.grime,
        tuning.rim_power,
    );
    for h in [
        &assets.hull_ext_neutral,
        &assets.hull_ext_red,
        &assets.hull_ext_blue,
    ] {
        if let Some(m) = hull_mats.get_mut(h) {
            m.extension.params = params;
            m.extension.faction_color.w = tuning.rim_strength;
        }
    }

    // Camera bloom + the cool fill light.
    if let Ok(mut bloom) = bloom_q.single_mut() {
        bloom.intensity = tuning.bloom_intensity;
    }
    if let Ok(mut fill) = fill_q.single_mut() {
        fill.illuminance = tuning.fill_intensity;
    }
}

/// Set a shared material's emissive to `hue × scale`.
fn set_emissive(
    materials: &mut Assets<StandardMaterial>,
    handle: &Handle<StandardMaterial>,
    hue: [f32; 3],
    scale: f32,
) {
    if let Some(m) = materials.get_mut(handle) {
        m.emissive = LinearRgba::rgb(hue[0] * scale, hue[1] * scale, hue[2] * scale);
    }
}
