//! ECS gameplay components — the shared simulation model's data layer.
//!
//! These are the `bevy_ecs` [`Component`]s that gameplay systems attach to
//! entities. `bevy_ecs` is pulled in with `default-features = false` (HINT-004):
//! we want the pure entity/component/system data model, not Bevy's render,
//! window, app, or scheduler-heavy stack — `sim` stays headless (TR-002).
//!
//! Every component derives:
//! - [`Component`] — so it can live on an ECS entity;
//! - `Serialize`/`Deserialize` — so it replicates (E003) and persists (E004)
//!   without rework (TR-008, AD-002);
//! - `Copy`/`Clone`/`Debug`/`PartialEq` — value semantics and round-trip
//!   equality (the serde round-trip test asserts `deserialize(serialize(x)) == x`).
//!
//! The wrapped math type is `glam::Vec2`: gameplay is planar (the client renders
//! 3D, the sim is 2D), matching `motion::BodyState`.

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::Resource;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

/// World-space position of an entity on the 2D gameplay plane, in sim units.
///
/// At Tier 0 these are sector-relative (never large absolute world coordinates,
/// which would lose `f32` precision) — see [`crate::motion::BodyState::pos`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position(pub Vec2);

/// Linear velocity of an entity, in sim units per second.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Velocity(pub Vec2);

impl Position {
    /// Position at the origin.
    pub const ZERO: Self = Self(Vec2::ZERO);

    /// Construct from a 2D vector.
    pub const fn new(value: Vec2) -> Self {
        Self(value)
    }
}

impl Velocity {
    /// Zero velocity (at rest).
    pub const ZERO: Self = Self(Vec2::ZERO);

    /// Construct from a 2D vector.
    pub const fn new(value: Vec2) -> Self {
        Self(value)
    }
}

// --- E002 gameplay components -------------------------------------------------
//
// Same derive discipline as `Position`/`Velocity` above: `Component` so they
// live on entities, serde so they replicate/persist later (E003/E004), and
// value semantics. `ProjectileOwner` is the one exception — it wraps an
// `Entity`, whose id is runtime-local and not meaningful across the wire, so it
// is deliberately not `Serialize`/`Deserialize`.

/// Marker: the player-controlled ship.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ship;

/// Facing angle in radians — the direction the nose (and the fixed weapon) points.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heading(pub f32);

/// Turn rate in radians/s — the ship's angular velocity, carried with inertia
/// (the flight-model spins it up/down rather than snapping).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AngularVelocity(pub f32);

/// Remaining hit points; an entity is destroyed at or below zero.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Health(pub f32);

/// **Render-only** per-entity mesh scale (x,y,z), in sim units — a render hint, NOT read by any sim
/// system (so it's determinism-neutral). The windowed client emits it via `RenderEntity.scale` and
/// scales a UNIT mesh by it, so a structure's on-screen size comes from data (the mining scenario's
/// `assets/content/scenario.ron`) rather than a hardcoded mesh. Entities without it render at `ONE`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderScale(pub Vec3);

/// Phase E — the ship's **energy capacitor** (a dynamic, drainable power pool). Firing a weapon
/// drains `current`; it recharges from the reactor at `regen`/s toward `max` while you hold fire.
/// A weapon cannot fire when `current` is below its shot cost. `max`/`regen` are re-derived each
/// tick from the live `ShipStats.power_supply` (so reactor damage shrinks the pool). Attached only
/// to LIVE-spawned fitted ships — the headless sim/determinism tests never carry it (the weapon
/// gate is `Option`-skipped without it), keeping them byte-identical.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Energy {
    /// Live charge (`0..=max`).
    pub current: f32,
    /// Capacity (`power_supply · energy_capacity_secs`).
    pub max: f32,
    /// Gross recharge rate per second (`= power_supply`).
    pub regen: f32,
    /// Phase F — the **net** steady rate per second (`regen − continuous_draw − thrust_drain`):
    /// `> 0` charging, `< 0` draining. Drives the HUD's rate readout. (The per-shot weapon drain is
    /// an impulse, not part of this steady rate.)
    pub rate: f32,
}

/// Phase E — the ship's **heat** pool (the opposite of [`Energy`]). Firing adds heat; it
/// dissipates at `dissipation`/s. A weapon cannot fire while `current >= max` (overheated) until it
/// cools. The combat loop: fire to spend Energy + build Heat, then ease off to recover both.
/// Attached only to LIVE-spawned fitted ships (same `Option`-gate discipline as [`Energy`]).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heat {
    /// Live heat (`0..=max`); at `max` the ship is overheated.
    pub current: f32,
    /// Overheat threshold.
    pub max: f32,
    /// Cooling rate per second.
    pub dissipation: f32,
}

