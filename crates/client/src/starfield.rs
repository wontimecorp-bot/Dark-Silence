//! Refinement 25 — procedural GPU starfield background + live tuning.
//!
//! An **infinite-depth** starfield drawn by a custom WGSL fragment shader on a fullscreen quad
//! parented to the camera (the same camera-child trick the HUD bars use). The shader computes
//! several exponentially-spaced layers of **Voronoi hard-point stars** (blackbody-coloured, with
//! desynchronised twinkling) mapped to WORLD space, so they **parallax** as the camera pans/zooms —
//! far layers are nearly screen-locked (infinite), near layers drift. Bright stars output HDR values
//! so the camera's Bloom makes them glow.
//!
//! Entirely client render: a custom [`Material`] + the [`StarfieldTuning`] resource + two systems.
//! No sim/server/wire touch → determinism-neutral. See `assets/shaders/starfield.wgsl`.

use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy::window::PrimaryWindow;
use serde::{Deserialize, Serialize};

use crate::camera::MainCamera;

/// Path (under `assets/`) of the starfield fragment shader (loads via the R23 asset root).
const STARFIELD_SHADER: &str = "shaders/starfield.wgsl";

/// Hard cap on shader star layers — MUST match `MAX_LAYERS` in `starfield.wgsl`.
pub const MAX_LAYERS: usize = 16;

/// Linear interpolation helper (used for the per-layer "character" defaults: density/brightness/size).
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Geometric (exponential) interpolation `a·(b/a)^t` — used for the per-layer DISTANCE SCALES
/// (parallax + frequency) so the layers are spaced **exponentially** (each a multiplicative step in
/// scale), per the starfield spec.
fn geomix(a: f32, b: f32, t: f32) -> f32 {
    a * (b / a).powf(t)
}

/// One layer's GPU parameters (Refinement 26/32). 12×`f32` = 48 bytes so the std140 uniform array
/// stride is unambiguous (a 16-byte multiple). Field order + types MUST match `StarLayer` in the WGSL.
#[derive(ShaderType, Clone, Copy, Default)]
pub struct StarLayer {
    /// Parallax factor (0 = screen-locked / infinite, →1 = world-anchored / fast).
    pub parallax: f32,
    /// Cell frequency (cells per world unit — spacing/density).
    pub frequency: f32,
    /// Fraction of candidate cells that host a star (0..1).
    pub density: f32,
    /// Per-layer brightness factor.
    pub brightness: f32,
    /// Per-layer twinkle factor.
    pub twinkle: f32,
    /// Per-layer star size (pixel-radius) factor.
    pub size: f32,
    /// Stellar-class temperature RANGE in Kelvin feeding the blackbody color (Refinement 32):
    /// `temp_min` = the common cool end, `temp_max` = the rare hot end (per-star `tn` mixes between
    /// them). Repurposes the old `_pad0`/`_pad1` slots — no size change for these two.
    pub temp_min: f32,
    pub temp_max: f32,
    /// Per-layer flat color TINT multiplier (Refinement 32; white = no change).
    pub tint_r: f32,
    pub tint_g: f32,
    pub tint_b: f32,
    /// Per-layer twinkle SPEED (Refinement 34) — the pulse RATE for this layer (vs `twinkle` = the
    /// DEPTH). Reuses the old trailing pad slot, so `StarLayer` stays 12 f32 / 48 bytes (identical
    /// uniform array stride).
    pub twinkle_speed: f32,
}

/// GPU uniforms for the starfield shader. Field order + types MUST match `StarfieldParams` in the
/// WGSL (std140-ish layout via `ShaderType`).
#[derive(ShaderType, Clone, Copy, Default)]
pub struct StarfieldParams {
    /// Camera world XY (drives parallax).
    pub cam_pos: Vec2,
    /// Camera height above the plane (= zoom).
    pub height: f32,
    /// Vertical FOV (radians) — for the screen→world mapping.
    pub fov: f32,
    /// Viewport size in px.
    pub resolution: Vec2,
    /// Seconds since start (twinkle).
    pub time: f32,
    /// Refinement 34: the old GLOBAL `star_brightness`/`star_density`/`twinkle_amount` master
    /// multipliers were removed (everything is per-layer now). Kept as PADS so the uniform byte
    /// layout is byte-identical to R32/R33 (offsets unchanged → no alignment recompute); the shader
    /// no longer reads them.
    pub _pad_a: f32,
    pub _pad_b: f32,
    pub _pad_c: f32,
    /// How many of `layers` to draw.
    pub layer_count: u32,
    /// Analytic-coverage edge softness in pixels (Refinement 30): 0 = hard points (shimmer on
    /// motion), ~0.75 = smooth/stable. Reuses the 16-align pad slot (offset 44 → `layers` at 48), so
    /// the layout is unchanged; the WGSL field must match.
    pub edge_softness: f32,
    /// Per-layer parameters (Refinement 26); the shader reads `layers[0..layer_count]`.
    pub layers: [StarLayer; MAX_LAYERS],
    /// Refinement 34: the global `twinkle_speed` moved to per-layer (`StarLayer.twinkle_speed`); this
    /// trailing slot is kept as a PAD so the layout is unchanged. Not read by the shader.
    pub _pad_d: f32,
}

