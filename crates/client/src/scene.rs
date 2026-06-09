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
/// R49 — marker on the cool fill `DirectionalLight` so [`crate::ship_visuals::apply_ship_visuals`] can
/// live-tune its illuminance.
#[derive(Component)]
pub struct FillLight;

#[derive(Resource)]
pub struct RenderAssets {
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
    /// Mesh/material for a ship (other players / AI ships). Matches the E002
    /// player-ship look so any rendered ship reads identically to the local one.
    pub ship_mesh: Handle<Mesh>,
    pub ship_material: Handle<StandardMaterial>,
    /// Unit-cube far-LOD placeholder (Refinement 6): a voxelized STRUCTURE that's beyond the voxel
    /// LOD distance draws this scaled to its hull footprint (by the parent `Transform.scale`) as a
    /// cheap one-draw stand-in, instead of the `ship_mesh` (which is ship-shaped, not a unit cube).
    pub lod_box_mesh: Handle<Mesh>,
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
    /// Mining-skirmish structures (Phase 1): a beefy refinery outpost, an industrial mining
    /// transport, and the large central asteroid mine node. Faction tint is applied per-entity at
    /// draw time (Phase 2), so these base materials are neutral.
    pub outpost_mesh: Handle<Mesh>,
    pub outpost_material: Handle<StandardMaterial>,
    pub transport_mesh: Handle<Mesh>,
    pub transport_material: Handle<StandardMaterial>,
    pub minenode_mesh: Handle<Mesh>,
    pub minenode_material: Handle<StandardMaterial>,
    /// Phase 2 faction TINT materials (mining skirmish): a saturated team red / blue applied to any
    /// factioned **simple-mesh** entity (structures + projectiles) so friend/foe reads at a glance —
    /// the mesh shape conveys role, the colour conveys team. (Fitted ships render via the voxel hull
    /// path, so ship faction tint is a later follow-up.)
    pub faction_red_material: Handle<StandardMaterial>,
    pub faction_blue_material: Handle<StandardMaterial>,
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
    /// Faction-tinted hull plates (Refinement 5): the metallic hull material with a red/blue team
    /// base, selected by `RenderEntity.faction` in `sync_ship_hull` for the plain voxel look so a
    /// factioned structure/ship reads as its team colour.
    pub faction_red_hull_material: Handle<StandardMaterial>,
    pub faction_blue_hull_material: Handle<StandardMaterial>,
    /// The wreck hull plate material — the same metallic hull material but tinted with the
    /// darkened/desaturated [`WRECK_HULL_COLOR`] ("dead metal"). A severed chunk's / dead
    /// hulk's hull mesh ([`build_hull_mesh`]) wears it so a broken piece reads as debris
    /// (not a live ship) while keeping the real cell shape/size/scale.
    pub wreck_hull_material: Handle<StandardMaterial>,
    /// WHITE-base counterparts of the two hull materials (Fix #11 M3), used ONLY in the voxel
    /// look while module coloring is ON: the per-cell module hue is a vertex color, and
    /// StandardMaterial computes `vertex × base_color`, so the base must be white for the hues to
    /// show as-is. Structural cells carry [`HULL_COLOR`] as their vertex color so plating still
    /// reads normally. (A colored wreck's structural cells therefore lose the dead-metal tint
    /// while coloring is on — an accepted inspection-mode trade-off.)
    pub hull_material_white: Handle<StandardMaterial>,
    pub wreck_hull_material_white: Handle<StandardMaterial>,
    /// Material for the contour module-marker OVERLAY ([`build_module_overlay_mesh`]) — white
    /// base so the markers' per-vertex [`module_palette`] colors show as-is, sitting just above
    /// the smooth hull. (Fix #11 M3.)
    pub module_overlay_material: Handle<StandardMaterial>,
    /// R47 — the dark gunmetal material for the hard-surface FIXTURES ([`build_ship_fixtures`]): gun
    /// barrels, engine-nozzle housings, sensor dishes, shield nodes, the nose canopy. Shared.
    pub fixture_metal_material: Handle<StandardMaterial>,
    /// R47 — the bright warm HDR emissive material for the GLOW fixtures (engine nozzle cores +
    /// reactor vents + the aft exhaust plume); blooms via the camera Bloom. Shared.
    pub fixture_glow_material: Handle<StandardMaterial>,
    /// R48 — emissive running/nav-light materials (port red / starboard green / white spine) + the
    /// faction-tinted ACCENT materials (neutral / red / blue) for the spine strip + canopy cap.
    pub nav_red_material: Handle<StandardMaterial>,
    pub nav_green_material: Handle<StandardMaterial>,
    pub nav_white_material: Handle<StandardMaterial>,
    pub accent_neutral_material: Handle<StandardMaterial>,
    pub accent_red_material: Handle<StandardMaterial>,
    pub accent_blue_material: Handle<StandardMaterial>,
    /// R48 — the dynamic THROTTLE-reactive engine exhaust: a shared additive emissive cone (axis `+Y`,
    /// oriented + scaled per ship by [`crate::net::update_engine_exhaust`] so it flares aft and grows
    /// with speed). The per-ship engine PointLight uses no shared asset (its intensity is per-entity).
    pub engine_flame_mesh: Handle<Mesh>,
    pub engine_flame_material: Handle<StandardMaterial>,
    /// R48/R49 — the cinematic hull material (ExtendedMaterial: fresnel rim + panels + grime) per
    /// faction (neutral / red / blue). Used for the COMBAT hull look in `sync_ship_hull`; one handle
    /// per faction (not per cell-set) so it doesn't churn on carve. Live-tuned by `apply_ship_visuals`.
    pub hull_ext_neutral: Handle<crate::hull_shader::HullMaterial>,
    pub hull_ext_red: Handle<crate::hull_shader::HullMaterial>,
    pub hull_ext_blue: Handle<crate::hull_shader::HullMaterial>,
    /// R50 — shared assets for the particle effects (engine ion-trail + damage smoke/sparks): one small
    /// sphere mesh scaled per particle, an additive warm TRAIL material, an additive white-hot SPARK
    /// material, and a dark alpha-blend SMOKE material.
    pub particle_mesh: Handle<Mesh>,
    pub trail_material: Handle<StandardMaterial>,
    pub spark_material: Handle<StandardMaterial>,
    pub smoke_material: Handle<StandardMaterial>,
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

/// Per-cell **module color palette** (Fix #11 M3), keyed by the `kind` byte each render cell
/// carries — `0` = structural / empty plating, `1..=6` = the `ModuleKind`s in the server's
/// `render_cell_kind` order: 1 Reactor, 2 Thruster, 3 Weapon, 4 Shield, 5 Armor, 6 Utility.
/// Used as a per-vertex color in the voxel hull mesh and on the contour module-overlay markers
/// when the module-color toggle ([`crate::net::ModuleColorMode`]) is ON; structural cells reuse
/// [`HULL_COLOR`] so plating reads normally. Distinct, readable hues so module types are
/// tellable at a glance.
pub fn module_palette(kind: u8) -> Color {
    match kind {
        1 => Color::srgb(0.96, 0.78, 0.18), // Reactor — amber (power)
        2 => Color::srgb(0.96, 0.46, 0.14), // Thruster — orange (propulsion)
        3 => Color::srgb(0.90, 0.20, 0.20), // Weapon — red
        4 => Color::srgb(0.24, 0.66, 0.96), // Shield — cyan
        5 => Color::srgb(0.72, 0.74, 0.78), // Armor — bright steel
        6 => Color::srgb(0.52, 0.86, 0.42), // Utility — green
        7 => Color::srgb(0.62, 0.40, 0.92), // Sensor — violet (Phase C4)
        _ => HULL_COLOR,                    // 0 / unknown — structural plating
    }
}

/// A [`Color`] as a linear-RGBA vertex-color array — the `Mesh::ATTRIBUTE_COLOR` convention
/// (StandardMaterial multiplies it into `base_color`, so a white-base material shows it as-is).
fn color_rgba(c: Color) -> [f32; 4] {
    let l = c.to_linear();
    [l.red, l.green, l.blue, l.alpha]
}

/// Revise-B: the merged hull surface's slab half-thickness in `+Z`, in sim units — the
/// top face sits at `z = HULL_THICKNESS` so the plate has a touch of relief under the
/// top-down light without looking like a flat decal. Small (the camera is top-down, so
/// only the top face is normally seen); the side walls at the silhouette boundary give a
/// thin lip. Tunable for feel.
///
/// R47: raised from `0.1` to give the hard-surface hull a more substantial plate lip at the
/// silhouette — the sleeker metal (higher metallic / lower roughness) catches the key light on the
/// thicker side walls so the edge reads as beveled plating, and the 3D fixtures sit proud of it.
const HULL_THICKNESS: f32 = 0.18;

/// R51 — UV tiling multiplier on the hull-LOCAL coords: the baked plating texture (4 plates/tile)
/// repeats every `1.0 / HULL_UV_TILE` world units, so `1.2` ≈ a tile every ~2.6 cells (~plate ≈ ⅔
/// cell). Tunable in code for plate density.
const HULL_UV_TILE: f32 = 1.2;

/// Belt-and-suspenders cap: only ship-sized hulls get the beveled combat mesh (big structures stay
/// flat/chunked). Passed by [`crate::net::sync_ship_hull`] before calling [`build_hull_mesh_beveled`].
pub const DETAIL_MAX_CELLS: usize = 400;

/// R55 — the live combat-hull BEVEL dimensions (built from [`crate::ship_visuals::ShipVisualTuning`]).
/// The hull is ONE beveled solid traced from the cell silhouette: a vertical wall up to a `shoulder`,
/// then a CHAMFER (`bevel`) up to an inset flat top at `top`. `round_iters` smooths the silhouette (0 =
/// hard/angular, more = rounder). `PartialEq` so a change forces a rebuild.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HullStyle {
    /// Combat-hull thickness (the top face's `z`). Modest — the bevel + tilt + shadows carry the 3-D read.
    pub top: f32,
    /// Chamfer size: the top face is inset inward by `bevel` from the silhouette, and the chamfer rises
    /// over ~`bevel` of height → a ~45° beveled edge that catches the raking key light.
    pub bevel: f32,
    /// Silhouette smoothing (Chaikin corner-cut passes): `0` = hard/angular cells, `1` = lightly rounded,
    /// `2` = the contour-look roundness. Higher = rounder (and slightly smaller).
    pub round_iters: u32,
}

