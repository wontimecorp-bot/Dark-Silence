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

/// Hard cap on shader spectral classes (Refinement 35) — MUST match `MAX_CLASSES` in the WGSL.
/// 7 are used (M/K/G/F/A/B/O); 8 keeps a clean slot for headroom.
pub const MAX_CLASSES: usize = 8;

/// Number of real spectral classes packed/edited (M K G F A B O).
pub const NUM_CLASSES: usize = 7;

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

/// One layer's GPU parameters (Refinement 26/36). 8×`f32` = 32 bytes (a 16-byte multiple → unambiguous
/// uniform array stride). Field order + types MUST match `StarLayer` in the WGSL. R36: a layer is
/// DEPTH only (parallax/spacing/count + brightness & size depth multipliers) plus an OPTIONAL per-layer
/// tint overlay; star color/twinkle/edge are owned by the spectral class table.
#[derive(ShaderType, Clone, Copy, Default)]
pub struct StarLayer {
    /// Parallax factor (0 = screen-locked / infinite, →1 = world-anchored / fast).
    pub parallax: f32,
    /// Cell frequency (cells per world unit — spacing/density).
    pub frequency: f32,
    /// Fraction of candidate cells that host a star (0..1).
    pub density: f32,
    /// DEPTH brightness multiplier (× each star's class brightness — dims far layers).
    pub brightness: f32,
    /// DEPTH size multiplier (× each star's class pixel-radius).
    pub size: f32,
    /// OPTIONAL per-layer tint overlay (Refinement 36): the EFFECTIVE tint = `lerp(white, tint,
    /// strength)` packed CPU-side, so white = no-op. Multiplies on top of the class color.
    pub tint_r: f32,
    pub tint_g: f32,
    pub tint_b: f32,
}

/// One spectral class's GPU parameters (Refinement 35). 16×`f32` = 64 bytes (a 16-byte multiple, so
/// the uniform array stride is unambiguous; 13 real + 3 reserved). Field order/types MUST match
/// `SpectralClass` in the WGSL.
#[derive(ShaderType, Clone, Copy, Default)]
pub struct SpectralClass {
    /// Cumulative population threshold 0..1 (derived from the editable per-class `weight`): a star is
    /// the first class whose `cdf` exceeds a uniform hash.
    pub cdf: f32,
    /// Temperature RANGE (Kelvin) → blackbody color (`temp_min` = common cool end).
    pub temp_min: f32,
    pub temp_max: f32,
    /// Base HDR brightness (>1 blooms) + pixel-radius size for this class.
    pub brightness: f32,
    pub size: f32,
    /// Flat color tint (e.g. the violet nudge for O that blackbody can't reach).
    pub tint_r: f32,
    pub tint_g: f32,
    pub tint_b: f32,
    /// 0 = uniform (M/K/G) … 1 = confined to the galactic band (O/B/A).
    pub clustering: f32,
    /// Scintillation depth + rate for this class.
    pub twinkle: f32,
    pub twinkle_speed: f32,
    /// Edge AA px: ~0 = hard point (M) … higher = soft Gaussian (O).
    pub softness: f32,
    /// Within-class magnitude spread (0 = equal; higher = a few much brighter).
    pub mag_spread: f32,
    pub _pad0: f32,
    pub _pad1: f32,
    pub _pad2: f32,
}

