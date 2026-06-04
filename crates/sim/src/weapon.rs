//! Weapon timing: pure cooldown helpers plus the fixed-step firing and
//! projectile-advance systems.
//!
//! Phase 8 (E007 combat integration) adds the [`WeaponSource`] damage-typing
//! component the new damage pipeline reads, plus [`damage_event_from_hit`] — the
//! adapter that turns an E002 projectile [`SweptHit`] into the typed E007
//! [`DamageEvent`] (FR-001). The E002 [`Weapon`]/[`WeaponProfile`] stay the
//! fire-timing source; [`WeaponSource`] only adds the channel/penetration the
//! pipeline needs.

use crate::clock::FixedDt;
use crate::components::{
    Damage, Energy, Heading, Heat, Lifetime, Position, PrevPosition, Projectile, ProjectileMass,
    ProjectileOwner, Ship, Velocity, Weapon,
};
use crate::damage::{Channel, DamageEvent};
use crate::fitting::ShipStats;
use crate::intent::ShipIntent;
use crate::physics::SweptHit;
use crate::tuning::{SimTuning, Tuning};
use bevy_ecs::prelude::*;
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// Default damage a projectile carries (Damage > 0, INV-04) — the unfitted-ship
/// fallback when the shot's source is the [`Weapon`] component, which has no
/// per-shot damage field. Fitted ships use their [`ShipStats`] weapon profile's
/// `damage`.
pub(crate) const PROJECTILE_DAMAGE: f32 = 10.0;
/// Projectile time-to-live in seconds.
pub(crate) const PROJECTILE_LIFETIME: f32 = 3.0;
/// Phase M4/M5 — **fallback projectile inertial mass** for the UNFITTED gun (and any projectile
/// spawned without a [`ProjectileMass`](crate::components::ProjectileMass)). A fitted weapon now
/// carries its own per-weapon slug mass (`WeaponProfile::projectile_mass`); this is the global
/// default the legacy E002 `Weapon` path uses for recoil + knockback (`p = mass · velocity`).
/// Small relative to ship mass so a shot nudges rather than flings; **tunable**. Sim-side → part
/// of the determinism contract.
pub const PROJECTILE_MASS: f32 = 0.03;

// --- E007 damage-typing seam (T037) ---------------------------------------------

/// MVP penetration value per point of projectile [`Damage`] (NEW-CONFIG seam). The
/// fixed-forward gun derives `penetration = damage * PEN_PER_DAMAGE` so a harder-
/// hitting shot also punches deeper. Tuned so a seed autocannon shot
/// (`damage 12`) carries `penetration ≈ 36` — enough to clean-penetrate a thin
/// (steel, thickness ~1) plate but stopped by a thick one. A later content pass
/// sources per-weapon penetration from [`ModuleSpecifics::Weapon`](crate::fitting::ModuleSpecifics)
/// directly (NEW-CONFIG).
pub(crate) const PEN_PER_DAMAGE: f32 = 3.0;
/// MVP penetrator size for the overmatch test (NEW-CONFIG seam). A constant small
/// slug for the seed gun, below the overmatch ratio against any meaningful plate,
/// so the angle/penetration gate (not overmatch) decides the outcome. Later
/// content sources this per-weapon (NEW-CONFIG).
pub(crate) const PEN_SIZE: f32 = 1.0;

/// The damage-typing carrier the E007 pipeline reads off a fired projectile
/// (contracts/damage-api.md §5 `WeaponSource`, T037).
///
/// The E002 [`Weapon`]/[`WeaponProfile`] stay the fire-timing + damage source;
/// `WeaponSource` adds the **channel + penetration** the new
/// [`apply_damage`](crate::damage::apply_damage) pipeline needs but the legacy
/// path never carried. It is attached to each projectile at fire time and read at
/// hit time by [`damage_event_from_hit`].
///
/// **MVP defaults (NEW-CONFIG seam)**: the only delivery this epic is the
/// fixed-forward gun, typed [`Channel::Kinetic`] with penetration derived from the
/// shot's [`Damage`] ([`WeaponSource::from_damage`]). A later content pass sources
/// `channel`/`penetration`/`pen_size` per-weapon from
/// [`ModuleSpecifics::Weapon`](crate::fitting::ModuleSpecifics) /
/// [`WeaponProfile`] instead of these constants — without rippling into the E006
/// fitting suite (kept untouched this phase).
///
/// Derive discipline matches `crate::components`: serde as the replication
/// (E003) / persistence (E004) seam — present, not exercised this epic; value
/// semantics. `Copy` (it is small and attached per projectile).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeaponSource {
    /// The damage channel the shot delivers (selects the matrix row, FR-001).
    pub channel: Channel,
    /// Penetration value vs effective armor (`>= 0`, FR-005/008).
    pub penetration: f32,
    /// Penetrator size for the overmatch test vs plate thickness (`>= 0`, FR-007).
    pub pen_size: f32,
}

