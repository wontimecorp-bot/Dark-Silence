//! Refinement 2 — a basic **ranged sensor radar** HUD (top-right), client-only.
//!
//! The mining-skirmish arena is huge and sparse (the central asteroid sits ~1200 units from each
//! faction's outpost), far beyond what one screen shows — so without a radar the player can't tell
//! which way to fly. This is a minimal sensor: each frame it reads the embedded server's
//! [`server::ServerApp::render_state`] (already client-only, unquantized f32 — every entity's
//! `pos`/`kind`/`flags`/`faction`), and plots nearby contacts as coloured blips around a centre dot
//! (the player). Contacts beyond [`RADAR_RANGE`] are **pinned to the rim** as direction markers, so a
//! far landmark (the asteroid) shows which heading to take and slides inward as you approach.
//!
//! It reuses the camera-anchored unlit-mesh HUD idiom from [`crate::hud_bars`] (and
//! [`crate::render_sync::update_aim_pip`]): the disc + centre marker + a fixed pool of blip meshes
//! are parented to [`crate::camera::MainCamera`] at fixed camera-local offsets (the camera's rotation
//! is identity — looking straight down `-Z`, up `+Y` — so camera-local `+x`/`+y` are world `+x`/`+y`,
//! i.e. the radar is **north-up**). Each blip carries its own `unlit` material so its colour can be
//! set independently each frame; unused pool slots are simply hidden.
//!
//! Purely a client render/HUD overlay — no sim/server/protocol change, so it has zero effect on
//! determinism (the headless server worlds never run any of this).

use bevy::prelude::*;
use protocol::EntityKind;
use server::RenderEntity;
use sim::components::{Faction, TargetKind};

use crate::camera::MainCamera;
use crate::net::{LoopbackHost, NetClientState};

/// Camera-local depth (units in front of the camera) the radar floats at — matches the HUD bars so
/// it sits well in front of every ship and never gets occluded. (Same value as `hud_bars::HUD_DEPTH`.)
const HUD_DEPTH: f32 = 12.0;
/// Radar disc centre in camera-local units (top-right corner; the speed text is top-left, the score
/// top-centre, and the trapezoid bars run along the bottom). At [`HUD_DEPTH`] the visible half-extent
/// is ≈ `y ±4.97` / `x ±6.5` (down to a 4:3 window), so this keeps the whole disc on-screen.
const RADAR_CENTER: Vec2 = Vec2::new(4.7, 3.1);
/// On-screen radius of the radar disc (camera-local units).
const RADAR_RADIUS: f32 = 1.5;
/// World-space range (sim units) mapped to the disc edge; contacts farther than this pin to the rim.
const RADAR_RANGE: f32 = 700.0;
/// Size of the reusable blip pool (contacts beyond this are dropped — the skirmish has far fewer).
const MAX_BLIPS: usize = 48;
/// Base on-screen radius of a contact blip (camera-local units; scaled per-kind at runtime).
const BLIP_BASE_RADIUS: f32 = 0.07;
/// Disc sits at the HUD plane; blips/centre sit slightly closer to the camera so they draw on top.
const DISC_Z: f32 = -HUD_DEPTH;
const BLIP_Z: f32 = -HUD_DEPTH + 0.3;

/// Friend / foe / neutral blip colours (locked decision): green = your faction, red = enemy, grey =
/// neutral (the asteroid).
const BLIP_FRIEND: Color = Color::srgb(0.30, 0.95, 0.45);
const BLIP_FOE: Color = Color::srgb(0.95, 0.30, 0.28);
const BLIP_NEUTRAL: Color = Color::srgb(0.72, 0.74, 0.78);

/// One reusable radar blip — holds its own material handle so [`update_radar`] can recolour it each
/// frame (same idiom as [`crate::hud_bars::TrapSegment`]).
#[derive(Component)]
pub struct RadarBlip {
    pub material: Handle<StandardMaterial>,
}

