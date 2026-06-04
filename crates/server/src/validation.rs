//! Server-side input validation + authoritative resolution (OBJ5, Principle I).
//!
//! Every client input is untrusted (Principle I): this module is the pure,
//! unit-testable core of the validate-and-apply chokepoint. It holds four
//! independently testable hooks, none of which mutate game state on a bad input:
//!
//! - [`validate_input`] (T050, TR-011/020) — per-field validation: the analog
//!   `forward`/`strafe`/`turn` axes are **clamped** to the quantized `-1..=1`
//!   range, `toggle_assist` is **accepted** as-is, and `fire` is left for the
//!   rate gate. A value outside range is *bounded*, never trusted.
//! - [`fire_allowed`] (T051, TR-021) — the fire-rate gate, mirroring the sim's
//!   authoritative `sim::weapon::can_fire` cooldown check so a client cannot
//!   bypass it. The sim's `weapon_fire_system` is the authoritative gate; this
//!   helper exists so a test can assert "fire faster than cooldown → no extra
//!   projectile" without reaching into the schedule.
//! - [`apply_authoritative`] (T053, TR-012/019) — turn a validated intent into
//!   the `sim::ShipIntent` written onto the firer's OWN ship. The wire
//!   `ClientInput` carries no position or hit claim (a structural guarantee, see
//!   [`ValidatedIntent`]); motion comes from the server `sim`, and a validated
//!   but physically impossible input is governed by the sim's own constraint
//!   resolution — the server does NOT special-case it (TR-019).
//! - [`rewind`] + [`History`] (T054, TR-012/017) — baseline lag-compensated hit
//!   resolution. A per-entity transform-history ring is rewound to the firer's
//!   viewed time (interpolation delay + RTT, capped 500 ms; oldest-retained
//!   fallback, never extrapolated) so hit resolution stays server-authoritative
//!   against where targets actually were on the firer's screen.

use std::collections::VecDeque;

use glam::Vec2;
use protocol::QuantizedIntent;
use sim::collision::segment_circle_toi;
use sim::{ShipIntent, Weapon};

/// The validated, server-trusted form of one client input (T050, TR-020).
///
/// Distinct from [`protocol::QuantizedIntent`] (the *untrusted* wire form): the
/// analog axes here are guaranteed in `-1..=1` (clamped, never trusted), and the
/// type carries **no position or hit field** — the structural guarantee of
/// TR-012 that a client cannot assert where it is or what it hit. Motion and hits
/// are derived by the server `sim` alone.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ValidatedIntent {
    /// Forward (+1) / reverse (−1) thrust, clamped to `-1.0..=1.0`.
    pub forward: f32,
    /// Strafe left (+1) / right (−1), clamped to `-1.0..=1.0`.
    pub strafe: f32,
    /// Turn left (+1) / right (−1), clamped to `-1.0..=1.0`.
    pub turn: f32,
    /// Fire this step. Accepted into the intent, but the authoritative
    /// projectile is still gated by the sim weapon cooldown ([`fire_allowed`]).
    pub fire: bool,
    /// Toggle flight-assist this step. Any boolean is in-bounds, so it is
    /// accepted as-is (TR-020).
    pub toggle_assist: bool,
    /// Phase F — hold the afterburner this step. Accepted as-is (any boolean in-bounds).
    pub afterburner: bool,
}

impl From<ValidatedIntent> for ShipIntent {
    fn from(v: ValidatedIntent) -> Self {
        Self {
            forward: v.forward,
            strafe: v.strafe,
            turn: v.turn,
            fire: v.fire,
            toggle_assist: v.toggle_assist,
            afterburner: v.afterburner,
        }
    }
}

/// The valid analog-axis bound: a quantized wire axis maps to `-1.0..=1.0`
/// (TR-020). Anything outside is clamped, never trusted.
const AXIS_MIN: f32 = -1.0;
const AXIS_MAX: f32 = 1.0;

/// T050 (TR-011/020): per-field input validation — the pure clamp.
///
/// Maps an untrusted [`QuantizedIntent`] to a [`ValidatedIntent`] with each field
/// resolved by its single defined behavior (TR-020):
/// - `forward`/`strafe`/`turn`: the `i8` wire value is mapped to an analog axis
///   and **clamped** to `-1.0..=1.0`. A wire value outside `-1..=1` (which a
///   well-behaved quantizer never emits, but a hostile client might) is bounded,
///   not trusted and not rejected.
/// - `fire`: **accepted** into the intent here; the authoritative gate is the sim
///   weapon cooldown ([`fire_allowed`] / `weapon_fire_system`).
/// - `toggle_assist`: **accepted** as-is (any boolean is in-bounds).
///
/// Pure and total: it never panics and never mutates anything, so the clamp is
/// trivially unit-testable.
pub fn validate_input(intent: &QuantizedIntent) -> ValidatedIntent {
    ValidatedIntent {
        forward: clamp_axis(intent.forward),
        strafe: clamp_axis(intent.strafe),
        turn: clamp_axis(intent.turn),
        fire: intent.fire,
        toggle_assist: intent.toggle_assist,
        afterburner: intent.afterburner,
    }
}

