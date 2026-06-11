//! R99 Phase B (client) — dev-panel "Command Mode": RTS-style in-world clicking
//! to command allied AI ships, plus a selection-highlight ring.
//!
//! **CLIENT-ONLY, dev-gated.** Every system here is behind `#[cfg(feature =
//! "dev_panel")]` (the whole module is only compiled in then, see `lib.rs`), so
//! `--no-default-features` leaves the game unchanged. The picking + commanding
//! consume the Phase-A sim [`PlayerOrder`](sim::ai::PlayerOrder) component; they
//! do not alter sim/server behaviour.
//!
//! **Two-worlds discipline (the trap).** The client embeds the authoritative sim
//! as a `NonSend` [`LoopbackHost`]; there are TWO ECS worlds with DIFFERENT
//! `Entity` ids — the Bevy App world (render entities, the [`MainCamera`]) and the
//! embedded SERVER world ([`LoopbackHost::server`]'s `world()`/`world_mut()`,
//! which holds `AiBrain`, `AiStableId`, authoritative `Faction`, and where
//! `PlayerOrder` MUST be inserted). So: the cursor's WORLD point is computed from
//! the App-world camera, but ALL ship picking, faction classification, and order
//! application happen in SERVER-world space. A render-world `Entity` is never
//! assumed to equal a server `Entity`.
//!
//! **Selection is shared.** There is ONE selection: the dev panel's existing
//! `DevPanelState::ai_selected` (a parseable `u64` [`AiStableId`]). An in-world
//! left-click and the dev-panel inspection list both write it, so they never
//! drift apart.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::EguiContexts;
use glam::Vec2;
use sim::ai::{AiBrain, AiStableId, PlayerOrder, PlayerShip};
use sim::components::{Faction, Position};

use crate::camera::MainCamera;
use crate::dev_panel::DevPanelState;
use crate::net::{LoopbackHost, NetClientState};

/// In-world pick radius (sim/world units): a click within this distance of a
/// ship's centre selects/targets it. Sized generously so clicking is forgiving at
/// the top-down zoom (a fighter's footprint is only a couple of units across).
const PICK_RADIUS: f32 = 12.0;

/// Z of the selection ring, just UNDER the ship plane (ships render at `z = 0`) so
/// it reads as a ground ring rather than clipping the hull.
const RING_Z: f32 = -0.05;

/// Resolve a sim [`AiStableId`] value to its SERVER-world [`Entity`]. Stable,
/// allocation-light scan over `(Entity, &AiStableId)` — stable ids are unique, so
/// the first match is THE ship. Reused by the picking, ring, and dev-panel command
/// controls so they all resolve the shared `ai_selected` the same way.
pub fn server_entity_for_stable_id(world: &mut World, id: u64) -> Option<Entity> {
    let mut q = world.query::<(Entity, &AiStableId)>();
    q.iter(world).find(|(_, sid)| sid.0 == id).map(|(e, _)| e)
}

/// Merge a new nav `kind` into the ship's existing [`PlayerOrder`], PRESERVING any
/// style/posture overrides the user set, then re-insert it. Reading-then-cloning
/// the current order (or `settings_only()` when absent) is what keeps a previously
/// chosen profile/stance/posture alive across a fresh move/attack command.
fn merge_kind(world: &mut World, ship: Entity, new: PlayerOrder) {
    let mut order = world
        .get::<PlayerOrder>(ship)
        .cloned()
        .unwrap_or_else(PlayerOrder::settings_only);
    order.kind = new.kind;
    world.entity_mut(ship).insert(order);
}

/// Cursor → WORLD point on the ship plane (`z = 0`), tilt-correct.
///
/// The follow camera may be PITCHED (R53 `camera_tilt_deg`), so a naive
/// "screen → top-down" mapping is wrong. Instead we cast the cursor ray with
/// [`Camera::viewport_to_world`] (which honours the camera's real orientation) and
/// intersect it with the `z = 0` plane: `t = -origin.z / dir.z`, `world = origin +
/// t·dir`. `None` if there is no cursor, it is off-window, or the ray is parallel
/// to the plane (degenerate).
fn cursor_world_point(camera: &Camera, cam_tf: &GlobalTransform, window: &Window) -> Option<Vec2> {
    let cursor = window.cursor_position()?;
    let ray = camera.viewport_to_world(cam_tf, cursor).ok()?;
    let dir = ray.direction.as_vec3();
    if dir.z.abs() < f32::EPSILON {
        return None; // Ray parallel to the ship plane — no intersection.
    }
    let t = -ray.origin.z / dir.z;
    if t < 0.0 {
        return None; // Plane is behind the camera.
    }
    let world = ray.origin + t * dir;
    Some(Vec2::new(world.x, world.y))
}

