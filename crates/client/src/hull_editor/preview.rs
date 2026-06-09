//! R60 — the hull editor's render-to-texture 3-D PREVIEW.
//!
//! A dedicated preview camera (on its own [`RenderLayers`] so it can't see the game world, and vice
//! versa) renders the working hull — built with the real combat builder [`build_hull_mesh_beveled`] —
//! to an offscreen [`Image`]. The editor displays that image in an egui pane and orbits it. The
//! preview camera/light/hull exist only while `Designing` (spawned on enter, despawned on exit).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use sim::fitting::CellShape;

use super::{HullDesignSession, HullDesignState};
use crate::scene::{build_hull_mesh_beveled, HullStyle, CELL_SIZE};

/// A render layer the main game never uses → the preview scene is isolated both ways.
const PREVIEW_LAYER: usize = 2;

/// The offscreen render target + the shared preview hull material (created once at startup).
#[derive(Resource)]
pub struct PreviewTarget {
    pub image: Handle<Image>,
    material: Handle<StandardMaterial>,
}

/// Marks every preview-scene entity (camera, light, hull) so they can be despawned together on exit.
#[derive(Component)]
struct PreviewElement;
#[derive(Component)]
struct PreviewCamera;
#[derive(Component)]
struct PreviewHull;

pub struct PreviewPlugin;

impl Plugin for PreviewPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_preview_target)
            .add_systems(OnEnter(HullDesignState::Designing), spawn_preview_scene)
            .add_systems(OnExit(HullDesignState::Designing), despawn_preview_scene)
            .add_systems(
                Update,
                (rebuild_preview_hull, orbit_preview).run_if(in_state(HullDesignState::Designing)),
            );
    }
}

/// Create the offscreen image + the preview material once at startup (so the editor's `add_image`
/// always has a target, even before the first open).
fn setup_preview_target(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let size = Extent3d {
        width: 512,
        height: 512,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[10, 14, 20, 255],
        TextureFormat::Bgra8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    let image = images.add(image);
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.58, 0.62),
        metallic: 0.7,
        perceptual_roughness: 0.4,
        ..default()
    });
    commands.insert_resource(PreviewTarget { image, material });
}

/// Spawn the preview camera + key light + an (empty) hull holder on the preview layer.
fn spawn_preview_scene(mut commands: Commands, target: Res<PreviewTarget>) {
    let layer = RenderLayers::layer(PREVIEW_LAYER);
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::srgb(0.02, 0.03, 0.05)),
            ..default()
        },
        // R60/Bevy 0.18 — the render target is its OWN component (no longer a `Camera` field).
        RenderTarget::Image(target.image.clone().into()),
        // A 3/4 top-down view (above + slightly fore) so the silhouette + the beveled edges both read.
        Transform::from_xyz(0.0, -3.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
        // The preview camera needs its own ambient fill (it's per-camera in 0.18).
        AmbientLight {
            color: Color::WHITE,
            brightness: 600.0,
            ..default()
        },
        layer.clone(),
        PreviewElement,
        PreviewCamera,
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 8000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(4.0, -3.0, 8.0).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
        PreviewElement,
    ));
    commands.spawn((
        Transform::default(),
        Visibility::default(),
        layer,
        PreviewElement,
        PreviewHull,
    ));
}

fn despawn_preview_scene(mut commands: Commands, q: Query<Entity, With<PreviewElement>>) {
    for e in &q {
        commands.entity(e).despawn();
    }
}

/// Rebuild the preview hull mesh from the working cells whenever an edit dirtied the session.
fn rebuild_preview_hull(
    mut session: ResMut<HullDesignSession>,
    mut meshes: ResMut<Assets<Mesh>>,
    target: Res<PreviewTarget>,
    mut commands: Commands,
    q: Query<Entity, With<PreviewHull>>,
) {
    if !session.dirty {
        return;
    }
    session.dirty = false;
    let Some(entity) = q.iter().next() else {
        return;
    };
    let cells: Vec<(u16, u16, CellShape)> = session
        .working
        .cells
        .iter()
        .map(|c| (c.coord.0, c.coord.1, c.shape))
        .collect();
    if cells.is_empty() {
        commands
            .entity(entity)
            .remove::<Mesh3d>()
            .remove::<MeshMaterial3d<StandardMaterial>>();
        return;
    }
    let (cols, rows) = session.working.grid_dims;
    let center = Vec2::new(cols as f32 / 2.0, rows as f32 / 2.0);
    let mesh = build_hull_mesh_beveled(&cells, CELL_SIZE, center, HullStyle::default());
    commands.entity(entity).insert((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(target.material.clone()),
    ));
}

/// Fit the preview camera to the hull's size each frame + spin the hull from the dragged orbit.
fn orbit_preview(
    session: Res<HullDesignSession>,
    mut cam: Query<&mut Transform, (With<PreviewCamera>, Without<PreviewHull>)>,
    mut hull: Query<&mut Transform, (With<PreviewHull>, Without<PreviewCamera>)>,
) {
    let (cols, rows) = session.working.grid_dims;
    let extent = (cols.max(rows) as f32) * CELL_SIZE;
    let dist = (extent * 1.6).max(2.5);
    for mut tf in &mut cam {
        // Above (+Z) + slightly fore (−Y), looking down → a 3/4 top-down view, up = +Y.
        *tf = Transform::from_xyz(0.0, -dist * 0.55, dist).looking_at(Vec3::ZERO, Vec3::Y);
    }
    let (yaw, pitch) = session.orbit;
    for mut tf in &mut hull {
        tf.rotation = Quat::from_rotation_z(yaw) * Quat::from_rotation_x(pitch);
    }
}
