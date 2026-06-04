//! Scene setup (FR-001/FR-008/FR-012), networkized in E003 OBJ4.
//!
//! The scene spawns **only** the locally-owned, render-bound entities: the
//! lighting, the gunsight pip, and the LOCAL player ship. The gameplay **targets**
//! (dummies, asteroids, seeker) and projectiles are not spawned here — they are
//! authoritative on the embedded server ([`server::ServerApp::spawn_demo_world`])
//! and are rendered by reading the server world directly each tick
//! ([`crate::net::capture_render_state`], which find-or-spawns a mesh-bearing Bevy
//! entity per authoritative entity). This binds the render world to the world that
//! actually steps (Principle I).
//!
//! [`RenderAssets`] carries the mesh/material handles for ships, the per-kind
//! targets, and projectiles, so [`crate::net::capture_render_state`] can spawn each
//! rendered entity with the right look by [`protocol::EntityKind`] (+ the target
//! sub-kind in `flags`).

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use sim::components::{FlightAssist, Health, Ship, Velocity};

use crate::net::LocalShip;
use crate::render_sync::{AimPip, RenderInterp};
use sim::ShipIntent;

/// Render assets reused for the entities [`crate::net::capture_render_state`]
/// spawns from the server world: projectiles, **ships**, and per-kind **targets**,
/// keyed on [`protocol::EntityKind`] (+ the target sub-kind in `flags`).
#[derive(Resource)]
pub struct RenderAssets {
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
    /// Mesh/material for a ship (other players / AI ships). Matches the E002
    /// player-ship look so any rendered ship reads identically to the local one.
    pub ship_mesh: Handle<Mesh>,
    pub ship_material: Handle<StandardMaterial>,
    /// Per-`TargetKind` looks, picked by the server render entity's `flags` in
    /// [`crate::net::capture_render_state`] (the wire `EntityKind` only says
    /// "Target"): reddish dummy cube, grey asteroid sphere, green seeker dart —
    /// matching the E002 scene.
    pub dummy_mesh: Handle<Mesh>,
    pub dummy_material: Handle<StandardMaterial>,
    pub asteroid_mesh: Handle<Mesh>,
    pub asteroid_material: Handle<StandardMaterial>,
    pub seeker_mesh: Handle<Mesh>,
    pub seeker_material: Handle<StandardMaterial>,
    /// Localized shield-impact flash (FIX 0a refinement): a glowing cyan **arc segment
    /// of the shield ring** ([`build_arc_band_mesh`]) spawned once as a child of a
    /// rendered ship and **rotated** about Z so the lit slice faces the bullet impact
    /// bearing (`hit_dir`), shown ONLY for the split-second a shot strikes the still-up
    /// shield (`shield_flash > 0`), its alpha fading with the flash. This REPLACES the
    /// earlier small impact-point sphere (a flat ribbon reads as the deflector ring
    /// flaring, not a stray dot), which itself REPLACED the old full-ship bubble — the
    /// user disliked the whole-ship bloom.
    pub shield_arc_mesh: Handle<Mesh>,
    pub shield_material: Handle<StandardMaterial>,
    /// Ship-fragment debris (FIX 0b): a small irregular box + a darkened, desaturated
    /// ship-faction-tinted "metal fragment" material (clearly a ship piece, not a grey
    /// rock). Used for [`protocol::EntityKind::Debris`] chunks, scaled by the per-chunk
    /// size hint and given a deterministic per-id base rotation so fragments tumble and
    /// do not all align.
    pub debris_mesh: Handle<Mesh>,
    pub debris_material: Handle<StandardMaterial>,
    /// Revise-B seamless hull surface: the ONE uniform hull-plate material every near
    /// fitted ship's merged hull mesh ([`build_hull_mesh`]) uses. A solid metallic
    /// steel-blue/grey, normal-lit (NOT emissive) — so an undamaged ship reads as one
    /// continuous solid plate with NO visible cells or grid lines. Module colors are
    /// deliberately HIDDEN at this material (a fitted ship's per-cell `kind` is not used
    /// here); Phase 2 will reveal an exposed module cell at a breach by tinting only its
    /// quad (see [`build_hull_mesh`]'s `exposed` hook). Shared across all near ships (the
    /// per-ship variation is the geometry, not the material).
    pub hull_material: Handle<StandardMaterial>,
    /// The wreck hull plate material — the same metallic hull material but tinted with the
    /// darkened/desaturated [`WRECK_HULL_COLOR`] ("dead metal"). A severed chunk's / dead
    /// hulk's hull mesh ([`build_hull_mesh`]) wears it so a broken piece reads as debris
    /// (not a live ship) while keeping the real cell shape/size/scale.
    pub wreck_hull_material: Handle<StandardMaterial>,
}

/// Hull cell size, in sim units — the side length of one hull cell as laid out in the
/// merged hull surface mesh of a near fitted ship ([`build_hull_mesh`]).
///
/// **Shared scale (FIX carve location):** this is re-exported from the sim's
/// authoritative [`sim::fitting::CELL_WORLD_SIZE`] so the client render and the sim's
/// collision/carve geometry (the swept hit circle + the impact→cell-space carve
/// mapping) are in the SAME scale. If the cell size is ever retuned, change it in the
/// sim (`crates/sim/src/fitting/hull.rs`) and it propagates here automatically. Value
/// `0.32`: the old single fighter box was `1.6` wide on the legacy 5-wide grid, so
/// `1.6 / 5 = 0.32` keeps the silhouette the same physical size on the finer dense
/// grids (51-cell fighter on 9×11) while giving a crisper outline.
pub const CELL_SIZE: f32 = sim::fitting::CELL_WORLD_SIZE;

