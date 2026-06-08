//! Top-down follow camera with zoom (FR-001): renders the 2D gameplay plane
//! (sim X/Y) in 3D, viewed straight down the +Z axis.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use sim::components::Ship;

/// The gameplay camera; `height` is its distance above the plane (zoom).
#[derive(Component)]
pub struct MainCamera {
    pub height: f32,
}

const MIN_HEIGHT: f32 = 12.0;
const MAX_HEIGHT: f32 = 240.0;
const ZOOM_SPEED: f32 = 60.0;

/// Spawn the 3D camera looking straight down at the origin.
pub fn setup_camera(mut commands: Commands) {
    let height = 45.0;
    commands.spawn((
        Camera3d::default(),
        MainCamera { height },
        Transform::from_xyz(0.0, 0.0, height).looking_at(Vec3::ZERO, Vec3::Y),
        // Per-camera ambient fill (Bevy 0.18 makes `AmbientLight` a component).
        AmbientLight {
            color: Color::WHITE,
            brightness: 350.0,
            ..default()
        },
        // Refinement 25: HDR + Bloom so the starfield's bright stars (and emissive ship accents
        // later) glow. `Bloom` is `#[require(Hdr)]`, so this also switches the camera to HDR +
        // tonemapping — which changes how ALL camera-pass meshes (ships, HUD bars, radar) render.
        // `intensity` is then driven live by `StarfieldTuning` (dev panel); keep it modest.
        Tonemapping::TonyMcMapface,
        Bloom {
            // R48: nudged up so the emissive engine glow / nav lights / fresnel rim pop.
            intensity: 0.20,
            ..Bloom::NATURAL
        },
    ));
}

/// Keep the camera centred on the ship. R53 — a small configurable PITCH off straight-down
/// ([`ShipVisualTuning::camera_tilt_deg`]) reveals the hull's 3-D depth: the camera orbits to
/// `(x, y - sinθ·d, cosθ·d)` and looks AT the ship (which stays centred), `d = height` (zoom). At
/// `θ = 0` this is byte-identical to the old straight-down `(x, y, height)`. Aiming is heading-based,
/// so the tilt never touches controls.
pub fn follow_camera(
    ship_q: Query<&Transform, (With<Ship>, Without<MainCamera>)>,
    mut cam_q: Query<(&mut Transform, &MainCamera)>,
    tuning: Res<crate::ship_visuals::ShipVisualTuning>,
) {
    let Ok(ship) = ship_q.single() else {
        return;
    };
    let (x, y) = (ship.translation.x, ship.translation.y);
    let theta = tuning.camera_tilt_deg.to_radians();
    let (s, c) = (theta.sin(), theta.cos());
    for (mut tf, cam) in &mut cam_q {
        let d = cam.height;
        tf.translation = Vec3::new(x, y - s * d, c * d);
        tf.look_at(Vec3::new(x, y, 0.0), Vec3::Y);
    }
}

/// `=`/`+` zooms in, `-` zooms out.
pub fn zoom_camera(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut cam_q: Query<&mut MainCamera>,
) {
    let mut delta = 0.0;
    if keys.pressed(KeyCode::Minus) {
        delta += 1.0;
    }
    if keys.pressed(KeyCode::Equal) {
        delta -= 1.0;
    }
    if delta == 0.0 {
        return;
    }
    for mut cam in &mut cam_q {
        cam.height =
            (cam.height + delta * ZOOM_SPEED * time.delta_secs()).clamp(MIN_HEIGHT, MAX_HEIGHT);
    }
}
