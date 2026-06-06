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

use crate::net::{LoopbackHost, NetClientState};

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

/// Marker for the mining-skirmish per-faction refined-resources score line (top-centre).
#[derive(Component)]
pub struct ScoreText;

/// Spawn the mining-skirmish score readout (top-centre): `RED <n>   BLUE <n>`, the per-faction
/// refined-resources tally read from the embedded server world each frame. Hidden (blank) in any
/// world without the [`RefinedResources`] resource (e.g. the Sandbox before anything refines).
pub fn setup_score_hud(mut commands: Commands) {
    commands.spawn((
        Text::new(String::new()),
        TextFont {
            font_size: 20.0,
            ..default()
        },
        TextColor(Color::srgb(0.92, 0.92, 0.96)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Percent(38.0),
            ..default()
        },
        ScoreText,
    ));
}

/// Refresh the score line from the embedded server world's [`RefinedResources`] (like the energy
/// readout reads the server world). Blank when the resource is absent.
pub fn update_score_hud(
    host: Option<NonSend<LoopbackHost>>,
    mut q: Query<&mut Text, With<ScoreText>>,
) {
    let Ok(mut text) = q.single_mut() else {
        return;
    };
    text.0 = match host
        .as_ref()
        .and_then(|h| h.server.world().get_resource::<RefinedResources>())
    {
        Some(r) => format!("RED {:.0}    BLUE {:.0}", r.red, r.blue),
        None => String::new(),
    };
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
pub fn setup_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("SPD   0.0   FLIGHT"),
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

/// Spawn the **Energy numeric + net-rate** text readout (the Energy BAR itself is a camera-anchored
/// trapezoid mesh in [`crate::hud_bars`]). A compact row anchored at the bottom, left-of-centre so it
/// floats just above the Energy bar in the bottom EHA row. (Position is a first-pass guess — tunable.)
pub fn setup_energy_bars(mut commands: Commands) {
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
            row.spawn((
                Text::new("ENRG  --".to_string()),
                TextFont {
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.80, 0.90, 1.0)),
                BarLabel,
            ));
            row.spawn((
                Text::new("~".to_string()),
                TextFont {
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::srgb(0.6, 0.6, 0.65)),
                BarRate,
            ));
        });
}

/// Refresh the Energy numeric + net-rate text each frame from the local ship's live `Energy`, read
/// from the embedded server world (like the dev panel). The Energy BAR fill is drawn by the mesh
/// bar in [`crate::hud_bars`]; this is only the detail-on-focus number + the trend readout.
pub fn update_energy_hud(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
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

    for mut text in &mut labels {
        text.0 = match energy {
            Some((c, m)) => format!("ENRG {c:>3.0}/{m:<3.0}"),
            None => "ENRG  --".to_string(),
        };
    }

    // Energy net-rate readout: an ASCII direction glyph + the signed rate, coloured green (charging)
    // / red (draining) / dim (≈0) so the trend reads at a glance. ASCII `^`/`v`/`~` because the
    // default font has no triangle glyphs (the old `▲`/`▼` rendered as blank squares).
    if let Ok((mut text, mut color)) = rate_q.single_mut() {
        let (glyph, c, num) = match erate {
            Some(r) if r > 1.0 => ("^", Color::srgb(0.30, 0.90, 0.35), format!("+{r:.0}")),
            Some(r) if r < -1.0 => ("v", Color::srgb(0.95, 0.35, 0.25), format!("{r:.0}")),
            Some(r) => ("~", Color::srgb(0.60, 0.60, 0.65), format!("{r:+.0}")),
            None => ("~", Color::srgb(0.60, 0.60, 0.65), String::new()),
        };
        text.0 = format!("{glyph} {num}");
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
    ship_q: Query<(&Velocity, &FlightAssist), With<Ship>>,
    feedback: Res<HitFeedback>,
    mut text_q: Query<&mut Text, With<HudText>>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let Ok((vel, assist)) = ship_q.single() else {
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
    text.0 = format!("SPD {speed:>5.1}   {mode}{flash}");
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
