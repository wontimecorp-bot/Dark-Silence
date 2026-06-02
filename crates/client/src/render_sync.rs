//! Render-sync (ADR-0013, FR-004): the fixed-step `sim` state is mirrored into
//! interpolated Bevy `Transform`s so motion is smooth and frame-rate
//! independent. Also attaches visuals to projectiles the sim spawns at runtime.

use bevy::prelude::*;
use sim::components::{Heading, Position, Projectile};

use crate::scene::RenderAssets;

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

/// Shortest-path angular interpolation.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let diff = (b - a + PI).rem_euclid(TAU) - PI;
    a + diff * t
}
