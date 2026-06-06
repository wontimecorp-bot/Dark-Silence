//! Minimal diegetic HUD (FR-011): speed/throttle, flight-assist mode, an aiming
//! reticle, and hit/destroy feedback — no number spam (SC-006).
//!
//! The Phase-F HUD status bars (Energy/Heat/Afterburner + Shield/Armor/Hull) are camera-anchored
//! trapezoid MESH bars — see [`crate::hud_bars`]. This module keeps only the **Energy numeric +
//! net-rate text** (the detail-on-focus readout beside the Energy bar), read straight from the
//! embedded server world like the dev panel.

use bevy::prelude::*;
use sim::components::{Energy, FlightAssist, Ship, Velocity};
use sim::damage::HitKind;
use sim::{HitFeedback, RefinedResources};

use crate::fonts::FontAssets;
use crate::net::{LoopbackHost, NetClientState};

/// Shared HUD text tint (the SPD line + the Energy readouts).
const HUD_BLUE: Color = Color::srgb(0.80, 0.90, 1.0);
/// The mining-skirmish score line tint.
const SCORE_WHITE: Color = Color::srgb(0.92, 0.92, 0.96);

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

/// Marker for the mining-skirmish per-faction refined-resources score line (top-centre).
#[derive(Component)]
pub struct ScoreText;

/// Spawn the mining-skirmish score readout (top-centre): `RED <n>   BLUE <n>`, the per-faction
/// refined-resources tally read from the embedded server world each frame. Hidden (blank) in any
/// world without the [`RefinedResources`] resource (e.g. the Sandbox before anything refines).
pub fn setup_score_hud(mut commands: Commands, fonts: Res<FontAssets>) {
    // Multi-font line (Refinement 22): "RED "(label) <red>(mono) "    BLUE "(label) <blue>(mono).
    commands
        .spawn((
            Text::new(String::new()), // span 0: "RED " label
            TextFont {
                font: fonts.label.clone(),
                font_size: 20.0,
                ..default()
            },
            TextColor(SCORE_WHITE),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(10.0),
                left: Val::Percent(38.0),
                ..default()
            },
            ScoreText,
        ))
        .with_children(|p| {
            p.spawn((
                TextSpan::new(String::new()), // 1: red number (mono)
                TextFont {
                    font: fonts.mono.clone(),
                    font_size: 20.0,
                    ..default()
                },
                TextColor(SCORE_WHITE),
            ));
            p.spawn((
                TextSpan::new(String::new()), // 2: "    BLUE " label
                TextFont {
                    font: fonts.label.clone(),
                    font_size: 20.0,
                    ..default()
                },
                TextColor(SCORE_WHITE),
            ));
            p.spawn((
                TextSpan::new(String::new()), // 3: blue number (mono)
                TextFont {
                    font: fonts.mono.clone(),
                    font_size: 20.0,
                    ..default()
                },
                TextColor(SCORE_WHITE),
            ));
        });
}

/// Refresh the score line from the embedded server world's [`RefinedResources`] (like the energy
/// readout reads the server world). Blank when the resource is absent.
pub fn update_score_hud(
    host: Option<NonSend<LoopbackHost>>,
    roots: Query<Entity, With<ScoreText>>,
    mut writer: TextUiWriter,
) {
    let Ok(e) = roots.single() else {
        return;
    };
    match host
        .as_ref()
        .and_then(|h| h.server.world().get_resource::<RefinedResources>())
    {
        Some(r) => {
            *writer.text(e, 0) = "RED ".to_string();
            *writer.text(e, 1) = format!("{:.0}", r.red);
            *writer.text(e, 2) = "    BLUE ".to_string();
            *writer.text(e, 3) = format!("{:.0}", r.blue);
        }
        None => {
            for i in 0..4 {
                *writer.text(e, i) = String::new();
            }
        }
    }
}