/// Nearest ALLIED AI ship to `point` within [`PICK_RADIUS`] in the SERVER world: an
/// `AiBrain` carrier whose `Faction == player_faction`, EXCLUDING the player ship
/// (the [`PlayerShip`] marker). Returns its [`AiStableId`] value (the selection
/// token). Stable iteration with a distance-then-stable-id tiebreak (no RNG),
/// mirroring `gather_ai`.
fn nearest_allied_ai(
    world: &mut World,
    point: Vec2,
    player_faction: Option<Faction>,
) -> Option<u64> {
    let radius_sq = PICK_RADIUS * PICK_RADIUS;
    let mut best: Option<(f32, u64)> = None;
    let mut q = world
        .query_filtered::<(&AiStableId, &Position, &Faction), (With<AiBrain>, Without<PlayerShip>)>(
        );
    for (sid, pos, faction) in q.iter(world) {
        if player_faction != Some(*faction) {
            continue; // Only command our own side.
        }
        let d = (pos.0 - point).length_squared();
        if d > radius_sq {
            continue;
        }
        let wins = match best {
            None => true,
            Some((bd, bid)) => d < bd || (d == bd && sid.0 < bid),
        };
        if wins {
            best = Some((d, sid.0));
        }
    }
    best.map(|(_, id)| id)
}

/// Nearest ENEMY ship to `point` within [`PICK_RADIUS`] in the SERVER world: any
/// factioned entity whose `Faction != player_faction`. Returns the SERVER
/// [`Entity`] (what [`PlayerOrder::attack`] takes). Distance-then-entity-bits
/// tiebreak — stable, no RNG.
fn nearest_enemy(
    world: &mut World,
    point: Vec2,
    player_faction: Option<Faction>,
) -> Option<Entity> {
    let radius_sq = PICK_RADIUS * PICK_RADIUS;
    let mut best: Option<(f32, u64, Entity)> = None;
    let mut q = world.query::<(Entity, &Position, &Faction)>();
    for (e, pos, faction) in q.iter(world) {
        if player_faction == Some(*faction) {
            continue; // Skip allies (and the player's own side).
        }
        let d = (pos.0 - point).length_squared();
        if d > radius_sq {
            continue;
        }
        let bits = e.to_bits();
        let wins = match best {
            None => true,
            Some((bd, bb, _)) => d < bd || (d == bd && bits < bb),
        };
        if wins {
            best = Some((d, bits, e));
        }
    }
    best.map(|(_, _, e)| e)
}

