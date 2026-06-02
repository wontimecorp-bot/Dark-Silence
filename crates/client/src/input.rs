//! Keyboard input → `sim` intents (FR-013). Maps device keys to the
//! `ShipIntent` resource that `sim` systems consume; no motion math lives here.
//!
//! Controls: W/S thrust fwd/rev, A/D rotate left/right, Q/E strafe,
//! Space fire, F toggle flight-assist, =/- zoom.

use bevy::prelude::*;
use sim::components::{FlightAssist, Ship};
use sim::ShipIntent;

/// Read the keyboard each frame (in `PreUpdate`, before the fixed step) into the
/// shared `ShipIntent`.
pub fn read_input(keys: Res<ButtonInput<KeyCode>>, mut intent: ResMut<ShipIntent>) {
    let mut forward = 0.0;
    let mut strafe = 0.0;
    let mut turn = 0.0;
    if keys.pressed(KeyCode::KeyW) {
        forward += 1.0;
    }
    if keys.pressed(KeyCode::KeyS) {
        forward -= 1.0;
    }
    if keys.pressed(KeyCode::KeyA) {
        turn += 1.0; // counter-clockwise
    }
    if keys.pressed(KeyCode::KeyD) {
        turn -= 1.0; // clockwise
    }
    if keys.pressed(KeyCode::KeyQ) {
        strafe += 1.0; // left
    }
    if keys.pressed(KeyCode::KeyE) {
        strafe -= 1.0; // right
    }
    intent.forward = forward;
    intent.strafe = strafe;
    intent.turn = turn;
    intent.fire = keys.pressed(KeyCode::Space);
    // Assist toggle is handled client-side (below) to avoid fixed-step timing
    // edges, so the sim-level intent flag stays false here.
    intent.toggle_assist = false;
}

/// Flip the ship's flight-assist mode on a fresh `F` press. Done in the client
/// (not via the fixed-step intent) so a single key press toggles exactly once
/// regardless of how many fixed steps run this frame.
pub fn toggle_assist(keys: Res<ButtonInput<KeyCode>>, mut q: Query<&mut FlightAssist, With<Ship>>) {
    if keys.just_pressed(KeyCode::KeyF) {
        for mut assist in &mut q {
            *assist = match *assist {
                FlightAssist::On => FlightAssist::Off,
                FlightAssist::Off => FlightAssist::On,
            };
        }
    }
}
