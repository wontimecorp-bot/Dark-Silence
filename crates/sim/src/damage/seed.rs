//! Defense-layer seeding — turn a fitted [`Hull`] + [`Fit`] into the three live
//! E007 defense components (E007 live-demo wiring).
//!
//! [`apply_damage`](super::apply_damage) traverses **Shields → Armor → Hull →
//! Systems** on the per-target components [`Shields`], [`SectionArmor`], and
//! [`HullStructure`]. E006's [`build_layout`](crate::fitting::build_layout) seeds the
//! per-module health (the `FitLayout`), but nothing seeded those three *layer*
//! components — so a freshly-fitted ship had no shield, no armor facets, and no
//! structural backstop, and the damage pipeline ran but mutated nothing meaningful.
//!
//! [`seed_defense_layers`] closes that gap. It is the **shared** helper both the
//! player ship and the demo enemy seed their defenses from (Principle II): one code
//! path derives the layer state from the fit, so the player and the enemy are
//! symmetric and damageable on the exact same rules.
//!
//! The values are MVP, **tunable content** (grounded-but-scaled, ADR-0012): they are
//! chosen so sustained autocannon fire visibly penetrates, degrades modules, and
//! eventually severs/destroys a target without one-shotting it (see the per-field
//! docs). A later content pass authors per-section armor + hull HP from the hull row
//! directly; this helper is the demo seam.

use std::collections::BTreeSet;

use glam::Vec2;

use super::content::ArmorMaterial;
use super::layers::{ArmorFacet, HullStructure, SectionArmor, Shields};
use crate::fitting::{Fit, Hull, ModuleCatalog, ModuleSpecifics, SectionId};

/// Fallback shield capacity when the fit carries **no** shield module (neither seed
/// hull has a Shield hardpoint, so the demo ships take this path). A small pool so a
/// target is shield-defended (the pipeline's Shields layer is exercised) but the
/// shield depletes under sustained fire rather than being an impenetrable wall.
const DEFAULT_SHIELD_HP: f32 = 12.0;
/// Fallback shield regen/sec for the no-shield-module path — slow enough that
/// continuous fire out-damages it (the shield does not fully tank a stream).
const DEFAULT_SHIELD_REGEN: f32 = 1.0;

/// Per-section plate thickness when the fit carries **no** armor module. Thin steel
/// so the autocannon (penetration ≈ 3× damage) clean-penetrates and routes damage to
/// the module behind — an unarmored section is soft, not damage-immune.
const DEFAULT_ARMOR_THICKNESS: f32 = 2.0;
/// Per-section plate thickness contributed by a fitted armor module. Thicker than the
/// unarmored default so an armored ship deflects/absorbs more (a steeper or smaller
/// shot may now ricochet/non-penetrate) — but still penetrable by a square-on
/// autocannon burst, so the chain stays visible.
const ARMOR_THICKNESS_PER_MODULE: f32 = 6.0;

/// Hull structural HP per unit of `hull_base_mass` — the structural backstop scales
/// with the chassis size (a heavier hull is sturdier). Tuned against the **measured**
/// effective hull DPS of a square-on autocannon burst (≈ 1.6 hull/s after the
/// shield/armor-matrix/pen-tier/hull-matrix mitigation, repro test) so the fighter
/// (`hull_base_mass 8` → `hull_hp 12`) dies in ~8 s of steady fire — substantial but
/// not a slog, not a one-shot. The corvette (`hull_base_mass ~16+`) is proportionally
/// sturdier, taking longer to chew through.
const HULL_HP_PER_BASE_MASS: f32 = 1.5;
/// Floor on derived hull HP so any chassis has a finite, non-degenerate structural
/// pool (defensive — `hull_base_mass > 0` always, so this is belt-and-suspenders).
/// Also the fighter's effective hull pool (`8 * 1.5 = 12`, at the floor) → the ~8 s
/// kill the live demo wants.
const HULL_HP_FLOOR: f32 = 12.0;

/// Outward facet normal for a section sitting exactly on the grid centre (a *core*
/// section with no outward direction — e.g. the fighter's central reactor). `-X`
/// presents the core's **face** to the demo's nominal forward (+x) attacker so a
/// head-on shot penetrates (impact angle ≈ 0) instead of striking its back face and
/// ricocheting (the live-demo death bug). Any unit vector keeps the angle math well-
/// defined; `-X` is the demo-correct choice.
const CORE_FALLBACK_NORMAL: Vec2 = Vec2::NEG_X;

