//! Phase 4 — **automated turrets** for the mining skirmish. A turret is its own entity mounted on a
//! host (transport / outpost): it carries no `Position` (its world muzzle is computed each tick from
//! the host), independently **aims** at the nearest enemy by solving a projectile-intercept lead,
//! **slews** toward that aim under a turn-rate limit with a deterministic aim error, and **fires**
//! along its own heading when on-target — independent of any ship/host heading.
//!
//! **Aiming = orthogonal tunable parameters, not a monolithic level** (the user's design): a
//! [`TurretSpec::lead_order`] (`0` = aim current pos, `1` = first-order velocity lead, `2` = +
//! acceleration) is SEPARATE from the aim-quality knobs (`aim_sigma` jitter, `reaction_delay`,
//! `turn_rate`, `fire_range`, `fire_tolerance`). Each upgrades independently; the named presets
//! ([`TurretSpec::transport_preset`] = weaker, [`TurretSpec::outpost_preset`] = better) just bundle
//! them. (L2 reserves real target-acceleration; until per-target accel tracking lands it degrades to
//! the L1 solve — the presets use L1, differing only in aim quality.)
//!
//! **Deterministic aim error** (the sim has no RNG): a hash of `(turret entity bits, a per-turret
//! tick counter)` → a reproducible jitter, so two runs are identical. **Determinism:** gated on
//! [`ScenarioActive`](crate::ScenarioActive); a no-op in every non-scenario / test world.

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::clock::FixedDt;
use crate::components::{
    Damage, Faction, Heading, Lifetime, Position, PrevPosition, Projectile, ProjectileFaction,
    ProjectileMass, ProjectileOwner, Velocity,
};
use crate::damage::Channel;
use crate::tuning::SimTuning;
use crate::weapon::WeaponSource;

/// An automated turret entity, mounted on a host (transport/outpost). Holds the host link + mount
/// offset, its weapon params, and its dynamic firing/tracking state. The turret entity also carries
/// a [`Faction`] (the host's) and a [`Heading`] (its current aim); it has NO `Position` — its world
/// muzzle is `host.Position + mount_offset` each tick.
#[derive(Component, Clone, Copy, Debug)]
pub struct Turret {
    /// The host body this turret is mounted on (despawned with it).
    pub host: Entity,
    /// World-space mount offset from the host centre to the muzzle.
    pub mount_offset: Vec2,
    /// Damage channel of the turret's shots.
    pub channel: Channel,
    pub damage: f32,
    pub muzzle_speed: f32,
    pub fire_rate: f32,
    pub projectile_mass: f32,
    /// Seconds until the turret may fire again.
    pub cooldown: f32,
    /// Seconds left of the acquisition/reaction delay before it may fire a freshly-acquired target.
    pub reaction_timer: f32,
    /// The currently-tracked target (re-acquired each tick; a change resets `reaction_timer`).
    pub target: Option<Entity>,
    /// A per-turret monotonic counter seeding the deterministic aim jitter (advanced each tick).
    pub noise_tick: u32,
}

impl Turret {
    fn new(
        host: Entity,
        mount_offset: Vec2,
        channel: Channel,
        damage: f32,
        muzzle_speed: f32,
        fire_rate: f32,
        projectile_mass: f32,
    ) -> Self {
        Self {
            host,
            mount_offset,
            channel,
            damage,
            muzzle_speed,
            fire_rate,
            projectile_mass,
            cooldown: 0.0,
            reaction_timer: 0.0,
            target: None,
            noise_tick: 0,
        }
    }

    /// A LIGHT turret weapon (mining transport): modest damage + rate of fire.
    pub fn light(host: Entity, mount_offset: Vec2) -> Self {
        Self::new(host, mount_offset, Channel::Kinetic, 6.0, 220.0, 3.0, 0.02)
    }

    /// A HEAVIER turret weapon (refinery outpost): more damage, slightly slower.
    pub fn heavy(host: Entity, mount_offset: Vec2) -> Self {
        Self::new(host, mount_offset, Channel::Kinetic, 10.0, 240.0, 2.5, 0.03)
    }
}