/// GPU uniforms for the starfield shader. Field order + types MUST match `StarfieldParams` in the
/// WGSL (std140-ish layout via `ShaderType`). Refinement 35 added the galaxy globals + the `classes`
/// array; both arrays (`layers` 48-byte stride, `classes` 64-byte stride) are 16-aligned with one
/// explicit `_pad_layers` before `layers` (a headless test validates the layout — see the tests mod).
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
    /// How many of `layers` to draw.
    pub layer_count: u32,
    // --- Refinement 35/36 galaxy globals (all live + RON; the starfield is always the unified
    // spectral model — R36 dropped the toggle + the global edge softness, per-class `softness` owns it).
    /// Galactic band (the "Milky Way" lane): orientation / thickness / position / confine-strength /
    /// along-band clumpiness.
    pub band_angle: f32,
    pub band_width: f32,
    pub band_offset: f32,
    pub band_strength: f32,
    pub band_clumpiness: f32,
    /// Diffuse milky haze along the band + its color.
    pub haze_brightness: f32,
    pub haze_r: f32,
    pub haze_g: f32,
    pub haze_b: f32,
    /// Dark dust-lane occlusion: depth / noise scale / contrast.
    pub dust_depth: f32,
    pub dust_scale: f32,
    pub dust_contrast: f32,
    /// Galactic-core bulge: position along the band / size / brightness / color / star-density boost.
    pub core_along: f32,
    pub core_size: f32,
    pub core_brightness: f32,
    pub core_r: f32,
    pub core_g: f32,
    pub core_b: f32,
    pub core_density_boost: f32,
    /// Bright-star glare (diffraction): HDR brightness threshold / halo size+intensity / spike
    /// length+count+intensity.
    pub glare_threshold: f32,
    pub glare_halo_size: f32,
    pub glare_halo_intensity: f32,
    pub glare_spike_len: f32,
    pub glare_spike_count: f32,
    pub glare_spike_intensity: f32,
    /// Explicit pad so `layers` (array, 16-byte align) starts on a 16-byte boundary (offset 144).
    pub _pad0: f32,
    pub _pad1: f32,
    pub _pad2: f32,
    /// Per-layer depth parameters; the shader reads `layers[0..layer_count]`.
    pub layers: [StarLayer; MAX_LAYERS],
    /// Per-spectral-class parameters (Refinement 35); `NUM_CLASSES` used.
    pub classes: [SpectralClass; MAX_CLASSES],
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
    /// DEPTH brightness multiplier (× each star's class brightness).
    pub brightness: f32,
    /// DEPTH size multiplier (× each star's class size).
    pub size: f32,
    /// OPTIONAL per-layer tint overlay (R32/R33/R36): the picked hue, applied at `tint_strength`
    /// (default 0 = off — the swatch only matters when raised). Multiplies on top of the class color.
    /// `#[serde(default)]` so older `render_tuning.ron` files still load.
    #[serde(default = "default_tint")]
    pub tint: [f32; 3],
    #[serde(default = "default_tint_strength")]
    pub tint_strength: f32,
}

/// One spectral class's tunable parameters (Refinement 35) — the clean CPU form the dev panel edits.
/// `#[serde(default)]` (container) so a partial/old `render_tuning.ron` class entry fills missing
/// fields from [`ClassTuning::default`]. Packed into a [`SpectralClass`] (with a derived `cdf`) each
/// frame.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassTuning {
    /// Relative population weight (the CDF is derived from the 7 weights).
    pub weight: f32,
    pub temp_min: f32,
    pub temp_max: f32,
    pub brightness: f32,
    pub size: f32,
    pub tint: [f32; 3],
    pub clustering: f32,
    pub twinkle: f32,
    pub twinkle_speed: f32,
    pub softness: f32,
    pub mag_spread: f32,
}

impl Default for ClassTuning {
    /// A neutral G-ish fallback (only used to fill missing fields on deserialize; the real per-class
    /// blueprint values live in [`GalaxyTuning::default`]).
    fn default() -> Self {
        Self {
            weight: 1.0,
            temp_min: 5000.0,
            temp_max: 6000.0,
            brightness: 0.6,
            size: 1.0,
            tint: [1.0, 1.0, 1.0],
            clustering: 0.0,
            twinkle: 0.15,
            twinkle_speed: 0.5,
            softness: 0.4,
            mag_spread: 0.3,
        }
    }
}

