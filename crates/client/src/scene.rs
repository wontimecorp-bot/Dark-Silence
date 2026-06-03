//! Scene setup (FR-001/FR-008/FR-012), networkized in E003 OBJ4.
//!
//! The scene spawns **only** the locally-owned, render-bound entities: the
//! lighting, the gunsight pip, and the LOCAL player ship. The gameplay **targets**
//! (dummies, asteroids, seeker) and projectiles are not spawned here — they are
//! authoritative on the embedded server ([`server::ServerApp::spawn_demo_world`])
//! and are rendered by reading the server world directly each tick
//! ([`crate::net::capture_render_state`], which find-or-spawns a mesh-bearing Bevy
//! entity per authoritative entity). This binds the render world to the world that
//! actually steps (Principle I).
//!
//! [`RenderAssets`] carries the mesh/material handles for ships, the per-kind
//! targets, and projectiles, so [`crate::net::capture_render_state`] can spawn each
//! rendered entity with the right look by [`protocol::EntityKind`] (+ the target
//! sub-kind in `flags`).

use bevy::prelude::*;
use sim::components::{FlightAssist, Health, Ship, Velocity};

use crate::net::LocalShip;
use crate::render_sync::{AimPip, RenderInterp};
use sim::ShipIntent;

/// Render assets reused for the entities [`crate::net::capture_render_state`]
/// spawns from the server world: projectiles, **ships**, and per-kind **targets**,
/// keyed on [`protocol::EntityKind`] (+ the target sub-kind in `flags`).
#[derive(Resource)]
pub struct RenderAssets {
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
    /// Mesh/material for a ship (other players / AI ships). Matches the E002
    /// player-ship look so any rendered ship reads identically to the local one.
    pub ship_mesh: Handle<Mesh>,
    pub ship_material: Handle<StandardMaterial>,
    /// Per-`TargetKind` looks, picked by the server render entity's `flags` in
    /// [`crate::net::capture_render_state`] (the wire `EntityKind` only says
    /// "Target"): reddish dummy cube, grey asteroid sphere, green seeker dart —
    /// matching the E002 scene.
    pub dummy_mesh: Handle<Mesh>,
    pub dummy_material: Handle<StandardMaterial>,
    pub asteroid_mesh: Handle<Mesh>,
    pub asteroid_material: Handle<StandardMaterial>,
    pub seeker_mesh: Handle<Mesh>,
    pub seeker_material: Handle<StandardMaterial>,
    /// Localized shield-impact flash (FIX 0a): a small bright cyan disc/sphere spawned
    /// once as a child of a rendered ship and placed on the shield surface AT the
    /// bullet impact point (`hit_dir`), shown ONLY for the split-second a shot strikes
    /// the still-up shield (`shield_flash > 0`), its alpha fading with the flash. This
    /// REPLACES the old full-ship bubble — the user disliked the whole-ship bloom.
    pub shield_impact_mesh: Handle<Mesh>,
    pub shield_material: Handle<StandardMaterial>,
    /// Ship-fragment debris (FIX 0b): a small irregular box + a darkened, desaturated
    /// ship-faction-tinted "metal fragment" material (clearly a ship piece, not a grey
    /// rock). Used for [`protocol::EntityKind::Debris`] chunks, scaled by the per-chunk
    /// size hint and given a deterministic per-id base rotation so fragments tumble and
    /// do not all align.
    pub debris_mesh: Handle<Mesh>,
    pub debris_material: Handle<StandardMaterial>,
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

    // Per-kind remote target looks (dummies/asteroids/seeker now arrive over the
    // network; these mirror the original E002 scene meshes/colours).
    let dummy_mesh = meshes.add(Cuboid::new(1.4, 1.4, 1.4)); // reddish practice cube
    let dummy_material = materials.add(Color::srgb(0.75, 0.35, 0.30));
    let asteroid_mesh = meshes.add(Sphere::new(0.9)); // grey drifting rock
    let asteroid_material = materials.add(Color::srgb(0.55, 0.5, 0.45));
    let seeker_mesh = meshes.add(Cuboid::new(1.2, 0.6, 0.3)); // green seeker dart
    let seeker_material = materials.add(Color::srgb(0.35, 0.85, 0.40));

    // Localized shield-impact flash (FIX 0a): a SMALL bright disc/sphere placed AT the
    // bullet impact on the shield surface — NOT a full-ship bubble (the user disliked
    // the whole-ship bloom). This is the PROTOTYPE material — each spawned flash clones
    // its own instance so its alpha can fade per-frame with `shield_flash` (a shared
    // handle could not fade one flash independently). Bright cyan with a strong cyan
    // emissive so the hit point reads as a glowing deflector spark; `alpha_mode: Blend`
    // so the alpha-driven fade is visible. Starts fully transparent (`alpha 0`) —
    // `update_shield_bubble` raises the alpha to `shield_flash` only on an actual
    // shield impact.
    let shield_impact_mesh = meshes.add(Sphere::new(0.45));
    let shield_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.4, 0.75, 1.0, 0.0),
        emissive: LinearRgba::rgb(0.2, 0.7, 1.2),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    // Ship-fragment debris (FIX 0b): a small irregular box that reads as a torn metal
    // ship piece, with a darkened, desaturated ship-faction tint (clearly a fragment
    // of the blue ship, NOT a grey asteroid). Per-chunk scale + a deterministic id-
    // derived spin are applied at spawn (`net::spawn_render_entity`) so fragments
    // tumble and do not all align.
    let debris_mesh = meshes.add(Cuboid::new(0.7, 0.5, 0.4));
    let debris_material = materials.add(Color::srgb(0.22, 0.38, 0.55));

    commands.insert_resource(RenderAssets {
        projectile_mesh,
        projectile_material,
        ship_mesh: ship_mesh.clone(),
        ship_material: ship_material.clone(),
        dummy_mesh,
        dummy_material,
        asteroid_mesh,
        asteroid_material,
        seeker_mesh,
        seeker_material,
        shield_impact_mesh,
        shield_material,
        debris_mesh,
        debris_material,
    });

    // The LOCAL player ship — spawned here deterministically so the `LocalShip`
    // tag never depends on Startup-system ordering (the old `setup_loopback_host`
    // tagging-by-`With<Ship>` could run first and miss the ship, freezing it).
    //
    // It carries exactly the components the render/input/HUD path queries by
    // `With<Ship>`: `ShipIntent` (input writes it), `FlightAssist` (toggle + HUD),
    // `Velocity` (HUD SPD, set from the server's authoritative speed by
    // `net::capture_render_state`), `Health` (HUD), plus the mesh/material/transform.
    //
    // It also carries a `RenderInterp` (snapped to the origin): the local ship is
    // no longer special-cased — it renders from the embedded server's world like
    // every other entity. `net::capture_render_state` rolls its `RenderInterp`
    // prev→curr each fixed step from the authoritative pose, and
    // `render_sync::interpolate_transforms` blends it into the `Transform` each
    // frame (E002's smooth fixed-step interpolation). The net plugin's `Startup`
    // maps this entity under the client's authoritative ship id.
    commands.spawn((
        Ship,
        LocalShip,
        ShipIntent::default(),
        FlightAssist::On,
        Velocity(Vec2::ZERO),
        Health(100.0),
        RenderInterp::snapped(Vec2::ZERO, 0.0),
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