impl Energy {
    /// Spawn seed for a live ship (Phase E): **full charge**, sized to the default capacitor.
    /// `energy_system` re-derives `max`/`regen` from the live `ShipStats`/`SimTuning` each tick, so
    /// this only needs to be a sensible tick-0 value.
    pub fn seed(power_supply: f32) -> Self {
        let t = crate::tuning::SimTuning::default();
        let max = (power_supply * t.energy_capacity_secs).max(0.0);
        Self {
            current: max,
            max,
            regen: power_supply.max(0.0),
            rate: 0.0,
        }
    }
}

impl Heat {
    /// Spawn seed for a live ship (Phase E): **cold**, sized to the default heat pool.
    pub fn seed() -> Self {
        let t = crate::tuning::SimTuning::default();
        Self {
            current: 0.0,
            max: t.heat_capacity,
            dissipation: t.heat_dissipation,
        }
    }
}

/// Phase F — the **afterburner** boost pool. Holding the afterburner (`ShipIntent::afterburner`)
/// drains `current` and multiplies translational thrust in `ship_motion_system`; releasing
/// recharges it; the boost is gated on `current > 0`. A self-contained resource — it does NOT
/// touch [`Energy`]. Attached only to LIVE-spawned ships (same `Option`-gate discipline).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Afterburner {
    /// Live charge (`0..=max`).
    pub current: f32,
    /// Pool capacity.
    pub max: f32,
    /// Recharge per second while NOT boosting.
    pub regen: f32,
    /// Drain per second while boosting.
    pub drain: f32,
}

impl Afterburner {
    /// Spawn seed for a live ship (Phase F): **full**, sized to the default pool.
    pub fn seed() -> Self {
        let t = crate::tuning::SimTuning::default();
        Self {
            current: t.afterburner_capacity,
            max: t.afterburner_capacity,
            regen: t.afterburner_regen_rate,
            drain: t.afterburner_drain_rate,
        }
    }
}

/// Phase F — a depleting **armor-HP layer** between the shield and the hull. A penetrating hit
/// (not a ricochet) that gets past the shield depletes `current` and the hull is **protected from
/// carving while armor holds** (`current > 0`); once `current <= 0` (or the component is absent),
/// hits carve the hull as before. Armor does NOT regenerate (it depletes until a repair). `max` is
/// seeded from `ShipStats.armor_value` (Σ fitted armor plate). Attached only to LIVE-spawned fitted
/// ships — the headless sim/determinism tests never carry it, so `apply_damage` carves exactly as
/// today there (the gate is `Option`-skipped), keeping them byte-identical.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArmorHp {
    /// Live armor HP (`0..=max`); while `> 0` the hull is shielded from carving.
    pub current: f32,
    /// Armor capacity (`= ShipStats.armor_value`).
    pub max: f32,
}

impl ArmorHp {
    /// Spawn seed for a live ship (Phase F): **full** armor at `max = armor_value`.
    pub fn seed(armor_value: f32) -> Self {
        let max = armor_value.max(0.0);
        Self { current: max, max }
    }
}

/// Refinement 10 — a structure's inertial mass for ship↔structure RAM collisions (the outpost /
/// transport, which have no fit-derived mass before voxelization). A heavier `RamMass` means a ram
/// barely nudges it; paired with [`Movable`] it can drift. Windowed-scenario only.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RamMass(pub f32);

/// Refinement 11 — a structure's **oriented-box** collision half-extents (world units), so a square
/// block collides as a tight box instead of an under-covering inscribed circle (you bump at the real
/// edge, not ~6 u deep into a corner). `= grid · CELL_WORLD_SIZE · 0.5`. Used by the ship↔structure
/// ram (`structure_ram_system`) with the structure's `Heading` for orientation. The round rock has
/// NO `BoxCollider` → it keeps exact circle collision. Windowed-scenario only.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoxCollider(pub Vec2);

/// Refinement 10 — marks a structure the player can **shove** (a movable station): the ram imparts
/// velocity (finite mass) and [`structure_motion_system`](crate::collision::structure_motion_system)
/// integrates it (with drag). Absent → the structure is an immovable wall in the ram (the ship
/// bounces, the structure does not move). Windowed-scenario only.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movable;

