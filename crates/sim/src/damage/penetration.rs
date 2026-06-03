//! The armor-angle penetration gate (FR-005/006/007/008).
//!
//! [`resolve_penetration`] is the pure armor-gate calc: given a plate's nominal
//! thickness + material, the impact angle, and the shot's penetration value + size,
//! it returns one [`PenetrationResult`] (Ricochet / NonPenetration / Penetration /
//! OverPenetration) carrying the effective armor and (for a pass-into/through) the
//! surviving-damage tier fraction routed to the module behind.
//!
//! The invariants it realizes (all data-driven from [`PenetrationConfig`], FR-022):
//! - **INV-D03 — finite effective armor**: `effective = clamp(thickness * material
//!   / cos(angle), 0, effective_armor_cap)` — never `inf`/`NaN` as `cos(angle) → 0`
//!   (a grazing hit); the divisor is floored and the result clamped to the cap.
//! - **INV-D04 — overmatch bypasses angle**: when `pen_size >= overmatch_ratio *
//!   thickness` the angle/ricochet test is skipped and the result is **at least**
//!   `Penetration` (a large hit on thin plate cannot ricochet).
//! - **INV-D05 — tier ordering**: `pen_tier_non < pen_tier_over < pen_tier_full <=
//!   1.0`; a clean Penetration applies the full tier, an OverPenetration a strictly
//!   lower tier, a NonPenetration little/none.
//!
//! Pure (no ECS, no world): a deterministic function of its inputs + the config,
//! matching the glam-only style of `crate::collision`/`crate::physics`.

use super::content::{ArmorMaterial, PenetrationConfig};

/// The smallest `cos(angle)` the effective-armor divisor is allowed to take, so a
/// near-grazing hit (`cos → 0`) does not blow up before the cap clamp catches it
/// (INV-D03 belt-and-braces: floor the divisor *and* clamp the result). Tiny but
/// strictly positive.
const MIN_COS: f32 = 1.0e-4;

/// The outcome of the armor gate (FR-005/006/007/008, data-model.md).
///
/// Every variant carries the computed `effective_armor` (the angle-adjusted,
/// finite, clamped plate value — INV-D03). The two pass tiers additionally carry
/// the `surviving` fraction of the post-armor magnitude routed to the module
/// behind (the caller multiplies the post-matrix magnitude by this). `Copy`.
///
/// Tier ordering (INV-D05): `NonPenetration` (`pen_tier_non`, little/none) <
/// `OverPenetration` (`pen_tier_over`, reduced pass-through) < `Penetration`
/// (`pen_tier_full`, clean pass-into). `Ricochet` deposits no module-behind damage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PenetrationResult {
    /// Steep glancing hit past `ricochet_angle` (and not overmatched) → bounces,
    /// little/no damage (FR-006).
    Ricochet {
        /// The angle-adjusted, finite plate value (INV-D03).
        effective_armor: f32,
    },
    /// Hit the plate but `penetration < effective_armor` → only `pen_tier_non`
    /// applied to the armor; module behind untouched (FR-008).
    NonPenetration {
        /// The angle-adjusted, finite plate value (INV-D03).
        effective_armor: f32,
        /// `pen_tier_non` fraction (little/none) — INV-D05.
        surviving: f32,
    },
    /// Clean pass-into → `pen_tier_full` of the surviving magnitude routes to the
    /// module behind (FR-008/009).
    Penetration {
        /// The angle-adjusted, finite plate value (INV-D03).
        effective_armor: f32,
        /// `pen_tier_full` fraction — the strongest tier (INV-D05).
        surviving: f32,
    },
    /// Pass-through (shot exits) → reduced `pen_tier_over` of the surviving
    /// magnitude routes to the module behind (FR-008).
    OverPenetration {
        /// The angle-adjusted, finite plate value (INV-D03).
        effective_armor: f32,
        /// `pen_tier_over` fraction — strictly lower than full (INV-D05).
        surviving: f32,
    },
}

impl PenetrationResult {
    /// The (finite, clamped) effective armor every outcome carries.
    pub fn effective_armor(self) -> f32 {
        match self {
            PenetrationResult::Ricochet { effective_armor }
            | PenetrationResult::NonPenetration {
                effective_armor, ..
            }
            | PenetrationResult::Penetration {
                effective_armor, ..
            }
            | PenetrationResult::OverPenetration {
                effective_armor, ..
            } => effective_armor,
        }
    }

    /// The surviving-damage tier fraction routed to the module behind: `pen_tier_*`
    /// for a Non/Over/Penetration, `0.0` for a Ricochet (nothing gets behind).
    pub fn surviving(self) -> f32 {
        match self {
            PenetrationResult::Ricochet { .. } => 0.0,
            PenetrationResult::NonPenetration { surviving, .. }
            | PenetrationResult::Penetration { surviving, .. }
            | PenetrationResult::OverPenetration { surviving, .. } => surviving,
        }
    }
}

