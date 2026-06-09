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

use glam::Vec2;
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

impl ShipClass {
    /// R60 — every class, in tier order (for editor dropdowns).
    pub const ALL: [ShipClass; 12] = [
        ShipClass::Fighter,
        ShipClass::Corvette,
        ShipClass::Frigate,
        ShipClass::Destroyer,
        ShipClass::LightCruiser,
        ShipClass::HeavyCruiser,
        ShipClass::Battlecruiser,
        ShipClass::Battleship,
        ShipClass::Carrier,
        ShipClass::HeavyCarrier,
        ShipClass::Capital,
        ShipClass::Station,
    ];
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

impl ShipRole {
    /// R60 — every role (for editor dropdowns).
    pub const ALL: [ShipRole; 11] = [
        ShipRole::Interceptor,
        ShipRole::FastAttack,
        ShipRole::Patrol,
        ShipRole::Gunship,
        ShipRole::LineCombatant,
        ShipRole::Carrier,
        ShipRole::Support,
        ShipRole::Recon,
        ShipRole::Miner,
        ShipRole::Hauler,
        ShipRole::Utility,
    ];
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
/// R58 — a cell's occupied sub-region: the full unit square, a 45° corner triangle (half), or a small
/// corner triangle (quarter). Each shape is a CONVEX polygon in GRID space — a cell `(c,r)` spans
/// `[c,c+1]×[r,r+1]` (x = col, y = row) — via [`CellShape::corners`], so ONE path drives the carve
/// HITBOX (segment-vs-polygon), the MASS (area + centroid), and the RENDER (polygon edges). `Full` keeps
/// the exact legacy inscribed-circle hitbox + full mass/centre → byte-identical; only SUB-shapes use the
/// polygon. Half names = the kept right-angle corner (`HalfSW` keeps the SW corner, cutting NE). New
/// shapes = a new variant + its polygon (extensible).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CellShape {
    #[default]
    Full,
    /// 45° corner triangle (right angle at the named corner, legs 1.0, area 0.5).
    HalfSW,
    HalfSE,
    HalfNE,
    HalfNW,
    /// Small corner triangle (right angle at the named corner, legs 0.5, area 0.125) — sharp tips.
    QuarterSW,
    QuarterSE,
    QuarterNE,
    QuarterNW,
    /// R59 — corner CHAMFER (pentagon, area 0.875): the full square with the named corner cut by a
    /// 0.5×0.5 sliver. A subtle bevel for softening a corner without losing a cell-half.
    ChamferSW,
    ChamferSE,
    ChamferNE,
    ChamferNW,
    /// R59 — shallow 2:1 SLOPE (right-trapezoid, area 0.75): the cell minus a right-triangle whose right
    /// angle is at the named corner, with the LONG leg (1.0) along the H(orizontal, col/x) or V(ertical,
    /// row/y) edge from that corner and the SHORT leg (0.5) up the adjacent edge — a gentle ramp for
    /// tapering a hull edge without the hard 45° of a `Half`.
    SlopeSWH,
    SlopeSWV,
    SlopeSEH,
    SlopeSEV,
    SlopeNEH,
    SlopeNEV,
    SlopeNWH,
    SlopeNWV,
    /// R62 — shallower 3:1 slope (short leg 1/3, area 5/6) and 4:1 slope (short leg 1/4, area 7/8): the
    /// same corner-cut ramp as `Slope` but gentler, for sleeker / sharper multi-cell tapers.
    Slope3SWH,
    Slope3SWV,
    Slope3SEH,
    Slope3SEV,
    Slope3NEH,
    Slope3NEV,
    Slope3NWH,
    Slope3NWV,
    Slope4SWH,
    Slope4SWV,
    Slope4SEH,
    Slope4SEV,
    Slope4NEH,
    Slope4NEV,
    Slope4NWH,
    Slope4NWV,
    /// R62 — POINT / spire (isoceles triangle from a full base edge to the opposite-edge MIDPOINT apex,
    /// area 0.5): a sharp 1-cell nose tip + the apex cap of a cone. The named direction is where it points.
    PointN,
    PointS,
    PointE,
    PointW,
    /// R62 — ROUND / blunt cap (hexagon, area 0.9375): the named EDGE's two corners cut by 1/4 chamfers —
    /// a flat-rounded 1-cell cap.
    RoundN,
    RoundS,
    RoundE,
    RoundW,
    /// R62 — OCTAGON (area 0.875): all four corners 1/4-chamfered — a fully-rounded cell (pod / dome).
    Octagon,
    /// R64 — thin-triangle WEDGE: the SKINNY complement of a `Slope` (the diagonal sliver left when the
    /// fat part is cut away). A right triangle with one full leg (1.0) + a short leg `k` — `Wedge2*`
    /// k=1/2 (area 1/4), `Wedge3*` k=1/3 (area 1/6), `Wedge4*` k=1/4 (area 1/8). 8 slope orientations.
    /// These are the slim triangles to STACK for a thin sharp blade / wing.
    Wedge2SWH,
    Wedge2SWV,
    Wedge2SEH,
    Wedge2SEV,
    Wedge2NEH,
    Wedge2NEV,
    Wedge2NWH,
    Wedge2NWV,
    Wedge3SWH,
    Wedge3SWV,
    Wedge3SEH,
    Wedge3SEV,
    Wedge3NEH,
    Wedge3NEV,
    Wedge3NWH,
    Wedge3NWV,
    Wedge4SWH,
    Wedge4SWV,
    Wedge4SEH,
    Wedge4SEV,
    Wedge4NEH,
    Wedge4NEV,
    Wedge4NWH,
    Wedge4NWV,
    /// R64 — edge STRIP: a thin rectangular band covering one cell EDGE to thickness `t` (`34`=3/4,
    /// `12`=1/2, `14`=1/4, `18`=1/8; area = `t`). For thin walls / panel ribs / blade shafts.
    StripN34,
    StripN12,
    StripN14,
    StripN18,
    StripS34,
    StripS12,
    StripS14,
    StripS18,
    StripE34,
    StripE12,
    StripE14,
    StripE18,
    StripW34,
    StripW12,
    StripW14,
    StripW18,
}

impl CellShape {
    /// The convex polygon (CCW) for a cell at `(c, r)` in GRID space (`x = col`, `y = row`). `Full` = the
    /// unit square; the others are corner triangles. Used by the render tracer + the sub-shape hitbox.
    pub fn corners(self, c: u16, r: u16) -> Vec<Vec2> {
        let (c, r) = (c as f32, r as f32);
        let p = |x: f32, y: f32| Vec2::new(c + x, r + y);
        match self {
            CellShape::Full => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::HalfSW => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)],
            CellShape::HalfSE => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.0, 0.0)],
            CellShape::HalfNE => vec![p(1.0, 1.0), p(0.0, 1.0), p(1.0, 0.0)],
            CellShape::HalfNW => vec![p(0.0, 1.0), p(0.0, 0.0), p(1.0, 1.0)],
            CellShape::QuarterSW => vec![p(0.0, 0.0), p(0.5, 0.0), p(0.0, 0.5)],
            CellShape::QuarterSE => vec![p(1.0, 0.0), p(1.0, 0.5), p(0.5, 0.0)],
            CellShape::QuarterNE => vec![p(1.0, 1.0), p(0.5, 1.0), p(1.0, 0.5)],
            CellShape::QuarterNW => vec![p(0.0, 1.0), p(0.0, 0.5), p(0.5, 1.0)],
            // R59 chamfers (pentagons, CCW) — the named corner replaced by its two 0.5-along-edge points.
            CellShape::ChamferSW => {
                vec![
                    p(0.5, 0.0),
                    p(1.0, 0.0),
                    p(1.0, 1.0),
                    p(0.0, 1.0),
                    p(0.0, 0.5),
                ]
            }
            CellShape::ChamferSE => {
                vec![
                    p(0.0, 0.0),
                    p(0.5, 0.0),
                    p(1.0, 0.5),
                    p(1.0, 1.0),
                    p(0.0, 1.0),
                ]
            }
            CellShape::ChamferNE => {
                vec![
                    p(0.0, 0.0),
                    p(1.0, 0.0),
                    p(1.0, 0.5),
                    p(0.5, 1.0),
                    p(0.0, 1.0),
                ]
            }
            CellShape::ChamferNW => {
                vec![
                    p(0.0, 0.0),
                    p(1.0, 0.0),
                    p(1.0, 1.0),
                    p(0.5, 1.0),
                    p(0.0, 0.5),
                ]
            }
            // R59 slopes (right-trapezoids, CCW) — the cell minus the long-leg corner triangle.
            CellShape::SlopeSWH => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0), p(0.0, 0.5)],
            CellShape::SlopeSWV => vec![p(0.5, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::SlopeSEH => vec![p(0.0, 0.0), p(1.0, 0.5), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::SlopeSEV => vec![p(0.0, 0.0), p(0.5, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::SlopeNEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.5), p(0.0, 1.0)],
            CellShape::SlopeNEV => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.5, 1.0), p(0.0, 1.0)],
            CellShape::SlopeNWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 0.5)],
            CellShape::SlopeNWV => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.5, 1.0)],
            // R62 shallower slopes (same corner-cut ramp, short leg `k`): Slope3 k=1/3, Slope4 k=1/4.
            CellShape::Slope3SWH => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0), p(0.0, 1.0 / 3.0)],
            CellShape::Slope3SWV => vec![p(1.0 / 3.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope3SEH => vec![p(0.0, 0.0), p(1.0, 1.0 / 3.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope3SEV => vec![p(0.0, 0.0), p(2.0 / 3.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope3NEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 2.0 / 3.0), p(0.0, 1.0)],
            CellShape::Slope3NEV => vec![p(0.0, 0.0), p(1.0, 0.0), p(2.0 / 3.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope3NWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 2.0 / 3.0)],
            CellShape::Slope3NWV => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(1.0 / 3.0, 1.0)],
            CellShape::Slope4SWH => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0), p(0.0, 0.25)],
            CellShape::Slope4SWV => vec![p(0.25, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope4SEH => vec![p(0.0, 0.0), p(1.0, 0.25), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope4SEV => vec![p(0.0, 0.0), p(0.75, 0.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Slope4NEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.75), p(0.0, 1.0)],
            CellShape::Slope4NEV => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.75, 1.0), p(0.0, 1.0)],
            CellShape::Slope4NWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 0.75)],
            CellShape::Slope4NWV => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.25, 1.0)],
            // R62 points (triangle to the opposite-edge midpoint apex, CCW).
            CellShape::PointN => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.5, 1.0)],
            CellShape::PointS => vec![p(1.0, 1.0), p(0.0, 1.0), p(0.5, 0.0)],
            CellShape::PointE => vec![p(0.0, 1.0), p(0.0, 0.0), p(1.0, 0.5)],
            CellShape::PointW => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.0, 0.5)],
            // R62 round caps (hexagon — the named edge's two corners 1/4-chamfered, CCW).
            CellShape::RoundN => vec![
                p(0.0, 0.0),
                p(1.0, 0.0),
                p(1.0, 0.75),
                p(0.75, 1.0),
                p(0.25, 1.0),
                p(0.0, 0.75),
            ],
            CellShape::RoundS => vec![
                p(0.25, 0.0),
                p(0.75, 0.0),
                p(1.0, 0.25),
                p(1.0, 1.0),
                p(0.0, 1.0),
                p(0.0, 0.25),
            ],
            CellShape::RoundE => vec![
                p(0.0, 0.0),
                p(0.75, 0.0),
                p(1.0, 0.25),
                p(1.0, 0.75),
                p(0.75, 1.0),
                p(0.0, 1.0),
            ],
            CellShape::RoundW => vec![
                p(0.25, 0.0),
                p(1.0, 0.0),
                p(1.0, 1.0),
                p(0.25, 1.0),
                p(0.0, 0.75),
                p(0.0, 0.25),
            ],
            // R62 octagon — all four corners 1/4-chamfered (CCW).
            CellShape::Octagon => vec![
                p(0.25, 0.0),
                p(0.75, 0.0),
                p(1.0, 0.25),
                p(1.0, 0.75),
                p(0.75, 1.0),
                p(0.25, 1.0),
                p(0.0, 0.75),
                p(0.0, 0.25),
            ],
            // R64 wedges (thin triangles, CCW) — the complement of the same-name `Slope`, short leg `k`.
            CellShape::Wedge2SWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 0.5)],
            CellShape::Wedge2SWV => vec![p(0.0, 0.0), p(0.5, 0.0), p(0.0, 1.0)],
            CellShape::Wedge2SEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.5)],
            CellShape::Wedge2SEV => vec![p(0.5, 0.0), p(1.0, 0.0), p(1.0, 1.0)],
            CellShape::Wedge2NEH => vec![p(1.0, 0.5), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge2NEV => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.5, 1.0)],
            CellShape::Wedge2NWH => vec![p(0.0, 0.5), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge2NWV => vec![p(0.0, 0.0), p(0.5, 1.0), p(0.0, 1.0)],
            CellShape::Wedge3SWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0 / 3.0)],
            CellShape::Wedge3SWV => vec![p(0.0, 0.0), p(1.0 / 3.0, 0.0), p(0.0, 1.0)],
            CellShape::Wedge3SEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0 / 3.0)],
            CellShape::Wedge3SEV => vec![p(2.0 / 3.0, 0.0), p(1.0, 0.0), p(1.0, 1.0)],
            CellShape::Wedge3NEH => vec![p(1.0, 2.0 / 3.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge3NEV => vec![p(1.0, 0.0), p(1.0, 1.0), p(2.0 / 3.0, 1.0)],
            CellShape::Wedge3NWH => vec![p(0.0, 2.0 / 3.0), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge3NWV => vec![p(0.0, 0.0), p(1.0 / 3.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge4SWH => vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 0.25)],
            CellShape::Wedge4SWV => vec![p(0.0, 0.0), p(0.25, 0.0), p(0.0, 1.0)],
            CellShape::Wedge4SEH => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.25)],
            CellShape::Wedge4SEV => vec![p(0.75, 0.0), p(1.0, 0.0), p(1.0, 1.0)],
            CellShape::Wedge4NEH => vec![p(1.0, 0.75), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge4NEV => vec![p(1.0, 0.0), p(1.0, 1.0), p(0.75, 1.0)],
            CellShape::Wedge4NWH => vec![p(0.0, 0.75), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::Wedge4NWV => vec![p(0.0, 0.0), p(0.25, 1.0), p(0.0, 1.0)],
            // R64 strips (edge rectangles, CCW) — thickness `t` along the named edge.
            CellShape::StripN34 => vec![p(0.0, 0.25), p(1.0, 0.25), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::StripN12 => vec![p(0.0, 0.5), p(1.0, 0.5), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::StripN14 => vec![p(0.0, 0.75), p(1.0, 0.75), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::StripN18 => vec![p(0.0, 0.875), p(1.0, 0.875), p(1.0, 1.0), p(0.0, 1.0)],
            CellShape::StripS34 => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.75), p(0.0, 0.75)],
            CellShape::StripS12 => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.5), p(0.0, 0.5)],
            CellShape::StripS14 => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.25), p(0.0, 0.25)],
            CellShape::StripS18 => vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 0.125), p(0.0, 0.125)],
            CellShape::StripE34 => vec![p(0.25, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.25, 1.0)],
            CellShape::StripE12 => vec![p(0.5, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.5, 1.0)],
            CellShape::StripE14 => vec![p(0.75, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.75, 1.0)],
            CellShape::StripE18 => vec![p(0.875, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.875, 1.0)],
            CellShape::StripW34 => vec![p(0.0, 0.0), p(0.75, 0.0), p(0.75, 1.0), p(0.0, 1.0)],
            CellShape::StripW12 => vec![p(0.0, 0.0), p(0.5, 0.0), p(0.5, 1.0), p(0.0, 1.0)],
            CellShape::StripW14 => vec![p(0.0, 0.0), p(0.25, 0.0), p(0.25, 1.0), p(0.0, 1.0)],
            CellShape::StripW18 => vec![p(0.0, 0.0), p(0.125, 0.0), p(0.125, 1.0), p(0.0, 1.0)],
        }
    }

    /// Fraction of the unit cell's area this shape occupies (the mass multiplier).
    pub fn area_factor(self) -> f32 {
        match self {
            CellShape::Full => 1.0,
            CellShape::HalfSW | CellShape::HalfSE | CellShape::HalfNE | CellShape::HalfNW => 0.5,
            CellShape::QuarterSW
            | CellShape::QuarterSE
            | CellShape::QuarterNE
            | CellShape::QuarterNW => 0.125,
            CellShape::ChamferSW
            | CellShape::ChamferSE
            | CellShape::ChamferNE
            | CellShape::ChamferNW => 0.875,
            CellShape::SlopeSWH
            | CellShape::SlopeSWV
            | CellShape::SlopeSEH
            | CellShape::SlopeSEV
            | CellShape::SlopeNEH
            | CellShape::SlopeNEV
            | CellShape::SlopeNWH
            | CellShape::SlopeNWV => 0.75,
            CellShape::Slope3SWH
            | CellShape::Slope3SWV
            | CellShape::Slope3SEH
            | CellShape::Slope3SEV
            | CellShape::Slope3NEH
            | CellShape::Slope3NEV
            | CellShape::Slope3NWH
            | CellShape::Slope3NWV => 5.0 / 6.0,
            CellShape::Slope4SWH
            | CellShape::Slope4SWV
            | CellShape::Slope4SEH
            | CellShape::Slope4SEV
            | CellShape::Slope4NEH
            | CellShape::Slope4NEV
            | CellShape::Slope4NWH
            | CellShape::Slope4NWV => 0.875,
            CellShape::PointN | CellShape::PointS | CellShape::PointE | CellShape::PointW => 0.5,
            CellShape::RoundN | CellShape::RoundS | CellShape::RoundE | CellShape::RoundW => 0.9375,
            CellShape::Octagon => 0.875,
            CellShape::Wedge2SWH
            | CellShape::Wedge2SWV
            | CellShape::Wedge2SEH
            | CellShape::Wedge2SEV
            | CellShape::Wedge2NEH
            | CellShape::Wedge2NEV
            | CellShape::Wedge2NWH
            | CellShape::Wedge2NWV => 0.25,
            CellShape::Wedge3SWH
            | CellShape::Wedge3SWV
            | CellShape::Wedge3SEH
            | CellShape::Wedge3SEV
            | CellShape::Wedge3NEH
            | CellShape::Wedge3NEV
            | CellShape::Wedge3NWH
            | CellShape::Wedge3NWV => 1.0 / 6.0,
            CellShape::Wedge4SWH
            | CellShape::Wedge4SWV
            | CellShape::Wedge4SEH
            | CellShape::Wedge4SEV
            | CellShape::Wedge4NEH
            | CellShape::Wedge4NEV
            | CellShape::Wedge4NWH
            | CellShape::Wedge4NWV => 0.125,
            CellShape::StripN34
            | CellShape::StripS34
            | CellShape::StripE34
            | CellShape::StripW34 => 0.75,
            CellShape::StripN12
            | CellShape::StripS12
            | CellShape::StripE12
            | CellShape::StripW12 => 0.5,
            CellShape::StripN14
            | CellShape::StripS14
            | CellShape::StripE14
            | CellShape::StripW14 => 0.25,
            CellShape::StripN18
            | CellShape::StripS18
            | CellShape::StripE18
            | CellShape::StripW18 => 0.125,
        }
    }

    /// Centroid in GRID space (the mass-weighted centre). For `Full` + the corner TRIANGLES the corner
    /// mean IS the exact centroid (kept verbatim → R58 mass/COM stays BYTE-IDENTICAL); for the R59
    /// pentagons/trapezoids the corner mean is NOT the centroid, so those use the area-weighted shoelace
    /// centroid. `Full` is `(c+0.5, r+0.5)`.
    pub fn centroid(self, c: u16, r: u16) -> Vec2 {
        let pts = self.corners(c, r);
        match self {
            // Triangles + the square: corner mean = exact centroid (unchanged from R58).
            CellShape::Full
            | CellShape::HalfSW
            | CellShape::HalfSE
            | CellShape::HalfNE
            | CellShape::HalfNW
            | CellShape::QuarterSW
            | CellShape::QuarterSE
            | CellShape::QuarterNE
            | CellShape::QuarterNW => pts.iter().fold(Vec2::ZERO, |a, &p| a + p) / pts.len() as f32,
            // R59 pentagons/trapezoids: area-weighted polygon centroid.
            _ => polygon_centroid(&pts),
        }
    }

    pub fn is_full(self) -> bool {
        matches!(self, CellShape::Full)
    }

    /// R60/R62/R64 — every shape, in a stable order (for editor palette / iteration).
    pub const ALL: [CellShape; 86] = [
        CellShape::Full,
        CellShape::HalfSW,
        CellShape::HalfSE,
        CellShape::HalfNE,
        CellShape::HalfNW,
        CellShape::QuarterSW,
        CellShape::QuarterSE,
        CellShape::QuarterNE,
        CellShape::QuarterNW,
        CellShape::ChamferSW,
        CellShape::ChamferSE,
        CellShape::ChamferNE,
        CellShape::ChamferNW,
        CellShape::SlopeSWH,
        CellShape::SlopeSWV,
        CellShape::SlopeSEH,
        CellShape::SlopeSEV,
        CellShape::SlopeNEH,
        CellShape::SlopeNEV,
        CellShape::SlopeNWH,
        CellShape::SlopeNWV,
        CellShape::Slope3SWH,
        CellShape::Slope3SWV,
        CellShape::Slope3SEH,
        CellShape::Slope3SEV,
        CellShape::Slope3NEH,
        CellShape::Slope3NEV,
        CellShape::Slope3NWH,
        CellShape::Slope3NWV,
        CellShape::Slope4SWH,
        CellShape::Slope4SWV,
        CellShape::Slope4SEH,
        CellShape::Slope4SEV,
        CellShape::Slope4NEH,
        CellShape::Slope4NEV,
        CellShape::Slope4NWH,
        CellShape::Slope4NWV,
        CellShape::PointN,
        CellShape::PointS,
        CellShape::PointE,
        CellShape::PointW,
        CellShape::RoundN,
        CellShape::RoundS,
        CellShape::RoundE,
        CellShape::RoundW,
        CellShape::Octagon,
        CellShape::Wedge2SWH,
        CellShape::Wedge2SWV,
        CellShape::Wedge2SEH,
        CellShape::Wedge2SEV,
        CellShape::Wedge2NEH,
        CellShape::Wedge2NEV,
        CellShape::Wedge2NWH,
        CellShape::Wedge2NWV,
        CellShape::Wedge3SWH,
        CellShape::Wedge3SWV,
        CellShape::Wedge3SEH,
        CellShape::Wedge3SEV,
        CellShape::Wedge3NEH,
        CellShape::Wedge3NEV,
        CellShape::Wedge3NWH,
        CellShape::Wedge3NWV,
        CellShape::Wedge4SWH,
        CellShape::Wedge4SWV,
        CellShape::Wedge4SEH,
        CellShape::Wedge4SEV,
        CellShape::Wedge4NEH,
        CellShape::Wedge4NEV,
        CellShape::Wedge4NWH,
        CellShape::Wedge4NWV,
        CellShape::StripN34,
        CellShape::StripN12,
        CellShape::StripN14,
        CellShape::StripN18,
        CellShape::StripS34,
        CellShape::StripS12,
        CellShape::StripS14,
        CellShape::StripS18,
        CellShape::StripE34,
        CellShape::StripE12,
        CellShape::StripE14,
        CellShape::StripE18,
        CellShape::StripW34,
        CellShape::StripW12,
        CellShape::StripW14,
        CellShape::StripW18,
    ];

    /// R60 — a short human label for the editor dropdown / per-cell glyph.
    pub fn label(self) -> &'static str {
        match self {
            CellShape::Full => "Full",
            CellShape::HalfSW => "Half SW",
            CellShape::HalfSE => "Half SE",
            CellShape::HalfNE => "Half NE",
            CellShape::HalfNW => "Half NW",
            CellShape::QuarterSW => "Quarter SW",
            CellShape::QuarterSE => "Quarter SE",
            CellShape::QuarterNE => "Quarter NE",
            CellShape::QuarterNW => "Quarter NW",
            CellShape::ChamferSW => "Chamfer SW",
            CellShape::ChamferSE => "Chamfer SE",
            CellShape::ChamferNE => "Chamfer NE",
            CellShape::ChamferNW => "Chamfer NW",
            // H = shallow (rise:run 1:2), V = steep (2:1).
            CellShape::SlopeSWH => "Slope SW 1:2",
            CellShape::SlopeSWV => "Slope SW 2:1",
            CellShape::SlopeSEH => "Slope SE 1:2",
            CellShape::SlopeSEV => "Slope SE 2:1",
            CellShape::SlopeNEH => "Slope NE 1:2",
            CellShape::SlopeNEV => "Slope NE 2:1",
            CellShape::SlopeNWH => "Slope NW 1:2",
            CellShape::SlopeNWV => "Slope NW 2:1",
            CellShape::Slope3SWH => "Slope SW 1:3",
            CellShape::Slope3SWV => "Slope SW 3:1",
            CellShape::Slope3SEH => "Slope SE 1:3",
            CellShape::Slope3SEV => "Slope SE 3:1",
            CellShape::Slope3NEH => "Slope NE 1:3",
            CellShape::Slope3NEV => "Slope NE 3:1",
            CellShape::Slope3NWH => "Slope NW 1:3",
            CellShape::Slope3NWV => "Slope NW 3:1",
            CellShape::Slope4SWH => "Slope SW 1:4",
            CellShape::Slope4SWV => "Slope SW 4:1",
            CellShape::Slope4SEH => "Slope SE 1:4",
            CellShape::Slope4SEV => "Slope SE 4:1",
            CellShape::Slope4NEH => "Slope NE 1:4",
            CellShape::Slope4NEV => "Slope NE 4:1",
            CellShape::Slope4NWH => "Slope NW 1:4",
            CellShape::Slope4NWV => "Slope NW 4:1",
            CellShape::PointN => "Point N",
            CellShape::PointS => "Point S",
            CellShape::PointE => "Point E",
            CellShape::PointW => "Point W",
            CellShape::RoundN => "Round N",
            CellShape::RoundS => "Round S",
            CellShape::RoundE => "Round E",
            CellShape::RoundW => "Round W",
            CellShape::Octagon => "Octagon",
            CellShape::Wedge2SWH => "Wedge 1:2 SW-H",
            CellShape::Wedge2SWV => "Wedge 1:2 SW-V",
            CellShape::Wedge2SEH => "Wedge 1:2 SE-H",
            CellShape::Wedge2SEV => "Wedge 1:2 SE-V",
            CellShape::Wedge2NEH => "Wedge 1:2 NE-H",
            CellShape::Wedge2NEV => "Wedge 1:2 NE-V",
            CellShape::Wedge2NWH => "Wedge 1:2 NW-H",
            CellShape::Wedge2NWV => "Wedge 1:2 NW-V",
            CellShape::Wedge3SWH => "Wedge 1:3 SW-H",
            CellShape::Wedge3SWV => "Wedge 1:3 SW-V",
            CellShape::Wedge3SEH => "Wedge 1:3 SE-H",
            CellShape::Wedge3SEV => "Wedge 1:3 SE-V",
            CellShape::Wedge3NEH => "Wedge 1:3 NE-H",
            CellShape::Wedge3NEV => "Wedge 1:3 NE-V",
            CellShape::Wedge3NWH => "Wedge 1:3 NW-H",
            CellShape::Wedge3NWV => "Wedge 1:3 NW-V",
            CellShape::Wedge4SWH => "Wedge 1:4 SW-H",
            CellShape::Wedge4SWV => "Wedge 1:4 SW-V",
            CellShape::Wedge4SEH => "Wedge 1:4 SE-H",
            CellShape::Wedge4SEV => "Wedge 1:4 SE-V",
            CellShape::Wedge4NEH => "Wedge 1:4 NE-H",
            CellShape::Wedge4NEV => "Wedge 1:4 NE-V",
            CellShape::Wedge4NWH => "Wedge 1:4 NW-H",
            CellShape::Wedge4NWV => "Wedge 1:4 NW-V",
            CellShape::StripN34 => "Strip N 3/4",
            CellShape::StripN12 => "Strip N 1/2",
            CellShape::StripN14 => "Strip N 1/4",
            CellShape::StripN18 => "Strip N 1/8",
            CellShape::StripS34 => "Strip S 3/4",
            CellShape::StripS12 => "Strip S 1/2",
            CellShape::StripS14 => "Strip S 1/4",
            CellShape::StripS18 => "Strip S 1/8",
            CellShape::StripE34 => "Strip E 3/4",
            CellShape::StripE12 => "Strip E 1/2",
            CellShape::StripE14 => "Strip E 1/4",
            CellShape::StripE18 => "Strip E 1/8",
            CellShape::StripW34 => "Strip W 3/4",
            CellShape::StripW12 => "Strip W 1/2",
            CellShape::StripW14 => "Strip W 1/4",
            CellShape::StripW18 => "Strip W 1/8",
        }
    }

    /// R63 — reflect the shape across the VERTICAL axis (swap East↔West). Used by the editor's mirror
    /// tool so a mirrored cell's geometry flips correctly. An involution (`mirror_x ∘ mirror_x = id`).
    pub fn mirror_x(self) -> CellShape {
        use CellShape::*;
        match self {
            Full => Full,
            Octagon => Octagon,
            HalfSW => HalfSE,
            HalfSE => HalfSW,
            HalfNE => HalfNW,
            HalfNW => HalfNE,
            QuarterSW => QuarterSE,
            QuarterSE => QuarterSW,
            QuarterNE => QuarterNW,
            QuarterNW => QuarterNE,
            ChamferSW => ChamferSE,
            ChamferSE => ChamferSW,
            ChamferNE => ChamferNW,
            ChamferNW => ChamferNE,
            SlopeSWH => SlopeSEH,
            SlopeSEH => SlopeSWH,
            SlopeSWV => SlopeSEV,
            SlopeSEV => SlopeSWV,
            SlopeNEH => SlopeNWH,
            SlopeNWH => SlopeNEH,
            SlopeNEV => SlopeNWV,
            SlopeNWV => SlopeNEV,
            Slope3SWH => Slope3SEH,
            Slope3SEH => Slope3SWH,
            Slope3SWV => Slope3SEV,
            Slope3SEV => Slope3SWV,
            Slope3NEH => Slope3NWH,
            Slope3NWH => Slope3NEH,
            Slope3NEV => Slope3NWV,
            Slope3NWV => Slope3NEV,
            Slope4SWH => Slope4SEH,
            Slope4SEH => Slope4SWH,
            Slope4SWV => Slope4SEV,
            Slope4SEV => Slope4SWV,
            Slope4NEH => Slope4NWH,
            Slope4NWH => Slope4NEH,
            Slope4NEV => Slope4NWV,
            Slope4NWV => Slope4NEV,
            PointN => PointN,
            PointS => PointS,
            PointE => PointW,
            PointW => PointE,
            RoundN => RoundN,
            RoundS => RoundS,
            RoundE => RoundW,
            RoundW => RoundE,
            // R64 wedges mirror like slopes (SW↔SE, NE↔NW per H/V); strips: N/S fixed, E↔W.
            Wedge2SWH => Wedge2SEH,
            Wedge2SEH => Wedge2SWH,
            Wedge2SWV => Wedge2SEV,
            Wedge2SEV => Wedge2SWV,
            Wedge2NEH => Wedge2NWH,
            Wedge2NWH => Wedge2NEH,
            Wedge2NEV => Wedge2NWV,
            Wedge2NWV => Wedge2NEV,
            Wedge3SWH => Wedge3SEH,
            Wedge3SEH => Wedge3SWH,
            Wedge3SWV => Wedge3SEV,
            Wedge3SEV => Wedge3SWV,
            Wedge3NEH => Wedge3NWH,
            Wedge3NWH => Wedge3NEH,
            Wedge3NEV => Wedge3NWV,
            Wedge3NWV => Wedge3NEV,
            Wedge4SWH => Wedge4SEH,
            Wedge4SEH => Wedge4SWH,
            Wedge4SWV => Wedge4SEV,
            Wedge4SEV => Wedge4SWV,
            Wedge4NEH => Wedge4NWH,
            Wedge4NWH => Wedge4NEH,
            Wedge4NEV => Wedge4NWV,
            Wedge4NWV => Wedge4NEV,
            StripN34 => StripN34,
            StripN12 => StripN12,
            StripN14 => StripN14,
            StripN18 => StripN18,
            StripS34 => StripS34,
            StripS12 => StripS12,
            StripS14 => StripS14,
            StripS18 => StripS18,
            StripE34 => StripW34,
            StripW34 => StripE34,
            StripE12 => StripW12,
            StripW12 => StripE12,
            StripE14 => StripW14,
            StripW14 => StripE14,
            StripE18 => StripW18,
            StripW18 => StripE18,
        }
    }

    /// R63 — rotate the shape 90° CLOCKWISE (corner NE→SE→SW→NW→NE; slope long-leg axis H↔V;
    /// Point/Round N→E→S→W). Used to orient stamps by direction. `rotate_cw` applied 4× is the identity.
    pub fn rotate_cw(self) -> CellShape {
        use CellShape::*;
        match self {
            Full => Full,
            Octagon => Octagon,
            HalfNE => HalfSE,
            HalfSE => HalfSW,
            HalfSW => HalfNW,
            HalfNW => HalfNE,
            QuarterNE => QuarterSE,
            QuarterSE => QuarterSW,
            QuarterSW => QuarterNW,
            QuarterNW => QuarterNE,
            ChamferNE => ChamferSE,
            ChamferSE => ChamferSW,
            ChamferSW => ChamferNW,
            ChamferNW => ChamferNE,
            // Slope: corner rotates CW AND the long-leg axis swaps (H↔V).
            SlopeNEH => SlopeSEV,
            SlopeSEV => SlopeSWH,
            SlopeSWH => SlopeNWV,
            SlopeNWV => SlopeNEH,
            SlopeSEH => SlopeSWV,
            SlopeSWV => SlopeNWH,
            SlopeNWH => SlopeNEV,
            SlopeNEV => SlopeSEH,
            Slope3NEH => Slope3SEV,
            Slope3SEV => Slope3SWH,
            Slope3SWH => Slope3NWV,
            Slope3NWV => Slope3NEH,
            Slope3SEH => Slope3SWV,
            Slope3SWV => Slope3NWH,
            Slope3NWH => Slope3NEV,
            Slope3NEV => Slope3SEH,
            Slope4NEH => Slope4SEV,
            Slope4SEV => Slope4SWH,
            Slope4SWH => Slope4NWV,
            Slope4NWV => Slope4NEH,
            Slope4SEH => Slope4SWV,
            Slope4SWV => Slope4NWH,
            Slope4NWH => Slope4NEV,
            Slope4NEV => Slope4SEH,
            PointN => PointE,
            PointE => PointS,
            PointS => PointW,
            PointW => PointN,
            RoundN => RoundE,
            RoundE => RoundS,
            RoundS => RoundW,
            RoundW => RoundN,
            // R64 wedges rotate like slopes (corner CW + H↔V); strips: N→E→S→W.
            Wedge2NEH => Wedge2SEV,
            Wedge2SEV => Wedge2SWH,
            Wedge2SWH => Wedge2NWV,
            Wedge2NWV => Wedge2NEH,
            Wedge2SEH => Wedge2SWV,
            Wedge2SWV => Wedge2NWH,
            Wedge2NWH => Wedge2NEV,
            Wedge2NEV => Wedge2SEH,
            Wedge3NEH => Wedge3SEV,
            Wedge3SEV => Wedge3SWH,
            Wedge3SWH => Wedge3NWV,
            Wedge3NWV => Wedge3NEH,
            Wedge3SEH => Wedge3SWV,
            Wedge3SWV => Wedge3NWH,
            Wedge3NWH => Wedge3NEV,
            Wedge3NEV => Wedge3SEH,
            Wedge4NEH => Wedge4SEV,
            Wedge4SEV => Wedge4SWH,
            Wedge4SWH => Wedge4NWV,
            Wedge4NWV => Wedge4NEH,
            Wedge4SEH => Wedge4SWV,
            Wedge4SWV => Wedge4NWH,
            Wedge4NWH => Wedge4NEV,
            Wedge4NEV => Wedge4SEH,
            StripN34 => StripE34,
            StripE34 => StripS34,
            StripS34 => StripW34,
            StripW34 => StripN34,
            StripN12 => StripE12,
            StripE12 => StripS12,
            StripS12 => StripW12,
            StripW12 => StripN12,
            StripN14 => StripE14,
            StripE14 => StripS14,
            StripS14 => StripW14,
            StripW14 => StripN14,
            StripN18 => StripE18,
            StripE18 => StripS18,
            StripS18 => StripW18,
            StripW18 => StripN18,
        }
    }
}