/// The numeric Energy readout (`ENRG 72/120`) — the detail-on-focus layer beside the Energy mesh
/// bar (the bar itself is a camera-anchored trapezoid in [`crate::hud_bars`]).
#[derive(Component)]
pub struct BarLabel;

/// The Energy net-rate readout next to the ENRG number (Phase F): an ASCII direction glyph (`^`
/// charging / `v` draining / `~` ≈0 — the default font has no triangle glyphs) + the signed
/// per-second rate, coloured green (charging) / red (draining) / dim (≈0).
#[derive(Component)]
pub struct BarRate;

/// An unlit segment's colour (dim, so the lit portion reads as the level). Shared with the
/// Phase F trapezoid bars ([`crate::hud_bars`]).
pub(crate) fn seg_dim() -> Color {
    Color::srgb(0.12, 0.12, 0.16)
}

/// green→amber→red ramp where `bad = 0` is good (green) and `bad = 1` is critical (red).
/// Shared with the Phase F trapezoid bars ([`crate::hud_bars`]).
pub(crate) fn grade(bad: f32) -> Color {
    let b = bad.clamp(0.0, 1.0);
    if b < 0.5 {
        let k = b * 2.0; // green → amber
        Color::srgb(k, 0.85 - 0.10 * k, 0.18 * (1.0 - k))
    } else {
        let k = (b - 0.5) * 2.0; // amber → red
        Color::srgb(1.0 - 0.05 * k, 0.75 - 0.60 * k, 0.10 * k)
    }
}

/// Scale a colour's RGB by `k` (the critical-pulse brightness oscillation). Shared with the
/// Phase F trapezoid bars ([`crate::hud_bars`]).
pub(crate) fn scale_rgb(c: Color, k: f32) -> Color {
    let s = c.to_srgba();
    Color::srgb(s.red * k, s.green * k, s.blue * k)
}

/// Spawn the readout line (top-left). The aiming reticle is a world-space
/// gunsight pip ahead of the nose (see `render_sync::AimPip`), not a screen
/// overlay — the weapon fires along the heading, not at screen centre.
pub fn setup_hud(mut commands: Commands, fonts: Res<FontAssets>) {
    // Multi-font line (Refinement 22): "SPD "(label) <speed>(mono) "   {mode}{flash}"(label).
    commands
        .spawn((
            Text::new("SPD "), // span 0: label
            TextFont {
                font: fonts.label.clone(),
                font_size: 18.0,
                ..default()
            },
            TextColor(HUD_BLUE),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(10.0),
                left: Val::Px(10.0),
                ..default()
            },
            HudText,
        ))
        .with_children(|p| {
            p.spawn((
                TextSpan::new("  0.0"), // 1: speed value (mono, tabular)
                TextFont {
                    font: fonts.mono.clone(),
                    font_size: 18.0,
                    ..default()
                },
                TextColor(HUD_BLUE),
            ));
            p.spawn((
                TextSpan::new("   FLIGHT"), // 2: mode + transient flash (label)
                TextFont {
                    font: fonts.label.clone(),
                    font_size: 18.0,
                    ..default()
                },
                TextColor(HUD_BLUE),
            ));
        });
}

