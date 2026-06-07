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
    CollisionRadius, Damage, Energy, Faction, Heading, Heat, Lifetime, Position, PrevPosition,
    Projectile, ProjectileFaction, ProjectileMass, ProjectileOwner, RenderScale, Ship, Trigger,
    Velocity, Weapon, WeaponBank, WeaponGroups,
};
use crate::damage::{Channel, DamageEvent};
use crate::fitting::{ShipStats, ShipWeapons, WeaponProfile};
use crate::intent::ShipIntent;
use crate::physics::SweptHit;
use crate::tuning::{SimTuning, Tuning};
use bevy_ecs::prelude::*;
use glam::{Vec2, Vec3};
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
/// R42 — the client's projectile mesh radius (`Sphere::new(0.2)` in `client/scene.rs`). A fitted
/// weapon's caliber-derived world radius is emitted as a [`RenderScale`] of `radius / this` so the
/// shared sphere renders at the real size. **Keep in sync with the client mesh.** Unfitted / turret
/// shots carry no `RenderScale` and render at this base size (unchanged).
const PROJECTILE_MESH_RADIUS: f32 = 0.2;

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
            // R45 — multi-weapon firing: the full weapon list + per-weapon state + fire-group
            // assignment (recompute populates `ShipWeapons`/`WeaponBank`). All absent ⇒ the LEGACY
            // single-weapon fallback (the `Weapon` component), which the unit tests + the spawn tick
            // before the first re-derive rely on.
            Option<&ShipWeapons>,
            Option<&mut WeaponBank>,
            Option<&WeaponGroups>,
            Option<&mut Weapon>,
            // Phase E: the dynamic pools — present only on LIVE-spawned fitted ships. `Option` so the
            // headless sim/determinism worlds (no pools) keep the exact prior firing behavior.
            Option<&mut Energy>,
            Option<&mut Heat>,
            // Mining-skirmish: the shooter's team, stamped onto the shot for the friend/foe gate.
            // `Option` so an unfactioned ship (every determinism/test world) shoots exactly as before.
            Option<&Faction>,
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
            Option<&Faction>,
        ),
        (With<Ship>, Without<ShipStats>),
    >,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();

    // Fitted path: fire EVERY alive weapon, gated by fire groups (R45). `ShipWeapons`/`WeaponBank`
    // (recompute-maintained) carry the multi-weapon list + per-weapon state; when absent (unit tests,
    // the spawn tick before the first re-derive) it falls back to the legacy single-weapon path.
    for (
        owner,
        intent,
        pos,
        heading,
        mut ship_vel,
        stats,
        ship_weapons,
        weapon_bank,
        groups,
        weapon,
        mut energy,
        mut heat,
        faction,
    ) in &mut fitted
    {
        if let (Some(ship_weapons), Some(mut bank)) = (ship_weapons, weapon_bank) {
            // MULTI-weapon: fire each weapon whose fire group is ACTIVE and whose trigger is HELD,
            // each on its OWN cooldown/spool/shot-counter. The energy/heat pools are shared, so they
            // deplete as successive weapons fire this tick (a heavy salvo can self-gate on energy).
            for (slot, profile) in &ship_weapons.weapons {
                let map = groups.map(|g| g.for_slot(*slot)).unwrap_or_default();
                let state = bank.states.entry(*slot).or_default();
                if state.cooldown > 0.0 {
                    state.cooldown -= dt;
                }
                let trigger_held = match map.trigger {
                    Trigger::Primary => intent.fire_primary,
                    Trigger::Secondary => intent.fire_secondary,
                    Trigger::Off => false,
                };
                let active = map.group == intent.active_group && trigger_held;
                // R42 rotary spool, now per weapon: ramp while this weapon is being fired, decay idle.
                if profile.spin_up_time > 0.0 {
                    let step = dt / profile.spin_up_time;
                    state.spool = if active {
                        (state.spool + step).min(1.0)
                    } else {
                        (state.spool - step).max(0.0)
                    };
                } else {
                    state.spool = 1.0;
                }
                let shot_cost = profile.damage * sim.weapon_energy_per_damage;
                let energy_ok = energy.as_ref().is_none_or(|e| e.current >= shot_cost);
                let heat_ok = heat.as_ref().is_none_or(|h| h.current < h.max);
                if active && state.spool >= 1.0 && can_fire(state.cooldown) && energy_ok && heat_ok
                {
                    fire_one_weapon(
                        &mut commands,
                        owner,
                        pos.0,
                        heading.0,
                        &mut ship_vel.0,
                        stats.total_mass,
                        profile,
                        state.shot_counter,
                        faction.copied(),
                        &sim,
                    );
                    if let Some(e) = energy.as_mut() {
                        e.current = (e.current - shot_cost).max(0.0);
                    }
                    if let Some(h) = heat.as_mut() {
                        h.current += profile.heat;
                    }
                    state.shot_counter = state.shot_counter.wrapping_add(1);
                    state.cooldown = cooldown_after_fire(profile.fire_rate);
                }
            }
            continue;
        }

        // LEGACY single-weapon fallback (no `ShipWeapons`/`WeaponBank` yet): the original single
        // `Weapon`-component path, gated on PRIMARY fire (group-agnostic). Keeps the firing unit tests
        // + the spawn tick before the first re-derive working unchanged.
        let (Some(profile), Some(mut weapon)) = (stats.weapon, weapon) else {
            continue;
        };
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        if profile.spin_up_time > 0.0 {
            let step = dt / profile.spin_up_time;
            weapon.spool = if intent.fire_primary {
                (weapon.spool + step).min(1.0)
            } else {
                (weapon.spool - step).max(0.0)
            };
        } else {
            weapon.spool = 1.0;
        }
        let shot_cost = profile.damage * sim.weapon_energy_per_damage;
        let energy_ok = energy.as_ref().is_none_or(|e| e.current >= shot_cost);
        let heat_ok = heat.as_ref().is_none_or(|h| h.current < h.max);
        if stats.can_fire
            && intent.fire_primary
            && weapon.spool >= 1.0
            && can_fire(weapon.cooldown)
            && energy_ok
            && heat_ok
        {
            fire_one_weapon(
                &mut commands,
                owner,
                pos.0,
                heading.0,
                &mut ship_vel.0,
                stats.total_mass,
                &profile,
                weapon.shot_counter,
                faction.copied(),
                &sim,
            );
            if let Some(e) = energy.as_mut() {
                e.current = (e.current - shot_cost).max(0.0);
            }
            if let Some(h) = heat.as_mut() {
                h.current += profile.heat;
            }
            weapon.shot_counter = weapon.shot_counter.wrapping_add(1);
            weapon.cooldown = cooldown_after_fire(profile.fire_rate);
        }
    }

    // Unfitted path: the original Weapon-component behavior (E001/E002/E003) + M4 recoil.
    for (owner, intent, pos, heading, mut ship_vel, mut weapon, faction) in &mut unfitted {
        if weapon.cooldown > 0.0 {
            weapon.cooldown -= dt;
        }
        if intent.fire_primary && can_fire(weapon.cooldown) {
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
                // Mining-skirmish friend/foe: the shot carries the shooter's team (None = neutral).
                ProjectileFaction(faction.copied()),
            ));
            // Phase M4/M6 recoil — unfitted ships use the global Tuning mass + live slug mass.
            ship_vel.0 -= sim.projectile_mass * muzzle / tuning.mass.max(f32::MIN_POSITIVE);
            weapon.cooldown = cooldown_after_fire(weapon.fire_rate);
        }
    }
}