/// R59 — the area-weighted centroid of a (CCW) convex polygon, via the shoelace formula
/// `C = (1/(3·Σcross))·Σ (P_i+P_{i+1})·cross_i`. Used for the chamfer/slope shapes whose corner mean is
/// not their centroid. Falls back to the corner mean for a degenerate (zero-area) ring.
fn polygon_centroid(pts: &[Vec2]) -> Vec2 {
    let n = pts.len();
    let mut cross_sum = 0.0_f32;
    let mut acc = Vec2::ZERO;
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        cross_sum += cross;
        acc += (p + q) * cross;
    }
    if cross_sum.abs() < 1.0e-12 {
        return pts.iter().fold(Vec2::ZERO, |a, &p| a + p) / n as f32;
    }
    acc / (3.0 * cross_sum)
}

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
    /// R58 — the cell's sub-shape (full square or a corner triangle). `#[serde(default)]` → old RON
    /// without it loads as `Full`. The sim treats `Full` byte-identically to before.
    #[serde(default)]
    pub shape: CellShape,
    /// R66 — the cell's **hull (structural) material** id, into the [`CellMaterials`](super::materials::CellMaterials)
    /// `hull` catalog (only meaningful for a structural cell). `0` = Standard (the live
    /// `struct_cell_*` globals → byte-identical). `#[serde(default)]` → old RON loads as `0`.
    #[serde(default)]
    pub hull_material: u8,
    /// R66 — the cell's **armor material** id, into the [`CellMaterials`](super::materials::CellMaterials)
    /// `armor` catalog (plating on top of any cell). `0` = None (no plate → byte-identical).
    /// `#[serde(default)]` → old RON loads as `0`.
    #[serde(default)]
    pub armor_material: u8,
}

