//! The Module — the uniform, data-driven atom of fitting (FR-001, ADR-0008).
//!
//! Every installable device — reactor, thruster, weapon, shield, armor, utility
//! — is **one** uniform [`Module`] record (a content row, FR-025), distinguished
//! by its [`ModuleKind`] and a per-kind [`ModuleSpecifics`] payload that drives
//! effective-stat derivation (a later phase). The catalog rows are immutable
//! content keyed by a stable [`ModuleId`]; a [`crate::fitting::Fit`] references
//! them by id.
//!
//! Derive discipline matches the E001/E002 `sim` components (`components.rs`):
//! `Serialize`/`Deserialize` is present as a **seam** for replication (E003) and
//! persistence (E004) — these types are not serialized or stored this epic
//! (data-model.md serde note). Value-semantics derives give round-trip equality.

use serde::{Deserialize, Serialize};

use crate::damage::Channel;

/// Which class of device a [`Module`] is — selects the effective-stat it
/// contributes and which [`ModuleSpecifics`] payload it carries (data-model.md).
///
/// Parallels [`HardpointType`]: a slot of a given type accepts modules whose
/// [`Module::hardpoint_type`] equals it (INV-F01, FR-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModuleKind {
    /// Supplies power to the budget (`power_gen > 0`).
    Reactor,
    /// Contributes thrust / torque → top speed & agility.
    Thruster,
    /// Populates the `Weapon` fire params (FR-016).
    Weapon,
    /// Defensive shield HP / regen (carried for E007).
    Shield,
    /// Defensive armor value (carried for E007); its mass is the agility cost.
    Armor,
    /// Detection device — range/resolution sensing (Phase C; gameplay is a later
    /// feature, this carries the data shape).
    Sensor,
    /// Generic extensibility seam; no flight/weapon contribution this epic.
    Utility,
}

/// Weapon delivery family (Phase C) — the axis the fire system **branches on**
/// (projectile vs guided vs dropped vs hitscan beam). Distinct from the damage
/// [`Channel`] (which the armor/resistance system indexes). Only [`Ballistic`](WeaponClass::Ballistic)
/// is simulated today; the rest are data-staged for future delivery behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WeaponClass {
    /// Unguided kinetic/shell projectile on a ballistic path (the only delivery today).
    Ballistic,
    /// Guided self-propelled ordnance (rocket / missile / torpedo). Future tracking.
    Missile,
    /// Dropped/lobbed ordnance (guided or unguided). Future delivery.
    Bomb,
    /// Hitscan / continuous energy beam (particle / plasma / laser). Future delivery.
    DirectedEnergy,
}

/// Weapon ammunition / sub-category (Phase C) — a grouping tag (reload/UI/balance
/// bands), one level under [`WeaponClass`]. Categorical, not behavioral.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AmmoType {
    Kinetic,
    Shell,
    Rocket,
    Missile,
    Torpedo,
    UnguidedBomb,
    GuidedBomb,
    Particle,
    Photon,
    Plasma,
}

/// Propulsion role tag (Phase C) — categorizes a [`ModuleKind::Thruster`] as a main
/// drive, a maneuvering thruster, or reaction-control. Purely a grouping tag: the
/// stat derivation already SUMS `thrust_force`/`turn_torque`/`strafe_force` across all
/// thruster modules, so an engine + an RCS unit combine automatically; the numbers
/// (not the tag) differentiate them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropulsionType {
    /// Forward thrust (top speed / acceleration).
    MainDrive,
    /// Angular drive (turn torque).
    Maneuver,
    /// Reaction-control / strafe + attitude.
    Rcs,
}

/// Sensor family (Phase C) — the kind of detection a [`ModuleKind::Sensor`] provides.
/// Detection gameplay (AOI / signatures) is a later feature; this is the data shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorType {
    Radar,
    Lidar,
    Thermal,
    Em,
    Gravimetric,
}

/// The type gate on a slot/hardpoint (FR-006). A [`Module`] installs into a slot
/// only when its [`Module::hardpoint_type`] equals the slot's `slot_type`
/// (INV-F01). Parallels [`ModuleKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HardpointType {
    Reactor,
    Thruster,
    Weapon,
    Shield,
    Armor,
    Sensor,
    Utility,
}

/// The size gate on a slot/hardpoint (FR-007). **Ordered** `Small < Medium <
/// Large < XLarge`: a module fits a slot iff `module.hardpoint_size <=
/// slot.size` (a smaller module fits a larger slot, INV-F02).
///
/// The `#[repr(u8)]` + derived `Ord` make the size-fit check a plain comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlotSize {
    Small = 0,
    Medium = 1,
    Large = 2,
    XLarge = 3,
}

/// Which budget axis a [`Violation`] is on (data-model.md enum set).
///
/// The three independent ceilings a fit must respect (INV-F03): exceeding any
/// one invalidates the fit. Validation/derivation (later phases) consume this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    Power,
    Cpu,
    Mass,
}

