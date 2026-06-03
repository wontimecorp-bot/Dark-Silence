//! The data-driven damage-tuning content (FR-004/005/006/007/008/010/012/022).
//!
//! Damage balance is **content, not code** (FR-022, `NEW-CONFIG`): the resistance
//! matrix, the penetration/armor constants, the shield tuning, and the
//! stat-scaling floor are loaded into `sim` resources at startup and tunable
//! without code changes. Values are grounded-but-gameplay-scaled (ADR-0012) — not
//! real units — mirroring [`crate::tuning::Tuning`].
//!
//! Nothing in any code path hardcodes a balance number: the matrix comes from
//! [`default_resistance_matrix`], the armor gate reads [`PenetrationConfig`], the
//! shield system reads [`ShieldConfig`], and emergent-damage scaling reads
//! [`StatScalingConfig`]. This is the single authoring surface for that content
//! (FR-022; test-guarded by `crates/sim/tests/damage.rs`).
//!
//! The seed matrix satisfies the non-degenerate property (FR-023, INV-D11): each
//! channel is strong against (low mitigation on) its preferred layer and each
//! layer strongly resists (high mitigation on) at least one channel, with **no**
//! globally dominant channel and **no** universally-bypassed layer.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use super::event::Channel;
use super::resist::{DefenseLayer, ResistanceMatrix};

// --- The resistance matrix seed ----------------------------------------------
//
// Columns are `Channel` order: [Kinetic, ThermalEnergy, Blast, Em, Radiation].
// Rows are `DefenseLayer` order: [Shields, Armor, HullStructure, Systems].
//
// Named mitigation tiers (all `∈ [0, MAX_MITIGATION < 1)`): `LOW` = the channel
// gets through (its strong-vs layer); `HIGH` = the layer strongly resists it;
// `MID` = the neutral middle. The bold diagonal of strong pairings:
//   Shields  ── LOW vs ThermalEnergy   (energy melts shields)
//   Armor    ── LOW vs Kinetic         (slugs defeat plate by penetration, not %)
//   Hull     ── LOW vs Blast           (concussion chews structure)
//   Systems  ── LOW vs Em & Radiation  (EW/rads ignore plating, fry the device)
// Every channel therefore has a layer it beats (a LOW column entry) and every
// layer a channel it resists (a HIGH row entry) — INV-D11.

/// Low mitigation — a channel's **strong-vs** layer; most of it gets through.
const LOW: f32 = 0.10;
/// Mid mitigation — the neutral middle of the table.
const MID: f32 = 0.40;
/// High mitigation — a layer **resists** this channel (still `< 1.0`, INV-D02).
const HIGH: f32 = 0.70;

/// The const seed of the (layer × channel) mitigation matrix (FR-004/023).
///
/// Every cell is `∈ [LOW, HIGH] ⊂ [0, MAX_MITIGATION < 1)` (INV-D02). The shape is
/// the data-model.md contract: a low cell on each layer's strong channel, a high
/// cell where the layer resists, mid elsewhere — crossing effective-HP curves so
/// no channel/layer dominates (INV-D11, test-guarded).
const RESISTANCE_SEED: [[f32; Channel::COUNT]; DefenseLayer::COUNT] = [
    //        Kinetic  Thermal  Blast    Em       Radiation
    /* Shields */
    [HIGH, LOW, MID, MID, MID], // resists Kinetic; weak to Thermal
    /* Armor   */ [LOW, HIGH, MID, MID, MID], // resists Thermal; weak to Kinetic
    /* Hull    */ [MID, MID, LOW, HIGH, MID], // resists Em; weak to Blast
    /* Systems */ [MID, MID, HIGH, LOW, LOW], // resists Blast; weak to Em/Radiation
];

/// The data-driven default resistance matrix (FR-004/022).
///
/// The single source of the (layer × channel) mitigation balance: a const seed,
/// not a hardcoded number in any code path. Every cell `∈ [0, 1)` (INV-D02);
/// non-degenerate (INV-D11) — test-guarded in `crates/sim/tests/damage.rs`. Tuning
/// retunes these values; logic is untouched.
pub fn default_resistance_matrix() -> ResistanceMatrix {
    ResistanceMatrix {
        table: RESISTANCE_SEED,
    }
}

/// Per-section plate material; a multiplier on nominal armor thickness (a content
/// seam, data-model.md). The angle math reads `thickness * material_multiplier`.
///
/// `Copy`; serde as the replication/persistence seam. Authored per hull section
/// (E006/content); E007 only reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArmorMaterial {
    /// Baseline plate; multiplier `1.0`.
    Steel,
    /// Lighter, tougher per unit thickness; multiplier `> 1.0`.
    Composite,
}

