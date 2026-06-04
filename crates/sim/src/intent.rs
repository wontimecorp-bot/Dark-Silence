//! The pilot's per-step intent — the seam between client input and `sim`.
//!
//! Each controllable ship carries its OWN `ShipIntent` **component**: the
//! authoritative server can drive N independently-controlled ships in one shared
//! step (SC-001 / TR-002), the Bevy client writes the local player ship's
//! component each frame, and `sim` systems query it per-entity. Gameplay reacts
//! only to intents, never to raw device input (Principle II): the same systems
//! run unchanged whether intents come from the keyboard or the network.
//!
//! A ship without a `ShipIntent` component simply receives no piloted thrust;
//! AI-driven ships (and other entities) are steered by their own systems and do
//! not need one.

use bevy_ecs::prelude::Component;

/// Discrete pilot inputs for the current step, attached to the ship it pilots.
/// Axes are in `-1.0..=1.0`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
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
    /// Phase F — hold the afterburner (boost) this step. Boosts translational thrust while the
    /// [`Afterburner`](crate::components::Afterburner) pool has charge.
    pub afterburner: bool,
}
