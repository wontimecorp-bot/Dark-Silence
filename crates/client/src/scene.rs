//! Scene setup (FR-001/FR-008/FR-012), networkized in E003 OBJ4.
//!
//! The scene now spawns **only** the locally-owned, render-bound entities: the
//! lighting, the gunsight pip, and the LOCAL player ship (the one entity this
//! client simulates via prediction). The gameplay **targets** (dummies,
//! asteroids, seeker) are no longer spawned here — they are authoritative on the
//! embedded server ([`server::ServerApp::spawn_demo_world`]) and arrive over the
//! network as interpolated **remotes** with meshes attached by
//! [`crate::net::net_update`]. This is what binds the render world to the world
//! that actually steps (Principle I): the previous local gameplay entities had
//! no system stepping them, so they were frozen.
//!
//! [`RenderAssets`] now also carries the mesh/material handles for remote ships
//! and remote targets, so `net_update` can spawn each remote with the right look
//! by [`protocol::EntityKind`] (the projectile look is reused for runtime-spawned
//! projectiles whether local or remote).

use bevy::prelude::*;
use sim::components::{FlightAssist, Health, Ship, Velocity};

use crate::net::LocalShip;
use crate::render_sync::AimPip;
use sim::ShipIntent;

/// Render assets reused for runtime-spawned visuals: projectiles (E002), and —
/// for E003's networked render path — remote **ships** and remote **targets**
/// spawned by [`crate::net::net_update`] keyed on [`protocol::EntityKind`].
#[derive(Resource)]
pub struct RenderAssets {
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
    /// Mesh/material for a remote ship (other players / AI ships). Matches the
    /// E002 player-ship look so a remote ship reads identically to the local one.
    pub ship_mesh: Handle<Mesh>,
    pub ship_material: Handle<StandardMaterial>,
    /// Mesh/material for a remote target (dummy / asteroid / seeker). One generic
    /// destructible look; the kind is carried on the [`crate::render_sync::RemoteEntity`]
    /// for any future per-kind visual split.
    pub target_mesh: Handle<Mesh>,
    pub target_material: Handle<StandardMaterial>,
}

/// Spawn lighting, the gunsight pip, and the LOCAL player ship; register the
/// shared runtime render assets (projectile + remote ship/target looks).
pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
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

    // Ship look (dart-shaped cuboid long along +X, blue) — used for both the
    // local ship spawned below and any remote ship spawned by `net_update`.
    let ship_mesh = meshes.add(Cuboid::new(1.6, 0.6, 0.3));
    let ship_material = materials.add(Color::srgb(0.30, 0.65, 1.0));

    // Generic destructible-target look for remote targets (dummies/asteroids/
    // seeker now come from the network, not the scene).
    let target_mesh = meshes.add(Cuboid::new(1.4, 1.4, 1.4));
    let target_material = materials.add(Color::srgb(0.75, 0.35, 0.30));

    commands.insert_resource(RenderAssets {
        projectile_mesh,
        projectile_material,
        ship_mesh: ship_mesh.clone(),
        ship_material: ship_material.clone(),
        target_mesh,
        target_material,
    });

    // The LOCAL player ship — spawned here deterministically so the `LocalShip`
    // tag never depends on Startup-system ordering (the old `setup_loopback_host`
    // tagging-by-`With<Ship>` could run first and miss the ship, freezing it).
    //
    // It carries exactly the components the render/input/HUD path queries by
    // `With<Ship>`: `ShipIntent` (input writes it), `FlightAssist` (toggle + HUD),
    // `Velocity` (HUD SPD, driven from prediction by `net_update`), `Health` (HUD),
    // plus the mesh/material/transform. It is driven from CLIENT-SIDE PREDICTION
    // (its `Transform` is set by `net_update`), so it gets neither `RemoteEntity`
    // (it is not interpolated) nor `RenderInterp` (no local fixed-step sim runs).
    commands.spawn((
        Ship,
        LocalShip,
        ShipIntent::default(),
        FlightAssist::On,
        Velocity(Vec2::ZERO),
        Health(100.0),
        Mesh3d(ship_mesh),
        MeshMaterial3d(ship_material),
        Transform::from_xyz(0.0, 0.0, 0.0),
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
}
