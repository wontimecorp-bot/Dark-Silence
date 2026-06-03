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

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
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
    /// Localized shield-impact flash (FIX 0a refinement): a glowing cyan **arc segment
    /// of the shield ring** ([`build_arc_band_mesh`]) spawned once as a child of a
    /// rendered ship and **rotated** about Z so the lit slice faces the bullet impact
    /// bearing (`hit_dir`), shown ONLY for the split-second a shot strikes the still-up
    /// shield (`shield_flash > 0`), its alpha fading with the flash. This REPLACES the
    /// earlier small impact-point sphere (a flat ribbon reads as the deflector ring
    /// flaring, not a stray dot), which itself REPLACED the old full-ship bubble — the
    /// user disliked the whole-ship bloom.
    pub shield_arc_mesh: Handle<Mesh>,
    pub shield_material: Handle<StandardMaterial>,
    /// Ship-fragment debris (FIX 0b): a small irregular box + a darkened, desaturated
    /// ship-faction-tinted "metal fragment" material (clearly a ship piece, not a grey
    /// rock). Used for [`protocol::EntityKind::Debris`] chunks, scaled by the per-chunk
    /// size hint and given a deterministic per-id base rotation so fragments tumble and
    /// do not all align.
    pub debris_mesh: Handle<Mesh>,
    pub debris_material: Handle<StandardMaterial>,
}

/// Inner radius of the shield-impact arc band, in sim units — the near edge of the
/// glowing ring slice (just inside the deflector surface). FIX 0a refinement.
const SHIELD_ARC_INNER: f32 = 1.3;
/// Outer radius of the shield-impact arc band, in sim units — the far edge of the
/// glowing ring slice (just outside the deflector surface). FIX 0a refinement.
const SHIELD_ARC_OUTER: f32 = 1.7;
/// Half-angle of the shield-impact arc band, in radians (40°) — the arc spans
/// `[-SHIELD_ARC_HALF_ANGLE, +SHIELD_ARC_HALF_ANGLE]` about its centre bearing, so
/// the lit slice covers ~80° of the ring. FIX 0a refinement.
const SHIELD_ARC_HALF_ANGLE: f32 = std::f32::consts::PI * 40.0 / 180.0;
/// Number of angular segments across the shield-impact arc band — more segments give
/// a smoother curve at the cost of more triangles. FIX 0a refinement.
const SHIELD_ARC_SEGMENTS: u32 = 12;

/// Build a flat **annular sliver** (an arc segment of a ring) lying in the **XY plane**
/// (`z = 0`), centred on the **+X axis** and spanning `[-half_angle, +half_angle]`
/// (FIX 0a refinement — the shield-impact flash mesh).
///
/// For each of `segments + 1` angular steps at angle `a` it emits two vertices — an
/// inner `(inner_r·cos a, inner_r·sin a, 0)` and an outer `(outer_r·cos a, outer_r·sin
/// a, 0)` — and stitches consecutive inner/outer pairs into a [`PrimitiveTopology::TriangleList`]
/// (two triangles per quad). Every normal is `+Z` (`[0, 0, 1]`) so the ribbon faces the
/// top-down camera (which looks down `-Z` onto the XY plane), and each vertex carries a
/// simple `[u, 0]` UV so [`StandardMaterial`] is satisfied. Triangles are wound CCW as
/// seen from `+Z` (front face toward the camera); the shield material additionally sets
/// `cull_mode: None` + `double_sided: true` so the slice is never culled regardless of
/// winding.
///
/// The caller rotates the resulting mesh about Z to aim the centre bearing at the
/// impact direction (see [`crate::net::update_shield_bubble`]).
fn build_arc_band_mesh(inner_r: f32, outer_r: f32, half_angle: f32, segments: u32) -> Mesh {
    let segments = segments.max(1);
    let step_count = segments + 1;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(step_count as usize * 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(step_count as usize * 2);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(step_count as usize * 2);

    for i in 0..step_count {
        let t = i as f32 / segments as f32;
        let a = -half_angle + t * (2.0 * half_angle);
        let (sin, cos) = a.sin_cos();
        // Inner then outer vertex for this angular step.
        positions.push([inner_r * cos, inner_r * sin, 0.0]);
        positions.push([outer_r * cos, outer_r * sin, 0.0]);
        // Face the top-down camera (+Z toward it).
        normals.push([0.0, 0.0, 1.0]);
        normals.push([0.0, 0.0, 1.0]);
        // Simple band UVs (u across the arc, v inner→outer) — only present to satisfy
        // `StandardMaterial`'s vertex layout.
        uvs.push([t, 0.0]);
        uvs.push([t, 1.0]);
    }

    // Two triangles per quad between angular steps i and i+1. Vertex layout per step:
    // even index = inner, odd index = outer. Wound CCW as seen from +Z.
    let mut indices: Vec<u32> = Vec::with_capacity(segments as usize * 6);
    for i in 0..segments {
        let inner0 = i * 2;
        let outer0 = inner0 + 1;
        let inner1 = inner0 + 2;
        let outer1 = inner0 + 3;
        indices.extend_from_slice(&[inner0, outer0, outer1]);
        indices.extend_from_slice(&[inner0, outer1, inner1]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
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

    // Localized shield-impact flash (FIX 0a refinement): a glowing cyan ARC SEGMENT of
    // the shield ring (a flat annular sliver in the XY plane) — NOT a stray impact dot
    // and NOT a full-ship bubble (the user disliked the whole-ship bloom). The caller
    // rotates this child about Z so the lit slice faces the bullet impact bearing. This
    // is the PROTOTYPE material — each spawned flash clones its own instance so its alpha
    // can fade per-frame with `shield_flash` (a shared handle could not fade one flash
    // independently). Bright cyan with a strong cyan emissive so the slice reads as a
    // glowing deflector arc; `alpha_mode: Blend` so the alpha-driven fade is visible.
    // `cull_mode: None` + `double_sided: true` so the flat ribbon shows from the top-down
    // camera regardless of which face it presents. Starts fully transparent (`alpha 0`) —
    // `update_shield_bubble` raises the alpha to `shield_flash` only on an actual shield
    // impact.
    let shield_arc_mesh = meshes.add(build_arc_band_mesh(
        SHIELD_ARC_INNER,
        SHIELD_ARC_OUTER,
        SHIELD_ARC_HALF_ANGLE,
        SHIELD_ARC_SEGMENTS,
    ));
    let shield_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.4, 0.75, 1.0, 0.0),
        emissive: LinearRgba::rgb(0.2, 0.7, 1.2),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        double_sided: true,
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
        shield_arc_mesh,
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