/// Refinement 10 — the ship's **authored** (full-fit) cell count: the integrity BASELINE for the
/// HUD hull bar. Set once at fitted-ship spawn = the freshly-built `FitLayout.cells.len()`; it does
/// NOT shrink as cells are carved, so `live_cells / AuthoredCells` is the remaining hull-integrity
/// fraction the hull bar reads (a ship carved to 1–2 cells reads near-empty). Render-only — no sim
/// system reads it, it is off the wire, and it is attached only on the windowed player spawn, so
/// the headless/determinism worlds are byte-identical.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoredCells(pub u32);

/// Flight-assist mode: `On` damps drift toward heading; `Off` is decoupled,
/// full-momentum flight.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlightAssist {
    On,
    Off,
}

/// Marker: a fired projectile.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projectile;

/// Damage a projectile deals on hit (> 0, INV-04).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Damage(pub f32);

/// The inertial **mass of a fired projectile** (Phase M5) — the per-weapon slug mass it carries
/// to the hit, where [`crate::collision::fitted_damage_system`] deposits its momentum
/// (`projectile_mass · velocity`) as an impulse on the struck body. Set at fire from the weapon's
/// profile (a heavier gun → bigger knockback + recoil); the unfitted path uses a global fallback.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectileMass(pub f32);

/// Remaining lifetime in seconds; the projectile despawns at zero (INV-06).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lifetime(pub f32);

/// The entity's position on the previous fixed step — the tail of the swept
/// segment used for continuous collision so fast projectiles never tunnel.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrevPosition(pub Vec2);

/// The ship that fired a projectile (so a projectile cannot hit its owner).
/// Not serialized: `Entity` ids are runtime-local, not stable across the wire.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectileOwner(pub Entity);

/// The shooter's [`Faction`] stamped onto a projectile at fire (mining-skirmish friend/foe). Like
/// [`ProjectileOwner`], runtime-local (not serialized). `None` = an unfactioned shot → the
/// faction gate is a no-op (today's free-for-all), so every non-scenario / determinism / test world
/// is byte-unchanged.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectileFaction(pub Option<Faction>);

/// Mining-skirmish combat rules (a world resource). `friendly_fire` defaults OFF → a factioned
/// projectile only damages an ENEMY (faction-gated); a projectile with no [`ProjectileFaction`] is
/// unaffected by the gate (today's behavior). Read as `Option<Res<CombatRules>>` so a world that
/// never inserts it (the sim unit tests) degrades to the default.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatRules {
    /// `false` (default) = friendly fire OFF (a factioned shot only damages an enemy).
    pub friendly_fire: bool,
}

/// Marker: a destructible target.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target;

/// Which kind of target this is.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    /// Static practice dummy.
    Dummy,
    /// Drifts at constant velocity; also collides physically with the ship.
    Asteroid,
    /// Thrusts toward the player each step.
    Seeker,
    /// Mining-skirmish: a faction's stationary **refinery outpost** (beefy, mounts good turrets).
    Outpost,
    /// Mining-skirmish: a faction's **mining transport** (runs the load/unload loop; light turrets).
    Transport,
    /// Mining-skirmish: the central important **asteroid** both factions mine from.
    MineNode,
}

impl TargetKind {
    /// Stable wire tag for the target sub-kind, carried in
    /// `protocol::EntityRecord.flags` so a networked client can pick the right
    /// visual — the wire `EntityKind` only distinguishes Ship/Projectile/Target.
    /// Additive; not part of any gameplay invariant. **Append-only**: existing tags
    /// 0/1/2 are stable; scenario kinds extend with 3/4/5.
    pub fn as_u8(self) -> u8 {
        match self {
            TargetKind::Dummy => 0,
            TargetKind::Asteroid => 1,
            TargetKind::Seeker => 2,
            TargetKind::Outpost => 3,
            TargetKind::Transport => 4,
            TargetKind::MineNode => 5,
        }
    }

    /// Inverse of [`TargetKind::as_u8`]; `None` for an unknown tag.
    pub fn from_u8(v: u8) -> Option<TargetKind> {
        match v {
            0 => Some(TargetKind::Dummy),
            1 => Some(TargetKind::Asteroid),
            2 => Some(TargetKind::Seeker),
            3 => Some(TargetKind::Outpost),
            4 => Some(TargetKind::Transport),
            5 => Some(TargetKind::MineNode),
            _ => None,
        }
    }
}

/// Mining-skirmish **team allegiance** (the first faction/team concept). Friend/foe is `a != b`
/// (see [`hostile`]); an entity WITHOUT a `Faction` — every ship/target outside the skirmish,
/// including ALL determinism/botkit/test worlds — is neutral and behaves exactly as before, so the
/// faction-gated combat path is a strict no-op there. Attached only to scenario entities.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Faction {
    Red,
    Blue,
}

