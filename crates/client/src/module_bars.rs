//! Refinement 14 — **segmented per-module-type condition bars** on the HUD (Bevy UI).
//!
//! One horizontal bar per major module type (Reactor / Thruster / Weapon / Shield / Armor), split
//! into one segment per installed module of that type, each segment coloured by THAT module's live
//! condition (green = healthy → red = low → dark = destroyed). So the overall fill IS the aggregate
//! condition, empty/dark segments show how many (and which) are gone, and a `d/N down` count appears
//! when any are destroyed — letting the player see at a glance which subsystem the carve damage took
//! out (e.g. a dead reactor that "disabled" the ship at 80% hull).
//!
//! Entirely client-side: it reads the player ship's `Fit`/`FitLayout` from the embedded server world
//! read-only (like the other HUD bars) and calls the pure [`sim::fitting::module_conditions`] helper.

use bevy::prelude::*;
use sim::fitting::{module_conditions, Fit, FitLayout, ModuleCatalog, ModuleCondition, ModuleKind};

use crate::fonts::{FontAssets, IconAssets};
use crate::hud::{grade, scale_rgb, seg_dim};
use crate::net::{LoopbackHost, NetClientState};

/// The module types shown, in order (the major ones a pilot cares about).
const ROWS: [ModuleKind; 5] = [
    ModuleKind::Reactor,
    ModuleKind::Thruster,
    ModuleKind::Weapon,
    ModuleKind::Shield,
    ModuleKind::Armor,
];

/// Segments pre-spawned per row (extras hidden). The fighter has ≤2 of any type; headroom for hulls
/// with more hardpoints.
const MAX_SEG: usize = 12;

/// Short HUD label for a module type.
fn short_name(k: ModuleKind) -> &'static str {
    match k {
        ModuleKind::Reactor => "RCTR",
        ModuleKind::Thruster => "ENGINE",
        ModuleKind::Weapon => "WPN",
        ModuleKind::Shield => "SHLD",
        ModuleKind::Armor => "ARMR",
        ModuleKind::Sensor => "SNSR",
        ModuleKind::Utility => "UTIL",
    }
}

/// One module's condition fraction → its segment colour: dark red when destroyed, else the shared
/// green→amber→red ramp (full = green).
fn seg_color(f: f32) -> Color {
    if f <= 0.0 {
        // Refinement 21: a destroyed module's segment reads EMPTY (the bright broken-icon centred in
        // it marks the kill), instead of a dark-red fill.
        seg_dim()
    } else {
        grade(1.0 - f.clamp(0.0, 1.0))
    }
}

const LABEL_OK: Color = Color::srgb(0.78, 0.82, 0.90);
const LABEL_HURT: Color = Color::srgb(0.96, 0.42, 0.36);
/// The bright "destroyed" icon colour (alarm red-orange) for the broken-segment glyph (Refinement 21).
const ICON_DESTROYED_COLOR: Color = Color::srgb(0.98, 0.30, 0.22);

/// Root of the module-bar panel (toggled hidden when there is no fitted player ship).
#[derive(Component)]
pub struct ModuleBarPanel;
/// One module-type row container (hidden when the ship has no module of that kind).
#[derive(Component)]
pub struct ModuleBarRow {
    kind: ModuleKind,
}
/// One segment = one installed module of `kind` (segment `index` in its row).
#[derive(Component)]
pub struct ModuleSeg {
    kind: ModuleKind,
    index: usize,
}
/// The centred "destroyed" icon glyph child of segment (`kind`, `index`) — shown (via `Visibility`)
/// only when that module is destroyed (Refinement 21).
#[derive(Component)]
pub struct ModuleSegIcon {
    kind: ModuleKind,
    index: usize,
}
/// A row's type label (tinted red when any of that type are destroyed).
#[derive(Component)]
pub struct ModuleBarLabel {
    kind: ModuleKind,
}
/// A row's `d/N down` count text.
#[derive(Component)]
pub struct ModuleBarCount {
    kind: ModuleKind,
}

