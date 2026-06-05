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

/// The side length of one hull cell **in world (sim) units** — the single
/// authoritative cell→world scale shared by the collision/carve geometry (this
/// crate) and the client render (`crates/client/src/scene.rs::CELL_SIZE`, which is
/// kept synchronized to this value).
///
/// A hull is authored as a `(cols, rows)` cell-grid (see [`Hull::grid_dims`]); this
/// const is what turns a cell coordinate into a world distance. The collision circle
/// and the carve entry-point mapping ([`hull_collision_radius`] and the
/// impact→cell-space transform in `collision::fitted_damage_system`) use it so the
/// swept-cast hit circle matches the **visible** hull footprint and a shot carves
/// where it visually struck — not through the grid centre.
///
/// Value `0.32`: the old single ship box was `1.6` wide on the legacy 5-wide grid, so
/// `1.6 / 5 = 0.32` keeps the silhouette the same physical size on the finer dense
/// grids (the 9×11 fighter ≈ `2.88 × 3.52` world units). Tunable for feel (Phase 3);
/// when it changes the client's `CELL_SIZE` must change with it (the client re-exports
/// / mirrors this value with a sync comment).
pub const CELL_WORLD_SIZE: f32 = 0.32;

/// Stable, data-authored content id for a [`Hull`] catalog row (wire/save-safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HullId(pub u32);

/// A hull's **size tier** (Phase C) — the ordered displacement ladder, smallest→largest.
/// `#[repr(u8)]` + derived `Ord` make size-band comparisons a plain `<`. Distinct from
/// [`ShipRole`] (battlefield function): a tier groups many hull models; role is what one does.
/// Adding a *ship* of an existing tier is RON data; adding a *tier* is an enum edit (rare).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ShipClass {
    Fighter = 0,
    Corvette = 1,
    Frigate = 2,
    Destroyer = 3,
    LightCruiser = 4,
    HeavyCruiser = 5,
    Battlecruiser = 6,
    Battleship = 7,
    Carrier = 8,
    HeavyCarrier = 9,
    Capital = 10,
    Station = 11,
}

/// A hull's **battlefield role** (Phase C) — its function, orthogonal to [`ShipClass`] size.
/// E.g. a small hull can be (Corvette, Gunship) or (Corvette, Interceptor).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShipRole {
    Interceptor,
    FastAttack,
    Patrol,
    Gunship,
    LineCombatant,
    Carrier,
    Support,
    Recon,
    Miner,
    Hauler,
    Utility,
}

/// Identifies the **section** a [`GridCell`] belongs to — the coarse
/// damage/occupancy unit cells group into (ADR-0008). Multiple cells may share a
/// section; a [`Slot`] occupies cells within a single section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SectionId(pub u32);

/// One occupiable cell on the hull grid. A hull is now authored as a **dense filled
/// silhouette** (Phase 1A): every cell inside the designed ship shape is a `GridCell`,
/// not just the slot cells — the cell-grid is the visible hull body and the future
/// per-cell destruction substrate (ADR-0008, GDD §5 "simulate at cell granularity").
///
/// Cells come in two **kinds**, distinguished by [`structural`](GridCell::structural):
/// - a **module cell** (`structural == false`) sits on a [`Slot`]'s `coord` — it is a
///   hardpoint where a [`Module`](super::module::Module) installs; its live health is
///   the installed module's health (or `0` when empty).
/// - a **structural cell** (`structural == true`) is filler hull plating — the rest of
///   the silhouette. It carries no slot; in the layout it is seeded with a tunable
///   structural HP ([`STRUCT_CELL_HP`](super::content::STRUCT_CELL_HP)) so Phase 2 can
///   carve it away cell-by-cell.
///
/// The set of authored cells is still **sparse** in the sense that not every
/// `cols × rows` coordinate need exist (the silhouette need not fill the bounding box).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridCell {
    /// Grid coordinate `(col, row)`, in-bounds of the owning hull's `grid_dims`.
    pub coord: (u16, u16),
    /// The section this cell belongs to (the coarse damage unit).
    pub section: SectionId,
    /// `false` for a **module cell** (on a [`Slot`] coord — a hardpoint), `true` for a
    /// **structural cell** (filler hull plating). Lets downstream code (layout health
    /// seeding, Phase 1B voxel rendering) tell the two kinds apart without re-deriving
    /// the slot-coord match each time.
    pub structural: bool,
}

impl GridCell {
    /// Construct a **module cell** at `coord` in `section` (on a slot/hardpoint). The
    /// historical two-arg constructor: a slot cell is a non-structural module cell.
    pub const fn new(coord: (u16, u16), section: SectionId) -> Self {
        Self {
            coord,
            section,
            structural: false,
        }
    }

