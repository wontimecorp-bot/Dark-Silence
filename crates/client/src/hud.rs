//! Minimal diegetic HUD (FR-011): speed/throttle, flight-assist mode, an aiming
//! reticle, and hit/destroy feedback — no number spam (SC-006).
//!
//! Phase E adds two **segmented "VU-meter" bars** at the bottom — **Energy** (the weapon
//! capacitor; empties as you fire, recharges) and **Heat** (fills as you fire, dissipates) — read
//! straight from the embedded server world (like the dev panel), color-graded green→amber→red with
//! a critical pulse so you can track them in combat without looking away.

use bevy::prelude::*;
use sim::components::{Energy, FlightAssist, Health, Heat, Ship, Velocity};
use sim::damage::HitKind;
use sim::HitFeedback;

use crate::net::{LoopbackHost, NetClientState};

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

/// Which bottom HUD pool a segment/label belongs to (Phase E).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    /// Weapon capacitor — want it FULL (empties as you fire).
    Energy,
    /// Heat — want it EMPTY (fills as you fire; full = overheated).
    Heat,
}

/// One lit/unlit segment of a bottom VU-meter bar.
#[derive(Component)]
pub struct BarSegment {
    pub bar: BarKind,
    pub index: usize,
}

/// The text label (`ENRG 72/120`) at the head of a bar — the detail-on-focus layer.
#[derive(Component)]
pub struct BarLabel(pub BarKind);

/// Segments per bar (the "stacked vertical bars" you count at a glance).
const BAR_SEGMENTS: usize = 24;

/// An unlit segment's colour (dim, so the lit portion reads as the level).
fn seg_dim() -> Color {
    Color::srgb(0.12, 0.12, 0.16)
}

/// green→amber→red ramp where `bad = 0` is good (green) and `bad = 1` is critical (red).
fn grade(bad: f32) -> Color {
    let b = bad.clamp(0.0, 1.0);
    if b < 0.5 {
        let k = b * 2.0; // green → amber
        Color::srgb(k, 0.85 - 0.10 * k, 0.18 * (1.0 - k))
    } else {
        let k = (b - 0.5) * 2.0; // amber → red
        Color::srgb(1.0 - 0.05 * k, 0.75 - 0.60 * k, 0.10 * k)
    }
}

/// Scale a colour's RGB by `k` (the critical-pulse brightness oscillation).
fn scale_rgb(c: Color, k: f32) -> Color {
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

/// Spawn the two bottom segmented bars (Energy, Heat) — Phase E. A bottom-anchored, full-width
/// column that centres its two rows; each row is a text label + `BAR_SEGMENTS` thin vertical
/// segments (lit by `current/max`, coloured by `update_energy_hud`).
pub fn setup_energy_bars(mut commands: Commands) {
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(16.0),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(4.0),
            ..default()
        })
        .with_children(|col| {
            for (bar, name) in [(BarKind::Energy, "ENRG"), (BarKind::Heat, "HEAT")] {
                col.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(2.0),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Text::new(format!("{name}  --")),
                        TextFont {
                            font_size: 13.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.80, 0.90, 1.0)),
                        Node {
                            width: Val::Px(96.0),
                            ..default()
                        },
                        BarLabel(bar),
                    ));
                    for index in 0..BAR_SEGMENTS {
                        row.spawn((
                            Node {
                                width: Val::Px(7.0),
                                height: Val::Px(16.0),
                                ..default()
                            },
                            BackgroundColor(seg_dim()),
                            BarSegment { bar, index },
                        ));
                    }
                });
            }
        });
}

/// Refresh the bottom bars each frame from the local ship's live `Energy`/`Heat`, read from the
/// embedded server world (like the dev panel). Lit segments = `current/max`; colour grades
/// green→amber→red (Energy reddens as it EMPTIES, Heat as it FILLS) and the critical band pulses.
pub fn update_energy_hud(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    time: Res<Time>,
    mut segs: Query<(&BarSegment, &mut BackgroundColor)>,
    mut labels: Query<(&BarLabel, &mut Text), Without<HudText>>,
) {
    // Resolve the local ship's pools (None until it exists / is fitted).
    let (energy, heat) = match (host.as_ref(), net.as_ref()) {
        (Some(host), Some(net)) => match host.server.ship_entity_for(net.local_id) {
            Some(e) => {
                let w = host.server.world();
                (
                    w.get::<Energy>(e).map(|p| (p.current, p.max)),
                    w.get::<Heat>(e).map(|p| (p.current, p.max)),
                )
            }
            None => (None, None),
        },
        _ => (None, None),
    };
    let frac = |v: Option<(f32, f32)>| {
        v.map(|(c, m)| {
            if m > 0.0 {
                (c / m).clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
    };
    let efrac = frac(energy);
    let hfrac = frac(heat);
    // Critical-pulse brightness oscillation (0.55..=1.0), applied only in a bar's danger band.
    let pulse = 0.55 + 0.45 * (time.elapsed_secs() * 9.0).sin().abs();

    for (seg, mut bg) in &mut segs {
        let f = match seg.bar {
            BarKind::Energy => efrac,
            BarKind::Heat => hfrac,
        };
        let Some(f) = f else {
            bg.0 = seg_dim();
            continue;
        };
        if (seg.index as f32) >= f * BAR_SEGMENTS as f32 {
            bg.0 = seg_dim();
            continue;
        }
        let (bad, critical) = match seg.bar {
            BarKind::Energy => (1.0 - f, f < 0.2),
            BarKind::Heat => (f, f > 0.85),
        };
        let mut c = grade(bad);
        if critical {
            c = scale_rgb(c, pulse);
        }
        bg.0 = c;
    }

    for (lbl, mut text) in &mut labels {
        let (name, v) = match lbl.0 {
            BarKind::Energy => ("ENRG", energy),
            BarKind::Heat => ("HEAT", heat),
        };
        text.0 = match v {
            Some((c, m)) => format!("{name} {c:>3.0}/{m:<3.0}"),
            None => format!("{name}  --"),
        };
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