/// The angle-adjusted, finite, clamped effective armor (INV-D03).
///
/// `effective = clamp(thickness * material_multiplier / max(cos(angle), MIN_COS),
/// 0, effective_armor_cap)`. Monotonically increases as the impact angle grows
/// (steeper → more plate to cross) and stays **finite** as `cos(angle) → 0`: the
/// divisor is floored at [`MIN_COS`] and the result clamped to
/// `effective_armor_cap`. Pure.
pub fn effective_armor(
    thickness: f32,
    angle: f32,
    material: ArmorMaterial,
    cfg: &PenetrationConfig,
) -> f32 {
    let nominal = thickness * material.multiplier();
    let cos = angle.cos().abs().max(MIN_COS);
    (nominal / cos).clamp(0.0, cfg.effective_armor_cap)
}

/// Resolve the armor gate → one [`PenetrationResult`] (FR-005/006/007/008).
///
/// Inputs: the plate `thickness` (nominal) + `material`, the impact `angle`
/// (radians from the surface normal), the shot's `pen` (penetration value), and its
/// `size` (penetrator size for the overmatch test). The balance constants all come
/// from `cfg` ([`PenetrationConfig`], FR-022) — no hardcoded numbers.
///
/// Order of decision:
/// 1. Compute `effective` (finite, clamped — INV-D03).
/// 2. **Overmatch** (`size >= overmatch_ratio * thickness`) bypasses the angle/
///    ricochet test and forces **at least** `Penetration` (INV-D04). Whether it is
///    a clean `Penetration` or an `OverPenetration` is then decided by `pen` vs
///    `effective` as below (it can never be a `Ricochet`/`NonPenetration`).
/// 3. Otherwise, a steep glancing hit (`angle > ricochet_angle`) → `Ricochet`.
/// 4. Otherwise the penetration tier from `pen` vs `effective`:
///    - `pen >= 2 * effective` → clean `Penetration` (full tier);
///    - `pen >= effective` → `OverPenetration` (the shot is so far over the plate
///      it punches through and exits, depositing the reduced over-tier);
///    - else → `NonPenetration` (stopped by the plate; little/none).
///
/// Tier fractions are read from `cfg` (INV-D05 ordering). Pure; never panics.
pub fn resolve_penetration(
    thickness: f32,
    angle: f32,
    pen: f32,
    size: f32,
    material: ArmorMaterial,
    cfg: &PenetrationConfig,
) -> PenetrationResult {
    let effective = effective_armor(thickness, angle, material, cfg);
    let overmatched = size >= cfg.overmatch_ratio * thickness;

    // Ricochet only when steep AND not overmatched (INV-D04: overmatch ignores
    // the angle test). An overmatching hit on thin plate can never bounce.
    if !overmatched && angle.abs() > cfg.ricochet_angle {
        return PenetrationResult::Ricochet {
            effective_armor: effective,
        };
    }

    // The penetration tier from `pen` vs the (angle-adjusted) effective armor.
    // A clean pen needs a comfortable margin over the plate; merely meeting it is
    // an over-pen (punches through and exits); below it is a non-pen.
    if pen >= 2.0 * effective {
        PenetrationResult::Penetration {
            effective_armor: effective,
            surviving: cfg.pen_tier_full,
        }
    } else if pen >= effective {
        PenetrationResult::OverPenetration {
            effective_armor: effective,
            surviving: cfg.pen_tier_over,
        }
    } else if overmatched {
        // INV-D04: an overmatch must be **at least** Penetration even when `pen`
        // is modest — the plate is overwhelmed by sheer size, so it cannot stop the
        // round; it passes through (over-pen tier, the floor of "at least
        // Penetration").
        PenetrationResult::OverPenetration {
            effective_armor: effective,
            surviving: cfg.pen_tier_over,
        }
    } else {
        PenetrationResult::NonPenetration {
            effective_armor: effective,
            surviving: cfg.pen_tier_non,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PenetrationConfig {
        PenetrationConfig::default()
    }

    #[test]
    fn effective_armor_is_finite_and_grows_with_angle() {
        let c = cfg();
        // A thin plate so the growth is observable below the cap before clamping.
        let head_on = effective_armor(3.0, 0.0, ArmorMaterial::Steel, &c);
        let oblique = effective_armor(3.0, 1.0, ArmorMaterial::Steel, &c);
        assert!(head_on.is_finite());
        assert!(oblique.is_finite());
        assert!(oblique > head_on);
        // Grazing: cos → 0, still finite + clamped to the cap.
        let grazing = effective_armor(3.0, std::f32::consts::FRAC_PI_2, ArmorMaterial::Steel, &c);
        assert!(grazing.is_finite());
        assert!(grazing <= c.effective_armor_cap);
    }

    #[test]
    fn overmatch_never_ricochets() {
        let c = cfg();
        // Steep angle past ricochet, but a huge penetrator vs thin plate.
        let r = resolve_penetration(
            2.0,
            1.5,  // > ricochet_angle
            1.0,  // tiny pen
            10.0, // size >> overmatch_ratio * thickness
            ArmorMaterial::Steel,
            &c,
        );
        assert!(!matches!(r, PenetrationResult::Ricochet { .. }));
        assert!(r.surviving() > 0.0);
    }
}
