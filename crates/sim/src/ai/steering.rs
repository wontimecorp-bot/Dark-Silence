//! AI steering substrate (00008-ship-ai T006/T007, OBJ1): inertia-aware
//! steering primitives + 16-slot context maps that emit **`ShipIntent` only**.
//!
//! Everything here is a PURE deterministic function — no systems, no queries.
//! Brains (T010+) call the primitives / build a [`ContextMap`], resolve a
//! desired world-frame direction + throttle, and convert it through
//! [`steer_to_intent`] into a [`ShipIntent`] value that the CALLER writes to
//! the ship's component. Steering never touches `Velocity`/`Heading`/
//! `Position` (TR-001, V-6) — the real flight model (`ship_motion_system`)
//! consumes the intent exactly as it would a player's.
//!
//! **Conventions** (matching `flight.rs` / `intent.rs`): heading `0` = +X,
//! increasing CCW; `ShipIntent.turn > 0` = turn left (CCW, the `turn_ccw`
//! torque channel); throttle/forward in `0..=1`. Slot `i` of an `n`-slot
//! context map points along world angle `2π·i/n`.
//!
//! **Inertia awareness** (TR-003): ships are non-holonomic — they cannot snap
//! their heading, and at speed their momentum carries them along the velocity
//! vector regardless of where the nose points. Two mechanisms account for it:
//! [`compose_intent`] gates forward throttle by nose alignment (turn first,
//! then burn — the proven mining-transport nav pattern), and
//! [`reachability_bias`] blends the desired direction toward the current
//! velocity direction when speed is high, the desired direction is far off the
//! velocity vector, and the ship's turn authority is low — so a fast, sluggish
//! ship swings through reachable headings instead of chattering on a heading
//! its momentum cannot honor.
//!
//! **Context maps** (TR-002, AD-004, research "Context Steering"): 8–16 slot
//! interest + danger maps. Behaviors combine per-slot with `max`; resolution
//! uses Fray's MASKING (not subtraction) — slots whose danger exceeds the
//! minimum danger (plus `AiTuning.danger_mask_floor`) are masked out entirely,
//! so high interest can never override a lethal heading. The winner is the
//! highest-interest unmasked slot (deterministic tiebreak: lowest index), with
//! sub-slot gradient interpolation toward its better neighbor for a smooth
//! direction.

use glam::Vec2;

use crate::intent::ShipIntent;

/// Maximum context-map slots (AD-004). `AiTuning.slot_count` selects the
/// ACTIVE count `n ≤ MAX_SLOTS`; the arrays are always full-size so the type
/// is `Copy` and allocation-free.
pub const MAX_SLOTS: usize = 16;

/// Proportional gain mapping heading error (rad) to turn input (clamped ±1),
/// matching the mining transport's nav feel (`mining::TURN_GAIN`).
const TURN_GAIN: f32 = 2.5;

/// Reachability bias — speed soft-knee (world u/s): the momentum bias ramps in
/// as `speed / (speed + this)`, ≈half strength at this speed (~¼ of the seed
/// fighter's 80 u/s top speed).
const REACH_SPEED_SOFT: f32 = 20.0;

/// Reachability bias — turn-authority soft-knee (rad/s): agile ships (high
/// `turn/angular_drag`) get LESS momentum bias, as `this / (this + authority)`.
const REACH_TURN_SOFT: f32 = 2.0;

/// Reachability bias — cap on the velocity-direction blend weight, so the
/// desired direction always dominates (a biased heading, never a hijacked one).
const REACH_BIAS_MAX: f32 = 0.5;

/// Formation keeping — time constant (s) to close the slot error: desired
/// velocity = leader velocity + slot_error / this.
const FORMATION_CLOSE_TIME: f32 = 2.0;

/// Formation keeping — velocity-error magnitude (u/s) at which the throttle
/// saturates to 1. Near the slot with matched velocity the error → 0, so the
/// throttle → 0 and station-keeping is quiet (no chatter).
const FORMATION_THROTTLE_SPEED: f32 = 8.0;

/// Avoid — predictive lookahead (s): a threat is live if the CURRENT position
/// or the position `vel · this` ahead is inside the threat radius.
const AVOID_LOOKAHEAD: f32 = 0.5;

/// Waypoint following — the arrive slow-radius as a multiple of the caller's
/// `arrive_radius` on the FINAL waypoint (intermediate waypoints are flown at
/// full throttle).
const WAYPOINT_SLOW_FACTOR: f32 = 4.0;

/// Wrap an angle to `(-π, π]` (same convention as `mining`/`turret`).
pub fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}

// ---------------------------------------------------------------------------
// T006 — steering primitives (pure, deterministic)
// ---------------------------------------------------------------------------

/// Unit direction from `pos` toward `target`; `Vec2::ZERO` when coincident
/// (never NaN).
pub fn seek(pos: Vec2, target: Vec2) -> Vec2 {
    (target - pos).normalize_or_zero()
}

/// Seek with an arrival deceleration ramp: full throttle outside
/// `slow_radius`, ramping down linearly with distance inside it (the mining
/// transport's arrive feel — linear drag does the actual braking, so the ramp
/// only has to stop ADDING energy). `_vel` is reserved for closing-speed
/// damping; v1 matches the drag-braked mining nav exactly.
pub fn arrive(pos: Vec2, _vel: Vec2, target: Vec2, slow_radius: f32) -> (Vec2, f32) {
    let to = target - pos;
    let dist = to.length();
    let dir = to.normalize_or_zero();
    let throttle = (dist / slow_radius.max(f32::MIN_POSITIVE)).min(1.0);
    (dir, throttle)
}