impl Default for HullStyle {
    fn default() -> Self {
        // Mirrors the `ShipVisualTuning` defaults (the dev panel is the live source of truth).
        Self {
            top: 0.15,
            bevel: 0.05,
            round_iters: 1,
        }
    }
}

/// R53 — marker on the KEY directional light so [`crate::ship_visuals::apply_ship_visuals`] can live-tune
/// its shadows / illuminance / raking direction (mirrors [`FillLight`]).
#[derive(Component)]
pub struct KeyLight;

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

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices));
    // R51 — generate tangents so the baked normal-map plating lights correctly (the combat hull wears
    // it; the wreck/tile/contour paths just carry the harmless extra attribute). Ignored on the rare
    // error (indexed TriangleList with POSITION/NORMAL/UV_0 succeeds).
    let _ = mesh.generate_tangents();
    mesh
}

/// Build a flat **trapezoid** in the XY plane (`z = 0`), **anchored at its bottom edge on
/// `y = 0`**: the bottom edge spans `[-bottom_w/2, +bottom_w/2]`, the top edge spans
/// `[-top_w/2, +top_w/2]` at `y = height`. The normal is `+Z` so it faces the top-down
/// camera (the Phase F HUD trapezoid/ramp bars).
///
/// Anchoring the bottom at `y = 0` means a uniform `Transform` `scale.y` grows the shape
/// **upward**, so a row of segments with increasing `scale.y` reads as a short→tall ramp;
/// `top_w < bottom_w` gives each segment the tapered "battery cell" look. Two triangles,
/// wound CCW from `+Z`, with a simple UV so [`StandardMaterial`] is satisfied. NO vertex
/// color (unlike the shield arc): the HUD material is `unlit` and its `base_color` is set
/// per segment at draw time, so `base_color` shows as-is (no `vertex × base` multiply).
pub fn build_trapezoid_mesh(top_w: f32, bottom_w: f32, height: f32) -> Mesh {
    let hb = bottom_w * 0.5;
    let ht = top_w * 0.5;
    let positions: Vec<[f32; 3]> = vec![
        [-hb, 0.0, 0.0],    // 0 bottom-left
        [hb, 0.0, 0.0],     // 1 bottom-right
        [ht, height, 0.0],  // 2 top-right
        [-ht, height, 0.0], // 3 top-left
    ];
    let normals = vec![[0.0, 0.0, 1.0]; 4];
    let uvs: Vec<[f32; 2]> = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    let indices = vec![0u32, 1, 2, 0, 2, 3];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Build a **horizontally-tapered** flat trapezoid in the XY plane (`z = 0`), **anchored at its
/// bottom edge on `y = 0`** (so `scale.y` still grows it upward into a ramp). The bottom edge is
/// flat spanning `[-width/2, +width/2]`; the **left edge has height `left_h`, the right edge
/// `right_h`**, and the TOP edge slants between them. With `right_h < left_h` the segment **tapers
/// toward the right** on a clean flat baseline — the Phase-F afterburner/heat ramp look (vs
/// [`build_trapezoid_mesh`], which tapers toward the TOP and is used by the vertical stacks). `+Z`
/// normal (faces the top-down camera); two triangles wound CCW from `+Z`, with a simple UV.
pub fn build_trapezoid_mesh_h(left_h: f32, right_h: f32, width: f32) -> Mesh {
    let hw = width * 0.5;
    let positions: Vec<[f32; 3]> = vec![
        [-hw, 0.0, 0.0],    // 0 bottom-left
        [hw, 0.0, 0.0],     // 1 bottom-right
        [hw, right_h, 0.0], // 2 right edge (short side)
        [-hw, left_h, 0.0], // 3 left edge  (tall side)
    ];
    let normals = vec![[0.0, 0.0, 1.0]; 4];
    let uvs: Vec<[f32; 2]> = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    let indices = vec![0u32, 1, 2, 0, 2, 3];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
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
pub fn build_hull_mesh(
    cells: &[(u16, u16, u8)],
    cell_size: f32,
    center: Vec2,
    module_color: bool,
) -> Mesh {
    // Phase-2 reveal hook: with no breach model yet, no cell is ever exposed, so the
    // whole surface uses the uniform hull material. Phase 2 replaces this with a real
    // breach predicate and per-exposed-cell coloring.
    let exposed = |_col: u16, _row: u16| -> bool { false };
    build_hull_mesh_with(cells, cell_size, center, exposed, module_color, |_, _| {
        sim::fitting::CellShape::Full
    })
}

/// R59 — [`build_hull_mesh`] honouring each cell's [`CellShape`] for the module-colour / per-cell voxel
/// INSPECTION view (the **C** toggle): a sub-shape cell renders as its real triangle/pentagon/trapezoid
/// (matching the combat beveled view + the carve hitbox), while `Full` cells stay the exact square box
/// (byte-identical to [`build_hull_mesh`]). The kind/colour + neighbour culling are unchanged.
pub fn build_hull_mesh_shaped(
    cells: &[(u16, u16, u8, sim::fitting::CellShape)],
    cell_size: f32,
    center: Vec2,
    module_color: bool,
) -> Mesh {
    let shapes: std::collections::HashMap<(u16, u16), sim::fitting::CellShape> =
        cells.iter().map(|&(c, r, _, s)| ((c, r), s)).collect();
    let kinds: Vec<(u16, u16, u8)> = cells.iter().map(|&(c, r, k, _)| (c, r, k)).collect();
    let exposed = |_col: u16, _row: u16| -> bool { false };
    build_hull_mesh_with(&kinds, cell_size, center, exposed, module_color, |c, r| {
        shapes
            .get(&(c, r))
            .copied()
            .unwrap_or(sim::fitting::CellShape::Full)
    })
}

/// R59 — emit one SUB-SHAPE cell (triangle / chamfer pentagon / slope trapezoid) into the per-cell
/// voxel mesh: its [`CellShape`](sim::fitting::CellShape) polygon as a coloured TOP face at `z=+half`
/// plus a side wall down each polygon edge (the hypotenuse shows the cut; the cut region is absent).
/// The grid→local axis SWAP (row→x, col→y) reverses winding, so the local ring is flipped back to CCW
/// for a `+Z` top. `Full` cells never reach here — the caller keeps the exact legacy box for them.
#[allow(clippy::too_many_arguments)]
fn push_shaped_cell(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    shape: sim::fitting::CellShape,
    col: u16,
    row: u16,
    cell_size: f32,
    center: Vec2,
    half: f32,
    color: [f32; 4],
) {
    // Map each GRID-space corner (x=col, y=row) to the hull-local frame (row→x forward, col→y lateral) —
    // the SAME swap `build_hull_mesh_beveled` uses, so the C-view polygon aligns with the combat view.
    let mut ring: Vec<[f32; 2]> = shape
        .corners(col, row)
        .iter()
        .map(|g| [(g.y - center.y) * cell_size, (g.x - center.x) * cell_size])
        .collect();
    if ring.len() < 3 {
        return;
    }
    // The x/y swap flips orientation (grid-CCW → local-CW); flip back to CCW so the top faces +Z.
    let area2: f32 = (0..ring.len())
        .map(|i| {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    if area2 < 0.0 {
        ring.reverse();
    }
    // Top face (triangle fan) at z = +half, normal +Z.
    let base = positions.len() as u32;
    for &[x, y] in &ring {
        positions.push([x, y, half]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([x * HULL_UV_TILE, y * HULL_UV_TILE]);
        colors.push(color);
    }
    for i in 1..ring.len() as u32 - 1 {
        indices.extend_from_slice(&[base, base + i, base + i + 1]);
    }
    // A side wall down each polygon edge; outward normal `(d.y, -d.x)` for the CCW ring.
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1.0e-6);
        let normal = [d[1] / len, -d[0] / len, 0.0];
        let wbase = positions.len() as u32;
        for &corner in &[
            [a[0], a[1], 0.0],
            [b[0], b[1], 0.0],
            [b[0], b[1], half],
            [a[0], a[1], half],
        ] {
            positions.push(corner);
            normals.push(normal);
            uvs.push([corner[0] * HULL_UV_TILE, corner[1] * HULL_UV_TILE]);
            colors.push(color);
        }
        indices.extend_from_slice(&[wbase, wbase + 1, wbase + 2, wbase, wbase + 2, wbase + 3]);
    }
}

/// [`build_hull_mesh`] with an explicit `exposed(col, row)` predicate — the Phase-2
/// reveal seam. Today `exposed` is always `false` (breach reveal unused), so the merged
/// surface's geometry is one solid plate; the parameter exists so a breach phase can flag
/// exposed module cells without changing this mesh-construction code. (The `_exposed` flag
/// is threaded but not yet branched on.)
///
/// `module_color` (Fix #11 M3): when `true`, each cell's quads carry its [`module_palette`]
/// color as a per-vertex `Mesh::ATTRIBUTE_COLOR` so module cells are tellable apart at a glance
/// (the caller pairs this with a WHITE-base material so the colors show as-is); when `false`
/// the per-vertex color is white `[1,1,1,1]` so the material's own [`HULL_COLOR`]/wreck tint
/// shows through. `ATTRIBUTE_COLOR` is ALWAYS inserted either way, so flipping the toggle never
/// changes the material's `VERTEX_COLORS` pipeline specialization (no per-toggle shader recompile).
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
    module_color: bool,
    shape_of: impl Fn(u16, u16) -> sim::fitting::CellShape,
) -> Mesh {
    let half = HULL_THICKNESS;

    // Fast membership test for neighbour lookups (so shared interior edges emit no wall
    // and the plate stays gapless). Keyed by `(col, row)`.
    let present: std::collections::HashSet<(u16, u16)> =
        cells.iter().map(|&(c, r, _)| (c, r)).collect();

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit one CCW-from-`+Z` quad (two triangles) given its four corner positions, a shared
    // normal, simple UVs, and a per-vertex color (module hue or white). Corners are ordered
    // v0→v1→v2→v3 counter-clockwise as seen from the side the normal points to.
    let push_quad = |positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     colors: &mut Vec<[f32; 4]>,
                     indices: &mut Vec<u32>,
                     corners: [[f32; 3]; 4],
                     normal: [f32; 3],
                     color: [f32; 4]| {
        let base = positions.len() as u32;
        positions.extend_from_slice(&corners);
        for _ in 0..4 {
            normals.push(normal);
            colors.push(color);
        }
        // R50/R51: UV = the corner's hull-LOCAL XY × HULL_UV_TILE — anchored to the ship-local frame
        // (so the baked plating texture is PAINTED ON the hull + moves WITH it, no world-space swim) and
        // scaled for plate density. The R51 normal/ORM textures tile against these.
        for c in &corners {
            uvs.push([c[0] * HULL_UV_TILE, c[1] * HULL_UV_TILE]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    for &(col, row, kind) in cells {
        // Phase-2 hook (no-op today): a future breach phase tints `exposed` cells.
        let _exposed = exposed(col, row);
        // Module color (M3): the cell's palette hue when the toggle is on, else white so the
        // material's own hull/wreck color shows. Structural cells map to `HULL_COLOR`.
        let cell_color = if module_color {
            color_rgba(module_palette(kind))
        } else {
            [1.0, 1.0, 1.0, 1.0]
        };

        // R59 — a SUB-SHAPE cell renders as its real polygon (the C/module view now matches the combat
        // view + the hitbox); `Full` falls through to the exact legacy box below → byte-identical.
        let shape = shape_of(col, row);
        if !shape.is_full() {
            push_shaped_cell(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                shape,
                col,
                row,
                cell_size,
                center,
                half,
                cell_color,
            );
            continue;
        }

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
        // top-down camera). Coplanar across all cells → seamless.
        push_quad(
            &mut positions,
            &mut normals,
            &mut uvs,
            &mut colors,
            &mut indices,
            [
                [x0, y0, half],
                [x1, y0, half],
                [x1, y1, half],
                [x0, y1, half],
            ],
            [0.0, 0.0, 1.0],
            cell_color,
        );

        // Boundary side walls: only on edges with no present neighbour (silhouette edge,
        // or a carved-hole edge). Interior shared edges are covered → no wall, so
        // the surface stays gapless. Each wall drops from z=+half to z=0.
        // -X edge (toward a smaller row / aft). Neighbour is (col, row-1).
        let has_neg_x = row > 0 && present.contains(&(col, row - 1));
        if !has_neg_x {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                [[x0, y0, 0.0], [x0, y1, 0.0], [x0, y1, half], [x0, y0, half]],
                [-1.0, 0.0, 0.0],
                cell_color,
            );
        }
        // +X edge (toward a larger row / nose). Neighbour is (col, row+1).
        let has_pos_x = present.contains(&(col, row + 1));
        if !has_pos_x {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                [[x1, y1, 0.0], [x1, y0, 0.0], [x1, y0, half], [x1, y1, half]],
                [1.0, 0.0, 0.0],
                cell_color,
            );
        }
        // -Y edge (toward a smaller col). Neighbour is (col-1, row).
        let has_neg_y = col > 0 && present.contains(&(col - 1, row));
        if !has_neg_y {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                [[x1, y0, 0.0], [x0, y0, 0.0], [x0, y0, half], [x1, y0, half]],
                [0.0, -1.0, 0.0],
                cell_color,
            );
        }
        // +Y edge (toward a larger col). Neighbour is (col+1, row).
        let has_pos_y = present.contains(&(col + 1, row));
        if !has_pos_y {
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                [[x0, y1, 0.0], [x1, y1, 0.0], [x1, y1, half], [x0, y1, half]],
                [0.0, 1.0, 0.0],
                cell_color,
            );
        }
    }

    // R52/R55 — no tangents: the combat hull is real geometry now (see `build_hull_mesh_beveled`), and
    // the wreck/contour/module-colour paths never used a normal map, so the flat builder reverts to
    // POSITION/NORMAL/UV_0/COLOR only.
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

/// R55 — inset a CCW ring INWARD by `inset` via a per-vertex MITER offset (along the angle bisector,
/// scaled `1/cos(half-angle)`, clamped to avoid spikes at sharp corners). The beveled hull's chamfered
/// top edge runs between the original outer ring and this inset ring.
fn inset_ring(ring: &[Vec2], inset: f32) -> Vec<Vec2> {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let prev = ring[(i + n - 1) % n];
            let cur = ring[i];
            let next = ring[(i + 1) % n];
            let e0 = (cur - prev).normalize_or_zero();
            let e1 = (next - cur).normalize_or_zero();
            // Inward normal of a CCW edge dir `e` is `(-e.y, e.x)` (outward is `(e.y, -e.x)`).
            let n0 = Vec2::new(-e0.y, e0.x);
            let n1 = Vec2::new(-e1.y, e1.x);
            let mut bis = n0 + n1;
            if bis.length_squared() < 1.0e-8 {
                bis = n1; // straight or degenerate → just use the edge normal
            }
            let bis = bis.normalize_or_zero();
            let cos_h = bis.dot(n1).max(0.30); // clamp keeps sharp corners from spiking inward
            cur + bis * (inset / cos_h)
        })
        .collect()
}

