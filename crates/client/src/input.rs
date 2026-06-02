//! Keyboard input → `sim` intents (FR-013). Maps device keys to the local
//! player ship's `ShipIntent` **component** that `sim` systems consume; no
//! motion math lives here.
//!
//! Controls: W/S thrust fwd/rev, A/D rotate left/right, Q/E strafe,
//! Space fire, F toggle flight-assist, =/- zoom.
//!
//! E003 (T033) adds the netcode seam: [`build_client_input`] turns the local
//! ship's current `ShipIntent` into a **numbered** [`protocol::ClientInput`] —
//! a monotonic per-client `seq`, the current `tick`, and the redundant tail of
//! recent inputs (newest-first, capped by `protocol::MAX_INPUT_TAIL`) so a
//! single lost packet self-heals (TR-006/007/027). The component write above
//! still happens every frame for immediate local control (prediction, T034).

use bevy::prelude::*;
use protocol::{ClientInput, QuantizedIntent, MAX_INPUT_TAIL};
use sim::components::{FlightAssist, Ship};
use sim::ShipIntent;

/// Read the keyboard each frame (in `PreUpdate`, before the fixed step) into the
/// local player ship's `ShipIntent` component. Intent is per-entity now, so the
/// client writes the component the same systems read per-ship.
pub fn read_input(keys: Res<ButtonInput<KeyCode>>, mut q: Query<&mut ShipIntent, With<Ship>>) {
    let Ok(mut intent) = q.single_mut() else {
        // No local player ship yet (or more than one) — nothing to pilot.
        return;
    };
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

/// The monotonic per-client input sequence and the redundant-tail history
/// (TR-007/027). `next_seq` counts every input the client has ever produced; the
/// `tail` keeps the most recent quantized intents, **newest-first**, capped at
/// [`MAX_INPUT_TAIL`] so one lost `ClientInput` packet self-heals on the next.
///
/// Defaults to `seq == 1` for the first input (a `seq` of `0` reads as "no input
/// processed yet" on the server-side ack anchor, TR-008), and an empty tail.
#[derive(Default, Debug, Clone)]
pub struct InputSequencer {
    /// The `seq` the *next* produced input will carry (monotonic, never reused).
    next_seq: u32,
    /// Recent quantized intents, newest-first, length `0..=MAX_INPUT_TAIL`.
    tail: Vec<QuantizedIntent>,
}

impl InputSequencer {
    /// A fresh sequencer: the first input it produces carries `seq == 1`.
    pub fn new() -> Self {
        Self {
            next_seq: 1,
            tail: Vec::new(),
        }
    }
}

/// Build the numbered [`protocol::ClientInput`] for the current frame from the
/// local ship's `ShipIntent` (T033, TR-007).
///
/// Mints the next monotonic `seq`, stamps the supplied `tick`, quantizes
/// `intent`, pushes it to the front of the redundant tail (newest-first, capped
/// at [`MAX_INPUT_TAIL`]), and returns a [`ClientInput`] carrying that tail. The
/// component write in [`read_input`] is unchanged — that drives immediate local
/// control (prediction); this is the wire form sent to the server.
///
/// The returned `seq` is the same monotonic id the prediction layer (T034)
/// buffers an unacked input under, so reconciliation can drop acked inputs and
/// replay the rest.
pub fn build_client_input(
    sequencer: &mut InputSequencer,
    tick: u32,
    intent: ShipIntent,
) -> ClientInput {
    let seq = sequencer.next_seq;
    sequencer.next_seq = sequencer.next_seq.wrapping_add(1);

    // Newest-first: this frame's intent goes to the front, oldest falls off the
    // back so the tail never grows past the wire bound (TR-027).
    sequencer.tail.insert(0, QuantizedIntent::from(intent));
    sequencer.tail.truncate(MAX_INPUT_TAIL);

    // `ClientInput::new` also truncates defensively, so the wire bound holds even
    // if the tail were ever oversized.
    ClientInput::new(seq, tick, sequencer.tail.clone())
}