impl WeaponSource {
    /// The MVP fixed-forward gun typing derived from a shot's per-projectile
    /// [`Damage`] (NEW-CONFIG seam): [`Channel::Kinetic`], penetration scaled from
    /// the damage, a constant slug size. The single delivery this epic. Uses the
    /// compile-time [`PEN_PER_DAMAGE`]/[`PEN_SIZE`]; the live-tunable path
    /// (`weapon_fire_system` reading `SimTuning`) calls [`from_damage_with`].
    pub fn from_damage(damage: f32) -> Self {
        Self::from_damage_with(damage, PEN_PER_DAMAGE, PEN_SIZE)
    }

    /// [`from_damage`] with explicit penetration scaling + slug size (Phase M6 live tuning):
    /// `penetration = damage · pen_per_damage`, `pen_size = pen_size`. Channel is
    /// [`Channel::Kinetic`] (the unfitted/legacy default); fitted weapons use
    /// [`from_damage_typed`](WeaponSource::from_damage_typed) with their own channel.
    pub fn from_damage_with(damage: f32, pen_per_damage: f32, pen_size: f32) -> Self {
        Self::from_damage_typed(Channel::Kinetic, damage, pen_per_damage, pen_size)
    }

    /// Phase C — [`from_damage_with`] with an explicit damage [`Channel`] from the fitted weapon's
    /// `damage_type` (a fitted ship's `WeaponProfile::channel`). A weapon typed `Kinetic` is
    /// byte-identical to the old hardcoded path.
    pub fn from_damage_typed(
        channel: Channel,
        damage: f32,
        pen_per_damage: f32,
        pen_size: f32,
    ) -> Self {
        Self {
            channel,
            penetration: damage.max(0.0) * pen_per_damage,
            pen_size,
        }
    }
}

/// Build a typed [`DamageEvent`] from an E002 projectile hit (FR-001, T037;
/// contracts/damage-api.md §5 `damage_event_from_hit`).
///
/// The adapter that bridges the reused E002 swept-CCD ([`SweptHit`]) into the E007
/// pipeline: `channel`/`penetration`/`pen_size` come from the projectile's
/// [`WeaponSource`], `magnitude` from its [`Damage`] (`> 0`, INV-04 preserved),
/// the hit `point` from the reused [`SweptHit`], `dir` from the projectile's
/// travel direction (incoming, normalized), and `source` from the
/// [`ProjectileOwner`] (so wreck claiming + self-hit prevention still apply).
///
/// **Local-space caveat**: the returned `point`/`dir` are whatever space the
/// caller passes — [`apply_damage`](crate::damage::apply_damage) /
/// `resolve_entry_point` expect them in the target's **hull-local cell-space**, so
/// the combat system (T038) transforms the world-space `hit.point` /
/// projectile-velocity direction into the target's local frame **before** calling
/// this. Pure; reads only its arguments.
pub fn damage_event_from_hit(
    hit: &SweptHit,
    src: &WeaponSource,
    dmg: f32,
    dir: Vec2,
    owner: Option<Entity>,
) -> DamageEvent {
    DamageEvent {
        channel: src.channel,
        magnitude: dmg,
        penetration: src.penetration,
        pen_size: src.pen_size,
        point: hit.point,
        dir: dir.normalize_or_zero(),
        source: owner,
    }
}

/// A weapon may fire only once its cooldown has elapsed (INV-03).
pub fn can_fire(cooldown: f32) -> bool {
    cooldown <= 0.0
}

/// The cooldown (seconds) set immediately after firing, from the fire rate.
pub fn cooldown_after_fire(fire_rate: f32) -> f32 {
    debug_assert!(fire_rate > 0.0, "fire_rate must be positive (INV-10)");
    1.0 / fire_rate
}

