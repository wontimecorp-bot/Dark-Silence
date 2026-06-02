//! The Hull — a designer-authored 2D cell-grid chassis (FR-003, FR-004) and its
//! positional, typed, sized [`Slot`] inventory (FR-004, FR-020) — per ADR-0008.
//!
//! The hull is authored as a sparse cell-grid grouped into **sections** (the
//! coarse damage/occupancy unit). The same grid is both the fitting layout and
//! the E007 hit/armor map (ADR-0008): authored at section granularity now,
//! **cell-upgrade-ready** so fine per-cell destruction (E007+) is a content
//! upgrade on this structure, not a data-model refactor (HINT-004).
//!
//! A [`Slot`] occupies one authored cell (later: a contiguous group) and gates
//! installs by type + ordered size. Weapon mounts additionally expose a
//! [`FiringArc`] derived from position/facing — E006 defines the arc as **data**;
//! its enforcement (turret track / can-this-hit) is E007.
//!
//! Derive discipline matches `module.rs` and the E001/E002 components: serde as a
//! replication/persistence seam (not exercised this epic), value semantics.

use serde::{Deserialize, Serialize};

use super::module::{HardpointType, SlotSize};

/// Stable, data-authored content id for a [`Hull`] catalog row (wire/save-safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HullId(pub u32);

/// Identifies the **section** a [`GridCell`] belongs to — the coarse
/// damage/occupancy unit cells group into (ADR-0008). Multiple cells may share a
/// section; a [`Slot`] occupies cells within a single section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SectionId(pub u32);

/// One occupiable cell on the hull grid. The set of authored cells is **sparse**:
/// not every `cols × rows` coordinate need exist (data-model.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridCell {
    /// Grid coordinate `(col, row)`, in-bounds of the owning hull's `grid_dims`.
    pub coord: (u16, u16),
    /// The section this cell belongs to (the coarse damage unit).
    pub section: SectionId,
}

impl GridCell {
    /// Construct an authored cell at `coord` in `section`.
    pub const fn new(coord: (u16, u16), section: SectionId) -> Self {
        Self { coord, section }
    }
}

/// A weapon hardpoint's angular coverage (FR-020), **derived** from the slot's
/// position/facing on the hull. E006 defines it as fit data; E007 enforces it.
///
/// Invariant INV-F12: `half_angle ∈ (0, π]` — never a zero-width or wrap-around
/// arc. (The derivation function lives in the layout phase; this is the value.)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FiringArc {
    /// Arc center in radians (`hull_heading + slot.facing` when applied).
    pub center: f32,
    /// Half the angular width, in radians; bounded `(0, π]`.
    pub half_angle: f32,
}

/// Stable id for a [`Slot`], **unique within its owning hull** — the key in a
/// `Fit`'s slot→module map (data-model.md). Hull-local, not global.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SlotId(pub u32);

/// A typed, sized, positioned mount point on the hull grid (a.k.a. hardpoint).
///
/// `slot_type` + `size` gate which modules may be installed (INV-F01/F02):
/// a module installs iff `module.hardpoint_type == slot_type` and
/// `module.hardpoint_size <= size`. Weapon mounts (`is_weapon_mount`) expose a
/// derived [`FiringArc`] from `coord` + `facing` (FR-020).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// Unique within the owning hull; the `Fit` map key.
    pub id: SlotId,
    /// Module `hardpoint_type` must equal this (FR-006).
    pub slot_type: HardpointType,
    /// Module `hardpoint_size` must be `<=` this ordered size (FR-007).
    pub size: SlotSize,
    /// Grid position; in `grid_dims`, on an authored cell (drives occlusion
    /// depth + arc center).
    pub coord: (u16, u16),
    /// Mount facing on the hull, radians, wrapped `[0, 2π)` (drives arc center).
    pub facing: f32,
    /// If true, the slot exposes a derived [`FiringArc`] (weapon hardpoint).
    pub is_weapon_mount: bool,
}

/// A designer-authored 2D cell-grid chassis with budgets and a slot inventory
/// (FR-003, FR-004). Loaded into the `HullCatalog` resource at startup as content
/// (FR-025); a `Fit` references one hull by [`HullId`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hull {
    /// Stable catalog key referenced by a `Fit`.
    pub id: HullId,
    /// Display name (e.g. "Fighter", "Corvette"); non-empty.
    pub name: String,
    /// Cell-grid dimensions `(cols, rows)`; both `> 0`.
    pub grid_dims: (u16, u16),
    /// The authored set of occupiable cells (sparse; in-bounds, no dup coords).
    pub cells: Vec<GridCell>,
    /// Power budget ceiling (base; reactor `power_gen` *supplies* on top, this is
    /// the structural cap; `>= 0`).
    pub power_capacity: f32,
    /// CPU/control budget ceiling (`> 0`).
    pub cpu_capacity: f32,
    /// Max total fit mass the hull can carry (`> 0`).
    pub mass_capacity: f32,
    /// Chassis mass added before modules (`> 0`; empty-hull mass is never zero).
    pub hull_base_mass: f32,
    /// Positional slot inventory; each at an in-bounds authored cell, ids unique
    /// within the hull.
    pub slots: Vec<Slot>,
}

impl Hull {
    /// Look up a slot by its hull-local [`SlotId`]; `None` if no such slot.
    ///
    /// Null-safe accessor (no panic on a dangling id): validation and layout
    /// resolve `Fit` slot keys through this.
    pub fn slot(&self, id: SlotId) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hull() -> Hull {
        Hull {
            id: HullId(1),
            name: "Test".to_string(),
            grid_dims: (3, 3),
            cells: vec![
                GridCell::new((0, 0), SectionId(0)),
                GridCell::new((1, 1), SectionId(1)),
            ],
            power_capacity: 10.0,
            cpu_capacity: 10.0,
            mass_capacity: 100.0,
            hull_base_mass: 5.0,
            slots: vec![
                Slot {
                    id: SlotId(0),
                    slot_type: HardpointType::Reactor,
                    size: SlotSize::Small,
                    coord: (0, 0),
                    facing: 0.0,
                    is_weapon_mount: false,
                },
                Slot {
                    id: SlotId(1),
                    slot_type: HardpointType::Weapon,
                    size: SlotSize::Medium,
                    coord: (1, 1),
                    facing: 0.0,
                    is_weapon_mount: true,
                },
            ],
        }
    }

    #[test]
    fn slot_lookup_resolves_known_id_and_rejects_unknown() {
        let hull = sample_hull();
        assert_eq!(
            hull.slot(SlotId(1)).map(|s| s.slot_type),
            Some(HardpointType::Weapon)
        );
        assert!(hull.slot(SlotId(99)).is_none());
    }
}