impl GridCell {
    /// Construct a **module cell** at `coord` in `section` (on a slot/hardpoint). The
    /// historical two-arg constructor: a slot cell is a non-structural module cell.
    pub const fn new(coord: (u16, u16), section: SectionId) -> Self {
        Self {
            coord,
            section,
            structural: false,
            shape: CellShape::Full,
            hull_material: 0,
            armor_material: 0,
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
            shape: CellShape::Full,
            hull_material: 0,
            armor_material: 0,
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

/// Build a procedural **station hull** (Refinement 5/7) for the mining structures: a **solid filled**
/// `cols × rows` silhouette of structural cells (no modules/slots), so it reads as a coherent block
/// that matches the pre-voxelize box (the first-shot conversion is seamless) and you carve holes into
/// it. Since [`cell_depth`](crate::fitting::layout) is distance-to-nearest-edge, the CENTRE cell is
/// the deepest = the carve-to-core death point. World size is `grid · CELL_WORLD_SIZE`; the carve
/// pipeline seeds each cell with the structural-cell HP.
pub fn station_hull(id: HullId, name: &str, cols: u16, rows: u16) -> Hull {
    let mut cells = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            cells.push(GridCell::structural((col, row), SectionId(1)));
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

/// Build a procedural **disc station hull** (Refinement 11) for the carveable central rock: a
/// `diameter × diameter` grid filled to a **circle** (cells within `diameter/2` of the centre), so a
/// voxelized asteroid reads ROUND, not a square block. Like [`station_hull`] the cells are all
/// structural; the centre cell is the deepest ([`cell_depth`](crate::fitting::layout)) = the
/// carve-to-core death point. World diameter is `diameter · CELL_WORLD_SIZE`.
pub fn disc_hull(id: HullId, name: &str, diameter: u16) -> Hull {
    let d = diameter.max(1);
    let r = d as f32 * 0.5;
    let c = (d as f32 - 1.0) * 0.5; // grid centre (cell coords)
    let mut cells = Vec::new();
    for row in 0..d {
        for col in 0..d {
            let dx = col as f32 - c;
            let dy = row as f32 - c;
            if dx * dx + dy * dy <= r * r {
                cells.push(GridCell::structural((col, row), SectionId(1)));
            }
        }
    }
    Hull {
        id,
        name: name.to_string(),
        class: ShipClass::Station,
        role: ShipRole::Utility,
        grid_dims: (d, d),
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

    /// R58/R59 — every `CellShape`'s polygon is CCW, its shoelace area equals its `area_factor` (the mass
    /// weight), and its `centroid` lands inside the polygon. Guards the chamfer/slope corner lists.
    #[test]
    fn cell_shape_polygons_are_consistent() {
        for s in CellShape::ALL {
            let pts = s.corners(2, 3);
            let n = pts.len();
            // Shoelace signed area: CCW ⇒ positive, and its magnitude is the cell-area fraction.
            let a2: f32 = (0..n)
                .map(|i| {
                    let (p, q) = (pts[i], pts[(i + 1) % n]);
                    p.x * q.y - q.x * p.y
                })
                .sum();
            let area = a2 / 2.0;
            assert!(area > 0.0, "{s:?}: corners must be CCW (area {area})");
            assert!(
                (area - s.area_factor()).abs() < 1.0e-5,
                "{s:?}: shoelace area {area} != area_factor {}",
                s.area_factor()
            );
            // The centroid must be inside the convex polygon (left of every CCW edge).
            let c = s.centroid(2, 3);
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
                assert!(
                    cross > -1.0e-5,
                    "{s:?}: centroid {c:?} outside edge {a:?}->{b:?}"
                );
            }
        }
    }

    /// R63 — `mirror_x` reflects the polygon across `x = c+0.5` (and is an involution); `rotate_cw`
    /// rotates it 90° CW about the cell centre (4× = identity). Both preserve area. Verified
    /// GEOMETRICALLY by comparing the transformed corner SET to the actual reflected/rotated corners.
    #[test]
    fn cell_shape_mirror_and_rotate_are_geometric() {
        // A rounded multiset key for a Vec<Vec2> (order/winding-independent vertex comparison).
        let key = |pts: &[Vec2]| {
            let mut v: Vec<(i32, i32)> = pts
                .iter()
                .map(|p| ((p.x * 48.0).round() as i32, (p.y * 48.0).round() as i32))
                .collect();
            v.sort_unstable();
            v
        };
        let (c, r) = (2u16, 3u16);
        let (cx, cy) = (c as f32 + 0.5, r as f32 + 0.5);
        for s in CellShape::ALL {
            // mirror_x: reflect each corner about x = cx.
            let reflected: Vec<Vec2> = s
                .corners(c, r)
                .iter()
                .map(|p| Vec2::new(2.0 * cx - p.x, p.y))
                .collect();
            assert_eq!(
                key(&reflected),
                key(&s.mirror_x().corners(c, r)),
                "{s:?}: mirror_x polygon mismatch"
            );
            assert_eq!(
                s.mirror_x().mirror_x(),
                s,
                "{s:?}: mirror_x not an involution"
            );
            assert!((s.mirror_x().area_factor() - s.area_factor()).abs() < 1.0e-6);

            // rotate_cw: (x,y) → (cx + (y-cy), cy - (x-cx)) about the cell centre.
            let rotated: Vec<Vec2> = s
                .corners(c, r)
                .iter()
                .map(|p| Vec2::new(cx + (p.y - cy), cy - (p.x - cx)))
                .collect();
            assert_eq!(
                key(&rotated),
                key(&s.rotate_cw().corners(c, r)),
                "{s:?}: rotate_cw polygon mismatch"
            );
            assert_eq!(
                s.rotate_cw().rotate_cw().rotate_cw().rotate_cw(),
                s,
                "{s:?}: rotate_cw 4× != identity"
            );
            assert!((s.rotate_cw().area_factor() - s.area_factor()).abs() < 1.0e-6);
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