/// R55 — the COMBAT ship hull as ONE BEVELED solid traced from the cell silhouette (replaces the R52–54
/// per-cell plates). Reuses the contour builder's boundary tracing + Chaikin smoothing + earcut: each
/// ring is smoothed by `style.round_iters` (0 = hard/angular, more = rounder); the OUTER ring gets a
/// vertical wall (`z 0..shoulder`) + a CHAMFER (`shoulder → inset top`, the bevel that catches the raking
/// key light) + an inset flat top at `z = top`; HOLE rings (carved cavities) keep a plain vertical wall.
/// Output POSITION/NORMAL/UV (no per-vertex COLOR — the faction tint is the material `base_color`).
/// Carving rebuilds it from the live cells.
pub fn build_hull_mesh_beveled(
    cells: &[(u16, u16, sim::fitting::CellShape)],
    cell_size: f32,
    center: Vec2,
    style: HullStyle,
) -> Mesh {
    // R58 — trace the per-cell SHAPE polygons' silhouette (handles full squares + corner triangles), in
    // GRID space (`x = col`, `y = row`); then map each point to LOCAL (`row → x`, `col → y` swap, scaled).
    let loops = cell_boundary_loops_shaped(cells);

    // Local-space rings, oriented canonically (outer CCW, holes CW) — mirrors `build_hull_mesh_contour`.
    let mut outers: Vec<Vec<Vec2>> = Vec::new();
    let mut holes: Vec<Vec<Vec2>> = Vec::new();
    for raw in &loops {
        let mut pts: Vec<Vec2> = raw
            .iter()
            .map(|&g| Vec2::new((g.y - center.y) * cell_size, (g.x - center.x) * cell_size))
            .collect();
        let area2 = signed_area2(&pts);
        if area2 < -1.0e-6 {
            pts.reverse();
            outers.push(pts);
        } else if area2 > 1.0e-6 {
            pts.reverse();
            holes.push(pts);
        }
    }

    let top = style.top.max(0.01);
    let bevel = style.bevel.clamp(0.0, cell_size * 0.45);
    let shoulder = (top - bevel.min(top * 0.7)).max(0.0);

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Takes the buffers as args (not captured) so the top-face fill can push to them directly between calls.
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
        uvs.extend_from_slice(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    let single_outer = outers.len() == 1;
    for outer in &outers {
        let my_holes: Vec<Vec<Vec2>> = if single_outer {
            holes.clone()
        } else {
            holes
                .iter()
                .filter(|h| point_in_polygon(ring_centroid(h), outer))
                .cloned()
                .collect()
        };
        let outer_s = chaikin_closed(outer, style.round_iters);
        if outer_s.len() < 3 {
            continue;
        }
        let holes_s: Vec<Vec<Vec2>> = my_holes
            .iter()
            .map(|h| chaikin_closed(h, style.round_iters))
            .filter(|h| h.len() >= 3)
            .collect();
        let inset = inset_ring(&outer_s, bevel);

        // TOP FACE: the inset outer ring minus the holes, flat at z = top, normal +Z.
        let base = positions.len() as u32;
        for p in &inset {
            positions.push([p.x, p.y, top]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([0.0, 0.0]);
        }
        for h in &holes_s {
            for p in h {
                positions.push([p.x, p.y, top]);
                normals.push([0.0, 0.0, 1.0]);
                uvs.push([0.0, 0.0]);
            }
        }
        for t in ear_clip_with_holes(&inset, &holes_s) {
            indices.push(base + t);
        }

        // OUTER ring: vertical wall (0..shoulder) + CHAMFER (shoulder → inset top = the bevel).
        let n = outer_s.len();
        for i in 0..n {
            let a = outer_s[i];
            let b = outer_s[(i + 1) % n];
            let ai = inset[i];
            let bi = inset[(i + 1) % n];
            let d = b - a;
            let len = d.length();
            if len <= 1.0e-6 {
                continue;
            }
            let nrm = Vec2::new(d.y, -d.x) / len; // outward (CCW outer)
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [
                    [a.x, a.y, 0.0],
                    [b.x, b.y, 0.0],
                    [b.x, b.y, shoulder],
                    [a.x, a.y, shoulder],
                ],
                [nrm.x, nrm.y, 0.0],
            );
            // Chamfer normal (from the quad) points outward+up → catches the raking key light.
            let c0 = Vec3::new(a.x, a.y, shoulder);
            let c1 = Vec3::new(b.x, b.y, shoulder);
            let c2 = Vec3::new(bi.x, bi.y, top);
            let cn = (c1 - c0)
                .cross(c2 - c0)
                .try_normalize()
                .unwrap_or(Vec3::new(nrm.x, nrm.y, 0.0));
            push_quad(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut indices,
                [
                    [a.x, a.y, shoulder],
                    [b.x, b.y, shoulder],
                    [bi.x, bi.y, top],
                    [ai.x, ai.y, top],
                ],
                [cn.x, cn.y, cn.z],
            );
        }

        // HOLE rings (carved cavities): a plain vertical wall 0..top, normal into the cavity.
        for ring in &holes_s {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let d = b - a;
                let len = d.length();
                if len <= 1.0e-6 {
                    continue;
                }
                let nrm = Vec2::new(d.y, -d.x) / len;
                push_quad(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut indices,
                    [
                        [a.x, a.y, 0.0],
                        [b.x, b.y, 0.0],
                        [b.x, b.y, top],
                        [a.x, a.y, top],
                    ],
                    [nrm.x, nrm.y, 0.0],
                );
            }
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
// R47 — hard-surface FIXTURES: the 3D "ship parts" overlaid on the cell hull so a ship reads as a
// starfighter (gun barrels, engine nozzles + glow, reactor vent, sensor dishes, shield nodes, nose
// canopy). Built from the SAME live cell set as the hull (on a `cells_hash` change), so a shot-off
// weapon/engine cell drops its barrel/nozzle. Client render only — determinism-neutral.
// ============================================================================

/// A growable triangle-mesh buffer (positions/normals/uvs/colors/indices) the fixture builder appends
/// boxes into, finalized to a [`Mesh`] (or `None` when empty).
#[derive(Default)]
struct MeshBuf {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl MeshBuf {
    /// One CCW-from-`normal` quad (two triangles), winding matching the hull mesh's faces.
    fn quad(&mut self, corners: [[f32; 3]; 4], normal: [f32; 3], color: [f32; 4]) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&corners);
        for _ in 0..4 {
            self.normals.push(normal);
            self.colors.push(color);
        }
        self.uvs.push([0.0, 0.0]);
        self.uvs.push([1.0, 0.0]);
        self.uvs.push([1.0, 1.0]);
        self.uvs.push([0.0, 1.0]);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    fn into_mesh(self) -> Option<Mesh> {
        if self.positions.is_empty() {
            return None;
        }
        Some(
            Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
            )
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
            .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
            .with_inserted_indices(Indices::U32(self.indices)),
        )
    }
}

/// Append an axis-aligned box `[x0,x1]×[y0,y1]×[z0,z1]` (6 faces, outward normals, winding matching
/// the hull mesh). Vertex color is white so the fixture wears its material colour as-is.
#[allow(clippy::too_many_arguments)]
fn push_box(buf: &mut MeshBuf, x0: f32, x1: f32, y0: f32, y1: f32, z0: f32, z1: f32) {
    let c = [1.0, 1.0, 1.0, 1.0];
    // +Z top / -Z bottom
    buf.quad(
        [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
        [0.0, 0.0, 1.0],
        c,
    );
    buf.quad(
        [[x0, y1, z0], [x1, y1, z0], [x1, y0, z0], [x0, y0, z0]],
        [0.0, 0.0, -1.0],
        c,
    );
    // -X / +X
    buf.quad(
        [[x0, y0, z0], [x0, y1, z0], [x0, y1, z1], [x0, y0, z1]],
        [-1.0, 0.0, 0.0],
        c,
    );
    buf.quad(
        [[x1, y1, z0], [x1, y0, z0], [x1, y0, z1], [x1, y1, z1]],
        [1.0, 0.0, 0.0],
        c,
    );
    // -Y / +Y
    buf.quad(
        [[x1, y0, z0], [x0, y0, z0], [x0, y0, z1], [x1, y0, z1]],
        [0.0, -1.0, 0.0],
        c,
    );
    buf.quad(
        [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
        [0.0, 1.0, 0.0],
        c,
    );
}

/// Which shared material a fixture mesh wears — lets [`build_ship_fixtures`] return several role-tagged
/// meshes the caller spawns with the matching material (the [`FixtureRole::Accent`] is faction-tinted).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FixtureRole {
    /// Dark gunmetal greebles: gun barrels, nozzle housings, sensor dishes, shield nodes, canopy frame.
    Metal,
    /// Warm emissive engine nozzle cores / reactor vents / exhaust plume.
    Glow,
    /// Port (left, `-Y`) red running light.
    NavRed,
    /// Starboard (right, `+Y`) green running light.
    NavGreen,
    /// White spine light.
    NavWhite,
    /// Faction-tinted emissive accent: a spine strip + a canopy cap (material chosen by faction).
    Accent,
}

/// Build the hard-surface fixture meshes for a fitted ship from its live cell set, each tagged with the
/// [`FixtureRole`] (= shared material) the caller spawns it with, plus the per-THRUSTER flame origins
/// (local nozzle-mouth positions) the caller spawns throttle-reactive engine flames at. Empty roles
/// are omitted. Cell convention matches [`build_hull_mesh_with`]: row→`+X` forward, col→`+Y` lateral,
/// around `center`. Intended for SMALL ships (the single-mesh near path) — NOT the chunked path.
pub fn build_ship_fixtures(
    cells: &[(u16, u16, u8)],
    cell_size: f32,
    center: Vec2,
    top: f32,
) -> (Vec<(Mesh, FixtureRole)>, Vec<Vec3>) {
    // R53 — `top` is the live combat-hull thickness ([`HullStyle::top`]) so the fixtures sit on (and
    // scale with) the taller hull instead of the const `HULL_THICKNESS`.
    let s = cell_size;
    let mut metal = MeshBuf::default();
    let mut glow = MeshBuf::default();
    let mut nav_red = MeshBuf::default();
    let mut nav_green = MeshBuf::default();
    let mut nav_white = MeshBuf::default();
    let mut accent = MeshBuf::default();
    let mut thrusters: Vec<Vec3> = Vec::new();

    let world = |col: u16, row: u16| -> (f32, f32) {
        (
            ((row as f32 + 0.5) - center.y) * cell_size,
            ((col as f32 + 0.5) - center.x) * cell_size,
        )
    };

    // Nose cell (canopy + white spine light): the forward-most present cell (max row), ties broken
    // toward the centre column so the canopy sits on the spine.
    let nose: Option<(u16, u16)> =
        cells
            .iter()
            .map(|&(c, r, _)| (c, r))
            .fold(None, |best, (c, r)| match best {
                None => Some((c, r)),
                Some((bc, br)) => {
                    if r > br {
                        Some((c, r))
                    } else if r == br {
                        let dc = (c as f32 + 0.5 - center.x).abs();
                        let dbc = (bc as f32 + 0.5 - center.x).abs();
                        Some(if dc < dbc { (c, r) } else { (bc, br) })
                    } else {
                        Some((bc, br))
                    }
                }
            });

    // Port (min col, `-Y`) + starboard (max col, `+Y`) wing cells for the running lights; ties broken
    // toward mid-body (row nearest the centre) so the lights sit on the wings, not the nose/tail.
    let pick_side = |want_max: bool| -> Option<(u16, u16)> {
        cells
            .iter()
            .map(|&(c, r, _)| (c, r))
            .fold(None, |best, (c, r)| match best {
                None => Some((c, r)),
                Some((bc, br)) => {
                    let better = if want_max { c > bc } else { c < bc };
                    if better {
                        Some((c, r))
                    } else if c == bc {
                        let dr = (r as f32 + 0.5 - center.y).abs();
                        let dbr = (br as f32 + 0.5 - center.y).abs();
                        Some(if dr < dbr { (c, r) } else { (bc, br) })
                    } else {
                        Some((bc, br))
                    }
                }
            })
    };
    let port = pick_side(false);
    let starboard = pick_side(true);

    for &(col, row, kind) in cells {
        let (cx, cy) = world(col, row);
        match kind {
            // Weapon → a forward gun barrel proud of the cell front (+X).
            3 => push_box(
                &mut metal,
                cx + s * 0.20,
                cx + s * 0.95,
                cy - s * 0.12,
                cy + s * 0.12,
                top * 0.40,
                top * 0.95,
            ),
            // Thruster → aft nozzle housing (metal) + emissive nozzle core (glow), at the rear (-X).
            // R49: the static plume is GONE — a per-thruster throttle-reactive FLAME (see
            // `update_engine_flames`) is spawned at this thruster's nozzle mouth instead; record it.
            2 => {
                push_box(
                    &mut metal,
                    cx - s * 0.70,
                    cx - s * 0.12,
                    cy - s * 0.34,
                    cy + s * 0.34,
                    top * 0.10,
                    top * 1.00,
                );
                // Nozzle core — raised above the housing top so it reads from above.
                push_box(
                    &mut glow,
                    cx - s * 0.84,
                    cx - s * 0.58,
                    cy - s * 0.26,
                    cy + s * 0.26,
                    top * 1.05,
                    top * 1.50,
                );
                // The flame's local origin: the nozzle mouth (rear of the housing, engine height).
                thrusters.push(Vec3::new(cx - s * 0.72, cy, top * 1.25));
            }
            // Reactor → a glowing top vent.
            1 => push_box(
                &mut glow,
                cx - s * 0.30,
                cx + s * 0.30,
                cy - s * 0.30,
                cy + s * 0.30,
                top * 1.00,
                top * 1.25,
            ),
            // Sensor → a flat top dish.
            7 => push_box(
                &mut metal,
                cx - s * 0.32,
                cx + s * 0.32,
                cy - s * 0.32,
                cy + s * 0.32,
                top * 1.00,
                top * 1.18,
            ),
            // Shield → a small raised emitter node.
            4 => push_box(
                &mut metal,
                cx - s * 0.16,
                cx + s * 0.16,
                cy - s * 0.16,
                cy + s * 0.16,
                top * 1.00,
                top * 1.55,
            ),
            _ => {}
        }
    }

    // Cockpit canopy (metal frame) + a faction-accent cap + a white spine light on the nose.
    if let Some((col, row)) = nose {
        let (cx, cy) = world(col, row);
        push_box(
            &mut metal,
            cx - s * 0.28,
            cx + s * 0.28,
            cy - s * 0.20,
            cy + s * 0.20,
            top * 1.00,
            top * 1.70,
        );
        // Accent cap riding just above the canopy (faction-tinted emissive).
        push_box(
            &mut accent,
            cx - s * 0.20,
            cx + s * 0.12,
            cy - s * 0.14,
            cy + s * 0.14,
            top * 1.70,
            top * 1.86,
        );
        // White spine light just aft of the canopy.
        push_box(
            &mut nav_white,
            cx - s * 0.52,
            cx - s * 0.30,
            cy - s * 0.08,
            cy + s * 0.08,
            top * 1.05,
            top * 1.35,
        );
    }

    // Faction accent strip down the spine: a thin emissive line on every centre-column cell. R49 — the
    // true centre column is `(center.x - 0.5).round()` (e.g. a 9-wide grid has `center.x = 4.5` → col 4,
    // the spine); the old `center.x.round()` gave 5 (off to one side — the reported "dashed line").
    let center_col = (center.x - 0.5).round() as i32;
    for &(col, row, _) in cells {
        if col as i32 == center_col {
            let (cx, cy) = world(col, row);
            push_box(
                &mut accent,
                cx - s * 0.35,
                cx + s * 0.35,
                cy - s * 0.05,
                cy + s * 0.05,
                top * 1.00,
                top * 1.12,
            );
        }
    }

    // Port (red) / starboard (green) running lights on the wing edges.
    if let Some((col, row)) = port {
        let (cx, cy) = world(col, row);
        push_box(
            &mut nav_red,
            cx - s * 0.10,
            cx + s * 0.10,
            cy - s * 0.50,
            cy - s * 0.26,
            top * 0.60,
            top * 1.05,
        );
    }
    if let Some((col, row)) = starboard {
        let (cx, cy) = world(col, row);
        push_box(
            &mut nav_green,
            cx - s * 0.10,
            cx + s * 0.10,
            cy + s * 0.26,
            cy + s * 0.50,
            top * 0.60,
            top * 1.05,
        );
    }

    let mut out = Vec::new();
    for (buf, role) in [
        (metal, FixtureRole::Metal),
        (glow, FixtureRole::Glow),
        (nav_red, FixtureRole::NavRed),
        (nav_green, FixtureRole::NavGreen),
        (nav_white, FixtureRole::NavWhite),
        (accent, FixtureRole::Accent),
    ] {
        if let Some(m) = buf.into_mesh() {
            out.push((m, role));
        }
    }
    (out, thrusters)
}

// ============================================================================
// Fix #11 M2/M3 — smoothed marching-squares hull CONTOUR mesh (the "rounded look").
// An alternative to the blocky per-cell [`build_hull_mesh`], selectable at runtime via the
// `HullRenderMode` toggle (default OFF = the voxel mesh). It traces the cell set's boundary
// into grid-corner loop(s), rounds them with Chaikin corner-cutting, and fills the silhouette
// by ear-clipping — so a chunk reads as a rounded plate. M3 lifts the first-cut limitations:
// carved-through INTERIOR HOLES are now cut out of the top face (triangulated with holes via
// `earcutr`) and SIDE WALLS are dropped along every ring (the outer silhouette AND each hole),
// so a hole reads as a real walled cavity — matching what [`build_hull_mesh`] already does
// per cell.
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

/// R58 — trace the silhouette of per-cell SHAPE polygons (full squares + corner triangles) into ordered
/// grid-space loops (`x = col`, `y = row`, CCW around the material). Each cell emits its CCW polygon
/// edges; an edge is INTERNAL (culled) iff its REVERSE also exists (shared between two cells), so the
/// kept edges are the hull boundary — INCLUDING the diagonals of sub-shapes. Endpoints are quantized to
/// the half-grid (coords are multiples of 0.5) for exact matching; the kept edges link head-to-tail into
/// loops. (Full-cell hulls reproduce the axis-aligned silhouette; half-cells add clean 45° diagonals;
/// quarter junctions may have minor artifacts — a later refinement.)
fn cell_boundary_loops_shaped(cells: &[(u16, u16, sim::fitting::CellShape)]) -> Vec<Vec<Vec2>> {
    use std::collections::{HashMap, HashSet};
    let key = |v: Vec2| ((v.x * 2.0).round() as i32, (v.y * 2.0).round() as i32);

    // All directed CCW polygon edges + a set of their (start,end) keys for the reverse test.
    let mut edges: Vec<(Vec2, Vec2)> = Vec::new();
    let mut present: HashSet<((i32, i32), (i32, i32))> = HashSet::new();
    for &(c, r, shape) in cells {
        let poly = shape.corners(c, r);
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            edges.push((a, b));
            present.insert((key(a), key(b)));
        }
    }
    // Keep only boundary edges (reverse absent → not shared with a neighbour).
    let mut next: HashMap<(i32, i32), (Vec2, (i32, i32))> = HashMap::new();
    let mut start_pt: HashMap<(i32, i32), Vec2> = HashMap::new();
    for &(a, b) in &edges {
        if !present.contains(&(key(b), key(a))) {
            next.insert(key(a), (b, key(b)));
            start_pt.insert(key(a), a);
        }
    }
    // Link head-to-tail into closed loops (deterministic start order).
    let mut starts: Vec<(i32, i32)> = next.keys().copied().collect();
    starts.sort_unstable();
    let mut used: HashSet<(i32, i32)> = HashSet::new();
    let mut loops = Vec::new();
    for &s in &starts {
        if used.contains(&s) {
            continue;
        }
        let mut pts = Vec::new();
        let mut cur = s;
        while used.insert(cur) {
            if let Some(&p) = start_pt.get(&cur) {
                pts.push(p);
            }
            match next.get(&cur) {
                Some(&(_, nk)) => cur = nk,
                None => break,
            }
            if cur == s {
                break;
            }
        }
        if pts.len() >= 3 {
            loops.push(pts);
        }
    }
    loops
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

/// Triangulate a **CCW outer ring with CW hole rings** (holes cut out) via `earcutr` → index
/// triples into the COMBINED vertex list `[outer, holes[0], holes[1], …]` — the SAME order the
/// caller pushes positions, so `base + index` maps directly. Output triangles are forced CCW
/// (we set the top face's normals to `+Z`, and a CW triangle would be back-face-culled by the
/// top-down camera). Returns empty on an earcut failure (graceful).
fn ear_clip_with_holes(outer: &[Vec2], holes: &[Vec<Vec2>]) -> Vec<u32> {
    if outer.len() < 3 {
        return Vec::new();
    }
    // Flat `[x, y, x, y, …]` coords: outer first, then each hole in order. earcut keys holes off
    // `hole_indices` (the VERTEX index where each hole starts), not winding.
    let total: usize = outer.len() + holes.iter().map(|h| h.len()).sum::<usize>();
    let mut coords: Vec<f32> = Vec::with_capacity(total * 2);
    for p in outer {
        coords.push(p.x);
        coords.push(p.y);
    }
    let mut hole_indices: Vec<usize> = Vec::with_capacity(holes.len());
    let mut idx = outer.len();
    for h in holes {
        hole_indices.push(idx);
        for p in h {
            coords.push(p.x);
            coords.push(p.y);
        }
        idx += h.len();
    }
    let Ok(tris) = earcutr::earcut(&coords, &hole_indices, 2) else {
        return Vec::new();
    };
    let pt = |i: usize| Vec2::new(coords[i * 2], coords[i * 2 + 1]);
    let mut out = Vec::with_capacity(tris.len());
    for t in tris.chunks(3) {
        if t.len() < 3 {
            continue;
        }
        let (a, b, c) = (pt(t[0]), pt(t[1]), pt(t[2]));
        // Force CCW (front toward +Z): flip a CW triangle by swapping its last two indices.
        if (b - a).perp_dot(c - a) >= 0.0 {
            out.extend_from_slice(&[t[0] as u32, t[1] as u32, t[2] as u32]);
        } else {
            out.extend_from_slice(&[t[0] as u32, t[2] as u32, t[1] as u32]);
        }
    }
    out
}

/// Mean of a ring's vertices — used to test which outer loop a hole belongs to.
fn ring_centroid(ring: &[Vec2]) -> Vec2 {
    if ring.is_empty() {
        return Vec2::ZERO;
    }
    let mut c = Vec2::ZERO;
    for p in ring {
        c += *p;
    }
    c / ring.len() as f32
}

/// Even-odd ray-cast point-in-polygon. Assigns a hole loop to the outer loop that contains it
/// when a cell set has multiple disconnected bodies (rare — connectivity normally severs them
/// into separate entities, but transiently a render entity can hold several components).
fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (pi, pj) = (poly[i], poly[j]);
        if (pi.y > p.y) != (pj.y > p.y) {
            let x = pi.x + (p.y - pi.y) / (pj.y - pi.y) * (pj.x - pi.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Build the **smoothed contour** hull mesh for a cell set — the rounded-look alternative to
/// [`build_hull_mesh`] (Fix #11 M2/M3). Same `(cells, cell_size, center)` contract and the same
/// local frame (`x = forward ← row`, `y = lateral ← col`, scaled by `cell_size`, measured from
/// `center`), so it drops in at the same parent transform.
///
/// Traces the cell-set boundary into loops, classifies each by winding (after the col↔row swap
/// an OUTER loop is CW / negative area, a HOLE is CCW / positive), Chaikin-smooths each ring,
/// triangulates the outer-minus-holes top face at `z = HULL_THICKNESS` (`+Z`), and drops a SIDE
/// WALL (`z = 0 … HULL_THICKNESS`) along every ring so the silhouette has a lip and carved holes
/// read as real walled cavities. Wall normals fall out of one emission order: for corners
/// `[a_bot, b_bot, b_top, a_top]` the front face normal is `(dy, −dx)` — OUTWARD for the CCW
/// outer ring and INTO-the-cavity for the CW hole rings, so a single pattern serves both.
pub fn build_hull_mesh_contour(cells: &[(u16, u16, u8)], cell_size: f32, center: Vec2) -> Mesh {
    let present: std::collections::HashSet<(u16, u16)> =
        cells.iter().map(|&(c, r, _)| (c, r)).collect();
    let loops = cell_boundary_loops(&present);

    // Local-space rings, oriented canonically for earcut + walls: outer CCW, holes CW.
    let mut outers: Vec<Vec<Vec2>> = Vec::new();
    let mut holes: Vec<Vec<Vec2>> = Vec::new();
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
        if area2 < -1.0e-6 {
            pts.reverse(); // CW outer → CCW (top face toward +Z; outward wall normal)
            outers.push(pts);
        } else if area2 > 1.0e-6 {
            pts.reverse(); // CCW hole → CW (canonical for earcut; into-cavity wall normal)
            holes.push(pts);
        }
        // else: a degenerate (≈0 area) loop — skip.
    }

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Emit one CCW-from-its-normal quad (two triangles); same convention as `build_hull_mesh`.
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

    let single_outer = outers.len() == 1;
    for outer in &outers {
        // Holes inside this outer loop (single-body fast path = all of them).
        let my_holes: Vec<Vec<Vec2>> = if single_outer {
            holes.clone()
        } else {
            holes
                .iter()
                .filter(|h| point_in_polygon(ring_centroid(h), outer))
                .cloned()
                .collect()
        };

        let outer_s = chaikin_closed(outer, CONTOUR_CHAIKIN_ITERS);
        if outer_s.len() < 3 {
            continue;
        }
        let holes_s: Vec<Vec<Vec2>> = my_holes
            .iter()
            .map(|h| chaikin_closed(h, CONTOUR_CHAIKIN_ITERS))
            .filter(|h| h.len() >= 3)
            .collect();

        // TOP FACE: outer-minus-holes, flat at z = HULL_THICKNESS, normal +Z. Positions pushed
        // in the combined order `[outer, holes…]` that `ear_clip_with_holes` indexes into.
        let base = positions.len() as u32;
        for p in &outer_s {
            positions.push([p.x, p.y, HULL_THICKNESS]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([0.0, 0.0]);
        }
        for h in &holes_s {
            for p in h {
                positions.push([p.x, p.y, HULL_THICKNESS]);
                normals.push([0.0, 0.0, 1.0]);
                uvs.push([0.0, 0.0]);
            }
        }
        for t in ear_clip_with_holes(&outer_s, &holes_s) {
            indices.push(base + t);
        }

        // SIDE WALLS along every ring (outer silhouette + each hole). The (dy, −dx) normal is
        // outward for the CCW outer and into-the-cavity for the CW holes (see fn doc).
        for ring in std::iter::once(&outer_s).chain(holes_s.iter()) {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let d = b - a;
                let len = d.length();
                if len <= 1.0e-6 {
                    continue;
                }
                let nrm = Vec2::new(d.y, -d.x) / len;
                push_quad(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut indices,
                    [
                        [a.x, a.y, 0.0],
                        [b.x, b.y, 0.0],
                        [b.x, b.y, HULL_THICKNESS],
                        [a.x, a.y, HULL_THICKNESS],
                    ],
                    [nrm.x, nrm.y, 0.0],
                );
            }
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

/// How far above the hull top face (`z = HULL_THICKNESS`) the contour module markers float, so
/// they never z-fight the rounded surface beneath them.
const MODULE_OVERLAY_LIFT: f32 = 0.01;
/// Module marker quad size as a fraction of a cell (centred) — markers read as insets on the
/// smooth hull, not a gapless recolor.
const MODULE_OVERLAY_FRAC: f32 = 0.6;

/// Build the **module-color overlay** mesh (Fix #11 M3) for the contour ("rounded") look: one
/// small flat colored quad per MODULE cell (`kind != 0`), centred on the cell at
/// `z = HULL_THICKNESS + MODULE_OVERLAY_LIFT`, normal +Z, carrying the [`module_palette`] hue as
/// `ATTRIBUTE_COLOR`. Rendered as a SECOND child over the smooth uniform hull so modules read
/// crisp without fighting the rounded fill (the "multiple layers" approach). Returns `None` when
/// the cell set has no module cells (the caller then skips / tears down the overlay child). Uses
/// the same `(cells, cell_size, center)` local frame as the hull builders.
pub fn build_module_overlay_mesh(
    cells: &[(u16, u16, u8)],
    cell_size: f32,
    center: Vec2,
) -> Option<Mesh> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let z = HULL_THICKNESS + MODULE_OVERLAY_LIFT;
    let h = cell_size * 0.5 * MODULE_OVERLAY_FRAC;

    for &(col, row, kind) in cells {
        if kind == 0 {
            continue; // structural / empty plating — no marker
        }
        let cx = ((row as f32 + 0.5) - center.y) * cell_size;
        let cy = ((col as f32 + 0.5) - center.x) * cell_size;
        let color = color_rgba(module_palette(kind));
        let base = positions.len() as u32;
        positions.extend_from_slice(&[
            [cx - h, cy - h, z],
            [cx + h, cy - h, z],
            [cx + h, cy + h, z],
            [cx - h, cy + h, z],
        ]);
        for _ in 0..4 {
            normals.push([0.0, 0.0, 1.0]);
            colors.push(color);
        }
        uvs.push([0.0, 0.0]);
        uvs.push([1.0, 0.0]);
        uvs.push([1.0, 1.0]);
        uvs.push([0.0, 1.0]);
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    if indices.is_empty() {
        return None; // no module cells → no overlay
    }
    Some(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices)),
    )
}

/// Spawn lighting, the gunsight pip, and the LOCAL player ship; register the
/// shared runtime render assets (projectile + remote ship/target looks).
pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut hull_ext: ResMut<Assets<crate::hull_shader::HullMaterial>>,
) {
    // R53 — crisp shadow map; the scene is ship-sized so a high-res map + a tight cascade gives sharp
    // contact shadows that make the per-cell plate relief + fixtures read as 3-D even near-top-down.
    commands.insert_resource(bevy::light::DirectionalLightShadowMap { size: 4096 });

    // Lighting: a key directional light so PBR primitives read (ambient fill is attached to the camera
    // in `camera::setup_camera`). R53 — it now CASTS SHADOWS (`KeyLight` marker → `apply_ship_visuals`
    // live-tunes its shadows/illuminance/raking direction); a tight `CascadeShadowConfig` concentrates
    // the shadow-map resolution near the ship. The Transform here is a placeholder — `apply_ship_visuals`
    // re-aims it from the azimuth/elevation tuning on the first frame.
    commands.spawn((
        KeyLight,
        DirectionalLight {
            illuminance: 9000.0,
            shadows_enabled: true,
            shadow_normal_bias: 1.8,
            ..default()
        },
        bevy::light::CascadeShadowConfigBuilder {
            num_cascades: 2,
            minimum_distance: 0.1,
            maximum_distance: 120.0,
            first_cascade_far_bound: 30.0,
            ..default()
        }
        .build(),
        Transform::from_xyz(6.0, 8.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    // R48/R49 — a dim COOL fill light from roughly the opposite side so the unlit faces of the gritty
    // hull aren't pure black (cinematic key+fill); no shadows (it's a fill). Subtle top-down (you mostly
    // see the key-lit top face) — the fresnel rim is the real edge cue. Illuminance is live-tuned by
    // `apply_ship_visuals` via the `FillLight` marker.
    commands.spawn((
        FillLight,
        DirectionalLight {
            illuminance: 2200.0,
            color: Color::srgb(0.55, 0.7, 1.0),
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(-7.0, -6.0, 14.0).looking_at(Vec3::ZERO, Vec3::Y),
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
    // Unit cube — the far-LOD placeholder for a voxelized structure, scaled to its footprint by the
    // parent transform (Refinement 6).
    let lod_box_mesh = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let ship_material = materials.add(Color::srgb(0.30, 0.65, 1.0));

    // Per-kind remote target looks (dummies/asteroids/seeker now arrive over the
    // network; these mirror the original E002 scene meshes/colours).
    let dummy_mesh = meshes.add(Cuboid::new(1.4, 1.4, 1.4)); // reddish practice cube
    let dummy_material = materials.add(Color::srgb(0.75, 0.35, 0.30));
    let asteroid_mesh = meshes.add(Sphere::new(0.9)); // grey drifting rock
    let asteroid_material = materials.add(Color::srgb(0.55, 0.5, 0.45));
    let seeker_mesh = meshes.add(Cuboid::new(1.2, 0.6, 0.3)); // green seeker dart
    let seeker_material = materials.add(Color::srgb(0.35, 0.85, 0.40));
    // Mining-skirmish structures (Phase 1; Refinement 4): UNIT meshes scaled per-entity by the
    // structure's `RenderScale` (from `assets/content/scenario.ron`, carried over `RenderEntity.scale`)
    // so the on-screen size is data-driven. Faction tint is applied per-entity at draw time (Phase 2).
    let outpost_mesh = meshes.add(Cuboid::new(1.0, 1.0, 1.0)); // beefy refinery outpost (unit box)
    let outpost_material = materials.add(Color::srgb(0.46, 0.47, 0.53));
    let transport_mesh = meshes.add(Cuboid::new(1.0, 1.0, 1.0)); // industrial mining transport (unit box)
    let transport_material = materials.add(Color::srgb(0.55, 0.52, 0.46));
    let minenode_mesh = meshes.add(Sphere::new(0.5)); // central asteroid (unit-diameter sphere)
    let minenode_material = materials.add(Color::srgb(0.50, 0.46, 0.40));
    // Phase 2 faction tint materials (saturated team colours, slightly emissive so they read as
    // team identity under the top-down light).
    let faction_red_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.22, 0.20),
        emissive: LinearRgba::rgb(0.25, 0.03, 0.02),
        ..default()
    });
    let faction_blue_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.22, 0.42, 0.92),
        emissive: LinearRgba::rgb(0.03, 0.08, 0.30),
        ..default()
    });

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
    // R47 — sleeker hard-surface metal: higher metallic + lower roughness so the plate reads as a
    // polished sci-fi hull (sharper speculars that bloom) rather than matte paint.
    let hull_material = materials.add(StandardMaterial {
        base_color: HULL_COLOR,
        metallic: 0.85,
        perceptual_roughness: 0.35,
        ..default()
    });

    // Faction-tinted hull plates (Refinement 5): the SAME metallic plate, base shifted toward the
    // team colour so a factioned VOXEL hull (the carveable structures, and ships) reads red/blue at a
    // glance — a moderate steel tint, not flat paint. Used by the plain (non-module-colour) voxel look
    // in `sync_ship_hull`; the white-base module-colour view + wrecks keep their own materials.
    let faction_red_hull_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.22, 0.20),
        metallic: 0.85,
        perceptual_roughness: 0.35,
        ..default()
    });
    let faction_blue_hull_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.22, 0.34, 0.62),
        metallic: 0.85,
        perceptual_roughness: 0.35,
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

    // WHITE-base hull plates (Fix #11 M3): identical metal/roughness to the tinted plates above,
    // but a white base so per-cell module vertex colors show as-is (StandardMaterial computes
    // `vertex × base_color`). Used ONLY by the voxel look while module coloring is ON.
    let hull_material_white = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        metallic: 0.85,
        perceptual_roughness: 0.35,
        ..default()
    });
    let wreck_hull_material_white = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        metallic: 0.5,
        perceptual_roughness: 0.75,
        ..default()
    });

    // Contour module-marker overlay material (Fix #11 M3): white base so the markers' per-vertex
    // `module_palette` colors show as-is; lit like the hull (it floats just above the top face).
    let module_overlay_material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        metallic: 0.2,
        perceptual_roughness: 0.5,
        ..default()
    });

    // R47 — hard-surface FIXTURE materials. `fixture_metal` is dark polished gunmetal for the
    // structural greebles (gun barrels, nozzle housings, sensor dishes, shield nodes, the nose
    // canopy). `fixture_glow` is a bright HDR warm emissive (engine nozzle cores + reactor vents)
    // that blooms via the camera Bloom. Both are SHARED across all ships (the per-ship variation is
    // the fixture geometry built in `build_ship_fixtures`).
    let fixture_metal_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.10, 0.11, 0.13),
        metallic: 0.95,
        perceptual_roughness: 0.40,
        ..default()
    });
    let fixture_glow_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.30, 0.14, 0.05),
        // >1 emissive so the engine/reactor glow + aft exhaust plume bloom under the camera Bloom.
        emissive: LinearRgba::rgb(3.0, 1.3, 0.3),
        ..default()
    });

    // R48 — emissive nav/running lights (HDR so they bloom into bright dots).
    let mut nav_emissive = |r: f32, g: f32, b: f32| {
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.02, 0.02, 0.02),
            emissive: LinearRgba::rgb(r, g, b),
            ..default()
        })
    };
    let nav_red_material = nav_emissive(3.2, 0.05, 0.05);
    let nav_green_material = nav_emissive(0.05, 3.2, 0.12);
    let nav_white_material = nav_emissive(2.4, 2.4, 2.8);
    // R48 — faction-tinted accent (spine strip + canopy cap): neutral cool, team red, team blue.
    let accent_neutral_material = nav_emissive(0.6, 1.3, 2.4);
    let accent_red_material = nav_emissive(2.6, 0.35, 0.28);
    let accent_blue_material = nav_emissive(0.32, 1.0, 2.8);

    // R48 — the dynamic engine exhaust flame: a unit cone (axis +Y), additive emissive so it reads as
    // exhaust plasma; oriented + scaled per ship by `update_engine_exhaust`.
    let engine_flame_mesh = meshes.add(Cone::new(0.5, 1.0).mesh());
    let engine_flame_material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.55, 0.18, 1.0),
        emissive: LinearRgba::rgb(5.0, 2.2, 0.6),
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        double_sided: true,
        ..default()
    });

    // R48/R49/R52/R55 — the cinematic hull material per faction: PBR metal + the fresnel-rim extension,
    // rim-tinted neutral cool / team red / team blue. R52 dropped the R51 normal/ORM plating textures
    // (they faked relief → shimmered); the surface detail is now real geometry (the beveled hull,
    // `build_hull_mesh_beveled`). The rim params are live-tuned each frame by `apply_ship_visuals`.
    let make_hull_ext = |base: Color, rim: Vec4| crate::hull_shader::HullMaterial {
        base: StandardMaterial {
            base_color: base,
            metallic: 0.85,
            perceptual_roughness: 0.35,
            ..default()
        },
        extension: crate::hull_shader::hull_extension(rim),
    };
    let hull_ext_neutral = hull_ext.add(make_hull_ext(HULL_COLOR, Vec4::new(0.35, 0.6, 1.0, 0.9)));
    let hull_ext_red = hull_ext.add(make_hull_ext(
        Color::srgb(0.55, 0.22, 0.20),
        Vec4::new(1.4, 0.35, 0.30, 1.1),
    ));
    let hull_ext_blue = hull_ext.add(make_hull_ext(
        Color::srgb(0.22, 0.34, 0.62),
        Vec4::new(0.30, 0.6, 1.5, 1.1),
    ));

    // R50 — particle assets (shared; fade is via per-particle SCALE so one material per kind suffices).
    let particle_mesh = meshes.add(Sphere::new(0.5).mesh().ico(2).unwrap());
    let trail_material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.6, 0.25, 1.0),
        emissive: LinearRgba::rgb(2.4, 1.2, 0.4),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    let spark_material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.9, 0.6, 1.0),
        emissive: LinearRgba::rgb(5.0, 3.5, 1.6),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    let smoke_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.06, 0.06, 0.07, 0.6),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });

    commands.insert_resource(RenderAssets {
        projectile_mesh,
        projectile_material,
        ship_mesh: ship_mesh.clone(),
        ship_material: ship_material.clone(),
        lod_box_mesh,
        dummy_mesh,
        dummy_material,
        asteroid_mesh,
        asteroid_material,
        seeker_mesh,
        seeker_material,
        outpost_mesh,
        outpost_material,
        transport_mesh,
        transport_material,
        minenode_mesh,
        minenode_material,
        faction_red_material,
        faction_blue_material,
        shield_arc_mesh,
        shield_material,
        debris_mesh,
        debris_material,
        hull_material,
        faction_red_hull_material,
        faction_blue_hull_material,
        wreck_hull_material,
        hull_material_white,
        wreck_hull_material_white,
        module_overlay_material,
        fixture_metal_material,
        fixture_glow_material,
        nav_red_material,
        nav_green_material,
        nav_white_material,
        accent_neutral_material,
        accent_red_material,
        accent_blue_material,
        engine_flame_mesh,
        engine_flame_material,
        hull_ext_neutral,
        hull_ext_red,
        hull_ext_blue,
        particle_mesh,
        trail_material,
        spark_material,
        smoke_material,
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
    fn ear_clip_with_holes_fills_a_simple_square() {
        // CCW unit square, no holes → 2 triangles, area 1, all forced CCW (positive).
        let sq = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        let tris = ear_clip_with_holes(&sq, &[]);
        assert_eq!(tris.len(), 6, "a quad → 2 triangles");
        let mut total = 0.0;
        for t in tris.chunks(3) {
            let (a, b, c) = (sq[t[0] as usize], sq[t[1] as usize], sq[t[2] as usize]);
            let signed = (b - a).perp_dot(c - a) * 0.5;
            assert!(signed > 0.0, "triangles are forced CCW (front toward +Z)");
            total += signed;
        }
        assert!((total - 1.0).abs() < 1e-5);
    }

    #[test]
    fn ear_clip_with_holes_cuts_a_central_hole() {
        // CCW 4×4 outer with a CW 2×2 hole → filled area = 16 − 4 = 12.
        let outer = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(0.0, 4.0),
        ];
        let hole = vec![
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 3.0),
            Vec2::new(3.0, 3.0),
            Vec2::new(3.0, 1.0),
        ];
        assert!(
            signed_area2(&hole) < 0.0,
            "hole ring is CW (canonical for earcut)"
        );
        let tris = ear_clip_with_holes(&outer, std::slice::from_ref(&hole));
        assert!(!tris.is_empty(), "the hole-cut polygon triangulates");
        // Combined vertex list = outer then hole — the order the fn indexes into.
        let mut verts = outer.clone();
        verts.extend(hole.iter().copied());
        let mut total = 0.0;
        for t in tris.chunks(3) {
            let (a, b, c) = (
                verts[t[0] as usize],
                verts[t[1] as usize],
                verts[t[2] as usize],
            );
            total += (b - a).perp_dot(c - a).abs() * 0.5;
        }
        assert!(
            (total - 12.0).abs() < 1e-4,
            "outer 16 − hole 4 = 12 (got {total})"
        );
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
    fn contour_mesh_of_a_block_has_top_face_and_walls() {
        let cells: Vec<(u16, u16, u8)> = (0..3)
            .flat_map(|c| (0..3).map(move |r| (c, r, 0u8)))
            .collect();
        let mesh = build_hull_mesh_contour(&cells, 0.32, Vec2::new(1.5, 1.5));
        let pos = mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap();
        assert!(pos.len() >= 3, "a solid block contour has a filled face");
        if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ns)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        {
            // Top face points +Z (toward the top-down camera); side walls are horizontal.
            assert!(ns.iter().any(|n| n[2] > 0.5), "has an upward top face");
            assert!(
                ns.iter().any(|n| n[2].abs() < 1e-3),
                "has vertical side walls"
            );
        }
    }

    /// Total area of a contour mesh's TOP face (triangles whose 3 vertices all sit at
    /// `z = HULL_THICKNESS`) — lets a hole test assert the filled face shrank.
    fn top_face_area(mesh: &Mesh) -> f32 {
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(pos)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            return 0.0;
        };
        let Some(Indices::U32(idx)) = mesh.indices() else {
            return 0.0;
        };
        let mut area = 0.0;
        for t in idx.chunks(3) {
            let (p0, p1, p2) = (pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]);
            let is_top = |p: &[f32; 3]| (p[2] - HULL_THICKNESS).abs() < 1e-4;
            if is_top(&p0) && is_top(&p1) && is_top(&p2) {
                let a = Vec2::new(p0[0], p0[1]);
                let b = Vec2::new(p1[0], p1[1]);
                let c = Vec2::new(p2[0], p2[1]);
                area += (b - a).perp_dot(c - a).abs() * 0.5;
            }
        }
        area
    }

    /// Count a mesh's vertices sitting at `z == 0` (every side-wall quad contributes two) — a
    /// proxy for "how many side walls were emitted".
    fn wall_floor_verts(mesh: &Mesh) -> usize {
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) => {
                ps.iter().filter(|p| p[2].abs() < 1e-4).count()
            }
            _ => 0,
        }
    }

    #[test]
    fn contour_with_interior_hole_cuts_a_hole() {
        // A 5×5 block, solid vs. with the centre cell (2,2) carved out.
        let solid_cells: Vec<(u16, u16, u8)> = (0..5)
            .flat_map(|c| (0..5).map(move |r| (c, r, 0u8)))
            .collect();
        let holed_cells: Vec<(u16, u16, u8)> = solid_cells
            .iter()
            .copied()
            .filter(|&(c, r, _)| !(c == 2 && r == 2))
            .collect();

        // The carved set has TWO boundary loops (outer silhouette + the hole).
        let present: std::collections::HashSet<(u16, u16)> =
            holed_cells.iter().map(|&(c, r, _)| (c, r)).collect();
        assert_eq!(
            cell_boundary_loops(&present).len(),
            2,
            "outer silhouette + one interior hole"
        );

        let solid = build_hull_mesh_contour(&solid_cells, 0.32, Vec2::new(2.5, 2.5));
        let holed = build_hull_mesh_contour(&holed_cells, 0.32, Vec2::new(2.5, 2.5));
        let (solid_area, holed_area) = (top_face_area(&solid), top_face_area(&holed));
        assert!(holed_area > 0.0, "holed hull still has a filled face");
        assert!(
            holed_area < solid_area,
            "carving an interior cell removes top-face area (solid {solid_area}, holed {holed_area})"
        );
    }

    #[test]
    fn contour_emits_side_walls() {
        let cells: Vec<(u16, u16, u8)> = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(c, r)| (c, r, 0u8))
            .collect();
        let mesh = build_hull_mesh_contour(&cells, 0.32, Vec2::new(1.0, 1.0));
        if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        {
            assert!(
                ps.iter().any(|p| p[2].abs() < 1e-4),
                "wall bottoms at z = 0"
            );
            assert!(
                ps.iter().any(|p| (p[2] - HULL_THICKNESS).abs() < 1e-4),
                "tops at z = HULL_THICKNESS"
            );
        } else {
            panic!("contour mesh has no positions");
        }
    }

    #[test]
    fn voxel_hole_is_walled() {
        // Regression lock on the ALREADY-working voxel path: carving the centre of a 3×3 adds
        // interior walls (each of the 4 cells around the hole walls its edge facing the gap).
        let solid: Vec<(u16, u16, u8)> = (0..3)
            .flat_map(|c| (0..3).map(move |r| (c, r, 0u8)))
            .collect();
        let holed: Vec<(u16, u16, u8)> = solid
            .iter()
            .copied()
            .filter(|&(c, r, _)| !(c == 1 && r == 1))
            .collect();
        let solid_walls =
            wall_floor_verts(&build_hull_mesh(&solid, 0.32, Vec2::new(1.5, 1.5), false));
        let holed_walls =
            wall_floor_verts(&build_hull_mesh(&holed, 0.32, Vec2::new(1.5, 1.5), false));
        assert!(
            holed_walls > solid_walls,
            "the interior hole adds side walls (solid {solid_walls}, holed {holed_walls})"
        );
    }

    #[test]
    fn module_palette_is_distinct_per_kind() {
        // Structural (0) reuses the hull color; the seven module kinds are pairwise distinct.
        assert_eq!(module_palette(0), HULL_COLOR);
        let kinds = [0u8, 1, 2, 3, 4, 5, 6, 7];
        for (i, &a) in kinds.iter().enumerate() {
            for &b in &kinds[i + 1..] {
                assert_ne!(
                    module_palette(a),
                    module_palette(b),
                    "kinds {a} and {b} must have distinct colors"
                );
            }
        }
    }

    /// Distinct linear vertex colors present in a mesh's `ATTRIBUTE_COLOR` (rounded so f32 jitter
    /// doesn't split a color into near-duplicates).
    fn distinct_vertex_colors(mesh: &Mesh) -> std::collections::HashSet<[i32; 4]> {
        let mut set = std::collections::HashSet::new();
        if let Some(bevy::mesh::VertexAttributeValues::Float32x4(cs)) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        {
            for c in cs {
                set.insert([
                    (c[0] * 1000.0) as i32,
                    (c[1] * 1000.0) as i32,
                    (c[2] * 1000.0) as i32,
                    (c[3] * 1000.0) as i32,
                ]);
            }
        }
        set
    }

    #[test]
    fn voxel_mesh_carries_vertex_colors() {
        // Two cells of different module kinds → ATTRIBUTE_COLOR present, one entry per position,
        // and at least two distinct colors when module coloring is ON.
        let cells = [(0u16, 0u16, 1u8), (1, 0, 3)]; // reactor + weapon
        let mesh = build_hull_mesh(&cells, 0.32, Vec2::new(1.0, 0.5), true);
        let pos_len = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) => p.len(),
            _ => panic!("no positions"),
        };
        let col_len = match mesh.attribute(Mesh::ATTRIBUTE_COLOR) {
            Some(bevy::mesh::VertexAttributeValues::Float32x4(c)) => c.len(),
            _ => panic!("ATTRIBUTE_COLOR must always be present"),
        };
        assert_eq!(col_len, pos_len, "one vertex color per position");
        assert!(
            distinct_vertex_colors(&mesh).len() >= 2,
            "mixed module kinds give distinct colors when coloring is on"
        );
    }

    #[test]
    fn voxel_mesh_color_off_is_uniform_white() {
        // Same mixed-kind cells with coloring OFF → ATTRIBUTE_COLOR still present, all white
        // (the material's own hull/wreck tint shows through; no pipeline flip-flop).
        let cells = [(0u16, 0u16, 1u8), (1, 0, 3)];
        let mesh = build_hull_mesh(&cells, 0.32, Vec2::new(1.0, 0.5), false);
        let colors = distinct_vertex_colors(&mesh);
        assert_eq!(
            colors.len(),
            1,
            "all vertices one color when coloring is off"
        );
        assert_eq!(
            colors.into_iter().next().unwrap(),
            [1000, 1000, 1000, 1000],
            "the single color is white"
        );
    }

    /// Positions of a mesh as a flat `Vec<[f32;3]>` (for equality checks).
    fn positions_of(mesh: &Mesh) -> Vec<[f32; 3]> {
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) => p.clone(),
            _ => panic!("no positions"),
        }
    }

    /// Count of vertices whose normal points +Z (the cell TOP face).
    fn top_face_verts(mesh: &Mesh) -> usize {
        match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(n)) => {
                n.iter().filter(|nv| nv[2] > 0.5).count()
            }
            _ => panic!("no normals"),
        }
    }

    #[test]
    fn shaped_voxel_builder_keeps_full_identical_and_renders_real_polygons() {
        use sim::fitting::CellShape;
        let center = Vec2::new(1.0, 1.0);
        // A `Full` cell via the shaped builder is BYTE-IDENTICAL to the legacy box builder.
        let legacy = build_hull_mesh(&[(0u16, 0u16, 0u8)], 0.32, center, false);
        let shaped_full =
            build_hull_mesh_shaped(&[(0u16, 0u16, 0u8, CellShape::Full)], 0.32, center, false);
        assert_eq!(
            positions_of(&legacy),
            positions_of(&shaped_full),
            "Full cells must render identically via the shaped builder"
        );
        assert_eq!(
            top_face_verts(&shaped_full),
            4,
            "a Full top face is a square (4 verts)"
        );
        // A HalfNE (triangle) cell has a 3-vertex top face → fewer verts than the square box.
        let half =
            build_hull_mesh_shaped(&[(0u16, 0u16, 0u8, CellShape::HalfNE)], 0.32, center, false);
        assert_eq!(
            top_face_verts(&half),
            3,
            "a HalfNE top face is one triangle (3 verts)"
        );
        assert!(
            positions_of(&half).len() < positions_of(&shaped_full).len(),
            "a triangle cell has fewer vertices than a full square box"
        );
        // A ChamferNE (pentagon) top face has 5 vertices; a SlopeNEH (trapezoid) has 4.
        let cham = build_hull_mesh_shaped(
            &[(0u16, 0u16, 0u8, CellShape::ChamferNE)],
            0.32,
            center,
            false,
        );
        assert_eq!(
            top_face_verts(&cham),
            5,
            "a chamfer top face is a pentagon (5 verts)"
        );
        let slope = build_hull_mesh_shaped(
            &[(0u16, 0u16, 0u8, CellShape::SlopeNEH)],
            0.32,
            center,
            false,
        );
        assert_eq!(
            top_face_verts(&slope),
            4,
            "a slope top face is a trapezoid (4 verts)"
        );
    }

    #[test]
    fn module_overlay_marks_only_module_cells() {
        // A 2×2 with one module cell (kind 4) and three structural → exactly one marker quad
        // (4 positions, 6 indices); palette color = shield cyan.
        let cells = [(0u16, 0u16, 0u8), (1, 0, 0), (0, 1, 4), (1, 1, 0)];
        let mesh = build_module_overlay_mesh(&cells, 0.32, Vec2::new(1.0, 1.0))
            .expect("one module cell → an overlay mesh");
        match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) => {
                assert_eq!(p.len(), 4, "one quad per module cell");
            }
            _ => panic!("no positions"),
        }
        // All-structural cells → no overlay at all.
        let plating = [(0u16, 0u16, 0u8), (1, 0, 0)];
        assert!(
            build_module_overlay_mesh(&plating, 0.32, Vec2::new(0.5, 0.5)).is_none(),
            "no module cells → no overlay mesh"
        );
    }

    #[test]
    fn trapezoid_mesh_is_bottom_anchored_and_camera_facing() {
        // bottom_w 2, top_w 1, height 3 → a quad (4 verts, 2 tris), bottom edge on y=0
        // spanning ±1, top edge at y=3 spanning ±0.5, every normal +Z.
        let mesh = build_trapezoid_mesh(1.0, 2.0, 3.0);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("trapezoid has no positions");
        };
        assert_eq!(ps.len(), 4, "trapezoid is one quad");
        // Bottom edge anchored on y = 0 (so scale.y grows it upward); top at y = height.
        assert!(
            ps.iter().filter(|p| p[1].abs() < 1e-6).count() == 2,
            "two bottom verts on y=0"
        );
        assert!(
            ps.iter().filter(|p| (p[1] - 3.0).abs() < 1e-6).count() == 2,
            "two top verts at y=3"
        );
        // Widths: bottom spans ±1 (bottom_w/2), top spans ±0.5 (top_w/2).
        let bottom_x: Vec<f32> = ps
            .iter()
            .filter(|p| p[1].abs() < 1e-6)
            .map(|p| p[0])
            .collect();
        let top_x: Vec<f32> = ps
            .iter()
            .filter(|p| (p[1] - 3.0).abs() < 1e-6)
            .map(|p| p[0])
            .collect();
        assert!(
            bottom_x.iter().any(|&x| (x + 1.0).abs() < 1e-6)
                && bottom_x.iter().any(|&x| (x - 1.0).abs() < 1e-6)
        );
        assert!(
            top_x.iter().any(|&x| (x + 0.5).abs() < 1e-6)
                && top_x.iter().any(|&x| (x - 0.5).abs() < 1e-6)
        );
        if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ns)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        {
            assert!(
                ns.iter().all(|n| n[2] > 0.99),
                "all normals +Z (face the top-down camera)"
            );
        }
        // 2 triangles.
        match mesh.indices() {
            Some(Indices::U32(i)) => assert_eq!(i.len(), 6, "two triangles"),
            _ => panic!("trapezoid has no U32 indices"),
        }
    }

    #[test]
    fn h_trapezoid_is_bottom_anchored_and_tapers_right() {
        // left_h 1.0, right_h 0.5, width 2 → flat bottom on y=0 spanning ±1; left edge taller than
        // the right edge (tapers toward the right); +Z normals.
        let mesh = build_trapezoid_mesh_h(1.0, 0.5, 2.0);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(ps)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("h-trapezoid has no positions");
        };
        assert_eq!(ps.len(), 4, "one quad");
        // Two bottom verts flat on y = 0.
        assert_eq!(
            ps.iter().filter(|p| p[1].abs() < 1e-6).count(),
            2,
            "flat bottom baseline on y=0"
        );
        // The left edge (x = -1) is taller than the right edge (x = +1).
        let left_top = ps
            .iter()
            .filter(|p| (p[0] + 1.0).abs() < 1e-6)
            .map(|p| p[1])
            .fold(0.0_f32, f32::max);
        let right_top = ps
            .iter()
            .filter(|p| (p[0] - 1.0).abs() < 1e-6)
            .map(|p| p[1])
            .fold(0.0_f32, f32::max);
        assert!(
            (left_top - 1.0).abs() < 1e-6 && (right_top - 0.5).abs() < 1e-6,
            "left edge tall ({left_top}), right edge short ({right_top}) — tapers right"
        );
        if let Some(bevy::mesh::VertexAttributeValues::Float32x3(ns)) =
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
        {
            assert!(ns.iter().all(|n| n[2] > 0.99), "all normals +Z");
        }
        match mesh.indices() {
            Some(Indices::U32(i)) => assert_eq!(i.len(), 6, "two triangles"),
            _ => panic!("h-trapezoid has no U32 indices"),
        }
    }
}
