//! R50 — a tiny client-render particle system (no external crate): fading bits that drift, shrink, and
//! despawn. Used for the engine ION-TRAIL + DAMAGE smoke/sparks. Particles are WORLD-space entities (so
//! they're left behind as the ship moves) and capped so they can't run away. Fade is via SCALE (shrink
//! to nothing) so one shared material per kind suffices — no per-particle material. Entirely client
//! render → determinism-neutral (the jitter RNG here is NOT the sim RNG).

use bevy::prelude::*;

/// Max live particles before new spawns are dropped (cosmetic — a silent cap is fine).
pub const PARTICLE_CAP: usize = 1500;

/// One fading particle: drifts by `vel`, scales `scale0 → scale1` over `life` seconds, then despawns.
#[derive(Component)]
pub struct Particle {
    pub vel: Vec3,
    pub age: f32,
    pub life: f32,
    pub scale0: f32,
    pub scale1: f32,
}

/// Live particle count (kept in sync by spawns + [`update_particles`]) for the spawn cap.
#[derive(Resource, Default)]
pub struct ParticleCount(pub usize);

/// Spawn one world-space particle (if under the cap).
#[allow(clippy::too_many_arguments)]
pub fn spawn_particle(
    commands: &mut Commands,
    count: &mut ParticleCount,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    pos: Vec3,
    vel: Vec3,
    life: f32,
    scale0: f32,
    scale1: f32,
) {
    if count.0 >= PARTICLE_CAP {
        return;
    }
    count.0 += 1;
    commands.spawn((
        Particle {
            vel,
            age: 0.0,
            life,
            scale0,
            scale1,
        },
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(pos).with_scale(Vec3::splat(scale0)),
    ));
}

/// Age every particle each frame: drift, shrink (scale lerp), and despawn at end-of-life.
pub fn update_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut count: ResMut<ParticleCount>,
    mut q: Query<(Entity, &mut Particle, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (e, mut p, mut tf) in &mut q {
        p.age += dt;
        if p.age >= p.life {
            commands.entity(e).despawn();
            count.0 = count.0.saturating_sub(1);
            continue;
        }
        let t = p.age / p.life;
        tf.translation += p.vel * dt;
        tf.scale = Vec3::splat(p.scale0 + (p.scale1 - p.scale0) * t);
    }
}

/// A cheap deterministic hash → `[0,1)` for particle jitter (render-only; not the sim RNG).
pub fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    (x & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
}

/// A unit-ish jitter vector in `[-1,1]^3` from a seed (z biased small — the scene is near-2D).
pub fn jitter(seed: u32) -> Vec3 {
    Vec3::new(
        hash01(seed) * 2.0 - 1.0,
        hash01(seed ^ 0x1234_5678) * 2.0 - 1.0,
        (hash01(seed ^ 0x2BD1_E995) * 2.0 - 1.0) * 0.25,
    )
}