/// Revise-B: the uniform solid hull color — a metallic steel-blue/grey. Used by the ONE
/// shared [`RenderAssets::hull_material`]; the merged hull mesh of every near fitted ship
/// wears it so an undamaged ship reads as a single solid plate with no visible cells.
/// Module colors are HIDDEN at this surface (revealed only at a breach in Phase 2).
pub const HULL_COLOR: Color = Color::srgb(0.30, 0.40, 0.52);

/// The "dead metal" wreck tint — a darkened, desaturated [`HULL_COLOR`] (≈60% brightness)
/// the client wears on a severed chunk's / destroyed hulk's hull mesh so a broken piece
/// reads as debris rather than a live ship, while keeping the real cell shape/size/scale.
/// Used by [`RenderAssets::wreck_hull_material`].
pub const WRECK_HULL_COLOR: Color = Color::srgb(0.18, 0.24, 0.31);

/// Revise-B: the merged hull surface's slab half-thickness in `+Z`, in sim units — the
/// top face sits at `z = HULL_THICKNESS` so the plate has a touch of relief under the
/// top-down light without looking like a flat decal. Small (the camera is top-down, so
/// only the top face is normally seen); the side walls at the silhouette boundary give a
/// thin lip. Tunable for feel.
const HULL_THICKNESS: f32 = 0.1;

/// Inner radius **fraction** of the shield-impact arc band — the near edge of the
/// glowing ring slice as a fraction of the (normalized) outer radius `1.0`. The mesh is
/// built normalized to outer radius `1.0` so it can be **scaled per ship** to hug any
/// hull (see [`crate::net::shield_radius_for`] / [`SHIELD_MARGIN`]); a `0.80` inner
/// fraction makes a band `0.80..1.0` of the radius — a slim crescent, not a fat ring.
/// Tunable for feel.
const SHIELD_ARC_INNER_FRAC: f32 = 0.80;
/// Half-angle of the shield-impact arc band, in radians (≈48°) — the arc spans
/// `[-SHIELD_ARC_HALF_ANGLE, +SHIELD_ARC_HALF_ANGLE]` about its centre bearing, so the
/// lit slice covers ~96° of the ring. Wider than the old hard-cut 80° because the ends
/// now taper smoothly to zero alpha (the vertex-color crescent), so the *visible* glow
/// reads narrower than the geometry. Tunable for feel.
const SHIELD_ARC_HALF_ANGLE: f32 = std::f32::consts::PI * 48.0 / 180.0;
/// Number of angular segments across the shield-impact arc band — more segments give a
/// smoother curve AND a smoother angular alpha taper (the per-vertex crescent gradient is
/// sampled at each segment boundary), at the cost of more triangles. Bumped from 12 so
/// the cosine taper reads smooth. Tunable for feel.
const SHIELD_ARC_SEGMENTS: u32 = 24;

/// Vertex-color tuning for the sleek shield crescent (FIX 0a polish). The arc carries a
/// per-vertex [`Mesh::ATTRIBUTE_COLOR`] (linear RGBA, premultiplied-feel for the additive
/// blend) that shapes the glow WITHOUT extra geometry:
///
/// - **Angular taper (the crescent):** alpha follows a raised-cosine bell across
///   `[-half_angle, +half_angle]` — `1.0` at the centre bearing, smoothly `0.0` at both
///   ends. This kills the old hard rectangular cut so the slice reads as a soft crescent
///   flare. [`SHIELD_TAPER_POWER`] sharpens (`>1`) or softens (`<1`) the bell.
/// - **Radial gradient:** brightness rises from the inner edge toward the outer (deflector
///   surface) edge via [`SHIELD_INNER_DIM`]..`1.0`, so the energy looks like it skins the
///   outside of the bubble.
/// - **White-hot core → cool blue rim:** the bright angular centre is tinted toward
///   [`SHIELD_CORE_COLOR`] (near-white cyan) and cools to [`SHIELD_RIM_COLOR`] (saturated
///   blue) toward the angular ends, mixed by the same bell. The whole thing is then driven
///   by `shield_flash` at draw time (material `base_color` alpha), so it still fades over
///   the ~0.25 s window.
///
/// All values are linear-space (the mesh stores `ATTRIBUTE_COLOR` linearly); the additive
/// material multiplies them by its `base_color`, so keep peaks moderate (≈`0.8`) — pure
/// `1.0` everywhere blows the additive blend out to white.
const SHIELD_CORE_COLOR: [f32; 3] = [0.78, 0.95, 1.0];
/// Cool blue the crescent rim/ends cool to (linear). See [`SHIELD_CORE_COLOR`].
const SHIELD_RIM_COLOR: [f32; 3] = [0.10, 0.45, 0.95];
/// Inner-edge brightness multiplier (`0..1`) for the radial gradient; the outer edge is
/// full `1.0`. See [`build_arc_band_mesh`].
const SHIELD_INNER_DIM: f32 = 0.35;
/// Exponent shaping the angular raised-cosine taper — `>1` sharpens the crescent to a
/// tighter central bloom, `<1` spreads it. See [`build_arc_band_mesh`].
const SHIELD_TAPER_POWER: f32 = 1.4;
/// Peak per-vertex alpha at the crescent centre/outer edge (linear). Kept below `1.0` so
/// the additive blend reads as a crisp cyan flare with a white-hot core rather than a
/// blown-out white blob. See [`build_arc_band_mesh`].
const SHIELD_PEAK_ALPHA: f32 = 0.85;

