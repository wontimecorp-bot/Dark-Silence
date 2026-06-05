//! Render-sync (ADR-0013, FR-004): the fixed-step `sim` state is mirrored into
//! interpolated Bevy `Transform`s so motion is smooth and frame-rate
//! independent. Also attaches visuals to projectiles the sim spawns at runtime.
//!
//! E003 OBJ4 renders the **windowed solo client** directly from the embedded
//! authoritative server's world (zero loopback latency makes the
//! predict/interpolate netcode a feel regression there). Every rendered entity —
//! the local ship, targets, and projectiles alike — carries a [`RenderInterp`]
//! whose prev/curr poses [`crate::net::capture_render_state`] rolls from the
//! server's [`server::ServerApp::render_state`] each fixed step, and
//! [`interpolate_transforms`] blends into the `Transform` each frame (E002's
//! smooth fixed-step interpolation). Non-local rendered entities additionally
//! carry a [`RemoteEntity`] tag keyed by their stable network [`EntityId`] so the
//! capture system can find/despawn them by id.
//!
//! (The snapshot-*interpolation* path — [`crate::interpolation`] — and client-side
//! *prediction* — [`crate::prediction`] — are unchanged and remain the path real
//! *remote* multiplayer uses; the windowed solo path does not run them.)
//!
//! The E002 gunsight pip and follow camera continue to read the local ship's
//! rendered `Transform`, so their feel is unchanged.

use std::collections::BTreeMap;

use bevy::prelude::*;
use protocol::{EntityId, EntityKind};
use sim::components::{Energy, Heading, Heat, Position, Projectile, Ship};
use sim::fitting::ShipStats;
use sim::SimTuning;

use crate::net::{LoopbackHost, NetClientState};
use crate::scene::RenderAssets;

/// The gunsight pip's normal colour (cyan) + its **can't-fire** colour (red) — used by
/// [`update_aim_pip`] to redden the reticle when the player can't sustain fire (Phase F: energy
/// below a shot's cost OR overheated), the eye-on-target counterpart to the energy/heat bars.
const AIM_PIP_READY: (Color, LinearRgba) =
    (Color::srgb(0.4, 1.0, 0.9), LinearRgba::rgb(0.2, 1.0, 0.8));
const AIM_PIP_BLOCKED: (Color, LinearRgba) = (
    Color::srgb(1.0, 0.28, 0.22),
    LinearRgba::rgb(1.3, 0.12, 0.08),
);

/// How far ahead of the ship's nose the gunsight pip sits, in sim units.
const AIM_DISTANCE: f32 = 5.0;

/// Tags a rendered entity as a **non-local** entity (a target, projectile, or
/// other ship), distinct from the local player's [`crate::net::LocalShip`]. Keyed
/// by its stable network [`EntityId`] so [`crate::net::capture_render_state`] can
/// find/update/despawn it across ticks as the authoritative
/// [`server::ServerApp::render_state`] changes. Like the local ship it renders via
/// its [`RenderInterp`] + [`interpolate_transforms`]; the tag exists only to
/// id-key it in the [`crate::net::NetRenderMap`].
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteEntity {
    /// Stable network id, matched to the per-frame interpolated set.
    pub id: EntityId,
    /// What kind of remote it is (picks the prefab/visual).
    pub kind: EntityKind,
}

/// Marker for the forward gunsight pip — a world-space marker placed ahead of
/// the ship along its heading, showing where the fixed weapon will fire.
#[derive(Component)]
pub struct AimPip;

/// Links a rendered ship to its shield-flash child entity (E007 live-demo) and holds
/// the per-bubble material handle so [`crate::net::capture_render_state`] can fade the
/// flash alpha each tick. The child is spawned **once** (lazily, the first tick the
/// ship is processed) and despawned with its parent (Bevy despawns children
/// recursively).
#[derive(Component, Clone, Debug)]
pub struct ShieldChild {
    /// The shield-bubble child entity.
    pub entity: Entity,
    /// The bubble's own (cloned) material, whose `base_color` alpha is faded with the
    /// shield-hit flash each tick.
    pub material: Handle<StandardMaterial>,
}

/// Marker for a shield-flash child entity (E007 live-demo) — the glowing cyan **arc
/// segment of the shield ring** ([`crate::scene::build_arc_band_mesh`]) parented to a
/// rendered ship and rotated about Z to face the impact bearing. Tagged so its
/// [`Visibility`] can be toggled by [`crate::net::capture_render_state`] (alpha is faded
/// via its material).
#[derive(Component, Clone, Copy, Debug)]
pub struct ShieldBubble;