/// Fixed-step weapon firing (FR-005): tick each ship's cooldown down, and on
/// that ship's own `fire` intent (when cool) spawn a projectile along its
/// heading at muzzle speed. The projectile records its spawn position as
/// `PrevPosition` so the very first swept test has a valid segment.
///
/// Intent is **per-entity**: the ship query carries each ship's own
/// [`ShipIntent`] component, so N independently-controlled ships fire from their
/// own inputs in one shared step. A ship without the component is not piloted
/// and does not fire.
///
/// **Override-or-fallback weapon source** (FR-014/016, the E006 rewire): a ship
/// that carries a fit-derived [`ShipStats`] component is gated on
/// [`ShipStats::can_fire`] (no weapon module ⇒ no fire) and fires with that fit's
/// [`WeaponProfile`](crate::fitting::WeaponProfile) params + damage; a ship
/// without [`ShipStats`] keeps the exact E002 [`Weapon`]-component behavior. The
/// [`Weapon`] component still owns the cooldown state machine (INV-03) on both
/// paths, so the cooldown gate is unchanged. A fitted ship that cannot fire still
/// has its cooldown ticked harmlessly.
pub fn weapon_fire_system(
    dt: Res<FixedDt>,
    tuning: Res<Tuning>,
    // Phase M6: live-tunable projectile/pen consts. `Option` so a minimal world (no `SimTuning`,
    // e.g. the headless sim tests) degrades to the const defaults rather than panicking.
    sim: Option<Res<SimTuning>>,
    mut commands: Commands,
    // Fitted ships: ShipStats gates firing + supplies the profile; the optional
    // Weapon component (present when a weapon module is installed) holds cooldown.
    // `&mut Velocity` (Phase M4): the shooter recoils + its motion is inherited by the shot.
    mut fitted: Query<
        (
            Entity,
            &ShipIntent,
            &Position,
            &Heading,
            &mut Velocity,
            &ShipStats,
            Option<&mut Weapon>,
            // Phase E: the dynamic pools — present only on LIVE-spawned fitted ships. `Option` so the
            // headless sim/determinism worlds (no pools) keep the exact prior firing behavior.
            Option<&mut Energy>,
            Option<&mut Heat>,
        ),
        With<Ship>,
    >,
    // Unfitted ships: the E002 Weapon-component behavior, + Phase M4 recoil/inheritance.
    mut unfitted: Query<
        (
            Entity,
            &ShipIntent,
            &Position,
            &Heading,
            &mut Velocity,
            &mut Weapon,
        ),
        (With<Ship>, Without<ShipStats>),
    >,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();

    // Fitted path: fit-derived can_fire + WeaponProfile (FR-016).
    for (owner, intent, pos, heading, mut ship_vel, stats, weapon, mut energy, mut heat) in
        &mut fitted
    {
        // No weapon module ⇒ cannot fire; if a Weapon component lingers, still
        // tick its cooldown so it stays a valid (idle) state machine.
        let (Some(profile), Some(mut weapon)) = (stats.weapon, weapon) else {
            continue;
        };
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        // Phase E: a shot costs Energy + builds Heat, and is gated on both (when the pools exist).
        // `is_none_or` ⇒ a ship without the pools (the headless/test path) fires exactly as before.
        let shot_cost = profile.damage * sim.weapon_energy_per_damage;
        let energy_ok = energy.as_ref().is_none_or(|e| e.current >= shot_cost);
        let heat_ok = heat.as_ref().is_none_or(|h| h.current < h.max);
        if stats.can_fire && intent.fire && can_fire(weapon.cooldown) && energy_ok && heat_ok {
            // Phase M4: the muzzle velocity (the gun's contribution) plus the shooter's own
            // velocity, so a moving ship's shots carry its motion (a true Newtonian gun).
            let muzzle = Vec2::from_angle(heading.0) * profile.muzzle_speed;
            let vel = muzzle + ship_vel.0;
            commands.spawn((
                Projectile,
                Position(pos.0),
                PrevPosition(pos.0),
                Velocity(vel),
                Damage(profile.damage),
                // Phase M5: the per-weapon slug mass, carried to the hit for the impulse.
                ProjectileMass(profile.projectile_mass),
                Lifetime(sim.projectile_lifetime),
                ProjectileOwner(owner),
                // E007 damage typing (T037) + Phase C: the shot carries the fitted weapon's own
                // damage `Channel` (`profile.channel`) with penetration derived from the shot
                // damage (M6: live `pen_per_damage`/`pen_size`). Seed autocannon = Kinetic →
                // byte-identical to the old hardcoded path.
                WeaponSource::from_damage_typed(
                    profile.channel,
                    profile.damage,
                    sim.pen_per_damage,
                    sim.pen_size,
                ),
            ));
            // Phase M4/M5 recoil: conserve momentum against the MUZZLE component only (the inherited
            // part was already the ship's momentum), using the per-weapon slug mass.
            // Δv = −(projectile_mass·muzzle)/ship_mass.
            ship_vel.0 -=
                profile.projectile_mass * muzzle / stats.total_mass.max(f32::MIN_POSITIVE);
            // Phase E: spend energy + add heat on the shot (no-op when the pools are absent).
            if let Some(e) = energy.as_mut() {
                e.current = (e.current - shot_cost).max(0.0);
            }
            if let Some(h) = heat.as_mut() {
                h.current += profile.heat;
            }
            weapon.cooldown = cooldown_after_fire(profile.fire_rate);
        }
    }

    // Unfitted path: the original Weapon-component behavior (E001/E002/E003) + M4 recoil.
    for (owner, intent, pos, heading, mut ship_vel, mut weapon) in &mut unfitted {
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        if intent.fire && can_fire(weapon.cooldown) {
            let muzzle = Vec2::from_angle(heading.0) * weapon.muzzle_speed;
            let vel = muzzle + ship_vel.0;
            commands.spawn((
                Projectile,
                Position(pos.0),
                PrevPosition(pos.0),
                Velocity(vel),
                Damage(sim.projectile_damage),
                // Phase M5/M6: the unfitted gun has no profile → the live fallback slug mass.
                ProjectileMass(sim.projectile_mass),
                Lifetime(sim.projectile_lifetime),
                ProjectileOwner(owner),
                // E007 damage typing (T037): harmless on the unfitted E002/E003
                // path (those targets resolve via the flat `Health` clamp, which
                // never reads `WeaponSource`); present so a fitted target a legacy
                // shot happens to hit still routes through the new pipeline.
                WeaponSource::from_damage_with(
                    sim.projectile_damage,
                    sim.pen_per_damage,
                    sim.pen_size,
                ),
            ));
            // Phase M4/M6 recoil — unfitted ships use the global Tuning mass + live slug mass.
            ship_vel.0 -= sim.projectile_mass * muzzle / tuning.mass.max(f32::MIN_POSITIVE);
            weapon.cooldown = cooldown_after_fire(weapon.fire_rate);
        }
    }
}