/// Active-braking arrive (R96 Part B): unlike [`arrive`] — which only ramps the
/// throttle DOWN inside the slow radius and lets linear drag do the braking —
/// this variant computes the kinematic stopping distance from the ship's CLOSING
/// speed and, once the ship is inside that distance, REQUESTS REVERSE THRUST so
/// it actively decelerates onto the goal instead of coasting through it.
///
/// **Negative-throttle reverse-brake convention** (documented): the returned
/// throttle is `-1.0` when the ship is inside its stopping distance — a NEGATIVE
/// throttle is a request for REVERSE thrust. The caller maps it to a
/// [`ShipIntent`] with `forward < 0`, keeping the nose pointed at the goal (the
/// turn channel) so the ship brakes NOSE-ON via the retro thrusters
/// (`flight.rs` routes `intent.forward < 0` to `reverse_force`, which is not
/// strafe-gated, so this brakes any fit). Outside the stopping distance the
/// throttle is a positive ramp: full burn to the brake point, then a short
/// linear ease-in across `slow_radius` so the ship doesn't slam from full-burn
/// straight to the brake transition.
///
/// **Model**: `dir` points at the goal; `dist` is the range; `v_close =
/// max(0, vel·dir)` is the speed component CLOSING on the goal (receding/lateral
/// motion never triggers a brake). The kinematic stopping distance under a
/// constant deceleration `decel` is `v_close² / (2·decel)`, scaled by
/// `brake_aggression` (> 1 brakes EARLIER, more conservatively; < 1 later). If
/// `dist <= stop_dist` the ship is inside the braking range → `(dir, -1.0)`.
/// Otherwise `throttle = ((dist − stop_dist) / slow_radius).clamp(0, 1)`.
///
/// **Determinism / no-NaN**: every denominator is floored
/// (`decel.max(EPS)`, `slow_radius.max(EPS)`), `normalize_or_zero` handles a
/// coincident goal, and `v_close` is non-negative — so degenerate inputs
/// (`dist = 0`, `decel = 0`, zero velocity) yield finite results, never NaN.
pub fn arrive_braked(
    pos: Vec2,
    vel: Vec2,
    target: Vec2,
    slow_radius: f32,
    decel: f32,
    brake_aggression: f32,
) -> (Vec2, f32) {
    let to = target - pos;
    let dist = to.length();
    let dir = to.normalize_or_zero();
    // Closing-speed component only — receding or purely lateral motion (or a
    // coincident goal where `dir == ZERO`) contributes no brake demand.
    let v_close = vel.dot(dir).max(0.0);
    let stop_dist = brake_aggression * v_close * v_close / (2.0 * decel.max(f32::MIN_POSITIVE));
    if dist <= stop_dist {
        // Inside the braking range → request REVERSE thrust (negative throttle).
        (dir, -1.0)
    } else {
        // Full burn to the brake point, then a short linear ease across the
        // slow radius (so the approach isn't a hard full-burn → brake snap).
        let throttle = ((dist - stop_dist) / slow_radius.max(f32::MIN_POSITIVE)).clamp(0.0, 1.0);
        (dir, throttle)
    }
}

/// First-order intercept point for a chaser/projectile closing at `speed` on a
/// target at `target_pos` moving at `target_vel`, or `None` when no positive
/// intercept time exists (the target outruns the chaser).
///
/// Runs the SAME L1 solve as [`crate::turret::aim_angle`] — identical quadratic
/// setup (`(v·v − s²)t² + 2(r·v)t + r·r = 0`, same operation order) through the
/// shared `turret::smallest_positive_root`, so gunnery lead and pursuit
/// steering always agree. `aim_angle` itself is untouched (golden-trio safe).
pub fn intercept_point(from: Vec2, speed: f32, target_pos: Vec2, target_vel: Vec2) -> Option<Vec2> {
    let r = target_pos - from;
    let v = target_vel;
    let a = v.dot(v) - speed * speed;
    let b = 2.0 * r.dot(v);
    let c = r.dot(r);
    let t = crate::turret::smallest_positive_root(a, b, c)?;
    Some(target_pos + v * t)
}

/// Pursue a moving target by steering at its intercept point (lead pursuit);
/// falls back to plain [`seek`] of the current position when there is no
/// intercept — the same graceful fallback as `turret::aim_angle`.
pub fn pursue_intercept(pos: Vec2, closing_speed: f32, target_pos: Vec2, target_vel: Vec2) -> Vec2 {
    match intercept_point(pos, closing_speed, target_pos, target_vel) {
        Some(p) => seek(pos, p),
        None => seek(pos, target_pos),
    }
}

/// Follow a waypoint route: skips ahead past every waypoint already within
/// `arrive_radius` (the LAST waypoint sticks), flies intermediate waypoints at
/// full throttle, and [`arrive`]s on the final one (slow radius =
/// [`WAYPOINT_SLOW_FACTOR`]`·arrive_radius`). Returns `(dir, throttle,
/// next_idx)`; an empty route is a quiet no-op `(ZERO, 0.0, current_idx)`.
pub fn waypoint_follow(
    pos: Vec2,
    vel: Vec2,
    waypoints: &[Vec2],
    current_idx: usize,
    arrive_radius: f32,
) -> (Vec2, f32, usize) {
    if waypoints.is_empty() {
        return (Vec2::ZERO, 0.0, current_idx);
    }
    let mut idx = current_idx.min(waypoints.len() - 1);
    while idx + 1 < waypoints.len() && (waypoints[idx] - pos).length() <= arrive_radius {
        idx += 1;
    }
    let target = waypoints[idx];
    if idx + 1 == waypoints.len() {
        let (dir, throttle) = arrive(pos, vel, target, WAYPOINT_SLOW_FACTOR * arrive_radius);
        (dir, throttle, idx)
    } else {
        (seek(pos, target), 1.0, idx)
    }
}