/// Revise-B seamless hull-surface tracking on a rendered fitted ship's PARENT entity.
///
/// When a fitted ship is **near** (the camera-distance LOD gate, see
/// [`crate::net::SHIP_VOXEL_LOD_DIST`]) it is drawn as the interpolated parent transform
/// PLUS exactly ONE child holding the merged hull-surface mesh
/// ([`crate::scene::build_hull_mesh`]) + the single [`RenderAssets::hull_material`] — so
/// the whole ship reads as one solid steel plate with NO visible cells (this REPLACES the
/// old per-cell-box voxel rendering). This tracks that single child, its [`Mesh`] handle
/// (so the handle is freed from [`Assets<Mesh>`] on despawn/rebuild and meshes do not leak
/// over a long session), and a cheap hash of the present cell set.
///
/// **Rebuild-on-cell-set-change hook (the Phase-2 erosion seam).** `cells_hash` is a cheap
/// order-independent hash of the present `(col, row, kind)` set. [`crate::net::capture_render_state`]
/// recomputes it from the server's cell payload each tick and rebuilds the hull mesh ONLY
/// when it changes. In revise-B (no destruction) the set never changes, so the mesh builds
/// **once on first sight** and the per-tick check is a no-op. In Phase 2, carving drops a
/// cell from `FitLayout.cells` → it drops from the payload → the hash changes → the mesh is
/// rebuilt with the hole (and side walls along the new breach edges), and the hull visibly
/// erodes with no further plumbing.
///
/// `voxelized` records the current LOD state so the capture system switches cleanly when
/// the ship crosses the distance threshold (near → build the hull-mesh child + drop the
/// parent's box mesh; far → despawn the child + free its mesh + restore the box mesh).
/// One spatial TILE of a BIG structure's **chunked** hull (the chunked-mesh optimization): the hull
/// is split into `HULL_TILE`-cell squares so a carve rebuilds only the tile(s) it touched, not the
/// whole ~8k-cell hull. Keyed in [`ShipHull::tiles`] by `(col / HULL_TILE, row / HULL_TILE)`.
#[derive(Default)]
pub struct HullTile {
    /// Order-independent hash of this tile's present `(col, row, kind)` cells — the per-tile rebuild
    /// trigger.
    pub hash: u64,
    /// This tile's mesh handle (freed from `Assets<Mesh>` on rebuild/despawn — no leak).
    pub mesh: Option<Handle<Mesh>>,
    /// This tile's child entity under the parent (despawned on rebuild / when the tile empties / far).
    pub child: Option<Entity>,
}

#[derive(Component, Default)]
pub struct ShipHull {
    /// Per-tile chunked hull meshes for a BIG structure (empty for a ship / a small hull). A carve
    /// rebuilds only the affected tile, not the whole hull. Mutually exclusive with the single
    /// `child`/`mesh` below — a big structure uses `tiles`; a ship uses the single child.
    pub tiles: BTreeMap<(u16, u16), HullTile>,
    /// The single hull-surface child entity (`None` until first built / while far).
    pub child: Option<Entity>,
    /// The hull mesh's handle, kept so it can be removed from [`Assets<Mesh>`] when the
    /// child is despawned or the mesh is rebuilt (no per-session mesh leak).
    pub mesh: Option<Handle<Mesh>>,
    /// Cheap order-independent hash of the present `(col, row, kind)` cell set the current
    /// mesh was built from — the rebuild-on-change trigger (Phase-2 erosion seam).
    pub cells_hash: u64,
    /// Whether this ship is currently drawn as the merged hull mesh (`true`, near) or the
    /// single coarse box (`false`, far / not yet built).
    pub voxelized: bool,
    /// Which hull mesh style the current child was built in: `true` = the smoothed contour
    /// (Fix #11 M2), `false` = the blocky per-cell voxel mesh. Compared against the live
    /// `HullRenderMode` so flipping the runtime toggle forces a one-shot rebuild in the new
    /// style (the cell set is unchanged, so `cells_hash` alone wouldn't trigger it).
    pub built_contour: bool,
    /// Whether the current child was built with module coloring ON (Fix #11 M3). Compared
    /// against the live `ModuleColorMode` so flipping the `C` toggle forces a rebuild (in voxel
    /// mode the vertex colors change; in contour mode the module-marker overlay child appears /
    /// disappears).
    pub built_module_color: bool,
    /// The contour module-color OVERLAY child entity (Fix #11 M3) — the thin colored markers on
    /// module cells drawn OVER the smooth contour hull. `None` unless in contour mode with module
    /// coloring on and the hull has at least one module cell. Despawned + freed alongside the hull
    /// child (rebuild / LOD-far).
    pub module_overlay_child: Option<Entity>,
    /// The overlay mesh's handle, kept so it is removed from [`Assets<Mesh>`] on rebuild / despawn
    /// (no per-session mesh leak), mirroring [`ShipHull::mesh`].
    pub module_overlay_mesh: Option<Handle<Mesh>>,
}

/// Previous + current sim snapshots for one entity. `interpolate_transforms`
/// blends between them by the fixed-step overstep fraction.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RenderInterp {
    pub prev_pos: Vec2,
    pub curr_pos: Vec2,
    pub prev_heading: f32,
    pub curr_heading: f32,
}