/// B2 — the in-world picking + commanding system (`Update`). Early-returns unless
/// command mode is ON. Left-click selects the nearest allied AI ship under the
/// cursor (or deselects); right-click commands the SELECTED ship to attack an enemy
/// under the cursor, else move there. All clicks are gated on egui so the panel is
/// never click-through.
#[allow(clippy::too_many_arguments)]
pub fn command_mode_system(
    mouse: Res<ButtonInput<MouseButton>>,
    cam_q: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    window_q: Query<&Window, With<PrimaryWindow>>,
    mut contexts: EguiContexts,
    host: Option<NonSendMut<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    mut state: ResMut<DevPanelState>,
) {
    if !state.command_mode {
        return;
    }
    // Only act on a fresh click this frame; cheap bail when nothing was pressed.
    let left = mouse.just_pressed(MouseButton::Left);
    let right = mouse.just_pressed(MouseButton::Right);
    if !left && !right {
        return;
    }
    // Gate on egui: a click that egui wants (over the panel) never commands the world.
    if contexts
        .ctx_mut()
        .map(|c| c.wants_pointer_input())
        .unwrap_or(false)
    {
        return;
    }
    let (Some(mut host), Some(net)) = (host, net) else {
        return;
    };
    let (Ok((camera, cam_tf)), Ok(window)) = (cam_q.single(), window_q.single()) else {
        return;
    };
    let Some(point) = cursor_world_point(camera, cam_tf, window) else {
        return;
    };

    // The player's side, read from the SERVER world (authoritative Faction).
    let player = host.server.ship_entity_for(net.local_id);
    let player_faction = player.and_then(|e| host.server.world().get::<Faction>(e).copied());

    let world = host.server.world_mut();

    if left {
        // Select the nearest allied AI ship under the cursor; clear if none.
        match nearest_allied_ai(world, point, player_faction) {
            Some(id) => state.ai_selected = id.to_string(),
            None => state.ai_selected.clear(),
        }
    }

    if right {
        // Right-click commands the CURRENTLY selected ship (if one resolves).
        let Ok(id) = state.ai_selected.trim().parse::<u64>() else {
            return;
        };
        let Some(ship) = server_entity_for_stable_id(world, id) else {
            return;
        };
        // Enemy under the cursor → attack it; else move to the point. Merge so the
        // ship's style/posture overrides survive the new nav command.
        let order = match nearest_enemy(world, point, player_faction) {
            Some(enemy) => PlayerOrder::attack(enemy),
            None => PlayerOrder::move_to(point),
        };
        merge_kind(world, ship, order);
    }

    // TODO: patrol-by-click (Shift+Right-click queue) — deferred (not trivial here).
}

/// Marker for the single B3 selection-ring entity, so its `Transform`/`Visibility`
/// query never aliases other render entities.
#[derive(Component)]
pub struct SelectionRingMarker;

/// Lazily-built handles + spawned entity for the B3 selection ring. The ring is a
/// flat emissive-cyan annulus on the ground plane; built once on first need (so it
/// costs nothing until command mode is used) and then shown/hidden + repositioned
/// each frame by [`update_selection_ring`].
#[derive(Resource, Default)]
pub struct SelectionRing {
    /// The spawned ring entity (`None` until first built).
    entity: Option<Entity>,
}

/// B3 — selection-highlight ring (App-world render). Each frame: when command mode
/// is ON and `ai_selected` resolves to a live server ship, place the ring at that
/// ship's SERVER `Position` (`z` just under the plane) and show it; otherwise hide
/// it. The ring is built lazily on first need. Stepping at frame rate against the
/// sim-tick position is fine for a ground ring (no interpolation needed).
#[allow(clippy::too_many_arguments)]
pub fn update_selection_ring(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ring: ResMut<SelectionRing>,
    mut ring_q: Query<(&mut Transform, &mut Visibility), With<SelectionRingMarker>>,
    host: Option<NonSendMut<LoopbackHost>>,
    state: Res<DevPanelState>,
) {
    // Resolve the selected ship's SERVER position (if any) while command mode is on.
    let target_pos: Option<Vec2> = (|| {
        if !state.command_mode {
            return None;
        }
        let mut host = host?;
        let id = state.ai_selected.trim().parse::<u64>().ok()?;
        let world = host.server.world_mut();
        let ship = server_entity_for_stable_id(world, id)?;
        world.get::<Position>(ship).map(|p| p.0)
    })();

    // Build the ring once, lazily, the first time we need to show it.
    if ring.entity.is_none() {
        if target_pos.is_none() {
            return; // Nothing to show yet — don't build until needed.
        }
        let mesh = meshes.add(Annulus::new(1.4, 1.8));
        let material = materials.add(StandardMaterial {
            base_color: Color::srgba(0.0, 1.0, 1.0, 0.85),
            emissive: LinearRgba::rgb(0.0, 4.0, 4.0),
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        });
        let e = commands
            .spawn((
                SelectionRingMarker,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                Transform::from_xyz(0.0, 0.0, RING_Z),
                Visibility::Hidden,
            ))
            .id();
        ring.entity = Some(e);
        // The transform/visibility set below runs next frame once the entity exists.
        return;
    }

    let Some(ring_entity) = ring.entity else {
        return;
    };
    let Ok((mut tf, mut vis)) = ring_q.get_mut(ring_entity) else {
        return;
    };
    match target_pos {
        Some(p) => {
            tf.translation = Vec3::new(p.x, p.y, RING_Z);
            *vis = Visibility::Visible;
        }
        None => *vis = Visibility::Hidden,
    }
}