/// Startup (after [`crate::camera::setup_camera`]): spawn the radar as children of the camera — a dim
/// translucent background disc, a bright centre marker (the player), and a hidden pool of blip meshes.
pub fn setup_radar(
    mut commands: Commands,
    cam_q: Query<Entity, With<MainCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(cam) = cam_q.single() else {
        return;
    };
    let disc_mesh = meshes.add(Circle::new(RADAR_RADIUS));
    let blip_mesh = meshes.add(Circle::new(BLIP_BASE_RADIUS));
    let center_mesh = meshes.add(Circle::new(BLIP_BASE_RADIUS * 1.6));

    let disc_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.04, 0.07, 0.06, 0.35),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let center_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.95, 1.0),
        unlit: true,
        ..default()
    });

    commands.entity(cam).with_children(|parent| {
        // Background disc (translucent), behind everything else on the radar.
        parent.spawn((
            Mesh3d(disc_mesh),
            MeshMaterial3d(disc_mat),
            Transform::from_xyz(RADAR_CENTER.x, RADAR_CENTER.y, DISC_Z),
        ));
        // Centre marker = the player (always at the radar centre).
        parent.spawn((
            Mesh3d(center_mesh),
            MeshMaterial3d(center_mat),
            Transform::from_xyz(RADAR_CENTER.x, RADAR_CENTER.y, BLIP_Z),
        ));
        // Reusable blip pool — each with its own material, hidden until assigned a contact.
        for _ in 0..MAX_BLIPS {
            let material = materials.add(StandardMaterial {
                base_color: BLIP_NEUTRAL,
                unlit: true,
                ..default()
            });
            parent.spawn((
                RadarBlip {
                    material: material.clone(),
                },
                Mesh3d(blip_mesh.clone()),
                MeshMaterial3d(material),
                Transform::from_xyz(RADAR_CENTER.x, RADAR_CENTER.y, BLIP_Z),
                Visibility::Hidden,
            ));
        }
    });
}

/// Update each frame: plot the world's contacts (read from the embedded server's `render_state`)
/// around the player at the radar centre. Far contacts pin to the rim as direction markers; each blip
/// is coloured friend/foe/neutral relative to the player's faction and sized by kind. Unused pool
/// slots are hidden. Mirrors [`crate::render_sync::update_aim_pip`]'s host/net access.
pub fn update_radar(
    host: Option<NonSendMut<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    mut blips: Query<(&RadarBlip, &mut Transform, &mut Visibility)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // `render_state` needs `&mut self`, so the host is taken mutably (like `capture_render_state`).
    let (Some(mut host), Some(net)) = (host, net) else {
        hide_all(&mut blips);
        return;
    };

    let entities = host.server.render_state();
    // The player's own pose anchors the radar; without it (startup) there is nothing to plot.
    let Some(my_pos) = entities
        .iter()
        .find(|e| e.id == net.local_id)
        .map(|e| e.pos)
    else {
        hide_all(&mut blips);
        return;
    };
    // The player's faction tag (0 if unfactioned, e.g. Sandbox) — drives friend/foe colouring.
    let my_tag: u8 = host
        .server
        .ship_entity_for(net.local_id)
        .and_then(|e| host.server.world().get::<Faction>(e))
        .map(|f| f.tint_tag())
        .unwrap_or(0);

    // Contacts worth showing: everything but the player, skipping projectiles/debris (just clutter).
    // Sorted by id so a given contact keeps the same pool slot frame-to-frame (no blip flicker).
    let mut contacts: Vec<&RenderEntity> = entities
        .iter()
        .filter(|e| e.id != net.local_id)
        .filter(|e| matches!(e.kind, EntityKind::Ship | EntityKind::Target))
        .collect();
    contacts.sort_by_key(|e| e.id.0);

    for (i, (blip, mut tf, mut vis)) in blips.iter_mut().enumerate() {
        let Some(e) = contacts.get(i) else {
            *vis = Visibility::Hidden;
            continue;
        };
        // World-relative offset → radar-local, clamped to the rim so far contacts show a direction.
        let mut off = (e.pos - my_pos) * (RADAR_RADIUS / RADAR_RANGE);
        if off.length() > RADAR_RADIUS {
            off = off.normalize_or_zero() * RADAR_RADIUS;
        }
        tf.translation = Vec3::new(RADAR_CENTER.x + off.x, RADAR_CENTER.y + off.y, BLIP_Z);
        tf.scale = Vec3::splat(blip_scale(e));
        *vis = Visibility::Visible;

        let color = if e.faction == 0 {
            BLIP_NEUTRAL
        } else if e.faction == my_tag {
            BLIP_FRIEND
        } else {
            BLIP_FOE
        };
        if let Some(m) = materials.get_mut(&blip.material) {
            m.base_color = color;
        }
    }
}

/// Relative blip size by kind: the asteroid is the biggest landmark, then structures, then ships.
fn blip_scale(e: &RenderEntity) -> f32 {
    match e.kind {
        EntityKind::Target => match TargetKind::from_u8(e.flags) {
            Some(TargetKind::MineNode) => 2.4,
            Some(TargetKind::Outpost) => 1.8,
            Some(TargetKind::Transport) => 1.3,
            _ => 1.0,
        },
        _ => 1.0,
    }
}

/// Hide every blip (no host/net yet, or the local ship isn't in the render set).
fn hide_all(blips: &mut Query<(&RadarBlip, &mut Transform, &mut Visibility)>) {
    for (_, _, mut vis) in blips.iter_mut() {
        *vis = Visibility::Hidden;
    }
}