/// The starfield background material (one uniform block at binding 0).
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct StarfieldMaterial {
    #[uniform(0)]
    pub params: StarfieldParams,
}

impl Material for StarfieldMaterial {
    fn fragment_shader() -> ShaderRef {
        STARFIELD_SHADER.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Opaque
    }
}

/// One layer's tunable parameters (Refinement 26) — the clean CPU-side form the dev panel edits
/// (no GPU padding). Packed into a [`StarLayer`] for the uniform each frame.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct LayerTuning {
    pub parallax: f32,
    pub frequency: f32,
    pub density: f32,
    pub brightness: f32,
    pub twinkle: f32,
    pub size: f32,
    /// Stellar-class temperature RANGE (Kelvin) for this layer's blackbody color (Refinement 32).
    /// `#[serde(default)]` so `render_tuning.ron` files saved before these fields existed still load.
    #[serde(default = "default_temp_min")]
    pub temp_min: f32,
    #[serde(default = "default_temp_max")]
    pub temp_max: f32,
    /// Per-layer flat color TINT (Refinement 32; the picked hue). Applied as a SECONDARY effect via
    /// `tint_strength` (R33) — the swatch only matters when strength > 0.
    #[serde(default = "default_tint")]
    pub tint: [f32; 3],
    /// Per-layer tint STRENGTH (Refinement 33), 0 = off (pure stellar color) … 1 = full `tint`
    /// multiply. The effective GPU tint = `lerp(white, tint, strength)` (computed CPU-side), so this
    /// is a real off-by-default secondary control. `#[serde(default)]` so older RONs load.
    #[serde(default = "default_tint_strength")]
    pub tint_strength: f32,
    /// Per-layer twinkle SPEED (Refinement 34) — the pulse RATE for this layer (vs `twinkle` = the
    /// DEPTH). 1.0 = the original rate. `#[serde(default)]` so older RONs load (replaces the removed
    /// global `twinkle_speed`).
    #[serde(default = "default_twinkle_speed")]
    pub twinkle_speed: f32,
}

/// Live, dev-panel-tunable starfield + bloom knobs (client-only; NOT behind the `dev_panel` feature
/// so the apply path always compiles). `Default` = a dim backdrop + low bloom so lit ships clearly
/// out-read the background, with the per-layer defaults reproducing R25's exponential 8-layer look.
#[derive(Resource, Clone, Copy, Serialize, Deserialize)]
pub struct StarfieldTuning {
    /// Camera bloom strength (applied to the `Bloom` component). Lives in the dev panel's
    /// "Camera / Post-processing" section (R34) — it's a camera post-process, not a starfield knob.
    pub bloom_intensity: f32,
    /// Layer count to draw (stored as f32 for the slider; rounded + clamped to [`MAX_LAYERS`]).
    pub layer_count: f32,
    /// Star-edge AA softness in px (Refinement 30): 0 = pure hard points (shimmer on motion), ~0.75 =
    /// smooth/stable (twinkle fully controllable). `#[serde(default)]` so `render_tuning.ron` files
    /// saved before this field still load.
    #[serde(default = "default_edge_softness")]
    pub edge_softness: f32,
    /// Per-layer parameters (R34: ALL look knobs are per-layer — the old global `star_brightness` /
    /// `star_density` / `twinkle_amount` / `twinkle_speed` masters were removed; serde ignores them
    /// in older `render_tuning.ron` files).
    pub layers: [LayerTuning; MAX_LAYERS],
}

/// Default star-edge softness (px) — stable by default so the field doesn't shimmer; also the value
/// substituted for `render_tuning.ron` files saved before `edge_softness` existed.
fn default_edge_softness() -> f32 {
    0.75
}

/// Default per-layer cool-end temperature (Kelvin) — R25's global default, now per-layer (R32); also
/// substituted for `render_tuning.ron` layers saved before `temp_min` existed.
fn default_temp_min() -> f32 {
    3000.0
}

/// Default per-layer hot-end temperature (Kelvin) — see [`default_temp_min`].
fn default_temp_max() -> f32 {
    30000.0
}

/// Default per-layer color tint — white (no change).
fn default_tint() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

/// Default per-layer tint strength (R33) — 0.0 = off (the tint is a secondary effect, off until
/// dialed up); also substituted for `render_tuning.ron` layers saved before `tint_strength` existed.
fn default_tint_strength() -> f32 {
    0.0
}

/// Default global twinkle SPEED multiplier (R32) — 1.0 = the original hardcoded pulse rate.
fn default_twinkle_speed() -> f32 {
    1.0
}