impl ArmorMaterial {
    /// The thickness multiplier this material applies to nominal plate (the
    /// effective-armor term `thickness * multiplier`). Grounded-but-scaled
    /// (ADR-0012); `> 0` and finite for every variant.
    pub fn multiplier(self) -> f32 {
        match self {
            ArmorMaterial::Steel => 1.0,
            ArmorMaterial::Composite => 1.4,
        }
    }
}

/// Grounded-but-scaled penetration / armor tuning (FR-005/006/007/008, ADR-0012).
///
/// The armor gate ([`resolve_penetration`](crate::damage::resolve_penetration))
/// reads this `Resource`; immutable at runtime (content reload only). All fields
/// satisfy the data-model constraints (see per-field docs); the penetration tiers
/// obey the strict ordering INV-D05.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PenetrationConfig {
    /// Impact angle (radians) past which a non-overmatching hit ricochets
    /// (FR-006). `∈ (0, π/2)`.
    pub ricochet_angle: f32,
    /// A hit overmatches (ignores angle, forces `Penetration`) when
    /// `pen_size >= overmatch_ratio * thickness` (FR-007). `> 0`.
    pub overmatch_ratio: f32,
    /// Upper clamp on `thickness * material / cos(angle)` so a near-grazing
    /// `cos → 0` stays **finite** (INV-D03). `> 0`, finite.
    pub effective_armor_cap: f32,
    /// Fraction of surviving damage a clean **Penetration** applies (INV-D05).
    /// `∈ (0, 1]`.
    pub pen_tier_full: f32,
    /// Fraction an **OverPenetration** applies — pass-through, reduced (INV-D05).
    /// `∈ (0, pen_tier_full)`.
    pub pen_tier_over: f32,
    /// Fraction a **NonPenetration** applies to the armor only — little/none
    /// (INV-D05). `∈ [0, pen_tier_over)`.
    pub pen_tier_non: f32,
}

impl Default for PenetrationConfig {
    /// The seed penetration tuning (ADR-0012). Tiers obey
    /// `pen_tier_non < pen_tier_over < pen_tier_full <= 1.0` (INV-D05).
    fn default() -> Self {
        Self {
            // ~67.5°: steeper than this (and not overmatched) bounces.
            ricochet_angle: std::f32::consts::FRAC_PI_2 * 0.75,
            // A penetrator ≥ 1.5× the plate thickness overmatches it.
            overmatch_ratio: 1.5,
            // A grazing hit's effective armor never exceeds ~8× the nominal plate.
            effective_armor_cap: 8.0,
            // Clean pen routes a third of the surviving magnitude behind the plate.
            pen_tier_full: 0.33,
            // Over-pen passes through, depositing only ~a tenth.
            pen_tier_over: 0.10,
            // Non-pen barely scuffs the plate (no module-behind damage).
            pen_tier_non: 0.0,
        }
    }
}

impl PenetrationConfig {
    /// Validate the data-model constraints + the tier ordering INV-D05. The
    /// content test asserts this; the armor gate relies on it.
    pub fn is_valid(&self) -> bool {
        let half_pi = std::f32::consts::FRAC_PI_2;
        self.ricochet_angle > 0.0
            && self.ricochet_angle < half_pi
            && self.overmatch_ratio > 0.0
            && self.effective_armor_cap > 0.0
            && self.effective_armor_cap.is_finite()
            // INV-D05: pen_tier_non < pen_tier_over < pen_tier_full <= 1.0
            && self.pen_tier_non >= 0.0
            && self.pen_tier_non < self.pen_tier_over
            && self.pen_tier_over < self.pen_tier_full
            && self.pen_tier_full <= 1.0
    }
}

/// Grounded-but-scaled shield tuning (FR-010, ADR-0012).
///
/// The shield system (US1) reads this `Resource`; immutable at runtime. A fitted
/// shield module's own `regen` overrides `shield_regen_default`.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShieldConfig {
    /// Default regen/sec applied while powered (`>= 0`). A fitted shield's `regen`
    /// overrides this.
    pub shield_regen_default: f32,
    /// Rate a shield depletes when `power_linked && !powered` — reactor lost →
    /// shields drop (`>= 0`, FR-013/INV-D14).
    pub unpowered_decay: f32,
}

impl Default for ShieldConfig {
    /// The seed shield tuning (ADR-0012).
    fn default() -> Self {
        Self {
            shield_regen_default: 5.0,
            unpowered_decay: 10.0,
        }
    }
}

impl ShieldConfig {
    /// Validate the data-model constraints (`>= 0`, finite). The content test
    /// asserts this.
    pub fn is_valid(&self) -> bool {
        self.shield_regen_default >= 0.0
            && self.shield_regen_default.is_finite()
            && self.unpowered_decay >= 0.0
            && self.unpowered_decay.is_finite()
    }
}

