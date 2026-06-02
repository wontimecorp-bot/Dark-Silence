//! Render-sync (ADR-0013, FR-004): the fixed-step `sim` state is mirrored into
//! interpolated Bevy `Transform`s so motion is smooth and frame-rate
//! independent. Also attaches visuals to projectiles the sim spawns at runtime.
//!
//! E003 OBJ4 (T044) networkizes this seam (AD-005, ADR-0013):
//! - the **local** ship renders from the *predicted* state + the smoothed
//!   reconciliation correction (driven by [`crate::net`], OBJ3) — it is the one
//!   entity that is simulated locally, so it keeps the fixed-step
//!   [`RenderInterp`] path for any non-networked single-player use;
//! - **remote** entities (other ships, projectiles, targets) are NOT simulated
//!   locally; they carry a [`RemoteEntity`] tag and render from snapshot
//!   *interpolation* ~100 ms in the past ([`crate::interpolation`], OBJ4), driven
//!   by [`crate::net::net_update`].
//!
//! The E002 gunsight pip and follow camera continue to read the local ship's
//! rendered `Transform`, so their feel is unchanged whether the ship's pose comes
//! from the local fixed step or from prediction.

use bevy::prelude::*;
use protocol::{EntityId, EntityKind};
use sim::components::{Heading, Position, Projectile, Ship};

use crate::scene::RenderAssets;

/// How far ahead of the ship's nose the gunsight pip sits, in sim units.
const AIM_DISTANCE: f32 = 5.0;

/// Tags a rendered entity as a **remote** (interpolated) entity, not the local
/// predicted ship (AD-005). Its `Transform` is driven from snapshot interpolation
/// ([`crate::interpolation::SnapshotBuffer::interpolate_remotes`]) by
/// [`crate::net::net_update`], keyed by its stable network [`EntityId`]. Remotes
/// are explicitly distinct from the local ship so the two render paths
/// (prediction vs interpolation) never cross.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteEntity {
    /// Stable network id, matched to the per-frame interpolated set.
    pub id: EntityId,
    /// What kind of remote it is (picks the prefab/visual).
    pub kind: EntityKind,
}

/// Marker for the forward gunsight pip — a world-space marker placed ahead of
/// the ship along its heading, showing where the fixed weapon will fire.
#[derive(Component)]
pub struct AimPip;

/// Previous + current sim snapshots for one entity. `interpolate_transforms`
/// blends between them by the fixed-step overstep fraction.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RenderInterp {
    pub prev_pos: Vec2,
    pub curr_pos: Vec2,
    pub prev_heading: f32,
    pub curr_heading: f32,
}

impl RenderInterp {
    /// Both snapshots set to the same pose (no interpolation on the first frame).
    pub fn snapped(pos: Vec2, heading: f32) -> Self {
        Self {
            prev_pos: pos,
            curr_pos: pos,
            prev_heading: heading,
            curr_heading: heading,
        }
    }
}

/// `FixedUpdate`, last in the chain: roll current → previous, then capture the
/// new sim pose. Heading is optional (only the ship has one).
pub fn capture_sim_state(mut q: Query<(&Position, Option<&Heading>, &mut RenderInterp)>) {
    for (pos, heading, mut interp) in &mut q {
        interp.prev_pos = interp.curr_pos;
        interp.prev_heading = interp.curr_heading;
        interp.curr_pos = pos.0;
        if let Some(h) = heading {
            interp.curr_heading = h.0;
        }
    }
}

/// `Update`: blend the rendered `Transform` between the two latest fixed-step
/// poses by the fixed-timestep overstep fraction — frame-rate-independent feel.
pub fn interpolate_transforms(
    fixed: Res<Time<Fixed>>,
    mut q: Query<(&RenderInterp, &mut Transform)>,
) {
    let alpha = fixed.overstep_fraction();
    for (interp, mut tf) in &mut q {
        let p = interp.prev_pos.lerp(interp.curr_pos, alpha);
        let h = lerp_angle(interp.prev_heading, interp.curr_heading, alpha);
        tf.translation.x = p.x;
        tf.translation.y = p.y;
        tf.rotation = Quat::from_rotation_z(h);
    }
}

/// Attach a mesh/material/transform (and a render-interp snapshot) to any
/// projectile the sim has spawned but that has no visual yet.
pub fn add_projectile_visuals(
    mut commands: Commands,
    assets: Res<RenderAssets>,
    q: Query<(Entity, &Position), (With<Projectile>, Without<Mesh3d>)>,
) {
    for (entity, pos) in &q {
        commands.entity(entity).insert((
            Mesh3d(assets.projectile_mesh.clone()),
            MeshMaterial3d(assets.projectile_material.clone()),
            Transform::from_xyz(pos.0.x, pos.0.y, 0.0),
            RenderInterp::snapped(pos.0, 0.0),
        ));
    }
}

/// Keep the gunsight pip a fixed distance ahead of the ship's nose, along the
/// (interpolated) heading — so it shows the actual firing line for the fixed
/// forward weapon. Runs after `interpolate_transforms` so it reads the smoothed
/// ship pose.
pub fn update_aim_pip(
    ship_q: Query<&Transform, (With<Ship>, Without<AimPip>)>,
    mut pip_q: Query<&mut Transform, With<AimPip>>,
) {
    let Ok(ship) = ship_q.single() else {
        return;
    };
    let forward = ship.rotation * Vec3::X; // ship nose is +X local
    for mut pip in &mut pip_q {
        pip.translation = ship.translation + forward * AIM_DISTANCE;
    }
}

/// Shortest-path angular interpolation.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let diff = (b - a + PI).rem_euclid(TAU) - PI;
    a + diff * t
}