/// A named reason a fit is invalid (FR-011, INV-F09; data-model.md enum set).
///
/// `validate_fit` (Phase 3) produces these; `valid == violations.is_empty()`.
/// Each variant names the offending rule so the fitting UI can surface it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Violation {
    /// A budget axis exceeded its capacity (INV-F03).
    OverBudget(Axis),
    /// A module's [`HardpointType`] does not match the slot's `slot_type`
    /// (INV-F01). Carries the offending slot + module ids.
    SlotTypeMismatch {
        slot: super::SlotId,
        module: ModuleId,
    },
    /// A module's [`SlotSize`] exceeds the slot's `size` (INV-F02). Carries the
    /// offending slot + module ids.
    SlotSizeMismatch {
        slot: super::SlotId,
        module: ModuleId,
    },
}

/// Stable, data-authored content id for a [`Module`] catalog row.
///
/// Unlike a runtime `bevy_ecs::entity::Entity`, this id is wire- and save-safe:
/// a [`crate::fitting::Fit`] references modules by `ModuleId`, never by entity
/// (data-model.md). Wraps a `u32` content key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModuleId(pub u32);

/// Per-kind parameters used to derive `ShipStats` (data-model.md effective-stat
/// table). The active variant must correspond to the module's [`ModuleKind`];
/// derivation (Phase 4) selects the contribution by matching on this.
///
/// `mass`, `power_draw`, `cpu_draw` are **universal** budget costs on every kind
/// and live on [`Module`] itself, not here — `ModuleSpecifics` carries only the
/// kind-specific effective-stat parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModuleSpecifics {
    /// Reactor: contributes only `power_gen` (on [`Module`]); no extra params.
    Reactor,
    /// Thruster — R92: ONE jet force along the mount SLOT's authored `facing` (body frame). The
    /// derive-time "flight computer" projects every jet onto the six control channels
    /// (forward / reverse / strafe-port / strafe-starboard / turn-CCW / turn-CW), with torque =
    /// `r × F` about the mass CoM — so PLACEMENT + FACING are the whole behavior (a main drive is a
    /// big aft-facing jet; an RCS block is a small one at an extremity). The old authored
    /// `turn_torque`/`strafe_force` fields are retired (serde ignores them in old RONs).
    Thruster {
        /// Propulsion role tag (main drive / maneuver / RCS) — categorization/UI only (a future
        /// exposure rule may gate on it; the physics comes from force + facing + placement).
        propulsion: PropulsionType,
        /// The jet's thrust force along its slot facing (`> 0`).
        thrust_force: f32,
    },
    /// Weapon: populates the `Weapon` component fire params (FR-016). Refinement 42 — authored from
    /// REAL ballistic specs (`caliber_mm` / `muzzle_velocity_ms` / `rpm`); the game DERIVES the
    /// game-space `muzzle_speed`/`fire_rate`/`damage`/`projectile_mass` + projectile radius from those
    /// via the global `SimTuning` weapon-physics scales (see `derive_weapon` in `stats.rs`). The four
    /// cooked outputs are optional per-weapon OVERRIDES: `Some(x)` ⇒ honor it (bypass physics — for
    /// energy/missile weapons that don't fit the caliber model); `None` ⇒ derive.
    Weapon {
        /// Delivery family (Phase C) — the axis the fire system branches on.
        class: WeaponClass,
        /// Ammunition / sub-category grouping tag (Phase C).
        ammo: AmmoType,
        /// Primary damage type (Phase C) — the armor/resistance [`Channel`] this weapon deals.
        /// Replaces the old hardcoded `Channel::Kinetic`.
        damage_type: Channel,
        /// Optional secondary damage type (Phase C) — e.g. a shell is `Kinetic` + `Blast`.
        secondary_damage_type: Option<Channel>,
        /// R42 — bore diameter in real millimetres. Drives projectile RADIUS (visual + collision) and
        /// the caliber³ slug-MASS model; with `muzzle_velocity_ms` it sets kinetic energy → damage.
        #[serde(default)]
        caliber_mm: f32,
        /// R42 — real muzzle velocity (m/s). Scaled to game `muzzle_speed` by `velocity_scale`.
        #[serde(default)]
        muzzle_velocity_ms: f32,
        /// R42 — rounds per minute. Scaled to game `fire_rate` (shots/s) by `rpm_scale`.
        #[serde(default)]
        rpm: f32,
        /// R42 — rotary spool-up time (s) to reach full RPM while firing; `0` = instant (non-rotary).
        #[serde(default)]
        spin_up_time: f32,
        /// R42 — shot dispersion half-angle (degrees); `0` = pinpoint. Applied as DETERMINISTIC
        /// per-shot angular noise (splitmix64 of owner + shot counter — no RNG).
        #[serde(default)]
        dispersion_deg: f32,
        /// R42 — max projectile travel in game units → `lifetime = range_units / muzzle_speed`.
        #[serde(default = "default_range_units")]
        range_units: f32,
        /// R42 — optional OVERRIDE of the derived game launch speed (`None` ⇒ derive from velocity).
        #[serde(default)]
        muzzle_speed: Option<f32>,
        /// R42 — optional OVERRIDE of the derived fire rate, shots/s (`None` ⇒ derive from rpm).
        #[serde(default)]
        fire_rate: Option<f32>,
        /// R42 — optional OVERRIDE of the derived per-shot damage (`None` ⇒ derive from KE).
        #[serde(default)]
        damage: Option<f32>,
        /// Phase M5 / R42 — the fired projectile's inertial **slug mass** that sets the shot's
        /// knockback on a target and the shooter's recoil (`momentum = mass · muzzle`). Optional
        /// OVERRIDE: `None` ⇒ derive from caliber³ density. Distinct from the module's install `mass`.
        #[serde(default)]
        projectile_mass: Option<f32>,
    },
    /// Shield: defense data consumed by E007 (not flight).
    Shield {
        /// Shield hit points (`>= 0`).
        shield_hp: f32,
        /// Shield regen per second (`>= 0`).
        regen: f32,
    },
    /// Armor: defense data consumed by E007; its `mass` is the agility cost.
    Armor {
        /// Armor value (`>= 0`).
        armor_value: f32,
    },
    /// Sensor: detection device (Phase C) — gameplay (AOI/signatures) is a later feature.
    Sensor {
        /// Sensor family.
        sensor_type: SensorType,
        /// Detection range (sim units, `> 0`).
        range: f32,
        /// Angular/positional resolution (`> 0`; higher = finer).
        resolution: f32,
    },
    /// R92 — energy storage (capacitor / battery — the catalog differentiates size/mass): adds flat
    /// capacity to the ship's energy pool. With a dead reactor the stored charge persists (regen 0)
    /// and drains as used — you fight on the stores until they're empty.
    EnergyStore {
        /// Added energy-pool capacity (`>= 0`), health-scaled at derive time.
        capacity: f32,
    },
    /// R92 — cargo hold volume. v1 derives the ship's `cargo_capacity` stat (displayed in fitting);
    /// pickup/loot gameplay consumes it in a later round.
    CargoBay {
        /// Cargo volume contribution (`>= 0`), health-scaled at derive time.
        volume: f32,
    },
    /// Utility: generic seam; no flight/weapon contribution this epic.
    Utility,
}

