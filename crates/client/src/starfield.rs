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

use crate::camera::MainCamera;

/// Path (under `assets/`) of the starfield fragment shader (loads via the R23 asset root).
const STARFIELD_SHADER: &str = "shaders/starfield.wgsl";

/// Hard cap on shader star layers — MUST match `MAX_LAYERS` in `starfield.wgsl`.
pub const MAX_LAYERS: u32 = 16;

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
    /// Live tunables (see [`StarfieldTuning`]).
    pub star_brightness: f32,
    pub star_density: f32,
    pub twinkle_amount: f32,
    pub layer_count: u32,
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

/// Live, dev-panel-tunable starfield + bloom knobs (client-only; NOT behind the `dev_panel` feature
/// so the apply path always compiles). `Default` = a dim backdrop + low bloom so lit ships clearly
/// out-read the background.
#[derive(Resource, Clone, Copy)]
pub struct StarfieldTuning {
    /// Camera bloom strength (applied to the `Bloom` component).
    pub bloom_intensity: f32,
    /// Overall star brightness multiplier.
    pub star_brightness: f32,
    /// Star density (0..1, fraction of candidate cells that host a star).
    pub star_density: f32,
    /// Twinkle amount (0 = steady).
    pub twinkle_amount: f32,
    /// Parallax layer count (stored as f32 for the slider; rounded + clamped to [`MAX_LAYERS`]).
    pub layer_count: f32,
}

impl Default for StarfieldTuning {
    fn default() -> Self {
        Self {
            bloom_intensity: 0.15,
            star_brightness: 1.0,
            star_density: 1.0,
            twinkle_amount: 1.0,
            layer_count: 8.0,
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
        mat.params = StarfieldParams {
            cam_pos: tf.translation.truncate(),
            height: cam.height,
            fov,
            resolution: Vec2::new(window.width(), window.height()),
            time: time.elapsed_secs(),
            star_brightness: tuning.star_brightness,
            star_density: tuning.star_density,
            twinkle_amount: tuning.twinkle_amount,
            layer_count: (tuning.layer_count.round() as u32).clamp(1, MAX_LAYERS),
        };
    }
}
