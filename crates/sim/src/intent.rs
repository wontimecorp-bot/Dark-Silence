//! The pilot's per-step intent — the seam between client input and `sim`.
//!
//! The Bevy client's input system writes this resource each frame; `sim`
//! systems read it. Gameplay reacts only to intents, never to raw device input
//! (Principle II): the same systems run unchanged when E003 feeds intents from
//! the network instead of the keyboard.

use bevy_ecs::prelude::Resource;

/// Discrete pilot inputs for the current step. Axes are in `-1.0..=1.0`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct ShipIntent {
    /// Forward (`+1`) / reverse (`-1`) thrust.
    pub forward: f32,
    /// Strafe left (`+1`) / right (`-1`).
    pub strafe: f32,
    /// Turn left (`+1`) / right (`-1`).
    pub turn: f32,
    /// Fire the weapon this step.
    pub fire: bool,
    /// Toggle the flight-assist mode this step (edge-triggered by the client).
    pub toggle_assist: bool,
}