/// Galaxy-wide tuning (Refinement 35): the spectral-class table + the galactic band / haze+dust /
/// core / bright-star glare globals. `#[serde(default)]` (container) so older `render_tuning.ron`
/// files (or partial blocks) fill any missing field from [`GalaxyTuning::default`] (the blueprint).
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct GalaxyTuning {
    // Galactic band (the Milky Way lane).
    pub band_angle: f32,
    pub band_width: f32,
    pub band_offset: f32,
    pub band_strength: f32,
    pub band_clumpiness: f32,
    // Diffuse milky haze + dark dust lanes.
    pub haze_brightness: f32,
    pub haze_color: [f32; 3],
    pub dust_depth: f32,
    pub dust_scale: f32,
    pub dust_contrast: f32,
    // Galactic-core bulge.
    pub core_along: f32,
    pub core_size: f32,
    pub core_brightness: f32,
    pub core_color: [f32; 3],
    pub core_density_boost: f32,
    // Bright-star glare (diffraction halo + spikes).
    pub glare_threshold: f32,
    pub glare_halo_size: f32,
    pub glare_halo_intensity: f32,
    pub glare_spike_len: f32,
    pub glare_spike_count: f32,
    pub glare_spike_intensity: f32,
    /// The 7 spectral classes M, K, G, F, A, B, O (in that order).
    pub classes: [ClassTuning; NUM_CLASSES],
}

impl Default for GalaxyTuning {
    /// The blueprint: a real M-heavy population, a galactic band, milky haze + dust, a warm core, and
    /// glare on the brightest stars. Every value is dev-panel-tunable + RON-persisted.
    fn default() -> Self {
        // (weight%, temp_min, temp_max, brightness, size, tint, clustering, twinkle, tw_speed, softness, mag_spread)
        let class = |weight,
                     temp_min,
                     temp_max,
                     brightness,
                     size,
                     tint: [f32; 3],
                     clustering,
                     twinkle,
                     twinkle_speed,
                     softness,
                     mag_spread| ClassTuning {
            weight,
            temp_min,
            temp_max,
            brightness,
            size,
            tint,
            clustering,
            twinkle,
            twinkle_speed,
            softness,
            mag_spread,
        };
        let classes = [
            // M — red dwarfs: the bedrock (~76%), faint, tight points.
            class(
                76.45,
                2400.0,
                3500.0,
                0.08,
                0.7,
                [1.0, 1.0, 1.0],
                0.0,
                0.1,
                0.4,
                0.4,
                0.3,
            ),
            // K — orange dwarfs.
            class(
                12.1,
                3500.0,
                5000.0,
                0.25,
                0.9,
                [1.0, 1.0, 1.0],
                0.0,
                0.12,
                0.4,
                0.4,
                0.3,
            ),
            // G — yellow (solar).
            class(
                7.6,
                5000.0,
                6000.0,
                0.6,
                1.0,
                [1.0, 1.0, 1.0],
                0.0,
                0.15,
                0.5,
                0.5,
                0.4,
            ),
            // F — yellow-white.
            class(
                3.0,
                6000.0,
                7500.0,
                1.2,
                1.3,
                [1.0, 1.0, 1.0],
                0.2,
                0.2,
                0.6,
                0.6,
                0.5,
            ),
            // A — white, brilliant (analytic coverage).
            class(
                0.6,
                7500.0,
                10000.0,
                2.2,
                1.7,
                [1.0, 1.0, 1.0],
                0.5,
                0.25,
                0.8,
                0.8,
                0.6,
            ),
            // B — blue-white giants (soft, slight blue tint).
            class(
                0.13,
                10000.0,
                30000.0,
                3.5,
                2.2,
                [0.85, 0.9, 1.0],
                0.8,
                0.3,
                1.0,
                1.0,
                0.7,
            ),
            // O — blue supergiants (boosted weight for visibility; violet-blue tint; wide soft glare).
            class(
                0.02,
                30000.0,
                45000.0,
                6.0,
                2.8,
                [0.85, 0.8, 1.0],
                1.0,
                0.4,
                1.2,
                1.2,
                0.8,
            ),
        ];
        Self {
            band_angle: 0.35,
            band_width: 0.4,
            band_offset: 0.0,
            band_strength: 1.0,
            band_clumpiness: 0.5,
            haze_brightness: 0.04,
            haze_color: [0.7, 0.78, 1.0],
            dust_depth: 0.6,
            dust_scale: 0.08,
            dust_contrast: 1.5,
            core_along: 0.0,
            core_size: 0.25,
            core_brightness: 0.15,
            core_color: [1.0, 0.85, 0.6],
            core_density_boost: 0.4,
            glare_threshold: 2.0,
            glare_halo_size: 6.0,
            glare_halo_intensity: 0.3,
            glare_spike_len: 10.0,
            glare_spike_count: 4.0,
            glare_spike_intensity: 0.2,
            classes,
        }
    }
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
    /// Per-layer DEPTH parameters (R36: parallax/frequency/density + brightness & size depth
    /// multipliers + the optional tint overlay). Star character lives in `galaxy.classes`.
    pub layers: [LayerTuning; MAX_LAYERS],
    /// Refinement 35: the galaxy model (spectral-class table + band/haze/dust/core/glare). `#[serde]`
    /// default = the blueprint, so older `render_tuning.ron` files (without `galaxy`) load fine.
    #[serde(default = "default_galaxy")]
    pub galaxy: GalaxyTuning,
}

