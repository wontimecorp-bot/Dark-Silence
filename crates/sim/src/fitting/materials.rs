//! Typed per-cell **hull** + **armor** materials (R66).
//!
//! A cell carries a `hull_material` id (what its STRUCTURAL plating is made of —
//! drives base HP + mass) and an optional `armor_material` id (plating on top —
//! drives the directional penetration plate, carve resistance, and mass). Both are
//! indices into this [`CellMaterials`] catalog, a `bevy_ecs` [`Resource`] defaulted in
//! code, RON/dev-panel-editable, and applied **windowed-only** (so the headless
//! determinism worlds keep `Default` → byte-identical).
//!
//! **Determinism anchor:** id `0` is the baseline everywhere — hull material `0`
//! ("Standard") resolves to the live `SimTuning.struct_cell_hp`/`struct_cell_mass`
//! (the existing globals, via the `fallback` arg), and armor material `0` ("None")
//! resolves to [`ARMOR_NONE`] (all-zero). So a hull whose cells are all material `0/0`
//! reproduces today's HP/mass/gate/carve exactly. Material ids `> 0` are additive new
//! kinds (light/heavy hull · light/medium/heavy armor + future composite/reactive/…).

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use super::content::{STRUCT_CELL_HP, STRUCT_CELL_MASS};

/// A **hull (structural) material** profile — what a structural plating cell is made
/// of. Only consulted for STRUCTURAL cells (a module cell weighs its module); id `0`
/// "Standard" is never read from here (it falls back to the live `struct_cell_*`),
/// so this entry is the dev-panel display baseline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HullMaterialDef {
    /// Display name (e.g. "Standard", "Light", "Heavy").
    pub name: String,
    /// Structural-cell base hit points (hull erosion toughness).
    pub cell_hp: f32,
    /// Structural-cell mass (ship inertia + wreck mass per plating cell).
    pub mass: f32,
}

/// The Copy numeric core of an [`ArmorMaterialDef`], returned by
/// [`CellMaterials::armor_params`] for the hot gate/carve/mass paths (no `String`
/// clone, no panic).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArmorParams {
    /// Nominal plate thickness fed into the penetration gate (`0` = no plate).
    pub thickness: f32,
    /// Material hardness multiplier on thickness (`effective_armor = thickness·mult/cos`).
    pub multiplier: f32,
    /// Extra carve budget per cell (head-on / interior resistance).
    pub carve_hp: f32,
    /// Extra mass per cell (the agility tradeoff).
    pub mass: f32,
}

/// The "no armor" baseline — id `0`. `thickness 0` ⇒ `+0` at the gate, `carve_hp 0` +
/// `mass 0` ⇒ byte-identical to an unarmored cell.
pub const ARMOR_NONE: ArmorParams = ArmorParams {
    thickness: 0.0,
    multiplier: 1.0,
    carve_hp: 0.0,
    mass: 0.0,
};

/// An **armor material** profile — optional plating painted on a cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArmorMaterialDef {
    /// Display name (e.g. "None", "Light", "Medium", "Heavy").
    pub name: String,
    /// Nominal plate thickness fed into the penetration gate (`0` = no plate).
    pub thickness: f32,
    /// Material hardness multiplier on thickness.
    pub multiplier: f32,
    /// Extra carve budget per cell (head-on / interior resistance).
    pub carve_hp: f32,
    /// Extra mass per cell.
    pub mass: f32,
}

/// The typed per-cell materials catalog (R66) — `hull[0]` = Standard, `armor[0]` =
/// None. A `bevy_ecs` [`Resource`]; `Default` reproduces today's behaviour for id
/// `0/0`. Inserted at `ServerApp::new` (default) and overridden **windowed-only**
/// from the dev settings, so the headless determinism worlds keep the default.
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellMaterials {
    /// Hull (structural) materials, indexed by [`super::hull::GridCell::hull_material`].
    pub hull: Vec<HullMaterialDef>,
    /// Armor materials, indexed by [`super::hull::GridCell::armor_material`].
    pub armor: Vec<ArmorMaterialDef>,
}