/// Build a flat **annular sliver** (an arc segment of a ring) lying in the **XY plane**
/// (`z = 0`), centred on the **+X axis** and spanning `[-half_angle, +half_angle]`
/// (FIX 0a polish — the sleek shield-impact crescent mesh).
///
/// **Normalized.** `inner_frac` is the inner radius as a fraction of the OUTER radius,
/// which is fixed at `1.0` — so the mesh is unit-sized and the caller applies a per-ship
/// uniform **scale** to make the band hug any hull (see [`crate::net::shield_radius_for`]).
/// For each of `segments + 1` angular steps at angle `a` it emits two vertices — an inner
/// `(inner_frac·cos a, inner_frac·sin a, 0)` and an outer `(cos a, sin a, 0)` — and
/// stitches consecutive inner/outer pairs into a [`PrimitiveTopology::TriangleList`] (two
/// triangles per quad). Every normal is `+Z` (`[0, 0, 1]`) so the ribbon faces the
/// top-down camera (which looks down `-Z` onto the XY plane), and each vertex carries a
/// simple UV so [`StandardMaterial`] is satisfied. Triangles are wound CCW as seen from
/// `+Z`; the shield material additionally sets `cull_mode: None` + `double_sided: true` so
/// the slice is never culled regardless of winding.
///
/// **Vertex colors shape the look** (no extra geometry) — angular cosine taper to a soft
/// crescent, a radial inner→outer gradient, and a white-hot core cooling to a blue rim. See
/// the [`SHIELD_CORE_COLOR`] doc block for the exact gradient terms.
///
/// The caller rotates the resulting mesh about Z to aim the centre bearing at the impact
/// direction and scales it to the ship's shield radius (see
/// [`crate::net::update_shield_bubble`]).
fn build_arc_band_mesh(inner_frac: f32, half_angle: f32, segments: u32) -> Mesh {
    let segments = segments.max(1);
    let step_count = segments + 1;
    let inner_r = inner_frac.clamp(0.0, 0.999);
    let outer_r = 1.0_f32;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(step_count as usize * 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(step_count as usize * 2);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(step_count as usize * 2);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(step_count as usize * 2);

    for i in 0..step_count {
        let t = i as f32 / segments as f32;
        let a = -half_angle + t * (2.0 * half_angle);
        let (sin, cos) = a.sin_cos();

        // Inner then outer vertex for this angular step.
        positions.push([inner_r * cos, inner_r * sin, 0.0]);
        positions.push([outer_r * cos, outer_r * sin, 0.0]);
        normals.push([0.0, 0.0, 1.0]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([t, 0.0]);
        uvs.push([t, 1.0]);

        // Angular taper: raised-cosine bell, 1.0 at centre (t = 0.5) → 0.0 at the ends.
        // `cos²(π·(t−0.5))` is the smooth bell; powering it sharpens/softens the crescent.
        let bell = (std::f32::consts::PI * (t - 0.5))
            .cos()
            .powf(2.0 * SHIELD_TAPER_POWER);

        // White-hot core (high bell) cooling to a blue rim (low bell), mixed by the bell.
        let mix = |hot: f32, cool: f32| cool + (hot - cool) * bell;
        let r = mix(SHIELD_CORE_COLOR[0], SHIELD_RIM_COLOR[0]);
        let g = mix(SHIELD_CORE_COLOR[1], SHIELD_RIM_COLOR[1]);
        let b = mix(SHIELD_CORE_COLOR[2], SHIELD_RIM_COLOR[2]);

        // Radial gradient: dim at the inner edge, full at the outer (deflector) edge.
        let inner_a = SHIELD_PEAK_ALPHA * bell * SHIELD_INNER_DIM;
        let outer_a = SHIELD_PEAK_ALPHA * bell;
        colors.push([r, g, b, inner_a]);
        colors.push([r, g, b, outer_a]);
    }

    // Two triangles per quad between angular steps i and i+1. Vertex layout per step:
    // even index = inner, odd index = outer. Wound CCW as seen from +Z.
    let mut indices: Vec<u32> = Vec::with_capacity(segments as usize * 6);
    for i in 0..segments {
        let inner0 = i * 2;
        let outer0 = inner0 + 1;
        let inner1 = inner0 + 2;
        let outer1 = inner0 + 3;
        indices.extend_from_slice(&[inner0, outer0, outer1]);
        indices.extend_from_slice(&[inner0, outer1, inner1]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices))
}

/// Revise-B: build ONE merged **seamless solid hull surface** mesh for a fitted ship
/// from its present cells, in the ship's LOCAL frame (XY plane), so the whole ship draws
/// as a single mesh + the single [`RenderAssets::hull_material`].
///
/// **Seamless, not voxels.** This REPLACES the old per-cell-box rendering (one `Cuboid`
/// child per cell). Each present cell emits a **gapless, coplanar** quad covering the
/// FULL cell footprint (a `CELL_SIZE × CELL_SIZE` square, no inter-cell gap) at the
/// cell's local position. The local position matches the existing cell-offset convention
/// (so the nose still points `+X`): the hull silhouettes author **forward = +row** /
/// **lateral = +col**, but the ship's local nose is `+X`, so `row` maps to the forward
/// (`+X`) axis and `col` to the lateral (`+Y`) axis, measured from the `center` origin
/// (cell-space; a ship passes the grid centre, so this is the classic grid-centred layout):
///   `cx = ((row + 0.5) − center.y)·CELL_SIZE`  (forward, +X)
///   `cy = ((col + 0.5) − center.x)·CELL_SIZE`  (lateral, +Y)
/// Adjacent cells therefore share an exact edge; since every quad is the same material,
/// coplanar at `z = HULL_THICKNESS`, and `+Z` (camera-facing) — from the top-down camera
/// the union reads as one continuous solid plate with NO internal seams or grid lines.
///
/// **Thickness.** The top face sits at `z = HULL_THICKNESS` (slight relief under the
/// top-down light, never a flat decal). Each cell also emits **boundary side walls**:
/// for each of the cell's four edges that has NO present neighbour (a silhouette edge or
/// a Phase-2 breach edge) a vertical quad is dropped from `z = HULL_THICKNESS` to
/// `z = 0`, giving the plate a thin lip and giving carved holes real walls. Interior
/// shared edges emit no wall (they are covered), so the surface stays gapless and cheap.
/// The top-down camera mainly sees the top faces; the walls matter when the hull is
/// carved (Phase 2).
///
/// **Phase-2 reveal hook.** The signature already carries each cell's `kind` and an
/// `exposed` predicate so a later phase can color a breach-exposed module cell
/// differently. For revise-B the mesh is geometry-only (single uniform material, no
/// per-cell color, modules HIDDEN), so `kind`/`exposed` are accepted but unused beyond
/// this hook; when Phase 2 lands, an exposed module cell's top quad can be split into a
/// second submesh / vertex-color set so [`RenderAssets::hull_material`] stays the body
/// plate and only the exposed cell shows its module hue. Documented at the call site too.
///
/// `cells` is the present cell list (`(col, row, kind)`); `cell_size` is [`CELL_SIZE`]. An
/// empty `cells` yields an empty mesh (the caller never voxelizes a non-fitted entity, so
/// this is defensive).
///
/// `center` is the **cell-space** point (in `(col, row)` units) the cells are laid out
/// around: each cell `c` renders at the swap+scale of `(c − center)`. A whole ship passes
/// the **grid centre** `(cols·0.5, rows·0.5)` — its `Position` sits at the grid centre, so
/// the silhouette is centred on the ship (this keeps ship rendering byte-identical to the
/// old `(rows·0.5, cols·0.5)`-baked-in behaviour). A **severed chunk** passes its own
/// **cell-COM** (the mean of just its cells) — its `Position` is the chunk COM in world,
/// so its cells render around that point, sitting exactly where the chunk drifted to.
/// (The carrier `grid_dims` is no longer needed here — `center` fully determines the
/// layout — so the caller derives `center` from `grid_dims` (ship) or the cells (chunk).)
pub fn build_hull_mesh(cells: &[(u16, u16, u8)], cell_size: f32, center: Vec2) -> Mesh {
    // Phase-2 reveal hook: with no breach model yet, no cell is ever exposed, so the
    // whole surface uses the uniform hull material. Phase 2 replaces this with a real
    // breach predicate and per-exposed-cell coloring.
    let exposed = |_col: u16, _row: u16| -> bool { false };
    build_hull_mesh_with(cells, cell_size, center, exposed)
}

/// [`build_hull_mesh`] with an explicit `exposed(col, row)` predicate — the Phase-2
/// reveal seam. Today `exposed` is always `false` (modules hidden), so the merged
/// surface is one uniform-material solid plate; the parameter exists so a breach phase
/// can flag exposed module cells without changing this mesh-construction code. (The
/// `_exposed` flag is threaded but not yet branched on — Phase 2 will emit a distinct
/// vertex attribute / submesh for exposed cells.)
///
/// `center` is the cell-space layout origin (see [`build_hull_mesh`]): each cell renders
/// at the swap+scale of `(c − center)`. The carrier `grid_dims` is no longer needed here
/// — `center` fully determines where the cells sit — so the silhouette is laid out
/// identically for a whole ship (center = grid centre) and a severed chunk (center = its
/// own cell-COM).
fn build_hull_mesh_with(
    cells: &[(u16, u16, u8)],
    cell_size: f32,
    center: Vec2,
    exposed: impl Fn(u16, u16) -> bool,
) -> Mesh {
    let half = HULL_THICKNESS;

    // Fast membership test for neighbour lookups (so shared interior edges emit no wall
    // and the plate stays gapless). Keyed by `(col, row)`.
    let present: std::collections::HashSet<(u16, u16)> =
        cells.iter().map(|&(c, r, _)| (c, r)).collect();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit one CCW-from-`+Z` quad (two triangles) given its four corner positions, a
    // shared normal, and simple UVs. Corners are ordered v0→v1→v2→v3 counter-clockwise
    // as seen from the side the normal points to.
    let push_quad = |positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     indices: &mut Vec<u32>,
                     corners: [[f32; 3]; 4],
                     normal: [f32; 3]| {
        let base = positions.len() as u32;
        positions.extend_from_slice(&corners);
        for _ in 0..4 {
            normals.push(normal);
        }
        uvs.push([0.0, 0.0]);
        uvs.push([1.0, 0.0]);
        uvs.push([1.0, 1.0]);
        uvs.push([0.0, 1.0]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    for &(col, row, _kind) in cells {
        // Phase-2 hook (no-op today): a future breach phase tints `exposed` cells; for
        // revise-B every cell is body plate (the predicate is always false).
        let _exposed = exposed(col, row);

        // Cell centre in the local frame: row→forward(+X), col→lateral(+Y), measured
        // from `center` (cell-space): `center.y` is the row origin (forward), `center.x`
        // the col origin (lateral). A ship passes the grid centre `(cols·0.5, rows·0.5)`,
        // so this is identical to the old `rows·0.5`/`cols·0.5` baked-in centring; a
        // severed chunk passes its cell-COM so its cells sit around the chunk `Position`.
        let cx = ((row as f32 + 0.5) - center.y) * cell_size;
        let cy = ((col as f32 + 0.5) - center.x) * cell_size;
        let h = cell_size * 0.5;
        // Cell footprint extents (gapless — full cell, no fill gap).
        let (x0, x1) = (cx - h, cx + h);
        let (y0, y1) = (cy - h, cy + h);

        // Top face at z = +HULL_THICKNESS, normal +Z, wound CCW seen from +Z (the
        // top-down camera). Coplanar + same material across all cells → seamless.
        push_quad(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut indices,
            [
                [x0, y0, half],
                [x1, y0, half],
                [x1, y1, half],
                [x0, y1, half],
            ],
            [0.0, 0.0, 1.0],
        );

        // Boundary side walls: only on edges with no present neighbour (silhouette edge,
        // or a Phase-2 carved-hole edge). Interior shared edges are covered → no wall, so
        // the surface stays gapless. Each wall drops from z=+half to z=0.
        // -X edge (toward a smaller row / aft). Neighbour is (col, row-1).
        let has_neg_x = row > 0 && present.contains(&(col, row - 1));
        if !has_neg_x {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [[x0, y0, 0.0], [x0, y1, 0.0], [x0, y1, half], [x0, y0, half]],
                [-1.0, 0.0, 0.0],
            );
        }
        // +X edge (toward a larger row / nose). Neighbour is (col, row+1).
        let has_pos_x = present.contains(&(col, row + 1));
        if !has_pos_x {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [[x1, y1, 0.0], [x1, y0, 0.0], [x1, y0, half], [x1, y1, half]],
                [1.0, 0.0, 0.0],
            );
        }
        // -Y edge (toward a smaller col). Neighbour is (col-1, row).
        let has_neg_y = col > 0 && present.contains(&(col - 1, row));
        if !has_neg_y {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [[x1, y0, 0.0], [x0, y0, 0.0], [x0, y0, half], [x1, y0, half]],
                [0.0, -1.0, 0.0],
            );
        }
        // +Y edge (toward a larger col). Neighbour is (col+1, row).
        let has_pos_y = present.contains(&(col + 1, row));
        if !has_pos_y {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [[x0, y1, 0.0], [x1, y1, 0.0], [x1, y1, half], [x0, y1, half]],
                [0.0, 1.0, 0.0],
            );
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

// ============================================================================
// Fix #11 M2 — smoothed marching-squares hull CONTOUR mesh (the "rounded look").
// An alternative to the blocky per-cell [`build_hull_mesh`], selectable at runtime via the
// `HullRenderMode` toggle (default OFF = the voxel mesh). It traces the cell set's boundary
// into grid-corner loop(s), rounds them with Chaikin corner-cutting, and fills the OUTER loop
// by ear-clipping — so a chunk reads as a rounded plate. First cut: flat top face only (the
// top-down camera sees it); interior holes are left filled and side walls are TODO (cosmetic).
// ============================================================================

/// Chaikin corner-cutting passes for the contour — more = rounder (and slightly smaller, since
/// each pass pulls corners inward). `2` is a moderate "rounded-voxel" look. Tunable for feel.
const CONTOUR_CHAIKIN_ITERS: u32 = 2;

/// 2× the signed area of a closed polygon (shoelace). Positive = CCW, negative = CW.
fn signed_area2(pts: &[Vec2]) -> f32 {
    let n = pts.len();
    let mut a = 0.0;
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        a += p.x * q.y - q.x * p.y;
    }
    a
}

/// Trace the boundary of a cell set into ordered grid-corner loops, CCW around the material in
/// `(col=x, row=y)` cell space (outer loops are CCW / positive area; holes are CW). Each present
/// cell contributes one CCW directed edge per side whose neighbour is ABSENT; the edges link
/// head-to-tail into closed loops. A boundary that pinches itself at a corner (rare for a
/// 4-connected chunk) may drop an edge there — acceptable for the cosmetic contour.
fn cell_boundary_loops(present: &std::collections::HashSet<(u16, u16)>) -> Vec<Vec<(i32, i32)>> {
    let is_present = |c: i32, r: i32| c >= 0 && r >= 0 && present.contains(&(c as u16, r as u16));
    // start corner -> end corner, one directed boundary edge per start.
    let mut next: std::collections::HashMap<(i32, i32), (i32, i32)> =
        std::collections::HashMap::new();
    for &(cu, ru) in present {
        let (c, r) = (cu as i32, ru as i32);
        // CCW around the cell (interior on the left): bottom, right, top, left edges.
        if !is_present(c, r - 1) {
            next.insert((c, r), (c + 1, r)); // bottom (-row side)
        }
        if !is_present(c + 1, r) {
            next.insert((c + 1, r), (c + 1, r + 1)); // right (+col side)
        }
        if !is_present(c, r + 1) {
            next.insert((c + 1, r + 1), (c, r + 1)); // top (+row side)
        }
        if !is_present(c - 1, r) {
            next.insert((c, r + 1), (c, r)); // left (-col side)
        }
    }
    // Link the directed edges into closed loops (deterministic start order).
    let mut starts: Vec<(i32, i32)> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut used: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
    let mut loops = Vec::new();
    for &start in &starts {
        if used.contains(&start) {
            continue;
        }
        let mut pts = Vec::new();
        let mut cur = start;
        while used.insert(cur) {
            pts.push(cur);
            match next.get(&cur) {
                Some(&nx) => cur = nx,
                None => break,
            }
            if cur == start {
                break;
            }
        }
        if pts.len() >= 3 {
            loops.push(pts);
        }
    }
    loops
}

/// One or more Chaikin corner-cutting passes on a CLOSED loop → rounded. Each pass replaces
/// every edge `a→b` with the points `0.75a+0.25b` and `0.25a+0.75b`, so corners get cut and
/// the loop smooths (and shrinks slightly toward its interior).
fn chaikin_closed(pts: &[Vec2], iterations: u32) -> Vec<Vec2> {
    let mut cur = pts.to_vec();
    for _ in 0..iterations {
        let n = cur.len();
        if n < 3 {
            break;
        }
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let a = cur[i];
            let b = cur[(i + 1) % n];
            out.push(a * 0.75 + b * 0.25);
            out.push(a * 0.25 + b * 0.75);
        }
        cur = out;
    }
    cur
}

/// Whether `p` is inside the CCW triangle `a,b,c`, **boundary included** (a point on an
/// edge counts as inside). Ear-clipping needs the inclusive test so a reflex vertex lying
/// exactly on a candidate ear's edge (the concave-notch case) still blocks the ear — a
/// strict test would let the ear poke across the notch and fill area outside the polygon.
fn point_in_tri(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    (b - a).perp_dot(p - a) >= 0.0
        && (c - b).perp_dot(p - b) >= 0.0
        && (a - c).perp_dot(p - c) >= 0.0
}

/// Ear-clipping triangulation of a simple **CCW** polygon → index triples into `poly`. Handles
/// convex and concave simple polygons (no holes). Bails gracefully on a degenerate/self-
/// intersecting input (returns whatever ears it found).
fn ear_clip(poly: &[Vec2]) -> Vec<u32> {
    let n = poly.len();
    let mut tris = Vec::new();
    if n < 3 {
        return tris;
    }
    let mut idx: Vec<usize> = (0..n).collect();
    let mut guard = 0usize;
    let max_iter = 4 * n;
    while idx.len() > 3 && guard < max_iter {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let i0 = idx[(i + m - 1) % m];
            let i1 = idx[i];
            let i2 = idx[(i + 1) % m];
            let (a, b, c) = (poly[i0], poly[i1], poly[i2]);
            // Convex (CCW) vertex?
            if (b - a).perp_dot(c - a) <= 0.0 {
                continue;
            }
            // A convex tip is a valid ear iff no REFLEX vertex of the remaining polygon
            // lies in the candidate triangle (boundary included — see `point_in_tri`).
            // Only reflex vertices can invalidate an ear; testing convex ones (which may
            // sit harmlessly on an edge) would spuriously block valid ears.
            let blocked = idx.iter().enumerate().any(|(k, &j)| {
                if j == i0 || j == i1 || j == i2 {
                    return false;
                }
                let jp = poly[idx[(k + m - 1) % m]];
                let jn = poly[idx[(k + 1) % m]];
                let reflex = (poly[j] - jp).perp_dot(jn - jp) <= 0.0;
                reflex && point_in_tri(poly[j], a, b, c)
            });
            if blocked {
                continue;
            }
            tris.extend_from_slice(&[i0 as u32, i1 as u32, i2 as u32]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break; // degenerate — bail with what we have
        }
    }
    if idx.len() == 3 {
        tris.extend_from_slice(&[idx[0] as u32, idx[1] as u32, idx[2] as u32]);
    }
    tris
}

/// Build the **smoothed contour** hull mesh for a cell set — the rounded-look alternative to
/// [`build_hull_mesh`] (Fix #11 M2). Same `(cells, cell_size, center)` contract and the same
/// local frame (`x = forward ← row`, `y = lateral ← col`, scaled by `cell_size`, measured from
/// `center`), so it drops in at the same parent transform. Traces the boundary, Chaikin-smooths
/// it, and fills the outer loop (flat top face at `z = HULL_THICKNESS`, normal `+Z`).
pub fn build_hull_mesh_contour(cells: &[(u16, u16, u8)], cell_size: f32, center: Vec2) -> Mesh {
    let present: std::collections::HashSet<(u16, u16)> =
        cells.iter().map(|&(c, r, _)| (c, r)).collect();
    let loops = cell_boundary_loops(&present);

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for raw in &loops {
        // Grid corner (col,row) → local 2D, the SAME mapping `build_hull_mesh` uses for cell
        // centres: x (forward) ← row, y (lateral) ← col. (The col↔row swap flips winding, so an
        // OUTER loop — CCW in (col,row) — is CW / negative-area here; a hole is CCW / positive.)
        let mut pts: Vec<Vec2> = raw
            .iter()
            .map(|&(cc, rr)| {
                Vec2::new(
                    (rr as f32 - center.y) * cell_size,
                    (cc as f32 - center.x) * cell_size,
                )
            })
            .collect();
        let area2 = signed_area2(&pts);
        if area2 >= -1.0e-6 {
            continue; // a hole (positive) or degenerate loop — skip (left filled-over for now)
        }
        pts.reverse(); // CW outer → CCW so the filled top face winds toward the +Z camera
        let smooth = chaikin_closed(&pts, CONTOUR_CHAIKIN_ITERS);
        if smooth.len() < 3 {
            continue;
        }
        let tris = ear_clip(&smooth);
        let base = positions.len() as u32;
        for p in &smooth {
            positions.push([p.x, p.y, HULL_THICKNESS]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([0.0, 0.0]);
        }
        for t in tris {
            indices.push(base + t);
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Spawn lighting, the gunsight pip, and the LOCAL player ship; register the
/// shared runtime render assets (projectile + remote ship/target looks).
pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Lighting: a key directional light so PBR primitives read (ambient fill is
    // attached to the camera in `camera::setup_camera`).
    commands.spawn((
        DirectionalLight {
            illuminance: 9000.0,
            ..default()
        },
        Transform::from_xyz(6.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Shared projectile visuals (a small glowing bullet).
    let projectile_mesh = meshes.add(Sphere::new(0.2));
    let projectile_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.9, 0.35),
        emissive: LinearRgba::rgb(1.2, 0.7, 0.1),
        ..default()
    });

    // Ship look (dart-shaped cuboid long along +X, blue) — used for both the
    // local ship spawned below and any remote ship spawned by `net_update`.
    let ship_mesh = meshes.add(Cuboid::new(1.6, 0.6, 0.3));
    let ship_material = materials.add(Color::srgb(0.30, 0.65, 1.0));

    // Per-kind remote target looks (dummies/asteroids/seeker now arrive over the
    // network; these mirror the original E002 scene meshes/colours).
    let dummy_mesh = meshes.add(Cuboid::new(1.4, 1.4, 1.4)); // reddish practice cube
    let dummy_material = materials.add(Color::srgb(0.75, 0.35, 0.30));
    let asteroid_mesh = meshes.add(Sphere::new(0.9)); // grey drifting rock
    let asteroid_material = materials.add(Color::srgb(0.55, 0.5, 0.45));
    let seeker_mesh = meshes.add(Cuboid::new(1.2, 0.6, 0.3)); // green seeker dart
    let seeker_material = materials.add(Color::srgb(0.35, 0.85, 0.40));

    // Localized shield-impact flash (FIX 0a polish): a sleek glowing cyan ENERGY CRESCENT
    // of the shield ring — a flat annular sliver in the XY plane whose per-vertex colors
    // taper it to a soft crescent with a white-hot core (NOT a stray dot, NOT a full-ship
    // bubble, NOT a hard-cut rectangle). The mesh is built NORMALIZED to outer radius 1.0
    // so the caller can SCALE it per ship to hug any hull (fighter 9×11, corvette 13×15);
    // it is also rotated about Z so the lit slice faces the bullet impact bearing. This is
    // the PROTOTYPE material — each spawned flash clones its own instance so its alpha can
    // fade per-frame with `shield_flash` (a shared handle could not fade one flash
    // independently).
    //
    // `AlphaMode::Add` makes the flare read as EMITTED energy (additive bloom) rather than
    // a flat decal; the per-vertex colors carry the cyan/white-hot gradient, so the
    // `base_color` is a moderate cyan that the vertex colors and the `shield_flash` alpha
    // multiply — kept below pure white so additive doesn't blow out. `cull_mode: None` +
    // `double_sided: true` so the flat ribbon shows from the top-down camera regardless of
    // which face it presents. Starts fully transparent (`alpha 0`) — `update_shield_bubble`
    // raises the alpha to `shield_flash` only on an actual shield impact.
    let shield_arc_mesh = meshes.add(build_arc_band_mesh(
        SHIELD_ARC_INNER_FRAC,
        SHIELD_ARC_HALF_ANGLE,
        SHIELD_ARC_SEGMENTS,
    ));
    let shield_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.45, 0.8, 1.0, 0.0),
        emissive: LinearRgba::rgb(0.2, 0.7, 1.2),
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..default()
    });

    // Ship-fragment debris (FIX 0b): a small irregular box that reads as a torn metal
    // ship piece, with a darkened, desaturated ship-faction tint (clearly a fragment
    // of the blue ship, NOT a grey asteroid). Per-chunk scale + a deterministic id-
    // derived spin are applied at spawn (`net::spawn_render_entity`) so fragments
    // tumble and do not all align.
    let debris_mesh = meshes.add(Cuboid::new(0.7, 0.5, 0.4));
    let debris_material = materials.add(Color::srgb(0.22, 0.38, 0.55));

    // Revise-B seamless hull surface: ONE uniform hull-plate material for every near
    // fitted ship's merged hull mesh (`build_hull_mesh`). A solid metallic steel-blue/grey,
    // normal-lit (NOT emissive) — so an undamaged ship reads as one continuous solid plate
    // with NO visible cells or grid lines. Module colors are HIDDEN here (the per-cell kind
    // is not used by this material); Phase 2 will reveal an exposed module cell at a breach.
    // A modest metallic/low-perceptual-roughness so the plate catches the top-down key
    // light and reads as metal rather than flat paint.
    let hull_material = materials.add(StandardMaterial {
        base_color: HULL_COLOR,
        metallic: 0.6,
        perceptual_roughness: 0.55,
        ..default()
    });

    // Wreck hull plate: the SAME metallic plate as a live ship but tinted "dead metal"
    // (the darkened/desaturated `WRECK_HULL_COLOR`) so a severed chunk / destroyed hulk
    // reads as debris while keeping the real cell shape/size/scale (it shares
    // `build_hull_mesh`). A touch rougher so it reads as scorched/lifeless rather than a
    // polished live hull.
    let wreck_hull_material = materials.add(StandardMaterial {
        base_color: WRECK_HULL_COLOR,
        metallic: 0.5,
        perceptual_roughness: 0.75,
        ..default()
    });

    commands.insert_resource(RenderAssets {
        projectile_mesh,
        projectile_material,
        ship_mesh: ship_mesh.clone(),
        ship_material: ship_material.clone(),
        dummy_mesh,
        dummy_material,
        asteroid_mesh,
        asteroid_material,
        seeker_mesh,
        seeker_material,
        shield_arc_mesh,
        shield_material,
        debris_mesh,
        debris_material,
        hull_material,
        wreck_hull_material,
    });

    // The LOCAL player ship — spawned here deterministically so the `LocalShip`
    // tag never depends on Startup-system ordering (the old `setup_loopback_host`
    // tagging-by-`With<Ship>` could run first and miss the ship, freezing it).
    //
    // It carries exactly the components the render/input/HUD path queries by
    // `With<Ship>`: `ShipIntent` (input writes it), `FlightAssist` (toggle + HUD),
    // `Velocity` (HUD SPD, set from the server's authoritative speed by
    // `net::capture_render_state`), `Health` (HUD), plus the mesh/material/transform.
    //
    // It also carries a `RenderInterp` (snapped to the origin): the local ship is
    // no longer special-cased — it renders from the embedded server's world like
    // every other entity. `net::capture_render_state` rolls its `RenderInterp`
    // prev→curr each fixed step from the authoritative pose, and
    // `render_sync::interpolate_transforms` blends it into the `Transform` each
    // frame (E002's smooth fixed-step interpolation). The net plugin's `Startup`
    // maps this entity under the client's authoritative ship id.
    commands.spawn((
        Ship,
        LocalShip,
        ShipIntent::default(),
        FlightAssist::On,
        Velocity(Vec2::ZERO),
        Health(100.0),
        RenderInterp::snapped(Vec2::ZERO, 0.0),
        Mesh3d(ship_mesh),
        MeshMaterial3d(ship_material),
        Transform::from_xyz(0.0, 0.0, 0.0),
    ));

    // Forward gunsight pip — a glowing marker ahead of the nose showing the
    // fixed weapon's firing line (positioned each frame by `update_aim_pip`).
    commands.spawn((
        AimPip,
        Mesh3d(meshes.add(Sphere::new(0.18))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.4, 1.0, 0.9),
            emissive: LinearRgba::rgb(0.2, 1.0, 0.8),
            ..default()
        })),
        Transform::from_xyz(5.0, 0.0, 0.0),
    ));
}