/// Fixed-step projectile advance (FR-006): record the previous position (the
/// tail of the swept segment), move by velocity, age the lifetime, and despawn
/// when it expires (INV-06).
pub fn projectile_step_system(
    dt: Res<FixedDt>,
    mut commands: Commands,
    mut q: Query<
        (
            Entity,
            &mut Position,
            &mut PrevPosition,
            &Velocity,
            &mut Lifetime,
        ),
        With<Projectile>,
    >,
) {
    let dt = dt.0;
    for (e, mut pos, mut prev, vel, mut life) in &mut q {
        prev.0 = pos.0;
        pos.0 += vel.0 * dt;
        life.0 -= dt;
        if life.0 <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_only_when_cool() {
        assert!(can_fire(0.0));
        assert!(can_fire(-0.1));
        assert!(!can_fire(0.2));
    }

    #[test]
    fn firing_sets_positive_cooldown_from_rate() {
        assert!(cooldown_after_fire(5.0) > 0.0);
        assert!((cooldown_after_fire(5.0) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn weapon_source_from_damage_is_kinetic_and_scales_penetration() {
        let src = WeaponSource::from_damage(12.0);
        assert_eq!(src.channel, Channel::Kinetic);
        assert!(src.penetration > 0.0, "a shot carries penetration");
        assert!((src.penetration - 12.0 * PEN_PER_DAMAGE).abs() < 1e-6);
        assert_eq!(src.pen_size, PEN_SIZE);
        // A zero/negative damage never yields negative penetration.
        assert_eq!(WeaponSource::from_damage(-5.0).penetration, 0.0);
    }

    #[test]
    fn damage_event_from_hit_carries_typing_geometry_and_owner() {
        let owner = Entity::from_raw_u32(7).expect("valid raw entity index");
        let src = WeaponSource::from_damage(20.0);
        let hit = SweptHit {
            toi: 0.5,
            point: Vec2::new(3.0, 4.0),
        };
        // The projectile travels right and slightly up; `dir` is normalized.
        let ev = damage_event_from_hit(&hit, &src, 20.0, Vec2::new(2.0, 0.0), Some(owner));
        assert_eq!(ev.channel, Channel::Kinetic);
        assert_eq!(
            ev.magnitude, 20.0,
            "magnitude is the projectile Damage (>0)"
        );
        assert_eq!(ev.penetration, src.penetration);
        assert_eq!(ev.pen_size, src.pen_size);
        assert_eq!(ev.point, hit.point, "hit point from the reused SweptHit");
        assert!(
            (ev.dir - Vec2::new(1.0, 0.0)).length() < 1e-6,
            "dir is the normalized incoming travel direction"
        );
        assert_eq!(ev.source, Some(owner), "source is the ProjectileOwner");
    }

    /// Phase E: a fitted ship's shot **drains Energy + adds Heat**, and firing is **gated** on
    /// having enough energy AND not being overheated.
    #[test]
    fn firing_drains_energy_adds_heat_and_gates_on_both() {
        use crate::components::{Energy, Heat};
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{build_layout, derive_ship_stats, seed_catalogs, Fit, SlotId};
        use crate::fitting::{ShipStats, HULL_FIGHTER};

        // A fighter fit that can fire (autocannon: damage 12, fire_rate 5, heat 3).
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let layout = build_layout(hull, &fit, &modules);
        let stats: ShipStats = derive_ship_stats(hull, &fit, &modules, &layout);
        let profile = stats.weapon.expect("autocannon fitted");
        let sim = SimTuning::default();
        let shot_cost = profile.damage * sim.weapon_energy_per_damage;

        let mut w = World::new();
        w.insert_resource(FixedDt(0.05));
        w.insert_resource(Tuning::default());
        w.insert_resource(sim);
        let ship = w
            .spawn((
                Ship,
                ShipIntent {
                    fire: true,
                    ..Default::default()
                },
                Position(Vec2::ZERO),
                Heading(0.0),
                Velocity(Vec2::ZERO),
                stats,
                Weapon {
                    cooldown: 0.0,
                    fire_rate: profile.fire_rate,
                    muzzle_speed: profile.muzzle_speed,
                },
                Energy {
                    current: 100.0,
                    max: 100.0,
                    regen: 0.0,
                },
                Heat {
                    current: 0.0,
                    max: 45.0,
                    dissipation: 0.0,
                },
            ))
            .id();

        let mut sched = Schedule::default();
        sched.add_systems(weapon_fire_system);

        // (1) Plenty of energy, cold → fires: energy drops by shot_cost, heat rises by profile.heat.
        sched.run(&mut w);
        assert!(
            (w.get::<Energy>(ship).unwrap().current - (100.0 - shot_cost)).abs() < 1e-3,
            "a shot drains shot_cost from energy"
        );
        assert!(
            (w.get::<Heat>(ship).unwrap().current - profile.heat).abs() < 1e-3,
            "a shot adds profile.heat"
        );

        // (2) Energy below one shot → no fire (no further drain).
        w.get_mut::<Energy>(ship).unwrap().current = shot_cost * 0.5;
        w.get_mut::<Weapon>(ship).unwrap().cooldown = 0.0;
        sched.run(&mut w);
        assert!(
            (w.get::<Energy>(ship).unwrap().current - shot_cost * 0.5).abs() < 1e-6,
            "blocked when below shot cost → energy unchanged"
        );

        // (3) Refill energy but overheated (heat == max) → still no fire.
        w.get_mut::<Energy>(ship).unwrap().current = 100.0;
        w.get_mut::<Heat>(ship).unwrap().current = 45.0;
        w.get_mut::<Weapon>(ship).unwrap().cooldown = 0.0;
        sched.run(&mut w);
        assert_eq!(
            w.get::<Energy>(ship).unwrap().current,
            100.0,
            "blocked while overheated → energy unchanged"
        );
    }
}