impl Default for StarfieldTuning {
    fn default() -> Self {
        // Per-layer defaults reproduce R25's exponential formula at `fi = (i/7).min(1)`, so layers
        // 0..7 match the current 8-layer look; 8..15 clamp to the nearest, ready to tune.
        let layers = std::array::from_fn(|i| {
            let fi = (i as f32 / 7.0).min(1.0);
            LayerTuning {
                // Distance scales spaced EXPONENTIALLY (geometric steps) — far layers nearly
                // screen-locked + dense, near layers parallaxing + sparse.
                parallax: geomix(0.015, 0.45, fi),
                frequency: geomix(2.5, 0.35, fi),
                // Per-layer character (not "spacing") — linear is fine.
                density: mix(0.40, 0.22, fi),
                brightness: mix(0.6, 1.0, fi),
                twinkle: 1.0,
                size: mix(0.9, 1.3, fi),
                // R32: per-layer stellar-class range + tint default to R25's global range / no tint,
                // so the default render is unchanged until the user tunes a layer.
                temp_min: default_temp_min(),
                temp_max: default_temp_max(),
                tint: default_tint(),
                tint_strength: default_tint_strength(),
                // R34: per-layer twinkle speed (was a global); 1.0 = the original pulse rate.
                twinkle_speed: default_twinkle_speed(),
            }
        });
        Self {
            bloom_intensity: 0.15,
            layer_count: 8.0,
            edge_softness: default_edge_softness(),
            layers,
        }
    }
}

/// Handle to the single starfield material, so [`update_starfield`] can rewrite its uniforms.
#[derive(Resource)]
pub struct StarfieldHandle(Handle<StarfieldMaterial>);

/// Spawn the fullscreen starfield quad as a child of the camera, far behind the scene.
pub fn setup_starfield(
    mut commands: Commands,
    cam_q: Query<Entity, With<MainCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StarfieldMaterial>>,
) {
    let Ok(cam) = cam_q.single() else {
        return;
    };
    // Oversized so it always covers the frustum at its far local depth, at any zoom.
    let mesh = meshes.add(Rectangle::new(2000.0, 2000.0));
    let mat = materials.add(StarfieldMaterial {
        params: StarfieldParams::default(),
    });
    commands.insert_resource(StarfieldHandle(mat.clone()));
    commands.entity(cam).with_children(|p| {
        p.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(mat),
            // Far in front of the camera (local -Z) but BEHIND the ships (local -12..-240) and within
            // the 1000 far-plane; the HUD bars at -12 stay in front. The 2000-unit quad is centred in
            // front of the camera so it always covers the frustum (no culling concern).
            Transform::from_xyz(0.0, 0.0, -600.0),
        ));
    });
}

/// Feed the camera world-pos / zoom / fov / resolution / time + the live tuning into the material
/// uniforms each frame, and apply the live bloom intensity to the camera.
pub fn update_starfield(
    handle: Option<Res<StarfieldHandle>>,
    tuning: Res<StarfieldTuning>,
    time: Res<Time>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cam_q: Query<(&Transform, &MainCamera, &Projection, &mut Bloom)>,
    mut materials: ResMut<Assets<StarfieldMaterial>>,
) {
    let Some(handle) = handle else {
        return;
    };
    let Ok((tf, cam, projection, mut bloom)) = cam_q.single_mut() else {
        return;
    };
    // Live bloom strength.
    bloom.intensity = tuning.bloom_intensity;
    let Ok(window) = windows.single() else {
        return;
    };
    let fov = match projection {
        Projection::Perspective(p) => p.fov,
        _ => std::f32::consts::FRAC_PI_4,
    };
    if let Some(mat) = materials.get_mut(&handle.0) {
        // Pack the clean per-layer tuning into the padded GPU array (the shader reads 0..layer_count).
        let layers = std::array::from_fn(|i| {
            let l = tuning.layers[i];
            StarLayer {
                parallax: l.parallax,
                frequency: l.frequency,
                density: l.density,
                brightness: l.brightness,
                twinkle: l.twinkle,
                size: l.size,
                temp_min: l.temp_min,
                temp_max: l.temp_max,
                // R33: tint is a SECONDARY effect via `tint_strength` — the effective GPU tint is
                // `lerp(white, tint, strength)` so the WGSL stays a plain `blackbody·tint` multiply
                // (no shader/struct change), and strength 0 ⇒ white ⇒ exact no-op.
                tint_r: mix(1.0, l.tint[0], l.tint_strength),
                tint_g: mix(1.0, l.tint[1], l.tint_strength),
                tint_b: mix(1.0, l.tint[2], l.tint_strength),
                // R34: per-layer twinkle SPEED (was a global multiplier).
                twinkle_speed: l.twinkle_speed,
            }
        });
        mat.params = StarfieldParams {
            cam_pos: tf.translation.truncate(),
            height: cam.height,
            fov,
            resolution: Vec2::new(window.width(), window.height()),
            time: time.elapsed_secs(),
            // R34: removed global multipliers — kept as zeroed pads (layout unchanged; shader ignores).
            _pad_a: 0.0,
            _pad_b: 0.0,
            _pad_c: 0.0,
            layer_count: (tuning.layer_count.round() as u32).clamp(1, MAX_LAYERS as u32),
            edge_softness: tuning.edge_softness,
            layers,
            _pad_d: 0.0,
        };
    }
}