/// Spawn the **Energy numeric + net-rate** text readout (the Energy BAR itself is a camera-anchored
/// trapezoid mesh in [`crate::hud_bars`]). A compact row anchored at the bottom, left-of-centre so it
/// floats just above the Energy bar in the bottom EHA row. (Position is a first-pass guess — tunable.)
pub fn setup_energy_bars(mut commands: Commands, fonts: Res<FontAssets>) {
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(46.0),
            left: Val::Percent(24.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|row| {
            // ENRG: "ENRG "(label) <cur>(mono) "/"(label) <max>(mono).
            row.spawn((
                Text::new("ENRG "),
                TextFont {
                    font: fonts.label.clone(),
                    font_size: 13.0,
                    ..default()
                },
                TextColor(HUD_BLUE),
                BarLabel,
            ))
            .with_children(|p| {
                p.spawn((
                    TextSpan::new(" --"),
                    TextFont {
                        font: fonts.mono.clone(),
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(HUD_BLUE),
                ));
                p.spawn((
                    TextSpan::new(String::new()),
                    TextFont {
                        font: fonts.label.clone(),
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(HUD_BLUE),
                ));
                p.spawn((
                    TextSpan::new(String::new()),
                    TextFont {
                        font: fonts.mono.clone(),
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(HUD_BLUE),
                ));
            });
            // Rate: "{glyph} "(label) <rate>(mono), tinted green/red/dim each frame.
            row.spawn((
                Text::new("~ "),
                TextFont {
                    font: fonts.label.clone(),
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.6, 0.6, 0.65)),
                BarRate,
            ))
            .with_children(|p| {
                p.spawn((
                    TextSpan::new(String::new()),
                    TextFont {
                        font: fonts.mono.clone(),
                        font_size: 13.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.6, 0.6, 0.65)),
                ));
            });
        });
}

/// Refresh the Energy numeric + net-rate text each frame from the local ship's live `Energy`, read
/// from the embedded server world (like the dev panel). The Energy BAR fill is drawn by the mesh
/// bar in [`crate::hud_bars`]; this is only the detail-on-focus number + the trend readout.
pub fn update_energy_hud(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    label_root: Query<Entity, With<BarLabel>>,
    rate_root: Query<Entity, With<BarRate>>,
    mut writer: TextUiWriter,
) {
    // Resolve the local ship's Energy pool (None until it exists / is fitted).
    let (energy, erate) = match (host.as_ref(), net.as_ref()) {
        (Some(host), Some(net)) => match host.server.ship_entity_for(net.local_id) {
            Some(e) => {
                let w = host.server.world();
                (
                    w.get::<Energy>(e).map(|p| (p.current, p.max)),
                    w.get::<Energy>(e).map(|p| p.rate),
                )
            }
            None => (None, None),
        },
        _ => (None, None),
    };

    if let Ok(e) = label_root.single() {
        match energy {
            Some((c, m)) => {
                *writer.text(e, 0) = "ENRG ".to_string();
                *writer.text(e, 1) = format!("{c:>3.0}");
                *writer.text(e, 2) = "/".to_string();
                *writer.text(e, 3) = format!("{m:<3.0}");
            }
            None => {
                *writer.text(e, 0) = "ENRG  --".to_string();
                *writer.text(e, 1) = String::new();
                *writer.text(e, 2) = String::new();
                *writer.text(e, 3) = String::new();
            }
        }
    }

    // Energy net-rate readout: an ASCII direction glyph (label) + the signed rate (mono), tinted
    // green (charging) / red (draining) / dim (≈0) on BOTH spans so the trend reads at a glance.
    // ASCII `^`/`v`/`~` because no triangle glyphs are guaranteed across faces.
    if let Ok(e) = rate_root.single() {
        let (glyph, col, num) = match erate {
            Some(r) if r > 1.0 => ("^", Color::srgb(0.30, 0.90, 0.35), format!("+{r:.0}")),
            Some(r) if r < -1.0 => ("v", Color::srgb(0.95, 0.35, 0.25), format!("{r:.0}")),
            Some(r) => ("~", Color::srgb(0.60, 0.60, 0.65), format!("{r:+.0}")),
            None => ("~", Color::srgb(0.60, 0.60, 0.65), String::new()),
        };
        *writer.text(e, 0) = format!("{glyph} ");
        *writer.text(e, 1) = num;
        *writer.color(e, 0) = TextColor(col);
        *writer.color(e, 1) = TextColor(col);
    }
}

/// The terse, diegetic label for a hit's legibility tag (FR-024, SC-005) — the
/// presentation mapping for the [`HitKind`] the sim resolved.
///
/// Presentation-only: the client never computes a `HitKind`, it only *displays*
/// the one `sim` handed it on the [`HitFeedback`]. Short, no numbers (no damage
/// spam): the player can tell ricochet vs penetration vs shield-absorb at a
/// glance. Pure and total over the 5 `HitKind`s.
pub fn hit_cue_label(kind: HitKind) -> &'static str {
    match kind {
        HitKind::ShieldAbsorbed => "SHIELD",
        HitKind::Ricochet => "RICOCHET",
        HitKind::Penetrated => "PEN",
        HitKind::OverPenetrated => "OVERPEN",
        HitKind::NoModule => "MISS",
    }
}