/// Hold a formation slot relative to a leader. The slot target is
/// `leader_pos + rotate(slot_offset, leader_heading)` (offsets are authored in
/// the leader's frame, +X = leader's nose). Velocity-matching model: the
/// desired velocity is the leader's velocity plus a term closing the slot
/// error over [`FORMATION_CLOSE_TIME`]; the output direction/throttle steer
/// along the VELOCITY ERROR, so on-slot with matched velocity the error → 0 →
/// zero direction + throttle → quiet station-keeping with no oscillation (the
/// VC2 no-chatter requirement).
pub fn formation_keep(
    pos: Vec2,
    vel: Vec2,
    leader_pos: Vec2,
    leader_vel: Vec2,
    leader_heading: f32,
    slot_offset: Vec2,
) -> (Vec2, f32) {
    let slot = leader_pos + Vec2::from_angle(leader_heading).rotate(slot_offset);
    let desired_vel = leader_vel + (slot - pos) / FORMATION_CLOSE_TIME;
    let vel_err = desired_vel - vel;
    let dir = vel_err.normalize_or_zero();
    let throttle = (vel_err.length() / FORMATION_THROTTLE_SPEED).min(1.0);
    (dir, throttle)
}

/// Away-from-nearest-threat direction, or `None` when no threat is live. A
/// threat `(pos, radius)` is live when the ship's current position OR its
/// [`AVOID_LOOKAHEAD`]-predicted position is inside the radius; "nearest" =
/// smallest clearance (strictly-less comparison, so equal clearances break to
/// the lowest slice index — deterministic). Dead-center on a threat falls back
/// to `+X` (a fixed, documented escape heading — never NaN).
pub fn avoid(pos: Vec2, vel: Vec2, threats: &[(Vec2, f32)]) -> Option<Vec2> {
    let probe = pos + vel * AVOID_LOOKAHEAD;
    let mut best: Option<(f32, Vec2)> = None; // (clearance, threat pos)
    for &(tpos, radius) in threats {
        let clearance = (tpos - pos).length().min((tpos - probe).length()) - radius;
        if clearance < 0.0 && best.is_none_or(|(bc, _)| clearance < bc) {
            best = Some((clearance, tpos));
        }
    }
    best.map(|(_, tpos)| {
        let away = (pos - tpos).normalize_or_zero();
        if away == Vec2::ZERO {
            Vec2::X
        } else {
            away
        }
    })
}

/// T025 — combat range-band controller: the signed RADIAL desire for holding a
/// standoff ring of radius `standoff` around a target. Returns a value in
/// `[-1, 1]`: `> 0` = too far → close in, `< 0` = too close → open range,
/// `0` = exactly on the ring. The response is linear across the band
/// `standoff ± standoff·band_frac` and saturates at ±1 outside it, so a ship
/// eases onto its ring instead of bang-banging across it. This is what makes
/// archetypes read differently in combat (TR-006/TR-011): a Brawler's small
/// standoff yields `+` (close and slug) at the same distance where a Kiter's
/// large standoff yields `−` (open range and shoot). Pure + deterministic;
/// degenerate `standoff/band_frac ≤ 0` clamps safely (never NaN).
pub fn range_band_radial(dist: f32, standoff: f32, band_frac: f32) -> f32 {
    let width = (standoff * band_frac).max(f32::MIN_POSITIVE);
    ((dist - standoff) / width).clamp(-1.0, 1.0)
}

/// TR-003 — inertia-aware reachability bias: blends `desired_dir` toward the
/// current VELOCITY direction when momentum makes the raw desire unreachable.
///
/// **Model** (simple, deterministic, tunable): blend weight
/// `w = misalign · speed_gate · agility_gate`, capped at [`REACH_BIAS_MAX`],
/// where `misalign = (1 − v̂·d̂)/2 ∈ [0,1]` (0 = already going that way),
/// `speed_gate = speed/(speed + REACH_SPEED_SOFT)` (slow ships can point
/// anywhere — no bias), and `agility_gate = REACH_TURN_SOFT/(REACH_TURN_SOFT +
/// turn_authority)` (agile ships need less help). `turn_authority` is the
/// ship's max turn rate in rad/s (`ShipStats::max_turn_rate()`; pass `0` for
/// "unknown" → maximum caution). Keys off velocity, not heading — momentum is
/// what constrains reachability. Degenerate blends (exact-opposite vectors at
/// the cap) fall back to the unbiased desire.
pub fn reachability_bias(desired_dir: Vec2, vel: Vec2, turn_authority: f32) -> Vec2 {
    let desired = desired_dir.normalize_or_zero();
    let speed = vel.length();
    if desired == Vec2::ZERO || speed <= f32::EPSILON {
        return desired;
    }
    let vel_dir = vel / speed;
    let misalign = (1.0 - vel_dir.dot(desired)) * 0.5;
    let speed_gate = speed / (speed + REACH_SPEED_SOFT);
    let agility_gate = REACH_TURN_SOFT / (REACH_TURN_SOFT + turn_authority.max(0.0));
    let w = (misalign * speed_gate * agility_gate).min(REACH_BIAS_MAX);
    let blended = (desired * (1.0 - w) + vel_dir * w).normalize_or_zero();
    if blended == Vec2::ZERO {
        desired
    } else {
        blended
    }
}