impl Faction {
    /// Render tint tag for `RenderEntity.faction` (client-only): `1` = Red, `2` = Blue (`0` = none).
    pub fn tint_tag(self) -> u8 {
        match self {
            Faction::Red => 1,
            Faction::Blue => 2,
        }
    }
}

/// Friend/foe test for the faction-gated combat path. `friendly_fire` → always hostile (damage
/// applies, today's free-for-all). Otherwise an entity with NO faction (`None`) is neutral and
/// hits/gets-hit by anyone (so every faction-less test/determinism world is byte-identical to
/// today); two factioned entities are hostile iff they differ.
pub fn hostile(a: Option<Faction>, b: Option<Faction>, friendly_fire: bool) -> bool {
    if friendly_fire {
        return true;
    }
    match (a, b) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

/// Circular proxy hitbox radius, > 0 (INV-05).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollisionRadius(pub f32);

/// Marker: this entity participates in the fitted **carve** pipeline — it can be shot
/// and eroded cell-by-cell.
///
/// An entity is carve-targetable iff it carries **`FitLayout` + `CollisionRadius` +
/// `Destructible`** (the three the [`fitted_damage_system`](crate::collision::fitted_damage_system)
/// query gates on). `Destructible` is the explicit **per-entity toggle**: removing it
/// from an entity makes that entity **inert** (a hit removes no cells), even though it
/// still keeps its `FitLayout`/`CollisionRadius` and still renders as its cells. It is
/// applied to live ships AND wreckage (severed chunks + destroyed-ship hulks) so all
/// three carve through the SAME code path; later gameplay can choose which pieces stay
/// destructible by adding/removing this marker.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Destructible;

/// The **frozen** cell-space reference point a wreck's cells render and carve around —
/// captured ONCE when the entity becomes wreckage and never recomputed.
///
/// It is the cell-space point whose world location is the entity's [`Position`]: a severed
/// chunk's COM at the instant of severing ([`sever_chunk`](crate::damage::sever_chunk)), or
/// a destroyed-ship hulk's grid centre ([`destroy_ship`](crate::damage::destroy_ship), whose
/// `Position` stays at the ship's grid centre). Both the client render
/// ([`hull_mesh_center`]) and the sim carve/armor-angle centre resolve to this anchor when
/// present (else the live cell-COM / grid-centre fallback).
///
/// **Why frozen:** without it, a wreck's reference was the LIVE cell-COM recomputed from the
/// current cells every update, so removing a cell shifted the COM and the whole piece visibly
/// jumped ("re-centres on its COM"). Freezing the anchor keeps every remaining cell exactly
/// where it is — only the carved cell disappears. Live ships need no anchor (their grid-centre
/// reference never drifts). Render-only / carve-only; not on any wire/snapshot path.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshAnchor(pub Vec2);

/// Default lifetime (seconds) a freshly-created wreck (severed chunk or destroyed hulk) drifts
/// before it is despawned (Phase M4). In frictionless space, inherited momentum never decays, so
/// debris would otherwise drift forever / litter the arena and waste sim+snapshot work. Tunable.
pub const WRECK_LIFETIME_SECS: f32 = 30.0;

/// Remaining drift time (seconds) for a `Wreck` body, set at creation to [`WRECK_LIFETIME_SECS`]
/// and decayed each fixed step by `dt` ([`crate::damage::destruction::wreck_lifetime_system`]);
/// when it reaches `0` the wreck despawns. This is the "despawn-when-old" bound that keeps drifting
/// debris from accumulating without imposing unphysical drag (space stays frictionless — the piece
/// coasts at full speed until its time is up). Complements the despawn-when-`FitLayout.cells`-empty
/// path (a fully carved wreck vanishes immediately regardless of remaining lifetime). Deterministic
/// (ticks by the fixed `dt`).
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WreckLifetime(pub f32);

/// A short-lived per-entity hit-flash timer (seconds), refreshed each time a hit
/// lands on this entity and decayed toward `0` each fixed step
/// ([`damage_flash_decay_system`](crate::collision::damage_flash_decay_system)).
///
/// Presentation-only (E007 live-demo visual feedback): retained as the hull-hit
/// timing seam. The client no longer scale-pulses the ship from it (the "zoom in and
/// out" the user disliked is gone); the brief deflector shimmer is driven by
/// [`ShieldHitFlash`] instead. Deterministic — it ticks down by the fixed `dt` like
/// every other timer, so server and client agree. Defaults to `0` for entities never
/// hit.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DamageFlash(pub f32);

/// A short-lived per-entity **shield-hit** flash timer (seconds), refreshed each time
/// a hit is absorbed by this entity's shield while it is up
/// ([`HitKind::ShieldAbsorbed`](crate::damage::HitKind)) and decayed toward `0` each
/// fixed step
/// ([`shield_hit_flash_decay_system`](crate::collision::shield_hit_flash_decay_system)).
///
/// Presentation-only (E007 live-demo visual feedback): the client renders a brief
/// translucent cyan **deflector shimmer** enveloping the ship for the split-second a
/// shot strikes the shield, fading as this timer bleeds out — a sci-fi shield flash
/// on impact, NOT a persistent bubble. There is no flash once the shield is depleted
/// (shots reach the hull). Deterministic — it ticks down by the fixed `dt` like every
/// other timer, so server and client agree. Defaults to `0` for entities whose shield
/// has not just taken a hit.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShieldHitFlash(pub f32);

/// The most recent shield impact's **direction** (unit, ship-centre → impact) and a
/// short-lived fade `timer` (seconds) — the seam the client uses to flash the
/// deflector at WHERE the bullet hit the shield instead of over the whole ship.
///
/// Refreshed each time a hit is absorbed by this entity's shield
/// ([`HitKind::ShieldAbsorbed`](crate::damage::HitKind)) in
/// [`fitted_damage_system`](crate::collision::fitted_damage_system), and decayed
/// toward `0` in lock-step with [`ShieldHitFlash`] by
/// [`shield_hit_flash_decay_system`](crate::collision::shield_hit_flash_decay_system).
///
/// **Transient runtime render feedback — deliberately NOT serialized**, mirroring
/// [`ProjectileOwner`] / [`crate::damage::DamageEvent`]: it is a per-frame visual cue
/// derived from the impact geometry, not replicated or persisted state (it would be
/// re-derived from the next hit anyway). The `dir` is a unit vector in **world space**
/// (the client rotates it into the ship's local frame before placing the flash); it is
/// `Vec2::ZERO` when there is no meaningful direction (the client then hides the flash).
/// Deterministic decay (ticks by the fixed `dt` like every other timer). Defaults to a
/// zero dir / zero timer for an entity whose shield has not just taken a hit.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LastShieldHit {
    /// Unit direction from the ship centre toward the impact point, in world space.
    /// `Vec2::ZERO` when no direction could be resolved (flash hidden client-side).
    pub dir: Vec2,
    /// Seconds remaining on the directional-flash fade; bled toward `0` each fixed
    /// step alongside [`ShieldHitFlash`].
    pub timer: f32,
}