impl Default for CellMaterials {
    fn default() -> Self {
        Self {
            hull: vec![
                // id 0 — Standard. NOT read from here (id 0 falls back to the live
                // struct_cell_* globals); these values are the dev-panel display baseline.
                HullMaterialDef {
                    name: "Standard".into(),
                    cell_hp: STRUCT_CELL_HP,
                    mass: STRUCT_CELL_MASS,
                },
                HullMaterialDef {
                    name: "Light".into(),
                    cell_hp: STRUCT_CELL_HP * 0.6,
                    mass: STRUCT_CELL_MASS * 0.5,
                },
                HullMaterialDef {
                    name: "Heavy".into(),
                    cell_hp: STRUCT_CELL_HP * 2.0,
                    mass: STRUCT_CELL_MASS * 2.0,
                },
            ],
            armor: vec![
                // id 0 — None (all-zero ⇒ byte-identical to an unarmored cell).
                ArmorMaterialDef {
                    name: "None".into(),
                    thickness: 0.0,
                    multiplier: 1.0,
                    carve_hp: 0.0,
                    mass: 0.0,
                },
                ArmorMaterialDef {
                    name: "Light".into(),
                    thickness: 1.0,
                    multiplier: 1.0,
                    carve_hp: 15.0,
                    mass: 1.0,
                },
                ArmorMaterialDef {
                    name: "Medium".into(),
                    thickness: 2.0,
                    multiplier: 1.0,
                    carve_hp: 30.0,
                    mass: 2.5,
                },
                ArmorMaterialDef {
                    name: "Heavy".into(),
                    thickness: 3.5,
                    multiplier: 1.2,
                    carve_hp: 50.0,
                    mass: 5.0,
                },
            ],
        }
    }
}

impl CellMaterials {
    /// The structural-cell HP for hull material `id`. **id `0` returns `fallback`**
    /// (the live `struct_cell_hp`) so the Standard path is byte-identical to today; an
    /// out-of-range id likewise falls back.
    pub fn hull_hp(&self, id: u8, fallback: f32) -> f32 {
        if id == 0 {
            return fallback;
        }
        self.hull
            .get(id as usize)
            .map(|h| h.cell_hp)
            .unwrap_or(fallback)
    }

    /// The structural-cell mass for hull material `id`. **id `0` returns `fallback`**
    /// (the live `struct_cell_mass`); out-of-range falls back.
    pub fn hull_mass(&self, id: u8, fallback: f32) -> f32 {
        if id == 0 {
            return fallback;
        }
        self.hull
            .get(id as usize)
            .map(|h| h.mass)
            .unwrap_or(fallback)
    }

    /// The Copy [`ArmorParams`] for armor material `id`. **id `0` (and out-of-range)
    /// returns [`ARMOR_NONE`]** (all-zero ⇒ `+0` at the gate/carve/mass → byte-identical).
    pub fn armor_params(&self, id: u8) -> ArmorParams {
        if id == 0 {
            return ARMOR_NONE;
        }
        match self.armor.get(id as usize) {
            Some(a) => ArmorParams {
                thickness: a.thickness,
                multiplier: a.multiplier,
                carve_hp: a.carve_hp,
                mass: a.mass,
            },
            None => ARMOR_NONE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_baselines_are_byte_identical_to_the_globals() {
        let m = CellMaterials::default();
        // Hull material 0 falls back to the passed global (the existing path).
        assert_eq!(m.hull_hp(0, 7.0), 7.0);
        assert_eq!(m.hull_mass(0, 3.0), 3.0);
        // Out-of-range also falls back.
        assert_eq!(m.hull_hp(99, 7.0), 7.0);
        // Armor 0 / out-of-range = None (all-zero).
        assert_eq!(m.armor_params(0), ARMOR_NONE);
        assert_eq!(m.armor_params(250), ARMOR_NONE);
        // A real material reads its profile.
        assert!(m.armor_params(3).thickness > m.armor_params(1).thickness);
        assert!(m.hull_mass(2, 3.0) > m.hull_mass(1, 3.0));
    }
}