/// The turret's aiming knobs — **orthogonal + independently upgradeable**. `lead_order` (the
/// prediction "smarts") is SEPARATE from the aim-quality fields (the "competence").
#[derive(Component, Clone, Copy, Debug)]
pub struct TurretSpec {
    /// `0` = aim the target's current position, `1` = first-order velocity lead, `2` = + accel.
    pub lead_order: u8,
    /// Magnitude (radians) of the deterministic aim jitter added to the solved angle.
    pub aim_sigma: f32,
    /// Acquisition/reaction delay (s) before firing on a freshly-acquired target.
    pub reaction_delay: f32,
    /// Max slew rate (rad/s) the turret rotates its aim.
    pub turn_rate: f32,
    /// Maximum engagement range.
    pub fire_range: f32,
    /// Fire only when |aim error| is within this tolerance (radians) — fire discipline.
    pub fire_tolerance: f32,
}

impl TurretSpec {
    /// WEAKER aim (mining transport): velocity lead but jittery, slow-tracking, short-ranged.
    pub fn transport_preset() -> Self {
        Self {
            lead_order: 1,
            aim_sigma: 0.12,
            reaction_delay: 0.6,
            turn_rate: 1.6,
            fire_range: 26.0,
            fire_tolerance: 0.10,
        }
    }

    /// BETTER aim (refinery outpost): tighter, faster-tracking, longer-ranged (still not a battle
    /// outpost). Same `lead_order` as the transport — the difference is the aim-quality knobs.
    pub fn outpost_preset() -> Self {
        Self {
            lead_order: 1,
            aim_sigma: 0.035,
            reaction_delay: 0.3,
            turn_rate: 3.0,
            fire_range: 38.0,
            fire_tolerance: 0.06,
        }
    }
}

/// Wrap an angle to `(-π, π]`.
fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}

/// Rotate `current` toward `desired` by at most `max_step` radians.
fn slew(current: f32, desired: f32, max_step: f32) -> f32 {
    let diff = wrap_angle(desired - current);
    wrap_angle(current + diff.clamp(-max_step, max_step))
}

