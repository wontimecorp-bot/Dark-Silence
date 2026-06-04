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
    /// Generic extensibility seam; no flight/weapon contribution this epic.
    Utility,
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
    /// Thruster: sums into total thrust/torque (against total mass).
    Thruster {
        /// Forward thrust force contribution (`> 0`).
        thrust_force: f32,
        /// Angular drive torque contribution (`> 0`).
        turn_torque: f32,
        /// Lateral (strafe) thrust contribution (`>= 0`).
        strafe_force: f32,
    },
    /// Weapon: populates the `Weapon` component fire params (FR-016).
    Weapon {
        /// Projectile launch speed (`> 0`).
        muzzle_speed: f32,
        /// Shots per second (`> 0`).
        fire_rate: f32,
        /// Damage per shot (`> 0`).
        damage: f32,
        /// Phase M5 — the fired projectile's inertial **mass** (`> 0`), the per-weapon slug mass
        /// that sets both the shot's knockback on a target and the shooter's recoil
        /// (`momentum = projectile_mass · muzzle_velocity`). Small relative to ship mass; a heavier
        /// gun (e.g. a railgun) hits + recoils harder. Distinct from the weapon module's own
        /// install `mass` (the cost axis). Tunable for feel.
        projectile_mass: f32,
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
    /// Utility: generic seam; no flight/weapon contribution this epic.
    Utility,
}

/// The uniform data-driven stat block — the atom of fitting (FR-001).
///
/// One record models every device. Universal costs (`mass`, `power_draw`,
/// `cpu_draw`) apply on **every** kind; `power_gen` supplies the budget (reactors
/// only, in practice); `health_max` seeds the per-cell hit-map health (the live
/// health lives in the hit-map instance state, not here). `specifics` carries
/// the per-kind effective-stat parameters and must correspond to `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Module {
    /// Stable catalog key referenced by a `Fit`.
    pub id: ModuleId,
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