/// Seed the three E007 defense-layer components for a fitted ship from its hull +
/// fit (E007 live-demo wiring, shared by the player ship and the demo enemy).
///
/// Returns `(shields, section_armor, hull_structure)` ready to insert alongside the
/// ship's [`Fit`]/`FitLayout`/`ShipStats`:
///
/// - **[`Shields`]** — if the fit installs a Shield module, a **full** pool from its
///   [`ModuleSpecifics::Shield`] `shield_hp`/`regen` (`power_linked = true`, so it
///   regenerates while the reactor lives and decays if the reactor is lost). Neither
///   seed hull has a Shield hardpoint, so in practice the fit has no shield module
///   and this falls back to a small default pool ([`DEFAULT_SHIELD_HP`]) — the target
///   is still shield-defended, exercising the Shields layer, but the pool is small
///   enough to deplete under sustained fire.
/// - **[`SectionArmor`]** — one [`ArmorFacet`] per **distinct** hull [`SectionId`].
///   Thickness comes from the fitted armor module (summed across armor modules,
///   [`ARMOR_THICKNESS_PER_MODULE`]) or the thin unarmored default
///   ([`DEFAULT_ARMOR_THICKNESS`]); material is [`ArmorMaterial::Steel`]. Each facet's
///   outward `normal` is derived from the **section's mean cell position relative to
///   the grid centre** (a section on the +x side of the hull faces +x), so the
///   armor-angle math has a meaningful per-section normal; a *core* section whose
///   cells sit exactly at the grid centre (no outward direction) defaults to
///   [`CORE_FALLBACK_NORMAL`] (`-X`), presenting its face to a head-on +x attacker.
/// - **[`HullStructure`]** — a full backstop sized from the hull's `hull_base_mass`
///   ([`HULL_HP_PER_BASE_MASS`], floored at [`HULL_HP_FLOOR`]) so a bigger chassis is
///   sturdier; always `> 0`.
///
/// **Pure / total** — reads only its arguments, mutates nothing — so the player and
/// the enemy derive their defenses on one shared code path (Principle II). A fit
/// referencing modules absent from `catalog` simply contributes nothing (no panic).
pub fn seed_defense_layers(
    hull: &Hull,
    fit: &Fit,
    catalog: &ModuleCatalog,
) -> (Shields, SectionArmor, HullStructure) {
    // --- Shields: fitted shield module, else a small default pool ----------------
    let fitted_shield = fit
        .assignments
        .values()
        .filter_map(|id| catalog.get(*id))
        .find_map(|m| match m.specifics {
            ModuleSpecifics::Shield { shield_hp, regen } => Some((shield_hp, regen)),
            _ => None,
        });
    let shields = match fitted_shield {
        Some((shield_hp, regen)) => Shields::full(shield_hp, regen, true),
        None => Shields::full(DEFAULT_SHIELD_HP, DEFAULT_SHIELD_REGEN, true),
    };

    // --- Armor: thickness from fitted armor modules (summed), else the default ---
    let fitted_armor_thickness: f32 = fit
        .assignments
        .values()
        .filter_map(|id| catalog.get(*id))
        .filter(|m| matches!(m.specifics, ModuleSpecifics::Armor { .. }))
        .count() as f32
        * ARMOR_THICKNESS_PER_MODULE;
    let thickness = if fitted_armor_thickness > 0.0 {
        fitted_armor_thickness
    } else {
        DEFAULT_ARMOR_THICKNESS
    };

    // One facet per distinct hull section, normal derived from the section's mean
    // cell position relative to the grid centre.
    let grid_centre = Vec2::new(hull.grid_dims.0 as f32 * 0.5, hull.grid_dims.1 as f32 * 0.5);
    let sections: BTreeSet<SectionId> = hull.cells.iter().map(|gc| gc.section).collect();
    let mut section_armor = SectionArmor::new();
    for section in sections {
        // Mean cell centre of this section, in grid cell-space.
        let mut sum = Vec2::ZERO;
        let mut count = 0u32;
        for gc in hull.cells.iter().filter(|gc| gc.section == section) {
            sum += Vec2::new(gc.coord.0 as f32 + 0.5, gc.coord.1 as f32 + 0.5);
            count += 1;
        }
        let normal = if count > 0 {
            let mean = sum / count as f32;
            let outward = mean - grid_centre;
            if outward.length_squared() > f32::EPSILON {
                outward.normalize()
            } else {
                // A section sitting exactly on the grid centre (the most-interior
                // *core* — e.g. the fighter's central reactor at (4,4)) has no
                // outward direction. Default it to face the demo's nominal **forward
                // attacker** (`-X`), so a head-on +x shot meets it square-on (impact
                // angle ≈ 0, it penetrates) rather than on its back face. A back-
                // facing default (`+X`) makes a +x shot a 180° hit — past
                // `ricochet_angle` — so a centred core would *ricochet every shot*
                // and could never be hull-killed (the E007 live-demo death bug).
                // `-X` keeps the core penetrable from the front while staying a unit
                // normal the angle math can use from any direction.
                CORE_FALLBACK_NORMAL
            }
        } else {
            CORE_FALLBACK_NORMAL
        };
        section_armor.sections.insert(
            section,
            ArmorFacet {
                thickness,
                material: ArmorMaterial::Steel,
                normal,
            },
        );
    }

    // --- Hull structure: a backstop scaled from the hull base mass ---------------
    let hull_hp = (hull.hull_base_mass * HULL_HP_PER_BASE_MASS).max(HULL_HP_FLOOR);
    let hull_structure = HullStructure::full(hull_hp);

    (shields, section_armor, hull_structure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::{
        seed_catalogs, Fit, SlotId, HULL_FIGHTER, MODULE_ARMOR_PLATE, MODULE_REACTOR_BASIC,
    };

    /// A fitted ship seeds non-degenerate layers: a shield present (default pool when
    /// no shield module fits, since no seed hull has a Shield hardpoint), one armor
    /// facet per distinct hull section, and hull HP `> 0`.
    #[test]
    fn fitted_ship_seeds_non_degenerate_layers() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

        let mut fit = Fit::new(HULL_FIGHTER);
        // A reactor + an armor plate (both valid on the fighter): the armor module
        // thickens the facets above the unarmored default.
        let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
        let _ = fit.install_module(SlotId(5), MODULE_ARMOR_PLATE, &hull, &modules);

        let (shields, armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);

        // Shield present (default pool — the fighter has no Shield hardpoint).
        assert!(shields.max > 0.0, "a fitted ship must be shield-defended");
        assert_eq!(shields.current, shields.max, "seeded full");
        assert!(shields.power_linked);

        // One facet per distinct hull section.
        let distinct_sections: BTreeSet<SectionId> =
            hull.cells.iter().map(|gc| gc.section).collect();
        assert_eq!(
            armor.sections.len(),
            distinct_sections.len(),
            "one armor facet per distinct hull section"
        );
        for facet in armor.sections.values() {
            assert!(facet.thickness > 0.0, "every facet has positive thickness");
            assert!(
                facet.normal.is_normalized() || facet.normal == Vec2::X,
                "every facet normal is a unit vector"
            );
        }
        // The fitted armor module thickens the facets above the unarmored default.
        assert!(
            armor
                .sections
                .values()
                .all(|f| f.thickness >= DEFAULT_ARMOR_THICKNESS),
            "fitted armor is at least the unarmored default"
        );

        // Hull structural backstop is non-degenerate.
        assert!(hull_structure.max > 0.0);
        assert_eq!(hull_structure.current, hull_structure.max);
    }

    /// An unarmored fit still seeds a facet per section (the thin default) and a
    /// shield — no fitted ship is ever damage-immune for lack of a defense module.
    #[test]
    fn unarmored_fit_still_seeds_facets_and_a_shield() {
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
        let fit = Fit::new(HULL_FIGHTER); // empty fit

        let (shields, armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);
        assert!(shields.max > 0.0);
        assert!(!armor.sections.is_empty(), "facets seeded even unarmored");
        for facet in armor.sections.values() {
            assert_eq!(
                facet.thickness, DEFAULT_ARMOR_THICKNESS,
                "unarmored sections take the thin default plate"
            );
        }
        assert!(hull_structure.max > 0.0);
    }
}
