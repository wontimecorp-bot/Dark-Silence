//! Scene setup (FR-001/FR-008/FR-012): spawn the tinted composed-primitive
//! ship, the static dummies, drifting asteroids, and the single seeker — each
//! with its `sim` gameplay components, a Bevy mesh/material, and a render-interp
//! snapshot. Also holds the shared projectile render assets.

use bevy::prelude::*;
use sim::components::{
    AngularVelocity, CollisionRadius, FlightAssist, Heading, Health, Position, Ship, Target,
    TargetKind, Velocity, Weapon,
};
use sim::Tuning;

use crate::render_sync::{AimPip, RenderInterp};

/// Render assets reused for every runtime-spawned projectile.
#[derive(Resource)]
pub struct RenderAssets {
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
}

/// Spawn lighting, the player ship, and the targets, and register the shared
/// projectile assets.
pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    tuning: Res<Tuning>,
) {
    // Lighting: a key directional light so PBR primitives read (ambient fill is
    // attached to the camera in `camera::setup_camera`).
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            ..default()
        },
        Transform::from_xyz(6.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Shared projectile visuals (a small glowing bullet).
    let projectile_mesh = meshes.add(Sphere::new(0.2));
    let projectile_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.9, 0.35),
        emissive: LinearRgba::rgb(1.2, 0.7, 0.1),
        ..default()
    });
    commands.insert_resource(RenderAssets {
        projectile_mesh,
        projectile_material,
    });

    // Player ship: a dart-shaped cuboid long along +X (its nose at heading 0).
    let ship_mesh = meshes.add(Cuboid::new(1.6, 0.6, 0.3));
    let ship_material = materials.add(Color::srgb(0.30, 0.65, 1.0));
    commands.spawn((
        Ship,
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        Health(100.0),
        // Flight-model is the default (drag-capped top speed, angular inertia,
        // shared power budget — the "Silent Death but better" feel). Press F to
        // toggle the decoupled/Newtonian mode.
        FlightAssist::On,
        CollisionRadius(0.8),
        Weapon {
            cooldown: 0.0,
            fire_rate: tuning.fire_rate,
            muzzle_speed: tuning.muzzle_speed,
        },
        Mesh3d(ship_mesh),
        MeshMaterial3d(ship_material),
        Transform::from_xyz(0.0, 0.0, 0.0),
        RenderInterp::snapped(Vec2::ZERO, 0.0),
    ));

    // Forward gunsight pip — a glowing marker ahead of the nose showing the
    // fixed weapon's firing line (positioned each frame by `update_aim_pip`).
    commands.spawn((
        AimPip,
        Mesh3d(meshes.add(Sphere::new(0.18))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.4, 1.0, 0.9),
            emissive: LinearRgba::rgb(0.2, 1.0, 0.8),
            ..default()
        })),
        Transform::from_xyz(5.0, 0.0, 0.0),
    ));

    // Targets.
    spawn_dummy(
        &mut commands,
        &mut meshes,
        &mut materials,
        Vec2::new(11.0, 4.0),
    );
    spawn_dummy(
        &mut commands,
        &mut meshes,
        &mut materials,
        Vec2::new(15.0, -5.0),
    );
    spawn_asteroid(
        &mut commands,
        &mut meshes,
        &mut materials,
        Vec2::new(-13.0, 7.0),
        Vec2::new(2.5, -1.2),
    );
    spawn_asteroid(
        &mut commands,
        &mut meshes,
        &mut materials,
        Vec2::new(-7.0, -11.0),
        Vec2::new(1.0, 2.0),
    );
    spawn_seeker(
        &mut commands,
        &mut meshes,
        &mut materials,
        Vec2::new(22.0, 16.0),
    );
}

fn spawn_dummy(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pos: Vec2,
) {
    commands.spawn((
        Target,
        TargetKind::Dummy,
        Position(pos),
        Velocity(Vec2::ZERO),
        CollisionRadius(0.9),
        Health(20.0),
        Mesh3d(meshes.add(Cuboid::new(1.4, 1.4, 1.4))),
        MeshMaterial3d(materials.add(Color::srgb(0.75, 0.35, 0.30))),
        Transform::from_xyz(pos.x, pos.y, 0.0),
        RenderInterp::snapped(pos, 0.0),
    ));
}

fn spawn_asteroid(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pos: Vec2,
    vel: Vec2,
) {
    commands.spawn((
        Target,
        TargetKind::Asteroid,
        Position(pos),
        Velocity(vel),
        CollisionRadius(0.9),
        Health(40.0),
        Mesh3d(meshes.add(Sphere::new(0.9))),
        MeshMaterial3d(materials.add(Color::srgb(0.55, 0.5, 0.45))),
        Transform::from_xyz(pos.x, pos.y, 0.0),
        RenderInterp::snapped(pos, 0.0),
    ));
}

fn spawn_seeker(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pos: Vec2,
) {
    commands.spawn((
        Target,
        TargetKind::Seeker,
        Position(pos),
        Velocity(Vec2::ZERO),
        CollisionRadius(0.7),
        Health(30.0),
        Mesh3d(meshes.add(Cuboid::new(1.2, 0.6, 0.3))),
        MeshMaterial3d(materials.add(Color::srgb(0.35, 0.85, 0.40))),
        Transform::from_xyz(pos.x, pos.y, 0.0),
        RenderInterp::snapped(pos, 0.0),
    ));
}