/// R45 — spawn ONE weapon's projectile (R42 dispersion + muzzle offset + caliber size) and apply the
/// shooter's recoil. Shared by the multi-weapon + legacy single-weapon firing paths; the caller owns
/// the per-weapon cooldown / spool / shot-counter and the energy/heat bookkeeping.
#[allow(clippy::too_many_arguments)]
fn fire_one_weapon(
    commands: &mut Commands,
    owner: Entity,
    pos: Vec2,
    heading: f32,
    ship_vel: &mut Vec2,
    total_mass: f32,
    profile: &WeaponProfile,
    shot_counter: u32,
    faction: Option<Faction>,
    sim: &SimTuning,
) {
    // R42 dispersion: scatter the round within the cone by a DETERMINISTIC per-shot angle (splitmix64
    // of owner + shot counter — no RNG); `0` ⇒ exactly on heading. The barrel POSITION stays on
    // heading; only the launch direction scatters.
    let aim = if profile.dispersion_rad > 0.0 {
        heading + crate::turret::aim_noise(owner.to_bits(), shot_counter) * profile.dispersion_rad
    } else {
        heading
    };
    // Phase M4: muzzle velocity + the shooter's own velocity (a true Newtonian gun).
    let muzzle = Vec2::from_angle(aim) * profile.muzzle_speed;
    let vel = muzzle + *ship_vel;
    // R18: spawn at the installed gun's world position (the body-frame muzzle offset, rotated by the
    // ship heading), not the ship centre.
    let spawn = pos + Vec2::from_angle(heading).rotate(profile.muzzle_offset);
    let mut shot = commands.spawn((
        Projectile,
        Position(spawn),
        PrevPosition(spawn),
        Velocity(vel),
        Damage(profile.damage),
        ProjectileMass(profile.projectile_mass),
        Lifetime(profile.lifetime),
        ProjectileOwner(owner),
        WeaponSource::from_damage_typed(
            profile.channel,
            profile.damage,
            sim.pen_per_damage,
            sim.pen_size,
        ),
        ProjectileFaction(faction),
    ));
    // R42: a caliber-derived radius gives the shot its on-screen size (RenderScale on the shared 0.2
    // sphere) AND its hit radius (CollisionRadius, Minkowski-summed in collision). `0` ⇒ legacy point.
    if profile.projectile_radius > 0.0 {
        shot.insert((
            CollisionRadius(profile.projectile_radius),
            RenderScale(Vec3::splat(
                profile.projectile_radius / PROJECTILE_MESH_RADIUS,
            )),
        ));
    }
    // Phase M4/M5 recoil: conserve momentum against the MUZZLE component only, using the slug mass.
    *ship_vel -= profile.projectile_mass * muzzle / total_mass.max(f32::MIN_POSITIVE);
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
                    fire_primary: true,
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
                    spool: 1.0,
                    shot_counter: 0,
                },
                Energy {
                    current: 100.0,
                    max: 100.0,
                    regen: 0.0,
                    rate: 0.0,
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

    // ---- R45 fire-group firing ---------------------------------------------------------------

    /// Count the live projectiles in the world (one per weapon that fired this tick).
    fn count_projectiles(w: &mut World) -> usize {
        w.query_filtered::<Entity, With<Projectile>>()
            .iter(w)
            .count()
    }

    /// A fighter with TWO autocannons (slots 3 & 4) on the MULTI-weapon path (`ShipWeapons` +
    /// `WeaponBank` present), plus generous Energy/Heat so the gates never block. `groups` assigns the
    /// fire groups/triggers; `intent` sets the active group + which triggers are held.
    fn spawn_two_autocannon_ship(
        w: &mut World,
        groups: WeaponGroups,
        intent: ShipIntent,
    ) -> Entity {
        use crate::components::{Energy, Heat, WeaponState};
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{
            build_layout, derive_ship_stats, derive_weapons, seed_catalogs, Fit, SlotId,
            HULL_FIGHTER,
        };

        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        fit.install_raw(SlotId(4), MODULE_AUTOCANNON);
        let layout = build_layout(hull, &fit, &modules);
        let stats = derive_ship_stats(hull, &fit, &modules, &layout);
        let sim = SimTuning::default();
        let weapons = derive_weapons(hull, &fit, &modules, &layout, &sim);
        assert_eq!(weapons.len(), 2, "two autocannons fitted in slots 3 & 4");
        let mut bank = WeaponBank::default();
        for (slot, _) in &weapons {
            bank.states.insert(*slot, WeaponState::default());
        }
        w.spawn((
            Ship,
            intent,
            Position(Vec2::ZERO),
            Heading(0.0),
            Velocity(Vec2::ZERO),
            stats,
            ShipWeapons { weapons },
            bank,
            groups,
            Energy {
                current: 1_000.0,
                max: 1_000.0,
                regen: 0.0,
                rate: 0.0,
            },
            Heat {
                current: 0.0,
                max: 1_000.0,
                dissipation: 0.0,
            },
        ))
        .id()
    }

    /// Build a minimal world (dt + tuning) ready for `weapon_fire_system`.
    fn fire_world() -> World {
        let mut w = World::new();
        w.insert_resource(FixedDt(0.05));
        w.insert_resource(Tuning::default());
        w.insert_resource(SimTuning::default());
        w
    }

    fn run_fire(w: &mut World) {
        let mut sched = Schedule::default();
        sched.add_systems(weapon_fire_system);
        sched.run(w);
    }

    #[test]
    fn unconfigured_ship_fires_both_weapons_on_primary() {
        // No `WeaponGroups` mapping ⇒ every weapon defaults to group 1 / Primary, so an
        // unconfigured ship fires ALL its weapons on Space (the Bug-#2 fix: both slots shoot).
        let mut w = fire_world();
        spawn_two_autocannon_ship(
            &mut w,
            WeaponGroups::default(),
            ShipIntent {
                fire_primary: true,
                active_group: 0,
                ..Default::default()
            },
        );
        run_fire(&mut w);
        assert_eq!(
            count_projectiles(&mut w),
            2,
            "both group-1 weapons fire on Space"
        );
    }

    #[test]
    fn a_weapon_fires_only_when_its_group_is_active() {
        use crate::components::FireMapping;
        use crate::fitting::SlotId;
        let mut groups = WeaponGroups::default();
        groups.mapping.insert(
            SlotId(3),
            FireMapping {
                group: 0, // group 1
                trigger: Trigger::Primary,
            },
        );
        groups.mapping.insert(
            SlotId(4),
            FireMapping {
                group: 1, // group 2
                trigger: Trigger::Primary,
            },
        );

        // Group 1 active → only the slot-3 weapon fires.
        let mut w = fire_world();
        spawn_two_autocannon_ship(
            &mut w,
            groups.clone(),
            ShipIntent {
                fire_primary: true,
                active_group: 0,
                ..Default::default()
            },
        );
        run_fire(&mut w);
        assert_eq!(
            count_projectiles(&mut w),
            1,
            "only the group-1 weapon fires when group 1 is active"
        );

        // Group 2 active → only the slot-4 weapon fires.
        let mut w2 = fire_world();
        spawn_two_autocannon_ship(
            &mut w2,
            groups,
            ShipIntent {
                fire_primary: true,
                active_group: 1,
                ..Default::default()
            },
        );
        run_fire(&mut w2);
        assert_eq!(
            count_projectiles(&mut w2),
            1,
            "only the group-2 weapon fires when group 2 is active"
        );
    }

    #[test]
    fn secondary_trigger_fires_only_on_secondary_fire() {
        use crate::components::FireMapping;
        use crate::fitting::SlotId;
        let mut groups = WeaponGroups::default();
        // Both in group 1; slot 3 on Primary, slot 4 on Secondary.
        groups.mapping.insert(
            SlotId(3),
            FireMapping {
                group: 0,
                trigger: Trigger::Primary,
            },
        );
        groups.mapping.insert(
            SlotId(4),
            FireMapping {
                group: 0,
                trigger: Trigger::Secondary,
            },
        );

        // Space only → only the Primary weapon fires.
        let mut w = fire_world();
        spawn_two_autocannon_ship(
            &mut w,
            groups.clone(),
            ShipIntent {
                fire_primary: true,
                fire_secondary: false,
                active_group: 0,
                ..Default::default()
            },
        );
        run_fire(&mut w);
        assert_eq!(
            count_projectiles(&mut w),
            1,
            "Space fires only the Primary weapon"
        );

        // Space + Ctrl → both fire.
        let mut w2 = fire_world();
        spawn_two_autocannon_ship(
            &mut w2,
            groups,
            ShipIntent {
                fire_primary: true,
                fire_secondary: true,
                active_group: 0,
                ..Default::default()
            },
        );
        run_fire(&mut w2);
        assert_eq!(
            count_projectiles(&mut w2),
            2,
            "Space+Ctrl fires both the Primary and Secondary weapons"
        );
    }
}