/// Clamp a quantized `i8` axis to the analog `-1.0..=1.0` range (TR-020). The
/// value is bounded, never trusted: a hostile `i8::MAX` becomes `+1.0`.
fn clamp_axis(value: i8) -> f32 {
    (value as f32).clamp(AXIS_MIN, AXIS_MAX)
}

/// T051 (TR-021): the fire-rate gate, mirroring the sim's authoritative cooldown.
///
/// Returns whether a `fire` intent may produce a projectile *right now* given the
/// firing entity's authoritative [`Weapon`]. This mirrors `sim::weapon::can_fire`
/// (the cooldown must have elapsed, `cooldown <= 0`), so a client firing faster
/// than the cooldown cannot bypass the gate: the server relies on the sim's own
/// `weapon_fire_system`, which performs the *authoritative* gate every step.
///
/// This helper does NOT fire; it only reports the gate decision, so a test can
/// assert "fire faster than cooldown → excess fires produce no projectile" purely
/// against the weapon state. The authoritative truth remains the sim system.
pub fn fire_allowed(weapon: &Weapon) -> bool {
    sim::weapon::can_fire(weapon.cooldown)
}

/// T053 (TR-012/019): produce the authoritative [`ShipIntent`] for a validated
/// input — the ONLY thing the server applies to the firer's own ship.
///
/// The server applies validated *intent* and nothing else (TR-012): the wire
/// `ClientInput` carries no position or hit claim ([`ValidatedIntent`] has no such
/// field — the structural guarantee), so there is nothing client-asserted to
/// ignore. Motion comes from the server `sim`. Per TR-019, a validated, in-bounds
/// input that would drive toward a physically impossible result is NOT
/// special-cased here: the server sets this intent on the ship and the sim's own
/// constraint resolution (collision, kinematic limits in the flight/collision
/// systems) governs the outcome. This is therefore a pure mapping, not a
/// physics-aware filter.
pub fn apply_authoritative(validated: ValidatedIntent) -> ShipIntent {
    validated.into()
}

// --- T054: lag-compensated hit resolution -------------------------------------

/// Interpolation delay (seconds) the client buffers remote entities by (TR-010,
/// baseline 100 ms). The firer saw targets at `now - (interp_delay + rtt)`, so the
/// server rewinds to that viewed time before resolving a hit.
pub const INTERP_DELAY: f32 = 0.100;

/// Hard cap on the rewindable interval (TR-017, 500 ms). A larger
/// `interp_delay + rtt` is clamped to this — beyond it, rewind is not trusted.
pub const MAX_REWIND: f32 = 0.500;

/// How many transform samples each entity's [`History`] retains. At 30 Hz a tick
/// is ~33.3 ms, so 20 samples cover ~666 ms — comfortably more than the 500 ms
/// `MAX_REWIND` window (≥ 15 ticks ≈ 500 ms is the floor; 20 gives margin so a
/// `MAX_REWIND` rewind never falls off the back of the ring during normal play).
pub const HISTORY_LEN: usize = 20;

/// A single retained transform sample: where an entity was at a given server time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformSample {
    /// Server time (seconds) this sample was taken at.
    pub time: f32,
    /// World-space position at that time.
    pub pos: Vec2,
    /// Collision radius at that time (carried so rewind resolution needs no
    /// separate live lookup; radius is effectively constant but kept per-sample
    /// to keep [`rewind`] self-contained and pure).
    pub radius: f32,
}

/// A per-entity transform-history ring (T054, TR-017).
///
/// Holds the most recent [`HISTORY_LEN`] [`TransformSample`]s, newest at the back.
/// Sized to cover ≥ 500 ms at 30 Hz so a lag-compensated rewind to the firer's
/// viewed time lands inside the retained window under normal latency; a viewed
/// time older than the oldest retained sample resolves against that oldest sample
/// (no extrapolation, TR-017).
#[derive(Clone, Debug, Default)]
pub struct History {
    samples: VecDeque<TransformSample>,
}

impl History {
    /// An empty history.
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(HISTORY_LEN),
        }
    }

    /// Record a transform sample at `time`, evicting the oldest if the ring is
    /// full (bounded — never grows past [`HISTORY_LEN`], TR-027 spirit).
    pub fn push(&mut self, time: f32, pos: Vec2, radius: f32) {
        if self.samples.len() == HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples
            .push_back(TransformSample { time, pos, radius });
    }

    /// Number of retained samples.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the history holds no samples yet.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The oldest retained sample, if any (the fallback for a too-old rewind).
    pub fn oldest(&self) -> Option<TransformSample> {
        self.samples.front().copied()
    }

    /// The newest retained sample, if any.
    pub fn newest(&self) -> Option<TransformSample> {
        self.samples.back().copied()
    }
}