/// Spawn the right-anchored panel of per-type rows (one row per [`ROWS`] kind, each with a label, a
/// segmented bar of `MAX_SEG` pre-spawned segments, and a count text). Pre-spawn + show/hide each
/// frame — no per-frame spawn/despawn.
pub fn setup_module_bars(mut commands: Commands, fonts: Res<FontAssets>, icons: Res<IconAssets>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(64.0),
                right: Val::Px(12.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                ..default()
            },
            ModuleBarPanel,
        ))
        .with_children(|panel| {
            for kind in ROWS {
                panel
                    .spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(6.0),
                            ..default()
                        },
                        ModuleBarRow { kind },
                    ))
                    .with_children(|row| {
                        row.spawn((
                            Text::new(short_name(kind)),
                            TextFont {
                                font: fonts.label.clone(),
                                font_size: 13.0,
                                ..default()
                            },
                            TextColor(LABEL_OK),
                            Node {
                                width: Val::Px(54.0),
                                ..default()
                            },
                            ModuleBarLabel { kind },
                        ));
                        row.spawn(Node {
                            width: Val::Px(120.0),
                            height: Val::Px(12.0),
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(2.0),
                            ..default()
                        })
                        .with_children(|bar| {
                            for index in 0..MAX_SEG {
                                bar.spawn((
                                    Node {
                                        flex_grow: 1.0,
                                        height: Val::Percent(100.0),
                                        // Centre the destroyed-icon child (Refinement 21).
                                        justify_content: JustifyContent::Center,
                                        align_items: AlignItems::Center,
                                        ..default()
                                    },
                                    BackgroundColor(seg_dim()),
                                    ModuleSeg { kind, index },
                                ))
                                .with_children(|seg| {
                                    // Centred "destroyed" icon IMAGE (game-icons PNG, R22), hidden
                                    // until this module breaks; `update_module_bars` toggles its
                                    // `Visibility` + pulses its tint. Fixed 10px box so the source
                                    // PNG scales down into the segment.
                                    seg.spawn((
                                        ImageNode {
                                            image: icons.module_destroyed.clone(),
                                            color: ICON_DESTROYED_COLOR,
                                            ..default()
                                        },
                                        Node {
                                            width: Val::Px(10.0),
                                            height: Val::Px(10.0),
                                            ..default()
                                        },
                                        Visibility::Hidden,
                                        ModuleSegIcon { kind, index },
                                    ));
                                });
                            }
                        });
                        // `d/N down` count — multi-font: numbers (mono) + "/"/" down" (label).
                        row.spawn((
                            Text::new(String::new()), // span 0: destroyed count (mono)
                            TextFont {
                                font: fonts.mono.clone(),
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(LABEL_HURT),
                            Node {
                                width: Val::Px(70.0),
                                ..default()
                            },
                            ModuleBarCount { kind },
                        ))
                        .with_children(|p| {
                            p.spawn((
                                TextSpan::new(String::new()), // 1: "/" label
                                TextFont {
                                    font: fonts.label.clone(),
                                    font_size: 12.0,
                                    ..default()
                                },
                                TextColor(LABEL_HURT),
                            ));
                            p.spawn((
                                TextSpan::new(String::new()), // 2: total count (mono)
                                TextFont {
                                    font: fonts.mono.clone(),
                                    font_size: 12.0,
                                    ..default()
                                },
                                TextColor(LABEL_HURT),
                            ));
                            p.spawn((
                                TextSpan::new(String::new()), // 3: " down" label
                                TextFont {
                                    font: fonts.label.clone(),
                                    font_size: 12.0,
                                    ..default()
                                },
                                TextColor(LABEL_HURT),
                            ));
                        });
                    });
            }
        });
}