/// Emergent-damage stat-scaling tuning (FR-012, ADR-0012).
///
/// `derive_ship_stats` (extended, US2) reads this `Resource`; immutable at
/// runtime. The floor keeps a damaged-but-alive module contributing *some* of its
/// stat (no cliff), while a destroyed module (health `0`) is a hard off (INV-D13).
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StatScalingConfig {
    /// Minimum contribution fraction a *damaged-but-alive* module keeps;
    /// `health_frac` is clamped to `[stat_health_floor, 1]` before scaling so a
    /// barely-alive thruster still gives *some* thrust (not a cliff). `∈ [0, 1)`.
    pub stat_health_floor: f32,
}

impl Default for StatScalingConfig {
    /// The seed stat-scaling tuning (ADR-0012).
    fn default() -> Self {
        Self {
            stat_health_floor: 0.1,
        }
    }
}

impl StatScalingConfig {
    /// Validate the data-model constraint `stat_health_floor ∈ [0, 1)`. The
    /// content test asserts this.
    pub fn is_valid(&self) -> bool {
        (0.0..1.0).contains(&self.stat_health_floor)
    }
}

/// Grounded-but-scaled salvage tuning (FR-018/019/020, ADR-0012).
///
/// The salvage walk ([`salvage_layout`](crate::damage::salvage::salvage_layout) +
/// [`intact_threshold`](crate::damage::intact_threshold)) reads this `Resource`;
/// immutable at runtime (content reload only). No salvage balance number is
/// hardcoded in any code path (FR-022): the clean-sever-vs-through-kill boundary
/// (INV-D12), the per-mass scrap conversion, and the over-kill scrap floor (INV-D09)
/// all live here.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SalvageConfig {
    /// The clean-sever-vs-through-kill boundary (INV-D12): a module salvages
    /// **intact** iff `health >= intact_fraction * health_max`. At/above → intact;
    /// below (in the limit `0`, a through-kill) → scrap. `∈ (0, 1]` — so a
    /// through-kill can never beat a careful clean sever (FR-018).
    pub intact_fraction: f32,
    /// The minimum scrap a wreck ever yields per `Scrap` outcome, and the over-kill
    /// floor (INV-D09): even a structural-only / fully-through-killed wreck yields
    /// `>= scrap_floor` so loot is **never** zero. `> 0`.
    pub scrap_floor: f32,
    /// Scrap quantity per unit of a through-killed module's `mass` (the salvage
    /// conversion rate). A heavier module yields more scrap. `>= 0`.
    pub scrap_per_mass: f32,
}

impl Default for SalvageConfig {
    /// The seed salvage tuning (ADR-0012). A module at/above half health salvages
    /// intact; a through-killed module yields `mass`-worth of scrap, floored at 1.
    fn default() -> Self {
        Self {
            intact_fraction: 0.5,
            scrap_floor: 1.0,
            scrap_per_mass: 1.0,
        }
    }
}

impl SalvageConfig {
    /// Validate the data-model constraints: `intact_fraction ∈ (0, 1]`,
    /// `scrap_floor > 0`, `scrap_per_mass >= 0` (all finite). The content test
    /// asserts this; the salvage walk relies on it.
    pub fn is_valid(&self) -> bool {
        self.intact_fraction > 0.0
            && self.intact_fraction <= 1.0
            && self.scrap_floor > 0.0
            && self.scrap_floor.is_finite()
            && self.scrap_per_mass >= 0.0
            && self.scrap_per_mass.is_finite()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_matrix_cells_are_bounded() {
        let matrix = default_resistance_matrix();
        assert!(matrix.is_bounded());
    }

    #[test]
    fn each_layer_has_its_strong_low_channel() {
        let m = default_resistance_matrix();
        // The strong-vs pairing of each layer is a LOW cell.
        assert_eq!(
            m.mitigation(DefenseLayer::Shields, Channel::ThermalEnergy),
            LOW
        );
        assert_eq!(m.mitigation(DefenseLayer::Armor, Channel::Kinetic), LOW);
        assert_eq!(
            m.mitigation(DefenseLayer::HullStructure, Channel::Blast),
            LOW
        );
        assert_eq!(m.mitigation(DefenseLayer::Systems, Channel::Em), LOW);
        assert_eq!(m.mitigation(DefenseLayer::Systems, Channel::Radiation), LOW);
    }

    #[test]
    fn default_configs_are_valid() {
        assert!(PenetrationConfig::default().is_valid());
        assert!(ShieldConfig::default().is_valid());
        assert!(StatScalingConfig::default().is_valid());
        assert!(SalvageConfig::default().is_valid());
    }

    #[test]
    fn armor_material_multipliers_are_positive_finite() {
        for mat in [ArmorMaterial::Steel, ArmorMaterial::Composite] {
            let mul = mat.multiplier();
            assert!(mul > 0.0 && mul.is_finite());
        }
    }
}