/// T054 (TR-017): the viewed time to rewind a firer's targets to.
///
/// `viewed = now - min(interp_delay + rtt, MAX_REWIND)`. Over loopback `rtt ≈ 0`,
/// so the rewind ≈ the interpolation delay. The interval is capped at
/// [`MAX_REWIND`] (500 ms) so a pathological RTT cannot rewind arbitrarily far.
pub fn viewed_time(now: f32, rtt: f32) -> f32 {
    let interval = (INTERP_DELAY + rtt.max(0.0)).min(MAX_REWIND);
    now - interval
}

/// T054 (TR-017): pure rewind of one entity's transform to `viewed_time`.
///
/// Interpolates linearly between the two retained samples bracketing
/// `viewed_time`. If `viewed_time` predates the oldest retained sample it resolves
/// against the **oldest** sample (NO extrapolation, TR-017); if it is newer than
/// the newest sample it resolves against the newest (also no forward
/// extrapolation). Returns `None` only when the history is empty (the entity has
/// no recorded transform yet). Pure and unit-testable.
pub fn rewind(history: &History, viewed_time: f32) -> Option<TransformSample> {
    let oldest = history.oldest()?;
    // Too old → oldest retained state, never extrapolated past the window.
    if viewed_time <= oldest.time {
        return Some(oldest);
    }
    let newest = history.newest()?;
    // Newer than anything retained → clamp to newest (no forward extrapolation).
    if viewed_time >= newest.time {
        return Some(newest);
    }
    // Find the bracketing pair [a, b] with a.time <= viewed_time <= b.time and
    // interpolate. The ring is small (≤ HISTORY_LEN), so a linear scan is fine.
    let mut prev = oldest;
    for &sample in history.samples.iter() {
        if sample.time >= viewed_time {
            let span = sample.time - prev.time;
            let t = if span > f32::EPSILON {
                (viewed_time - prev.time) / span
            } else {
                0.0
            };
            return Some(TransformSample {
                time: viewed_time,
                pos: prev.pos.lerp(sample.pos, t),
                // Radius is treated as constant across the pair; take the bracket
                // start's (they match in practice).
                radius: prev.radius,
            });
        }
        prev = sample;
    }
    // Unreachable given the bounds checks above, but fall back to newest.
    Some(newest)
}