impl RenderInterp {
    /// Both snapshots set to the same pose (no interpolation on the first frame).
    pub fn snapped(pos: Vec2, heading: f32) -> Self {
        Self {
            prev_pos: pos,
            curr_pos: pos,
            prev_heading: heading,
            curr_heading: heading,
        }
    }
}

/// `FixedUpdate`, last in the chain: roll current → previous, then capture the
/// new sim pose. Heading is optional (only the ship has one).
pub fn capture_sim_state(mut q: Query<(&Position, Option<&Heading>, &mut RenderInterp)>) {
    for (pos, heading, mut interp) in &mut q {
        interp.prev_pos = interp.curr_pos;
        interp.prev_heading = interp.curr_heading;
        interp.curr_pos = pos.0;
        if let Some(h) = heading {
            interp.curr_heading = h.0;
        }
    }
}

/// `Update`: blend the rendered `Transform` between the two latest fixed-step
/// poses by the fixed-timestep overstep fraction — frame-rate-independent feel.
///
/// The ship transform is position + rotation ONLY — there is NO scale animation. The
/// old E007 hit-pop scale-pulse (`1.0 + 0.4*flash`, the "zoom in and out" the user
/// disliked) was removed; the only damage visual is now the brief cyan shield
/// deflector flash (driven separately via the shield-bubble child's material alpha).
pub fn interpolate_transforms(
    fixed: Res<Time<Fixed>>,
    mut q: Query<(&RenderInterp, &mut Transform)>,
) {
    let alpha = fixed.overstep_fraction();
    for (interp, mut tf) in &mut q {
        let p = interp.prev_pos.lerp(interp.curr_pos, alpha);
        let h = lerp_angle(interp.prev_heading, interp.curr_heading, alpha);
        tf.translation.x = p.x;
        tf.translation.y = p.y;
        tf.rotation = Quat::from_rotation_z(h);
    }
}

/// Attach a mesh/material/transform (and a render-interp snapshot) to any
/// projectile the sim has spawned but that has no visual yet.
pub fn add_projectile_visuals(
    mut commands: Commands,
    assets: Res<RenderAssets>,
    q: Query<(Entity, &Position), (With<Projectile>, Without<Mesh3d>)>,
) {
    for (entity, pos) in &q {
        commands.entity(entity).insert((
            Mesh3d(assets.projectile_mesh.clone()),
            MeshMaterial3d(assets.projectile_material.clone()),
            Transform::from_xyz(pos.0.x, pos.0.y, 0.0),
            RenderInterp::snapped(pos.0, 0.0),
        ));
    }
}

/// Keep the gunsight pip a fixed distance ahead of the ship's nose, along the
/// (interpolated) heading — so it shows the actual firing line for the fixed
/// forward weapon. Runs after `interpolate_transforms` so it reads the smoothed
/// ship pose.
///
/// **Phase F — can't-fire tint:** the pip reddens when the local ship can't sustain fire, using the
/// SAME gate the sim's `weapon_fire_system` applies (`energy.current >= shot_cost && heat.current <
/// heat.max`, with `shot_cost = weapon.damage · weapon_energy_per_damage`), read from the embedded
/// server world. The eye is already on the reticle, so this registers "no shot" without looking away
/// at the bars. Ships with no weapon / no Energy+Heat pools stay the ready cyan.
pub fn update_aim_pip(
    ship_q: Query<&Transform, (With<Ship>, Without<AimPip>)>,
    mut pip_q: Query<(&mut Transform, &MeshMaterial3d<StandardMaterial>), With<AimPip>>,
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(ship) = ship_q.single() else {
        return;
    };
    let forward = ship.rotation * Vec3::X; // ship nose is +X local

    // Resolve "can't fire now" from the live pools via the exact sim gate (false when the ship /
    // weapon / pools are absent — e.g. before spawn, or an unarmed ship).
    let blocked = match (host.as_ref(), net.as_ref()) {
        (Some(host), Some(net)) => host
            .server
            .ship_entity_for(net.local_id)
            .map(|e| {
                let w = host.server.world();
                let sim = w.get_resource::<SimTuning>().copied().unwrap_or_default();
                match (w.get::<ShipStats>(e), w.get::<Energy>(e), w.get::<Heat>(e)) {
                    (Some(stats), Some(energy), Some(heat)) => match stats.weapon {
                        Some(weapon) => {
                            let shot_cost = weapon.damage * sim.weapon_energy_per_damage;
                            energy.current < shot_cost || heat.current >= heat.max
                        }
                        None => false,
                    },
                    _ => false,
                }
            })
            .unwrap_or(false),
        _ => false,
    };
    let (base, emissive) = if blocked {
        AIM_PIP_BLOCKED
    } else {
        AIM_PIP_READY
    };

    for (mut pip, mat) in &mut pip_q {
        pip.translation = ship.translation + forward * AIM_DISTANCE;
        if let Some(m) = materials.get_mut(&mat.0) {
            m.base_color = base;
            m.emissive = emissive;
        }
    }
}

/// Shortest-path angular interpolation.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let diff = (b - a + PI).rem_euclid(TAU) - PI;
    a + diff * t
}