#[cfg(test)]
mod contour_tests {
    use super::*;

    #[test]
    fn signed_area_sign_and_magnitude() {
        let ccw = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert!((signed_area2(&ccw) - 2.0).abs() < 1e-5); // 2×(unit square area 1) = 2, CCW>0
        let mut cw = ccw;
        cw.reverse();
        assert!((signed_area2(&cw) + 2.0).abs() < 1e-5); // CW → negative
    }

    #[test]
    fn boundary_loop_of_a_2x2_block_is_the_outer_perimeter() {
        let present: std::collections::HashSet<(u16, u16)> =
            [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().collect();
        let loops = cell_boundary_loops(&present);
        assert_eq!(loops.len(), 1, "a solid 2×2 has exactly one boundary loop");
        // 8 grid-corner steps around a 2×2 perimeter (one per unit cell-edge).
        assert_eq!(loops[0].len(), 8);
        // CCW in (col,row): positive shoelace (area 2×2=4 → 2×area=8).
        let pts: Vec<Vec2> = loops[0]
            .iter()
            .map(|&(c, r)| Vec2::new(c as f32, r as f32))
            .collect();
        assert!(signed_area2(&pts) > 0.0);
    }

    #[test]
    fn ear_clip_square_makes_two_triangles_with_full_area() {
        let sq = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = ear_clip(&sq);
        assert_eq!(tris.len(), 6, "a quad → 2 triangles");
        // The two triangles' areas sum to the square's area (1.0).
        let mut total = 0.0;
        for t in tris.chunks(3) {
            let (a, b, c) = (sq[t[0] as usize], sq[t[1] as usize], sq[t[2] as usize]);
            total += (b - a).perp_dot(c - a).abs() * 0.5;
        }
        assert!((total - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ear_clip_handles_a_concave_l_polygon() {
        // CCW L-shape (reflex vertex at (1,1)).
        let l = [
            Vec2::new(0.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(2.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 2.0),
            Vec2::new(0.0, 2.0),
        ];
        let tris = ear_clip(&l);
        assert_eq!(tris.len(), (l.len() - 2) * 3, "n-gon → n-2 triangles");
        let mut total = 0.0;
        for t in tris.chunks(3) {
            let (a, b, c) = (l[t[0] as usize], l[t[1] as usize], l[t[2] as usize]);
            total += (b - a).perp_dot(c - a).abs() * 0.5;
        }
        assert!((total - 3.0).abs() < 1e-5, "L area = 3 (got {total})");
    }

    #[test]
    fn chaikin_rounds_and_stays_inside_bounds() {
        let sq = [
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(0.0, 4.0),
        ];
        let out = chaikin_closed(&sq, 2);
        assert_eq!(out.len(), sq.len() * 4); // doubles each of 2 passes
        for p in &out {
            assert!(p.x >= -1e-4 && p.x <= 4.0 + 1e-4 && p.y >= -1e-4 && p.y <= 4.0 + 1e-4);
        }
    }

    #[test]
    fn contour_mesh_of_a_block_is_nonempty_and_upward_facing() {
        let cells: Vec<(u16, u16, u8)> = (0..3)
            .flat_map(|c| (0..3).map(move |r| (c, r, 0u8)))
            .collect();
        let mesh = build_hull_mesh_contour(&cells, 0.32, Vec2::new(1.5, 1.5));
        let pos = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap();
        assert!(pos.len() >= 3, "a solid block contour has a filled face");
        // All top-face normals point +Z (toward the top-down camera).
        if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ns)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        {
            assert!(ns.iter().all(|n| n[2] > 0.5));
        }
    }
}