    /// Construct a **structural** filler cell at `coord` in `section` (hull plating,
    /// no slot). Seeded with [`STRUCT_CELL_HP`](super::content::STRUCT_CELL_HP) in the
    /// layout so Phase 2 can carve it.
    pub const fn structural(coord: (u16, u16), section: SectionId) -> Self {
        Self {
            coord,
            section,
            structural: true,
        }
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
    /// Size tier (Phase C) — the ordered displacement ladder.
    pub class: ShipClass,
    /// Battlefield role (Phase C) — function, orthogonal to `class`.
    pub role: ShipRole,
    /// Cell-grid dimensions `(cols, rows)`; both `> 0`.
    pub grid_dims: (u16, u16),
    /// The authored set of occupiable cells — a **dense filled silhouette** (every
    /// cell inside the ship shape; in-bounds, no dup coords). Includes a [`GridCell`]
    /// for each [`Slot`]'s coord (a module cell) plus structural filler cells for the
    /// rest of the body (Phase 1A).
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

    /// The collision-circle radius (world units) that matches this hull's **visible
    /// footprint** — the half-extent of its longest grid axis in world units
    /// ([`hull_collision_radius`] on `grid_dims`).
    pub fn collision_radius(&self) -> f32 {
        hull_collision_radius(self.grid_dims)
    }
}

/// The collision-circle radius (world units) for a hull of the given
/// `grid_dims = (cols, rows)`, sized to the **visible hull footprint** so the
/// swept-cast hit circle matches what the player sees (FIX: the old hardcoded
/// `CollisionRadius(1.0)` was *smaller* than the rendered hull, so the swept hit
/// registered inside the silhouette and the impact point was off from the visible
/// edge).
///
/// It is the half-extent of the hull's **longest** grid axis in world units:
/// `max(cols, rows) · CELL_WORLD_SIZE · 0.5` — the distance from the ship centre to
/// the far edge of the silhouette's longest dimension. For the seed fighter (`9×11`)
/// this is `11 · 0.32 · 0.5 = 1.76`; for the corvette (`13×15`), `15 · 0.32 · 0.5 =
/// 2.4`. A degenerate `(0, 0)` hull yields `0.0` (defensive; never authored).
///
/// Using the **longest** axis (a circle that circumscribes the silhouette rather than
/// inscribing it) guarantees a shot that visually clips any edge of the hull registers
/// a hit; the impact→cell-space carve mapping then resolves WHERE on the hull it
/// landed, so the channel begins at the struck cell.
pub fn hull_collision_radius(grid_dims: (u16, u16)) -> f32 {
    let max_dim = grid_dims.0.max(grid_dims.1) as f32;
    max_dim * CELL_WORLD_SIZE * 0.5
}

/// Build a procedural **station hull** (Refinement 5 Phase 2) — a plated frame for the mining
/// structures: a perimeter shell `plating` cells thick plus a horizontal + vertical strut through the
/// centre. So it's mostly hollow (a bounded cell count instead of a solid `cols·rows` fill) yet fully
/// connected, and — since [`cell_depth`](crate::fitting::layout) is distance-to-nearest-edge — the
/// CENTRE cell is the deepest, i.e. the carve-to-core death point. All cells are structural (no
/// modules/slots); the carve pipeline seeds each with the structural-cell HP. World size is
/// `grid · CELL_WORLD_SIZE`. A larger `plating` fills more of the interior (→ solid at the limit).
pub fn station_hull(id: HullId, name: &str, cols: u16, rows: u16, plating: u16) -> Hull {
    let p = plating.max(1);
    let cx = cols.saturating_sub(1) / 2;
    let cy = rows.saturating_sub(1) / 2;
    let mut cells = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let near_edge = col < p || col + p >= cols || row < p || row + p >= rows;
            let on_v_strut = col.abs_diff(cx) < p;
            let on_h_strut = row.abs_diff(cy) < p;
            if near_edge || on_v_strut || on_h_strut {
                cells.push(GridCell {
                    coord: (col, row),
                    section: SectionId(1),
                    structural: true,
                });
            }
        }
    }
    Hull {
        id,
        name: name.to_string(),
        class: ShipClass::Station,
        role: ShipRole::Utility,
        grid_dims: (cols, rows),
        cells,
        power_capacity: 0.0,
        cpu_capacity: 0.0,
        mass_capacity: 0.0,
        hull_base_mass: 0.0,
        slots: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hull() -> Hull {
        Hull {
            id: HullId(1),
            name: "Test".to_string(),
            class: ShipClass::Fighter,
            role: ShipRole::Utility,
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