/// Refresh the segments/labels/counts each frame from the player ship's live `FitLayout`. Reads the
/// embedded server world read-only (same access as the trapezoid pool bars). Hides every row when
/// there is no fitted player ship (Sandbox / pre-spawn).
#[allow(clippy::too_many_arguments)] // Bevy system: many disjoint HUD queries + the text writer.
pub fn update_module_bars(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    time: Res<Time>,
    mut rows: Query<(&ModuleBarRow, &mut Node), Without<ModuleSeg>>,
    mut segs: Query<(&ModuleSeg, &mut Node, &mut BackgroundColor), Without<ModuleBarRow>>,
    mut icons: Query<(&ModuleSegIcon, &mut Visibility, &mut ImageNode)>,
    labels: Query<(Entity, &ModuleBarLabel)>,
    counts: Query<(Entity, &ModuleBarCount)>,
    mut writer: TextUiWriter,
) {
    // Per-kind conditions for the local fitted ship (empty if none → all rows hide).
    let conditions: Vec<ModuleCondition> = (|| -> Option<Vec<ModuleCondition>> {
        let host = host.as_ref()?;
        let net = net.as_ref()?;
        let e = host.server.ship_entity_for(net.local_id)?;
        let w = host.server.world();
        let fit = w.get::<Fit>(e)?;
        let layout = w.get::<FitLayout>(e)?;
        let catalog = w.get_resource::<ModuleCatalog>()?;
        Some(module_conditions(fit, layout, catalog))
    })()
    .unwrap_or_default();

    let find = |kind: ModuleKind| conditions.iter().find(|c| c.kind == kind);
    let destroyed = |kind: ModuleKind| {
        find(kind).map_or(0, |c| c.modules.iter().filter(|&&f| f <= 0.0).count())
    };

    // Rows: hide a type the ship doesn't carry.
    for (row, mut node) in &mut rows {
        node.display = if find(row.kind).is_some() {
            Display::Flex
        } else {
            Display::None
        };
    }
    // Segments: one per installed module, coloured by its condition; hide the spares.
    for (seg, mut node, mut bg) in &mut segs {
        match find(seg.kind) {
            Some(c) if seg.index < c.modules.len() => {
                node.display = Display::Flex;
                bg.0 = seg_color(c.modules[seg.index]);
            }
            _ => node.display = Display::None,
        }
    }
    // The "destroyed" icon image on a broken segment (Refinement 22): visible only when that module
    // is installed (index < count) AND destroyed (condition <= 0); when shown, its red tint PULSES
    // (motion = the strongest peripheral cue). Hidden otherwise (alive / spare).
    let pulse = 0.55 + 0.45 * (time.elapsed_secs() * 9.0).sin().abs();
    for (icon, mut vis, mut img) in &mut icons {
        let show = matches!(
            find(icon.kind),
            Some(c) if icon.index < c.modules.len() && c.modules[icon.index] <= 0.0
        );
        if show {
            *vis = Visibility::Visible;
            img.color = scale_rgb(ICON_DESTROYED_COLOR, pulse);
        } else {
            *vis = Visibility::Hidden;
        }
    }
    // Label tint (span 0) when any of that type are down — written via the text writer so it does not
    // contend with the icon/count component access.
    for (e, lbl) in &labels {
        let tint = if destroyed(lbl.kind) > 0 {
            LABEL_HURT
        } else {
            LABEL_OK
        };
        *writer.color(e, 0) = TextColor(tint);
    }
    // `d/N down` count spans (blank when none down): [0 dest mono][1 "/"][2 total mono][3 " down"].
    for (e, cnt) in &counts {
        let d = destroyed(cnt.kind);
        match find(cnt.kind) {
            Some(c) if d > 0 => {
                *writer.text(e, 0) = format!("{d}");
                *writer.text(e, 1) = "/".to_string();
                *writer.text(e, 2) = format!("{}", c.modules.len());
                *writer.text(e, 3) = " down".to_string();
            }
            _ => {
                for i in 0..4 {
                    *writer.text(e, i) = String::new();
                }
            }
        }
    }
}