/// Convert a chosen world-frame direction + throttle into a [`ShipIntent`]
/// for a ship at `heading`: turn input = proportional control on the wrapped
/// heading error ([`TURN_GAIN`], clamped ±1 — positive error = target to the
/// LEFT/CCW = positive turn, the `flight.rs` convention), forward throttle
/// gated by nose alignment (`max(0, nose·dir)`) so the ship turns before it
/// burns (the mining nav pattern). Strafe is 0 in v1 (most AI hulls cannot
/// strafe); fire fields stay default — combat (T026) sets them separately.
/// A zero direction → the default (coasting) intent.
pub fn compose_intent(desired_dir: Vec2, throttle: f32, heading: f32) -> ShipIntent {
    let dir = desired_dir.normalize_or_zero();
    if dir == Vec2::ZERO {
        return ShipIntent::default();
    }
    let err = wrap_angle(dir.to_angle() - heading);
    let turn = (err * TURN_GAIN).clamp(-1.0, 1.0);
    let align = Vec2::from_angle(heading).dot(dir).max(0.0);
    ShipIntent {
        forward: throttle.clamp(0.0, 1.0) * align,
        turn,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// T007 — 16-slot context maps (interest/danger + Fray masking)
// ---------------------------------------------------------------------------

/// World-frame unit direction of slot `i` in an `n`-slot map: angle `2π·i/n`.
pub fn slot_dir(i: usize, n_slots: usize) -> Vec2 {
    Vec2::from_angle(std::f32::consts::TAU * i as f32 / n_slots as f32)
}

/// Clamp the caller's active slot count (`AiTuning.slot_count`) to a usable
/// `1..=MAX_SLOTS` range (defensive — tuning is live-editable).
fn active_slots(n_slots: usize) -> usize {
    n_slots.clamp(1, MAX_SLOTS)
}

/// A 16-slot context map (AD-004): per-slot **interest** ("how much do I want
/// to go this way") and **danger** ("how lethal is this way"). Built fresh
/// each think by Active-tier brains (Mid-tier members run the danger mask
/// only); resolved with Fray masking via [`ContextMap::resolve`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ContextMap {
    pub interest: [f32; MAX_SLOTS],
    pub danger: [f32; MAX_SLOTS],
}

impl ContextMap {
    /// Write interest toward `dir` with cosine falloff: each active slot gains
    /// `weight · max(0, slot_dir·d̂)`. Combines per-slot with **max** (research:
    /// behaviors share the map by max, not sum). A zero `dir` is a no-op.
    pub fn add_interest_dir(&mut self, dir: Vec2, weight: f32, n_slots: usize) {
        let n = active_slots(n_slots);
        let d = dir.normalize_or_zero();
        if d == Vec2::ZERO {
            return;
        }
        for (i, slot) in self.interest[..n].iter_mut().enumerate() {
            *slot = slot.max(weight * slot_dir(i, n).dot(d).max(0.0));
        }
    }

    /// Write a uniform interest FLOOR `weight` across every active slot
    /// (max-combined like the directional writes). An "explore" baseline: it
    /// keeps SOME unmasked heading resolvable when the primary interest
    /// direction is fully masked by a head-on danger (e.g. an obstacle dead
    /// ahead) — so the ship picks a way AROUND instead of stalling. A `weight`
    /// at/below 0 is a no-op (the determinism-safe default). Kept small by the
    /// caller so it never overrides a real interest peak; equal-floor slots
    /// break to the lowest index in [`Self::resolve`] (deterministic).
    pub fn add_explore_floor(&mut self, weight: f32, n_slots: usize) {
        if weight <= 0.0 {
            return;
        }
        let n = active_slots(n_slots);
        for slot in &mut self.interest[..n] {
            *slot = slot.max(weight);
        }
    }

    /// Write danger toward `dir` with cosine falloff (same shape + max-combine
    /// as [`Self::add_interest_dir`], into the danger map).
    pub fn add_danger_dir(&mut self, dir: Vec2, weight: f32, n_slots: usize) {
        let n = active_slots(n_slots);
        let d = dir.normalize_or_zero();
        if d == Vec2::ZERO {
            return;
        }
        for (i, slot) in self.danger[..n].iter_mut().enumerate() {
            *slot = slot.max(weight * slot_dir(i, n).dot(d).max(0.0));
        }
    }

    /// Write danger from a positioned threat: direction = ship→threat, weight
    /// scaled by closeness (`weight · (1 − dist/radius)`, zero at/beyond the
    /// radius). Dead-center (coincident) means SURROUNDED: every active slot
    /// takes the full weight.
    pub fn add_danger_threat(
        &mut self,
        threat_pos: Vec2,
        ship_pos: Vec2,
        radius: f32,
        weight: f32,
        n_slots: usize,
    ) {
        let to = threat_pos - ship_pos;
        let dist = to.length();
        if dist >= radius {
            return;
        }
        let w = weight * (1.0 - dist / radius.max(f32::MIN_POSITIVE));
        if dist <= f32::EPSILON {
            let n = active_slots(n_slots);
            for slot in &mut self.danger[..n] {
                *slot = slot.max(w);
            }
        } else {
            self.add_danger_dir(to / dist, w, n_slots);
        }
    }

    /// Resolve the map to a smooth `(direction, strength)`, or `None` when no
    /// unmasked slot holds positive interest.
    ///
    /// Fray MASKING (research — never subtraction): find the minimum danger
    /// across active slots, mask out every slot whose danger exceeds
    /// `min_danger + danger_mask_floor` (`AiTuning.danger_mask_floor`; `0.0` =
    /// full block), then pick the highest-interest unmasked slot —
    /// deterministic tiebreak to the LOWEST index (strict `>` keeps the first
    /// maximum). Sub-slot interpolation: the winner blends toward its better
    /// unmasked neighbor by the linear gradient `t = ½·(w_better − w_worse) /
    /// (w_winner − w_worse) ∈ [0, ½]` (symmetric neighbors → exact slot
    /// direction; a straddled peak → the exact bisector). Strength = the
    /// winner's raw interest.
    pub fn resolve(&self, n_slots: usize, danger_mask_floor: f32) -> Option<(Vec2, f32)> {
        let n = active_slots(n_slots);
        let min_danger = self.danger[..n]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let masked = |i: usize| self.danger[i] > min_danger + danger_mask_floor;

        let mut best: Option<usize> = None;
        for i in 0..n {
            if masked(i) || self.interest[i] <= 0.0 {
                continue;
            }
            if best.is_none_or(|b| self.interest[i] > self.interest[b]) {
                best = Some(i);
            }
        }
        let i = best?;
        let wi = self.interest[i];

        // Sub-slot gradient toward the better unmasked neighbor.
        let neighbor = |j: usize| {
            if masked(j) {
                0.0
            } else {
                self.interest[j].max(0.0)
            }
        };
        let (prev, next) = ((i + n - 1) % n, (i + 1) % n);
        let (pv, nv) = (neighbor(prev), neighbor(next));
        let (j, wj, wk) = if nv > pv {
            (next, nv, pv)
        } else {
            (prev, pv, nv)
        };
        let dir = if wj > 0.0 && wi > wk {
            let t = (0.5 * (wj - wk) / (wi - wk)).clamp(0.0, 0.5);
            // nlerp between adjacent unit slot directions (≤45° apart): exact at
            // t = 0 (slot) and t = ½ (bisector), smooth between — never degenerate.
            (slot_dir(i, n) * (1.0 - t) + slot_dir(j, n) * t).normalize_or_zero()
        } else {
            slot_dir(i, n)
        };
        let dir = if dir == Vec2::ZERO {
            slot_dir(i, n)
        } else {
            dir
        };
        Some((dir, wi))
    }
}

/// THE V-6 SEAM — the steering substrate's ONLY output is a [`ShipIntent`]
/// **value**: the caller writes it to the ship's component; steering never
/// touches `Velocity`/`Heading`/`Position` (TR-001). Applies the TR-003
/// [`reachability_bias`] (`turn_authority` = the ship's max turn rate, rad/s)
/// and composes via [`compose_intent`] (proportional turn + alignment-gated
/// throttle, clamped ±1).
pub fn steer_to_intent(
    chosen_dir: Vec2,
    throttle: f32,
    heading: f32,
    current_vel: Vec2,
    turn_authority: f32,
) -> ShipIntent {
    compose_intent(
        reachability_bias(chosen_dir, current_vel, turn_authority),
        throttle,
        heading,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI, TAU};

    const N: usize = 16;

    // --- T006 primitives ---

    #[test]
    fn seek_returns_unit_direction_and_zero_when_coincident() {
        let d = seek(Vec2::ZERO, Vec2::new(10.0, 0.0));
        assert!((d - Vec2::X).length() < 1e-6);
        assert_eq!(
            seek(Vec2::ONE, Vec2::ONE),
            Vec2::ZERO,
            "coincident → zero, never NaN"
        );
    }

    #[test]
    fn arrive_full_throttle_outside_ramps_down_inside_slow_radius() {
        let target = Vec2::new(100.0, 0.0);
        let (dir, far) = arrive(Vec2::ZERO, Vec2::ZERO, target, 50.0);
        assert!((dir - Vec2::X).length() < 1e-6);
        assert_eq!(far, 1.0, "outside slow radius → full throttle");
        let (_, near) = arrive(Vec2::new(75.0, 0.0), Vec2::ZERO, target, 50.0);
        assert!(
            (near - 0.5).abs() < 1e-6,
            "halfway into the ramp → half throttle (got {near})"
        );
    }

    #[test]
    fn arrive_braked_cuts_to_reverse_inside_stopping_distance() {
        let target = Vec2::new(100.0, 0.0);
        let decel = 10.0;
        // Fast-closing ship just OUTSIDE its stopping distance → positive ramp,
        // not a brake. stop_dist = 1·40²/(2·10) = 80, so at dist 100 (pos x=20)
        // the ship is 20 u outside → ramp over a 50 u slow radius = 0.4.
        let fast = Vec2::new(40.0, 0.0);
        let (dir_out, t_out) = arrive_braked(Vec2::new(0.0, 0.0), fast, target, 50.0, decel, 1.0);
        assert!((dir_out - Vec2::X).length() < 1e-6, "steers at the goal");
        assert!(
            t_out > 0.0,
            "outside the brake point → positive ramp (got {t_out})"
        );
        assert!(
            (t_out - 0.4).abs() < 1e-5,
            "linear ramp across the slow radius"
        );

        // SAME fast ship moved INSIDE the stopping distance (dist 70 < 80) →
        // request reverse thrust (negative throttle).
        let (dir_in, t_in) = arrive_braked(Vec2::new(30.0, 0.0), fast, target, 50.0, decel, 1.0);
        assert!(
            (dir_in - Vec2::X).length() < 1e-6,
            "still steers at the goal"
        );
        assert_eq!(t_in, -1.0, "inside the brake point → reverse-brake request");

        // Higher brake_aggression brakes EARLIER: at the same far position the
        // stopping distance grows, so an aggression that pushes stop_dist past
        // the range flips the ramp into a brake.
        let (_, t_aggr) = arrive_braked(Vec2::new(0.0, 0.0), fast, target, 50.0, decel, 2.0);
        assert_eq!(
            t_aggr, -1.0,
            "aggression 2 → stop_dist 160 > range 100 → brakes earlier"
        );

        // A ship moving AWAY from the goal never brakes (v_close = 0).
        let (_, t_recede) = arrive_braked(
            Vec2::new(30.0, 0.0),
            Vec2::new(-40.0, 0.0),
            target,
            50.0,
            decel,
            1.0,
        );
        assert!(t_recede > 0.0, "receding → no brake demand, full ramp");

        // Degenerate inputs never NaN: coincident goal, zero decel, zero vel.
        let (d0, t0) = arrive_braked(target, fast, target, 50.0, decel, 1.0);
        assert!(d0.is_finite() && t0.is_finite() && d0 == Vec2::ZERO);
        let (_, t_z) = arrive_braked(Vec2::ZERO, fast, target, 50.0, 0.0, 1.0);
        assert!(
            t_z.is_finite(),
            "zero decel floors the denominator (got {t_z})"
        );
        let (_, t_still) =
            arrive_braked(Vec2::new(99.0, 0.0), Vec2::ZERO, target, 50.0, decel, 1.0);
        assert!(
            t_still.is_finite() && t_still >= 0.0,
            "stationary → finite ramp"
        );
    }

    #[test]
    fn pursue_intercept_matches_turret_aim_angle_lead() {
        // Crossing target — the L1 lead must agree EXACTLY in direction with gunnery's solve.
        let (pos, tpos, tvel, speed) =
            (Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(0.0, 8.0), 50.0);
        let dir = pursue_intercept(pos, speed, tpos, tvel);
        let expected = crate::turret::aim_angle(pos, tpos, tvel, Vec2::ZERO, speed, 1);
        assert!(
            wrap_angle(dir.to_angle() - expected).abs() < 1e-5,
            "pursuit lead agrees with turret::aim_angle (got {} vs {expected})",
            dir.to_angle()
        );
        assert!(dir.y > 0.05, "leads AHEAD of the crossing target");
    }

    #[test]
    fn pursue_intercept_unreachable_falls_back_to_seek() {
        // Target outrunning the chaser → chase its current position (aim_angle's fallback).
        let dir = pursue_intercept(
            Vec2::ZERO,
            50.0,
            Vec2::new(10.0, 0.0),
            Vec2::new(100.0, 0.0),
        );
        assert!((dir - Vec2::X).length() < 1e-5);
    }

    #[test]
    fn waypoint_follow_advances_past_reached_waypoints_and_arrives_on_last() {
        let route = [
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 100.0),
        ];
        // Standing on waypoint 0 → skip to 1, full throttle toward it.
        let (dir, throttle, idx) = waypoint_follow(Vec2::ZERO, Vec2::ZERO, &route, 0, 10.0);
        assert_eq!(idx, 1);
        assert!((dir - Vec2::X).length() < 1e-6);
        assert_eq!(throttle, 1.0, "intermediate waypoints fly at full throttle");
        // Near the final waypoint → arrive ramp throttles down; the last index sticks.
        let (_, t_last, idx_last) =
            waypoint_follow(Vec2::new(100.0, 80.0), Vec2::ZERO, &route, 2, 10.0);
        assert_eq!(idx_last, 2);
        assert!(t_last < 1.0, "final waypoint decelerates (got {t_last})");
        // Empty route is a quiet no-op.
        assert_eq!(
            waypoint_follow(Vec2::ZERO, Vec2::ZERO, &[], 0, 10.0),
            (Vec2::ZERO, 0.0, 0)
        );
    }

    #[test]
    fn formation_keep_is_quiet_on_slot_with_matched_velocity() {
        // On the rotated slot, moving exactly with the leader → zero dir + throttle (no chatter).
        let leader_vel = Vec2::new(5.0, 0.0);
        let slot_offset = Vec2::new(-4.0, 2.0);
        let leader_heading = 0.3;
        let on_slot = Vec2::from_angle(leader_heading).rotate(slot_offset);
        let (dir, throttle) = formation_keep(
            on_slot,
            leader_vel,
            Vec2::ZERO,
            leader_vel,
            leader_heading,
            slot_offset,
        );
        assert_eq!(dir, Vec2::ZERO);
        assert!(throttle.abs() < 1e-6);
    }

    #[test]
    fn formation_keep_steers_toward_the_heading_rotated_slot() {
        // Leader at origin heading +Y: an astern offset (0,-5) in leader frame rotates to (5,0).
        let (dir, throttle) = formation_keep(
            Vec2::ZERO,
            Vec2::ZERO,
            Vec2::ZERO,
            Vec2::ZERO,
            FRAC_PI_2,
            Vec2::new(0.0, -5.0),
        );
        assert!(
            (dir - Vec2::X).length() < 1e-5,
            "slot target rotates with the leader heading"
        );
        assert!(throttle > 0.0);
    }

    #[test]
    fn avoid_points_away_from_nearest_live_threat_only() {
        let threats = [(Vec2::new(3.0, 0.0), 5.0), (Vec2::new(0.0, 100.0), 5.0)];
        let away = avoid(Vec2::ZERO, Vec2::ZERO, &threats).expect("inside the first threat radius");
        assert!(
            (away - Vec2::new(-1.0, 0.0)).length() < 1e-6,
            "directly away from the threat"
        );
        assert_eq!(
            avoid(Vec2::new(50.0, 0.0), Vec2::ZERO, &threats),
            None,
            "clear of all threats"
        );
        // Lookahead: clear NOW but flying into the threat → still triggers.
        let inbound = avoid(
            Vec2::new(-4.0, 0.0),
            Vec2::new(20.0, 0.0),
            &[(Vec2::new(3.0, 0.0), 5.0)],
        );
        assert!(
            inbound.is_some(),
            "predicted position inside the radius triggers avoidance"
        );
    }

    #[test]
    fn range_band_radial_signs_close_far_and_holds_on_ring() {
        // Far outside the band → saturated "close in"; far inside → saturated
        // "open range"; exactly on the ring → hold; linear within the band.
        assert_eq!(range_band_radial(1000.0, 300.0, 0.25), 1.0);
        assert_eq!(range_band_radial(10.0, 300.0, 0.25), -1.0);
        assert_eq!(range_band_radial(300.0, 300.0, 0.25), 0.0);
        let half_in = range_band_radial(300.0 + 37.5, 300.0, 0.25);
        assert!(
            (half_in - 0.5).abs() < 1e-6,
            "linear in-band (got {half_in})"
        );
        // The archetype-divergence property (T025): at one distance, a small
        // (Brawler) standoff closes while a large (Kiter) standoff backs off.
        assert!(range_band_radial(500.0, 300.0, 0.25) > 0.0);
        assert!(range_band_radial(500.0, 850.0, 0.25) < 0.0);
        // Degenerate standoff never NaNs.
        assert!(range_band_radial(5.0, 0.0, 0.25).is_finite());
    }

    #[test]
    fn reachability_bias_passthrough_when_slow_and_biases_at_speed() {
        let desired = Vec2::Y;
        // Slow (or stationary) → unbiased.
        assert_eq!(reachability_bias(desired, Vec2::ZERO, 1.0), desired);
        // Fast along +X with desire +Y → biased TOWARD the velocity direction, but never past it.
        let fast = Vec2::new(60.0, 0.0);
        let out = reachability_bias(desired, fast, 1.0);
        assert!(
            out.dot(Vec2::X) > 0.0,
            "biased toward the momentum direction"
        );
        assert!(
            out.dot(Vec2::Y) > out.dot(Vec2::X),
            "the desire still dominates (w ≤ ½)"
        );
        // Higher turn authority → less bias (more agile, more trust in the raw desire).
        let agile = reachability_bias(desired, fast, 10.0);
        assert!(
            agile.dot(Vec2::Y) > out.dot(Vec2::Y),
            "agile ships get less momentum bias"
        );
    }

    // --- T007 context maps ---

    #[test]
    fn interest_toward_single_target_resolves_to_that_direction() {
        let mut map = ContextMap::default();
        let target_dir = Vec2::from_angle(0.7);
        map.add_interest_dir(target_dir, 1.0, N);
        let (dir, strength) = map.resolve(N, 0.0).expect("interest present");
        assert!(
            wrap_angle(dir.to_angle() - 0.7).abs() < TAU / N as f32 / 2.0,
            "resolves within half a slot of the target (got {})",
            dir.to_angle()
        );
        assert!(
            strength > 0.9,
            "winner strength ≈ the cosine peak (got {strength})"
        );
        // Exactly ON a slot direction → symmetric neighbors → the EXACT slot direction.
        let mut on_slot = ContextMap::default();
        on_slot.add_interest_dir(slot_dir(3, N), 1.0, N);
        let (d3, _) = on_slot.resolve(N, 0.0).expect("interest present");
        assert!(wrap_angle(d3.to_angle() - slot_dir(3, N).to_angle()).abs() < 1e-5);
    }

    #[test]
    fn danger_on_best_interest_masks_it_and_picks_best_unmasked() {
        let mut map = ContextMap::default();
        map.add_interest_dir(Vec2::X, 1.0, N); // best desire: +X …
        map.add_interest_dir(Vec2::Y, 0.8, N); // … fallback desire: +Y
        map.add_danger_dir(Vec2::X, 1.0, N); // danger dead-ahead on +X
        let (dir, strength) = map.resolve(N, 0.0).expect("an unmasked slot survives");
        // The whole +X hemisphere is masked (cosine danger > min 0); +Y (90° off) has zero danger.
        assert!(dir.dot(Vec2::Y) > 0.85, "picks the +Y fallback (got {dir})");
        assert!(dir.x <= 1e-6, "never tilts back into the danger hemisphere");
        assert!(
            (strength - 0.8).abs() < 1e-6,
            "strength = the unmasked winner's interest"
        );
        // Sanity: without the danger, +X wins.
        let mut clear = map;
        clear.danger = [0.0; MAX_SLOTS];
        let (clear_dir, _) = clear.resolve(N, 0.0).expect("interest present");
        assert!(clear_dir.dot(Vec2::X) > 0.99);
    }

    #[test]
    fn sub_slot_interpolation_returns_direction_between_straddled_slots() {
        // Interest exactly between slots 0 and 1 → equal weights → tie to slot 0,
        // better neighbor 1 → bisector = the exact straddle angle.
        let straddle = TAU / N as f32 / 2.0; // π/16
        let mut map = ContextMap::default();
        map.add_interest_dir(Vec2::from_angle(straddle), 1.0, N);
        let (dir, _) = map.resolve(N, 0.0).expect("interest present");
        let angle = dir.to_angle();
        assert!(
            wrap_angle(angle - straddle).abs() < 1e-5,
            "bisector of the straddled pair (got {angle}, want {straddle})"
        );
        assert!(
            angle > 0.0 && angle < TAU / N as f32,
            "strictly BETWEEN slot 0 and slot 1"
        );
    }

    #[test]
    fn equal_interest_ties_break_to_the_lowest_slot_index() {
        let mut map = ContextMap::default();
        map.interest[2] = 1.0;
        map.interest[10] = 1.0;
        let (dir, _) = map.resolve(N, 0.0).expect("interest present");
        assert!(
            wrap_angle(dir.to_angle() - slot_dir(2, N).to_angle()).abs() < 1e-5,
            "lowest index wins the tie (got {})",
            dir.to_angle()
        );
    }

    #[test]
    fn empty_or_fully_masked_map_resolves_none() {
        assert_eq!(
            ContextMap::default().resolve(N, 0.0),
            None,
            "no interest anywhere"
        );
        // Interest only inside a danger hemisphere → every interesting slot masked → None.
        let mut map = ContextMap::default();
        map.add_interest_dir(Vec2::X, 1.0, N);
        map.add_danger_dir(Vec2::X, 1.0, N);
        let resolved = map.resolve(N, 0.0);
        assert!(
            resolved.is_none_or(|(d, _)| d.dot(Vec2::X) < 0.5),
            "interest can never override the masked lethal heading (got {resolved:?})"
        );
    }

    #[test]
    fn danger_threat_scales_with_closeness_and_surrounds_when_coincident() {
        let mut map = ContextMap::default();
        map.add_danger_threat(Vec2::new(200.0, 0.0), Vec2::ZERO, 50.0, 1.0, N); // out of range
        assert_eq!(
            map,
            ContextMap::default(),
            "beyond the radius → no contribution"
        );
        map.add_danger_threat(Vec2::new(25.0, 0.0), Vec2::ZERO, 50.0, 1.0, N);
        assert!(
            (map.danger[0] - 0.5).abs() < 1e-5,
            "half-way in → half weight toward +X"
        );
        assert_eq!(
            map.danger[N / 2],
            0.0,
            "no danger written away from the threat"
        );
        let mut center = ContextMap::default();
        center.add_danger_threat(Vec2::ZERO, Vec2::ZERO, 50.0, 1.0, N);
        assert!(
            center.danger[..N].iter().all(|&d| (d - 1.0).abs() < 1e-6),
            "dead-center → surrounded"
        );
    }

    // --- compose / steer_to_intent (the V-6 seam) ---

    #[test]
    fn compose_intent_turn_sign_matches_the_flight_convention() {
        // flight.rs: turn > 0 drives the CCW torque channel and heading increases CCW;
        // intent.rs: "Turn left (+1) / right (-1)". Heading 0 = +X, so +Y is LEFT, -Y is RIGHT.
        let left = compose_intent(Vec2::Y, 1.0, 0.0);
        assert!(
            left.turn > 0.0,
            "target to the LEFT (CCW) → positive turn (got {})",
            left.turn
        );
        let right = compose_intent(Vec2::new(0.0, -1.0), 1.0, 0.0);
        assert!(
            right.turn < 0.0,
            "target to the RIGHT (CW) → negative turn (got {})",
            right.turn
        );
        // Large errors clamp to ±1; alignment gates the throttle (turn first, then burn).
        let behind = compose_intent(Vec2::new(-1.0, 0.0), 1.0, 0.0);
        assert_eq!(behind.turn.abs(), 1.0, "clamped to the stick limit");
        assert_eq!(behind.forward, 0.0, "no burn while pointing the wrong way");
        let ahead = compose_intent(Vec2::X, 1.0, 0.0);
        assert!((ahead.forward - 1.0).abs() < 1e-6 && ahead.turn.abs() < 1e-6);
        assert_eq!(ahead.strafe, 0.0, "strafe stays 0 in v1");
        // Sign convention holds away from heading 0 too (wrapped error across ±π).
        let wrapped = compose_intent(Vec2::from_angle(-3.0), 1.0, 3.0);
        assert!(
            wrapped.turn > 0.0,
            "shortest way across the ±π seam is CCW (got {})",
            wrapped.turn
        );
    }

    #[test]
    fn steer_to_intent_emits_only_a_clamped_intent_composed_with_the_bias() {
        let (dir, vel, heading, authority) = (Vec2::Y, Vec2::new(60.0, 0.0), 0.0, 1.0);
        let intent = steer_to_intent(dir, 1.0, heading, vel, authority);
        // Exactly the bias→compose pipeline (the V-6 seam returns a VALUE; caller writes it).
        assert_eq!(
            intent,
            compose_intent(reachability_bias(dir, vel, authority), 1.0, heading)
        );
        assert!((-1.0..=1.0).contains(&intent.turn) && (-1.0..=1.0).contains(&intent.forward));
        assert!(
            intent.turn > 0.0,
            "+Y desire from heading +X still turns left"
        );
        assert!(
            !intent.fire_primary && !intent.fire_secondary,
            "steering never fires"
        );
        // Zero direction → the default coasting intent.
        assert_eq!(
            steer_to_intent(Vec2::ZERO, 1.0, 1.0, Vec2::ZERO, 1.0),
            ShipIntent::default()
        );
        // PI sanity for the wrap helper used throughout.
        assert!((wrap_angle(PI + 0.1) + PI - 0.1).abs() < 1e-5);
    }
}