/// The smallest **strictly-positive** real root of `a·t² + b·t + c = 0`, or `None`.
fn smallest_positive_root(a: f32, b: f32, c: f32) -> Option<f32> {
    if a.abs() < f32::EPSILON {
        // Degenerate to the linear case b·t + c = 0.
        if b.abs() < f32::EPSILON {
            return None;
        }
        let t = -c / b;
        return (t > 0.0).then_some(t);
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let t1 = (-b - sq) / (2.0 * a);
    let t2 = (-b + sq) / (2.0 * a);
    [t1, t2].into_iter().filter(|&t| t > 0.0).reduce(f32::min)
}

/// The desired aim **angle** (radians) for a turret at `shooter` firing at speed `s` to intercept a
/// target at `tpos` moving at `tvel` (+ `tacc` for L2). `lead_order`: `0` = aim the current pos,
/// `1` = first-order linear lead (solve the intercept quadratic), `2` = + acceleration (bounded
/// refinement). Falls back to aiming at the current position when there is no positive intercept
/// time (e.g. the target outruns the projectile).
pub fn aim_angle(shooter: Vec2, tpos: Vec2, tvel: Vec2, tacc: Vec2, s: f32, lead_order: u8) -> f32 {
    if lead_order == 0 {
        return (tpos - shooter).to_angle();
    }
    // L1: |tpos + tvel·t − shooter| = s·t  →  (v·v − s²)t² + 2(r·v)t + (r·r) = 0.
    let r = tpos - shooter;
    let v = tvel;
    let a = v.dot(v) - s * s;
    let b = 2.0 * r.dot(v);
    let c = r.dot(r);
    let Some(mut t) = smallest_positive_root(a, b, c) else {
        return r.to_angle(); // no intercept → aim at the current position
    };
    if lead_order >= 2 {
        // L2: include acceleration via a couple of fixed (deterministic) refinement steps — predict
        // pos(t) = tpos + v·t + ½·a·t², then re-estimate the travel time = |pos(t) − shooter| / s.
        for _ in 0..2 {
            let predicted = tpos + v * t + 0.5 * tacc * t * t;
            t = (predicted - shooter).length() / s.max(f32::MIN_POSITIVE);
        }
        let aim_point = tpos + v * t + 0.5 * tacc * t * t;
        return (aim_point - shooter).to_angle();
    }
    let aim_point = tpos + v * t;
    (aim_point - shooter).to_angle()
}

/// SplitMix64 — a deterministic, well-mixed 64-bit hash (no external RNG crate).
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Deterministic aim jitter in `[-1, 1)` from a turret's entity bits + its per-turret tick counter
/// — reproducible (two runs identical), per-turret distinct, and varying each tick. No RNG.
pub fn aim_noise(turret_bits: u64, tick: u32) -> f32 {
    let h = splitmix64(turret_bits ^ ((tick as u64).wrapping_mul(0x2545_F491_4F6C_DD1D)));
    let u = (h >> 40) as f64 / (1u64 << 24) as f64; // 24 high bits → [0, 1)
    (u * 2.0 - 1.0) as f32
}

/// Fixed-step turret AI (gated on `ScenarioActive`). Per turret: track the host (despawn if gone),
/// acquire the nearest ENEMY-factioned body in range, solve the intercept lead + add the
/// deterministic jitter, slew the aim toward it under the turn-rate limit, and fire along the aim
/// when on-target + past the reaction delay + off cooldown. Fires the SAME projectile shape as
/// `weapon_fire_system`, so it flows through the existing collision + Phase-2 friend/foe pipeline.
pub fn turret_system(
    dt: Res<FixedDt>,
    sim: Option<Res<SimTuning>>,
    mut commands: Commands,
    bodies: Query<(Entity, &Position, &Velocity, Option<&Faction>)>,
    mut turrets: Query<(Entity, &mut Turret, &TurretSpec, &mut Heading, &Faction)>,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();
    for (turret_e, mut turret, spec, mut heading, t_faction) in &mut turrets {
        // Track the host (despawn the turret if its host is gone — e.g. a destroyed structure).
        let Ok((_, host_pos, host_vel, _)) = bodies.get(turret.host) else {
            commands.entity(turret_e).despawn();
            continue;
        };
        let turret_pos = host_pos.0 + turret.mount_offset;
        turret.cooldown = (turret.cooldown - dt).max(0.0);
        turret.noise_tick = turret.noise_tick.wrapping_add(1);

        // Acquire the nearest ENEMY-factioned body in range (neutrals + friendlies excluded; the
        // host is same-faction so it is skipped). Deterministic tie-break by entity bits.
        let mut best: Option<(Entity, f32, Vec2, Vec2)> = None; // (entity, dist², pos, vel)
        let range2 = spec.fire_range * spec.fire_range;
        for (e, p, v, f) in &bodies {
            let is_enemy = matches!(f, Some(&bf) if bf != *t_faction);
            if !is_enemy {
                continue;
            }
            let d2 = (p.0 - turret_pos).length_squared();
            if d2 > range2 {
                continue;
            }
            let better = match best {
                None => true,
                Some((be, bd2, _, _)) => d2 < bd2 || (d2 == bd2 && e.to_bits() < be.to_bits()),
            };
            if better {
                best = Some((e, d2, p.0, v.0));
            }
        }

        // Reset the reaction timer when the target changes (or is newly acquired).
        let new_target = best.map(|b| b.0);
        if new_target != turret.target {
            turret.target = new_target;
            turret.reaction_timer = spec.reaction_delay;
        }
        turret.reaction_timer = (turret.reaction_timer - dt).max(0.0);

        let Some((_, _, tpos, tvel)) = best else {
            continue; // no target → idle (hold aim)
        };

        // Lead solve → desired aim + deterministic jitter; slew toward it.
        let base = aim_angle(
            turret_pos,
            tpos,
            tvel,
            Vec2::ZERO,
            turret.muzzle_speed,
            spec.lead_order,
        );
        let desired =
            wrap_angle(base + aim_noise(turret_e.to_bits(), turret.noise_tick) * spec.aim_sigma);
        heading.0 = slew(heading.0, desired, spec.turn_rate * dt);

        // Fire discipline: on-target + past the reaction delay + off cooldown.
        let aim_err = wrap_angle(desired - heading.0).abs();
        if turret.reaction_timer <= 0.0 && turret.cooldown <= 0.0 && aim_err < spec.fire_tolerance {
            let dir = Vec2::from_angle(heading.0);
            let vel = dir * turret.muzzle_speed + host_vel.0;
            commands.spawn((
                Projectile,
                Position(turret_pos),
                PrevPosition(turret_pos),
                Velocity(vel),
                Damage(turret.damage),
                ProjectileMass(turret.projectile_mass),
                Lifetime(sim.projectile_lifetime),
                ProjectileOwner(turret_e),
                WeaponSource::from_damage_typed(
                    turret.channel,
                    turret.damage,
                    sim.pen_per_damage,
                    sim.pen_size,
                ),
                // The turret's team — so the shot is enemy-only via the Phase-2 friend/foe gate.
                ProjectileFaction(Some(*t_faction)),
            ));
            turret.cooldown = 1.0 / turret.fire_rate.max(f32::MIN_POSITIVE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn stationary_target_aims_directly_at_it() {
        // A still target → L1 aims straight at it (intercept point == its position).
        let a = aim_angle(
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::ZERO,
            Vec2::ZERO,
            50.0,
            1,
        );
        assert!(a.abs() < 1e-4, "aims along +x at a still target (got {a})");
        let a2 = aim_angle(
            Vec2::ZERO,
            Vec2::new(0.0, 10.0),
            Vec2::ZERO,
            Vec2::ZERO,
            50.0,
            1,
        );
        assert!((a2 - FRAC_PI_2).abs() < 1e-4, "aims along +y (got {a2})");
    }

    #[test]
    fn crossing_target_leads_ahead_of_current_position() {
        // Target at (10,0) crossing +y: L1 leads ABOVE the x-axis (angle > 0); L0 aims at the
        // current position (angle == 0). So the velocity-lead knob measurably changes the aim.
        let l1 = aim_angle(
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(0.0, 8.0),
            Vec2::ZERO,
            50.0,
            1,
        );
        let l0 = aim_angle(
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(0.0, 8.0),
            Vec2::ZERO,
            50.0,
            0,
        );
        assert!(
            l0.abs() < 1e-4,
            "L0 aims at the current position (got {l0})"
        );
        assert!(
            l1 > 0.05 && l1 < FRAC_PI_2,
            "L1 leads ahead of the crossing target (got {l1})"
        );
    }

    #[test]
    fn unreachable_target_falls_back_to_current_position() {
        // Target outrunning the projectile (moving +x at 100, muzzle 50) → no intercept → aim at
        // its current position (angle 0), not NaN.
        let a = aim_angle(
            Vec2::ZERO,
            Vec2::new(10.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::ZERO,
            50.0,
            1,
        );
        assert!(
            a.is_finite() && a.abs() < 1e-4,
            "graceful fallback to current pos (got {a})"
        );
    }

    #[test]
    fn aim_noise_is_deterministic_and_bounded() {
        // Same inputs → identical output (reproducible); in range [-1, 1); distinct turrets differ.
        assert_eq!(aim_noise(42, 7), aim_noise(42, 7));
        for t in 0..50u32 {
            let n = aim_noise(123, t);
            assert!(
                (-1.0..1.0).contains(&n),
                "noise in [-1,1) (got {n} at tick {t})"
            );
        }
        assert_ne!(
            aim_noise(1, 5),
            aim_noise(2, 5),
            "different turrets get different jitter"
        );
        assert_ne!(
            aim_noise(1, 5),
            aim_noise(1, 6),
            "the jitter varies tick to tick"
        );
    }
}