/// R42 — serde default for a weapon's `range_units` when omitted (game units of projectile travel).
/// Keeps an under-authored weapon firing a sane distance instead of a zero-lifetime dud.
fn default_range_units() -> f32 {
    1200.0
}

/// The uniform data-driven stat block — the atom of fitting (FR-001).
///
/// One record models every device. Universal costs (`mass`, `power_draw`,
/// `cpu_draw`) apply on **every** kind; `power_gen` supplies the budget (reactors
/// only, in practice); `health_max` seeds the per-cell hit-map health (the live
/// health lives in the hit-map instance state, not here). `specifics` carries
/// the per-kind effective-stat parameters and must correspond to `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Module {
    /// Stable catalog key referenced by a `Fit`.
    pub id: ModuleId,
    /// Display name (Phase C; e.g. "Autocannon", "Plasma Cannon"). Non-empty. (Adding this
    /// `String` makes `Module` no longer `Copy` — clone it, like [`super::hull::Hull`].)
    pub name: String,
    /// Selects which effective-stat this contributes.
    pub kind: ModuleKind,
    /// Power **supplied** to the budget (reactors `> 0`; most modules `0`).
    pub power_gen: f32,
    /// Power **consumed** from the budget (`>= 0`).
    pub power_draw: f32,
    /// CPU/control consumed from the budget (`>= 0`).
    pub cpu_draw: f32,
    /// Contributes to total ship mass (`> 0`; ∑ module mass → ship mass).
    pub mass: f32,
    /// Heat generated (authored now; thermal sim is E007 — carried, not
    /// simulated this epic).
    pub heat: f32,
    /// Max hit points of the installed module (`> 0`; seeds per-cell `Health`).
    pub health_max: f32,
    /// Gates which slot types accept this module (must equal slot `slot_type`).
    pub hardpoint_type: HardpointType,
    /// Must be `<= slot.size` (a smaller module fits a larger slot).
    pub hardpoint_size: SlotSize,
    /// Per-kind effective-stat parameters; corresponds to `kind`.
    pub specifics: ModuleSpecifics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_size_is_ordered_small_to_xlarge() {
        assert!(SlotSize::Small < SlotSize::Medium);
        assert!(SlotSize::Medium < SlotSize::Large);
        assert!(SlotSize::Large < SlotSize::XLarge);
        // A module of size S fits an L slot; an L module does not fit an S slot.
        assert!(SlotSize::Small <= SlotSize::Large);
        assert!(SlotSize::Large > SlotSize::Small);
    }

    #[test]
    fn module_id_round_trips_through_serde() {
        let id = ModuleId(42);
        let json = serde_json::to_string(&id).unwrap();
        let back: ModuleId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