/// T054 (TR-012/017): the server-authoritative hit-resolution entry point.
///
/// Resolves whether a shot sweeping `shot_prev → shot_now` (the projectile's swept
/// segment) hits a candidate target whose recorded motion is `target_history`,
/// **rewound to the firer's viewed time** (`now - min(interp_delay + rtt, cap)`).
/// Uses the sim's swept segment-circle primitive ([`segment_circle_toi`]) against
/// the rewound target position — hit resolution is server-authoritative and the
/// client's view of the target is reconstructed, not trusted.
///
/// Returns the time-of-impact `t ∈ [0, 1]` along the shot segment on a hit, or
/// `None` (miss / no target history). Pure: it reads history and shot endpoints,
/// mutates nothing.
pub fn resolve_hit(
    target_history: &History,
    shot_prev: Vec2,
    shot_now: Vec2,
    now: f32,
    rtt: f32,
) -> Option<f32> {
    let viewed = viewed_time(now, rtt);
    let target = rewind(target_history, viewed)?;
    segment_circle_toi(shot_prev, shot_now, target.pos, target.radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(forward: i8, strafe: i8, turn: i8, fire: bool, toggle: bool) -> QuantizedIntent {
        QuantizedIntent {
            forward,
            strafe,
            turn,
            fire,
            toggle_assist: toggle,
            afterburner: false,
        }
    }

    #[test]
    fn validate_clamps_out_of_range_axes_and_accepts_flags() {
        // A hostile client sends axes far outside the quantized -1..=1 range.
        let v = validate_input(&intent(i8::MAX, i8::MIN, 50, true, true));
        assert_eq!(v.forward, 1.0, "forward clamps to the upper bound");
        assert_eq!(v.strafe, -1.0, "strafe clamps to the lower bound");
        assert_eq!(v.turn, 1.0, "turn clamps to the upper bound");
        // Flags are accepted as-is (TR-020): any boolean is in-bounds.
        assert!(v.fire);
        assert!(v.toggle_assist);
    }

    #[test]
    fn validate_passes_in_range_axes_unchanged() {
        let v = validate_input(&intent(1, 0, -1, false, false));
        assert_eq!((v.forward, v.strafe, v.turn), (1.0, 0.0, -1.0));
        assert!(!v.fire);
        assert!(!v.toggle_assist);
    }

    #[test]
    fn fire_gate_mirrors_sim_cooldown() {
        let cool = Weapon {
            cooldown: 0.0,
            fire_rate: 5.0,
            muzzle_speed: 200.0,
        };
        let hot = Weapon {
            cooldown: 0.2,
            ..cool
        };
        assert!(fire_allowed(&cool), "a cool weapon may fire");
        assert!(!fire_allowed(&hot), "a weapon on cooldown may not fire");
    }

    #[test]
    fn apply_authoritative_carries_only_intent_no_position() {
        // The validated intent has no position/hit field to leak; apply maps it
        // straight to a ShipIntent the sim drives.
        let v = ValidatedIntent {
            forward: 0.5,
            strafe: -0.5,
            turn: 1.0,
            fire: true,
            toggle_assist: false,
            afterburner: false,
        };
        let intent: ShipIntent = apply_authoritative(v);
        assert_eq!(intent.forward, 0.5);
        assert_eq!(intent.strafe, -0.5);
        assert_eq!(intent.turn, 1.0);
        assert!(intent.fire);
        assert!(!intent.toggle_assist);
    }

    #[test]
    fn viewed_time_over_loopback_is_just_interp_delay() {
        // rtt ≈ 0 over loopback → rewind ≈ the interpolation delay.
        let now = 10.0;
        assert!((viewed_time(now, 0.0) - (now - INTERP_DELAY)).abs() < 1e-6);
    }

    #[test]
    fn viewed_time_caps_at_max_rewind() {
        let now = 10.0;
        // A pathological RTT cannot rewind further than MAX_REWIND.
        assert!((viewed_time(now, 5.0) - (now - MAX_REWIND)).abs() < 1e-6);
    }

    #[test]
    fn rewind_interpolates_within_the_ring() {
        let mut h = History::new();
        h.push(0.0, Vec2::new(0.0, 0.0), 1.0);
        h.push(1.0, Vec2::new(10.0, 0.0), 1.0);
        let s = rewind(&h, 0.5).expect("inside the ring");
        assert!(
            (s.pos.x - 5.0).abs() < 1e-4,
            "linear interpolation midpoint"
        );
    }

    #[test]
    fn rewind_too_old_falls_back_to_oldest_no_extrapolation() {
        let mut h = History::new();
        h.push(5.0, Vec2::new(100.0, 0.0), 1.0);
        h.push(6.0, Vec2::new(110.0, 0.0), 1.0);
        // viewed time predates the oldest sample → oldest retained, not extrapolated.
        let s = rewind(&h, 0.0).expect("history non-empty");
        assert_eq!(s.pos, Vec2::new(100.0, 0.0));
    }

    #[test]
    fn rewind_empty_history_is_none() {
        assert!(rewind(&History::new(), 1.0).is_none());
    }

    #[test]
    fn history_ring_is_bounded_to_history_len() {
        let mut h = History::new();
        for i in 0..(HISTORY_LEN as i32 + 10) {
            h.push(i as f32, Vec2::new(i as f32, 0.0), 1.0);
        }
        assert_eq!(h.len(), HISTORY_LEN, "ring never grows past the bound");
        // The oldest retained is the (10)th pushed, not the 0th (evicted).
        assert_eq!(h.oldest().unwrap().time, 10.0);
    }

    #[test]
    fn resolve_hit_uses_rewound_target_position() {
        // Target sat at the origin 100 ms ago (the firer's viewed time over
        // loopback), then moved away. A shot aimed where it WAS should hit.
        let now = 10.0;
        let mut h = History::new();
        h.push(now - INTERP_DELAY, Vec2::ZERO, 1.0);
        h.push(now, Vec2::new(50.0, 0.0), 1.0);
        let hit = resolve_hit(&h, Vec2::new(-10.0, 0.0), Vec2::new(10.0, 0.0), now, 0.0);
        assert!(hit.is_some(), "shot resolves against the rewound position");
    }

    #[test]
    fn resolve_hit_misses_present_position_when_target_has_moved() {
        // Same setup, but aim at where the target is NOW — the rewound position is
        // back at the origin, so a shot far away from the origin misses.
        let now = 10.0;
        let mut h = History::new();
        h.push(now - INTERP_DELAY, Vec2::ZERO, 1.0);
        h.push(now, Vec2::new(50.0, 0.0), 1.0);
        let hit = resolve_hit(&h, Vec2::new(40.0, 5.0), Vec2::new(60.0, 5.0), now, 0.0);
        assert!(
            hit.is_none(),
            "shot at the present position misses the rewound one"
        );
    }
}