/// Refresh the readout each frame from the ship state + transient hit feedback.
pub fn update_hud(
    ship_q: Query<(&Velocity, &FlightAssist), With<Ship>>,
    feedback: Res<HitFeedback>,
    roots: Query<Entity, With<HudText>>,
    mut writer: TextUiWriter,
) {
    let Ok(e) = roots.single() else {
        return;
    };
    let Ok((vel, assist)) = ship_q.single() else {
        // No live ship: collapse to a single label span.
        *writer.text(e, 0) = "-- SHIP DESTROYED --".to_string();
        *writer.text(e, 1) = String::new();
        *writer.text(e, 2) = String::new();
        return;
    };
    let speed = vel.0.length();
    let mode = match assist {
        FlightAssist::On => "FLIGHT",
        FlightAssist::Off => "NEWTON",
    };
    // The transient flash, refined by the sim's legibility tag (FR-024): KILL/HIT
    // is the base cue; when a `HitKind` is present it names the outcome (ricochet /
    // penetration / shield-absorb) — terse, diegetic, no damage numbers.
    let flash = if feedback.destroy_flash > 0.0 {
        "   KILL".to_string()
    } else if feedback.hit_flash > 0.0 {
        match feedback.last_kind {
            Some(kind) => format!("   HIT {}", hit_cue_label(kind)),
            None => "   HIT".to_string(),
        }
    } else {
        String::new()
    };
    *writer.text(e, 0) = "SPD ".to_string();
    *writer.text(e, 1) = format!("{speed:>5.1}"); // mono / tabular
    *writer.text(e, 2) = format!("   {mode}{flash}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The legibility guarantee (SC-005, FR-024): each of the 5 `HitKind`s maps to
    /// its distinct, terse, number-free label — the player can tell ricochet vs
    /// penetration vs shield-absorb apart at a glance.
    #[test]
    fn hit_cue_label_maps_every_hit_kind() {
        assert_eq!(hit_cue_label(HitKind::ShieldAbsorbed), "SHIELD");
        assert_eq!(hit_cue_label(HitKind::Ricochet), "RICOCHET");
        assert_eq!(hit_cue_label(HitKind::Penetrated), "PEN");
        assert_eq!(hit_cue_label(HitKind::OverPenetrated), "OVERPEN");
        assert_eq!(hit_cue_label(HitKind::NoModule), "MISS");
    }

    /// Every label is non-empty, distinct, and carries no digits — diegetic, not
    /// numeric spam (SC-005).
    #[test]
    fn hit_cue_labels_are_distinct_and_number_free() {
        let kinds = [
            HitKind::ShieldAbsorbed,
            HitKind::Ricochet,
            HitKind::Penetrated,
            HitKind::OverPenetrated,
            HitKind::NoModule,
        ];
        let labels: Vec<&str> = kinds.iter().map(|&k| hit_cue_label(k)).collect();
        for label in &labels {
            assert!(!label.is_empty(), "a hit cue label is empty");
            assert!(
                !label.chars().any(|c| c.is_ascii_digit()),
                "label {label:?} contains a digit — that is numeric spam (SC-005)"
            );
        }
        // Distinctness: 5 unique labels for 5 kinds.
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            labels.len(),
            "hit cue labels must be distinct"
        );
    }
}
