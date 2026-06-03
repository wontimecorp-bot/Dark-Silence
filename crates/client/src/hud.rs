//! Minimal diegetic HUD (FR-011): speed/throttle, flight-assist mode, an aiming
//! reticle, and hit/destroy feedback — no number spam (SC-006).

use bevy::prelude::*;
use sim::components::{FlightAssist, Health, Ship, Velocity};
use sim::damage::HitKind;
use sim::HitFeedback;

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

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
