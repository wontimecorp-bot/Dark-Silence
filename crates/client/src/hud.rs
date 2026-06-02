//! Minimal diegetic HUD (FR-011): speed/throttle, flight-assist mode, an aiming
//! reticle, and hit/destroy feedback — no number spam (SC-006).

use bevy::prelude::*;
use sim::components::{FlightAssist, Health, Ship, Velocity};
use sim::HitFeedback;

/// Marker for the readout text node.
#[derive(Component)]
pub struct HudText;

/// Spawn the readout line (top-left) and a centred aiming reticle.
pub fn setup_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("SPD   0.0   ASSIST   HP 100"),
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

    // Aiming reticle: a small dot at screen centre.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(6.0),
            height: Val::Px(6.0),
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.6)),
    ));
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
        FlightAssist::On => "ASSIST",
        FlightAssist::Off => "MANUAL",
    };
    let flash = if feedback.destroy_flash > 0.0 {
        "   KILL"
    } else if feedback.hit_flash > 0.0 {
        "   HIT"
    } else {
        ""
    };
    text.0 = format!("SPD {speed:>5.1}   {mode}   HP {:>3.0}{flash}", health.0);
}
