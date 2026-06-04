//! Minimal diegetic HUD (FR-011): speed/throttle, flight-assist mode, an aiming
//! reticle, and hit/destroy feedback — no number spam (SC-006).
//!
//! Phase E added a segmented **"VU-meter"** bar at the bottom — **Energy** (the weapon capacitor;
//! empties as you fire, recharges) — read straight from the embedded server world (like the dev
//! panel), color-graded green→amber→red with a critical pulse so you can track it in combat without
//! looking away. (Phase F moves **Heat** to a trapezoid **double-ramp** mesh bar — see
//! [`crate::hud_bars`] — so it is no longer a UI VU bar here.)

use bevy::prelude::*;
use sim::components::{Energy, FlightAssist, Health, Ship, Velocity};
use sim::damage::HitKind;
use sim::HitFeedback;

use crate::net::{LoopbackHost, NetClientState};

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

/// One lit/unlit segment of the bottom Energy VU-meter bar.
#[derive(Component)]
pub struct BarSegment {
    pub index: usize,
}

/// The text label (`ENRG 72/120`) at the head of the Energy bar — the detail-on-focus layer.
#[derive(Component)]
pub struct BarLabel;

/// The Energy net-rate readout at the RIGHT of the ENRG bar (Phase F): a direction glyph + the
/// signed per-second rate, coloured green (charging) / red (draining) / dim (≈0).
#[derive(Component)]
pub struct BarRate;

/// Segments per bar (the "stacked vertical bars" you count at a glance).
const BAR_SEGMENTS: usize = 24;

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
pub fn setup_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("SPD   0.0   FLIGHT   HP 100"),
        TextFont {
            font_size: 18.0,
            ..default()
        },
        TextColor(Color::srgb(0.80, 0.90, 1.0)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
        HudText,
    ));
}

/// Spawn the bottom segmented **Energy** bar (Phase E; Heat moved to the trapezoid double-ramp in
/// Phase F). A bottom-anchored, full-width row: a text label + `BAR_SEGMENTS` thin vertical segments
/// (lit by `current/max`, coloured by `update_energy_hud`) + the net-rate readout.
pub fn setup_energy_bars(mut commands: Commands) {
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(16.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(2.0),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Text::new("ENRG  --".to_string()),
                TextFont {
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.80, 0.90, 1.0)),
                Node {
                    width: Val::Px(96.0),
                    ..default()
                },
                BarLabel,
            ));
            for index in 0..BAR_SEGMENTS {
                row.spawn((
                    Node {
                        width: Val::Px(7.0),
                        height: Val::Px(16.0),
                        ..default()
                    },
                    BackgroundColor(seg_dim()),
                    BarSegment { index },
                ));
            }
            // The Energy net-rate readout (direction glyph + signed number).
            row.spawn((
                Text::new(" -".to_string()),
                TextFont {
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.6, 0.6, 0.65)),
                Node {
                    margin: UiRect::left(Val::Px(8.0)),
                    ..default()
                },
                BarRate,
            ));
        });
}

/// Refresh the bottom Energy bar each frame from the local ship's live `Energy`, read from the
/// embedded server world (like the dev panel). Lit segments = `current/max`; colour grades
/// green→amber→red (reddens as it EMPTIES) and the critical band pulses. (Heat now lives in the
/// trapezoid double-ramp — [`crate::hud_bars`].)
pub fn update_energy_hud(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    time: Res<Time>,
    mut segs: Query<(&BarSegment, &mut BackgroundColor)>,
    mut labels: Query<&mut Text, (With<BarLabel>, Without<HudText>, Without<BarRate>)>,
    mut rate_q: Query<(&mut Text, &mut TextColor), (With<BarRate>, Without<HudText>)>,
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
    let efrac = energy.map(|(c, m)| {
        if m > 0.0 {
            (c / m).clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    // Critical-pulse brightness oscillation (0.55..=1.0), applied only in the danger band.
    let pulse = 0.55 + 0.45 * (time.elapsed_secs() * 9.0).sin().abs();

    for (seg, mut bg) in &mut segs {
        let Some(f) = efrac else {
            bg.0 = seg_dim();
            continue;
        };
        if (seg.index as f32) >= f * BAR_SEGMENTS as f32 {
            bg.0 = seg_dim();
            continue;
        }
        let mut c = grade(1.0 - f);
        if f < 0.2 {
            c = scale_rgb(c, pulse);
        }
        bg.0 = c;
    }

    for mut text in &mut labels {
        text.0 = match energy {
            Some((c, m)) => format!("ENRG {c:>3.0}/{m:<3.0}"),
            None => "ENRG  --".to_string(),
        };
    }

    // Energy net-rate readout (right of the ENRG bar): a direction glyph + the signed rate, coloured
    // green (charging) / red (draining) / dim (≈0) so the trend reads at a glance.
    if let Ok((mut text, mut color)) = rate_q.single_mut() {
        let (glyph, c, num) = match erate {
            Some(r) if r > 1.0 => ("▲", Color::srgb(0.30, 0.90, 0.35), format!("+{r:.0}")),
            Some(r) if r < -1.0 => ("▼", Color::srgb(0.95, 0.35, 0.25), format!("{r:.0}")),
            Some(r) => ("–", Color::srgb(0.60, 0.60, 0.65), format!("{r:+.0}")),
            None => ("–", Color::srgb(0.60, 0.60, 0.65), String::new()),
        };
        text.0 = format!(" {glyph} {num}");
        color.0 = c;
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
    ship_q: Query<(&Velocity, &FlightAssist, &Health), With<Ship>>,
    feedback: Res<HitFeedback>,
    mut text_q: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let Ok((vel, assist, health)) = ship_q.single() else {
        text.0 = "-- SHIP DESTROYED --".to_string();
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
    text.0 = format!("SPD {speed:>5.1}   {mode}   HP {:>3.0}{flash}", health.0);
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