/// `render_tuning.ron` files saved before Refinement 35 have no `galaxy` block → use the blueprint.
fn default_galaxy() -> GalaxyTuning {
    GalaxyTuning::default()
}

/// Default per-layer color tint — white (no change). Used for the optional per-layer tint overlay.
fn default_tint() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

/// Default per-layer tint-overlay strength (R33/R36) — 0.0 = off (the tint overlay is a secondary,
/// off-by-default effect); also substituted for older `render_tuning.ron` layers without the field.
fn default_tint_strength() -> f32 {
    0.0
}

impl Default for StarfieldTuning {
    fn default() -> Self {
        // Per-layer DEPTH defaults reproduce R25's exponential spacing at `fi = (i/7).min(1)` (far
        // layers nearly screen-locked + dense + dim + small, near layers parallaxing + sparse).
        let layers = std::array::from_fn(|i| {
            let fi = (i as f32 / 7.0).min(1.0);
            LayerTuning {
                parallax: geomix(0.015, 0.45, fi),
                frequency: geomix(2.5, 0.35, fi),
                density: mix(0.40, 0.22, fi),
                brightness: mix(0.6, 1.0, fi),
                size: mix(0.9, 1.3, fi),
                // Optional per-layer tint overlay: off by default (strength 0 ⇒ no-op).
                tint: default_tint(),
                tint_strength: default_tint_strength(),
            }
        });
        Self {
            bloom_intensity: 0.15,
            layer_count: 8.0,
            layers,
            galaxy: GalaxyTuning::default(),
        }
    }
}