/// The ship's fixed forward weapon: fire timing + muzzle speed.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weapon {
    /// Seconds until the weapon can fire again (INV-03).
    pub cooldown: f32,
    /// Shots per second.
    pub fire_rate: f32,
    /// Projectile launch speed.
    pub muzzle_speed: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::world::World;

    /// Friend/foe truth table (mining skirmish). A faction-less side (`None`) is neutral and always
    /// hostile (so every non-scenario / determinism world is unchanged); two factions are hostile
    /// iff they differ; `friendly_fire` forces hostile regardless.
    #[test]
    fn hostile_truth_table() {
        use Faction::{Blue, Red};
        // Friendly fire OFF (the default scenario rule):
        assert!(
            !hostile(Some(Red), Some(Red), false),
            "same faction = not hostile"
        );
        assert!(!hostile(Some(Blue), Some(Blue), false));
        assert!(
            hostile(Some(Red), Some(Blue), false),
            "different factions = hostile"
        );
        assert!(hostile(Some(Blue), Some(Red), false));
        // A faction-less side is neutral → hostile to anyone (today's free-for-all, the gate no-op).
        assert!(hostile(None, Some(Red), false));
        assert!(hostile(Some(Red), None, false));
        assert!(hostile(None, None, false));
        // Friendly fire ON → always hostile (damage applies to anyone).
        assert!(
            hostile(Some(Red), Some(Red), true),
            "friendly fire on = hostile even to allies"
        );
    }

    /// The components must actually be usable as ECS data: spawn an entity with
    /// both, then read them back. This is the headless-ECS smoke test that proves
    /// `default-features = false` still gives us a working component model.
    #[test]
    fn components_attach_to_an_entity_and_read_back() {
        let mut world = World::new();
        let pos = Position::new(Vec2::new(1.0, 2.0));
        let vel = Velocity::new(Vec2::new(-3.0, 4.0));
        let entity = world.spawn((pos, vel)).id();

        assert_eq!(*world.get::<Position>(entity).unwrap(), pos);
        assert_eq!(*world.get::<Velocity>(entity).unwrap(), vel);
    }
}