/// A built-in starfield preset (Refinement 36): a display name + a fn producing a full
/// [`StarfieldTuning`]. The dev panel shows one button per entry that loads it into the live tuning.
pub type StarfieldPreset = (&'static str, fn() -> StarfieldTuning);

/// Built-in presets. "Galaxy (realistic)" = the blueprint default; "Plain stars" = the same spectral
/// population with the galactic structure (band/haze/core/glare) switched off. Easily extended.
pub const BUILTIN_STARFIELD_PRESETS: &[StarfieldPreset] = &[
    ("Galaxy (realistic)", preset_galaxy),
    ("Plain stars", preset_plain),
];

/// The realistic galaxy preset — identical to the code default (the blueprint).
pub fn preset_galaxy() -> StarfieldTuning {
    StarfieldTuning::default()
}

/// A plain multi-temperature starfield: the same spectral population, but with the galactic band,
/// haze/dust, core bulge and bright-star glare switched off (uniform distribution, no structure).
pub fn preset_plain() -> StarfieldTuning {
    let mut t = StarfieldTuning::default();
    let g = &mut t.galaxy;
    g.band_strength = 0.0; // no band confinement → every class spreads uniformly
    g.haze_brightness = 0.0; // no milky haze
    g.core_brightness = 0.0; // no core bulge glow
    g.core_density_boost = 0.0;
    g.glare_threshold = 1.0e6; // effectively no star glares
    t
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
                size: l.size,
                // R33/R36: the OPTIONAL per-layer tint overlay — effective tint = lerp(white, tint,
                // strength), so strength 0 ⇒ white ⇒ no-op. Multiplies on top of the class color.
                tint_r: mix(1.0, l.tint[0], l.tint_strength),
                tint_g: mix(1.0, l.tint[1], l.tint_strength),
                tint_b: mix(1.0, l.tint[2], l.tint_strength),
            }
        });
        // R35: pack the spectral-class table, deriving each class's cumulative `cdf` from the editable
        // weights (normalized). Unused slots (>= NUM_CLASSES) get cdf = 1.0 so they're never picked.
        let g = &tuning.galaxy;
        let total: f32 = g
            .classes
            .iter()
            .map(|c| c.weight.max(0.0))
            .sum::<f32>()
            .max(1e-6);
        let mut acc = 0.0f32;
        let classes = std::array::from_fn(|i| {
            if i < NUM_CLASSES {
                let c = g.classes[i];
                acc += c.weight.max(0.0);
                SpectralClass {
                    cdf: acc / total,
                    temp_min: c.temp_min,
                    temp_max: c.temp_max,
                    brightness: c.brightness,
                    size: c.size,
                    tint_r: c.tint[0],
                    tint_g: c.tint[1],
                    tint_b: c.tint[2],
                    clustering: c.clustering,
                    twinkle: c.twinkle,
                    twinkle_speed: c.twinkle_speed,
                    softness: c.softness,
                    mag_spread: c.mag_spread,
                    _pad0: 0.0,
                    _pad1: 0.0,
                    _pad2: 0.0,
                }
            } else {
                SpectralClass {
                    cdf: 1.0,
                    ..Default::default()
                }
            }
        });
        mat.params = StarfieldParams {
            cam_pos: tf.translation.truncate(),
            height: cam.height,
            fov,
            resolution: Vec2::new(window.width(), window.height()),
            time: time.elapsed_secs(),
            layer_count: (tuning.layer_count.round() as u32).clamp(1, MAX_LAYERS as u32),
            // R35/R36 galaxy globals (the starfield is always the unified spectral model).
            band_angle: g.band_angle,
            band_width: g.band_width,
            band_offset: g.band_offset,
            band_strength: g.band_strength,
            band_clumpiness: g.band_clumpiness,
            haze_brightness: g.haze_brightness,
            haze_r: g.haze_color[0],
            haze_g: g.haze_color[1],
            haze_b: g.haze_color[2],
            dust_depth: g.dust_depth,
            dust_scale: g.dust_scale,
            dust_contrast: g.dust_contrast,
            core_along: g.core_along,
            core_size: g.core_size,
            core_brightness: g.core_brightness,
            core_r: g.core_color[0],
            core_g: g.core_color[1],
            core_b: g.core_color[2],
            core_density_boost: g.core_density_boost,
            glare_threshold: g.glare_threshold,
            glare_halo_size: g.glare_halo_size,
            glare_halo_intensity: g.glare_halo_intensity,
            glare_spike_len: g.glare_spike_len,
            glare_spike_count: g.glare_spike_count,
            glare_spike_intensity: g.glare_spike_intensity,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            layers,
            classes,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Refinement 35: validate the `StarfieldParams` uniform layout HEADLESSLY — writing it through
    /// encase exercises the std140-ish layout (incl. the 16-byte array-alignment rule that panicked
    /// in R25/R26), so an alignment mistake fails `cargo test` instead of only showing as a gray
    /// field at game launch.
    #[test]
    fn starfield_params_uniform_layout_is_valid() {
        use bevy::render::render_resource::encase::UniformBuffer;
        let params = StarfieldParams::default();
        let mut buf = UniformBuffer::new(Vec::<u8>::new());
        buf.write(&params)
            .expect("StarfieldParams must be a valid std140 uniform layout");
    }
}
