//! AI steering substrate (00008-ship-ai T006/T007, OBJ1): inertia-aware
//! steering primitives + 16-slot context maps.
//!
//! Everything here is a PURE deterministic function — no systems, no queries.
//! Brains call the primitives / build a [`ContextMap`] to produce a DESIRED
//! WORLD-FRAME VELOCITY (`v_des`); the unified
//! [`allocate_intent`](crate::ai::control::allocate_intent) controller — the
//! SOLE motion composer since R101 S8 — turns that `v_des` + a facing into the
//! [`ShipIntent`](crate::intent::ShipIntent) the caller writes to the ship's
//! component. Steering never
//! touches `Velocity`/`Heading`/`Position` (TR-001, V-6) — the real flight model
//! (`ship_motion_system`) consumes the intent exactly as it would a player's.
//!
//! **R101 history**: the legacy `compose_intent` / `steer_to_intent` /
//! `reachability_bias` composer triad (and the test-only `arrive` /
//! `waypoint_follow` / `formation_keep` primitives) were RETIRED in R101 S8 once
//! every behavior arm routed its motion through `allocate_intent`. What remains
//! here is purely `v_des` PRODUCERS ([`seek`], [`intercept_point`],
//! [`pursue_intercept`], [`range_band_radial`], [`formation_desired_vel`]) plus
//! geometry/context utilities ([`wrap_angle`], [`slot_dir`], [`avoid`], the
//! [`ContextMap`]) the producers and the controller's obstacle deflection share.
//!
//! **Conventions** (matching `flight.rs` / `intent.rs`): heading `0` = +X,
//! increasing CCW; `ShipIntent.turn > 0` = turn left (CCW, the `turn_ccw`
//! torque channel); throttle/forward in `0..=1`. Slot `i` of an `n`-slot
//! context map points along world angle `2π·i/n`.
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

/// Maximum context-map slots (AD-004). `AiTuning.slot_count` selects the
/// ACTIVE count `n ≤ MAX_SLOTS`; the arrays are always full-size so the type
/// is `Copy` and allocation-free.
pub const MAX_SLOTS: usize = 16;

/// Formation keeping — time constant (s) to close the slot error: desired
/// velocity = leader velocity + slot_error / this.
const FORMATION_CLOSE_TIME: f32 = 2.0;

/// Avoid — predictive lookahead (s): a threat is live if the CURRENT position
/// or the position `vel · this` ahead is inside the threat radius.
const AVOID_LOOKAHEAD: f32 = 0.5;

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

/// The DESIRED WORLD-FRAME VELOCITY that holds a formation slot relative to a
/// leader (R101 S4 — the `allocate_intent` controller path; the SOLE formation
/// motion since R101 S8 retired the legacy `formation_keep` steer). The slot
/// target is `leader_pos + rotate(slot_offset, leader_heading)` (offsets authored
/// in the leader's frame, +X = leader's nose); the desired velocity is the
/// leader's velocity PLUS a term closing the slot error over
/// [`FORMATION_CLOSE_TIME`]: `leader_vel + (slot − pos) / FORMATION_CLOSE_TIME`.
/// The FormationKeep brain arm emits this as a
/// [`MoveCmd::v_des`](crate::ai::control::MoveCmd) (capped at the pace `v_max`)
/// into the unified controller. On-slot with matched velocity the closing term →
/// 0 → `v_des == leader_vel`, so a settled follower coasts WITH the leader (the
/// velocity-matching formation; no chatter).
pub fn formation_desired_vel(
    pos: Vec2,
    leader_pos: Vec2,
    leader_vel: Vec2,
    leader_heading: f32,
    slot_offset: Vec2,
) -> Vec2 {
    let slot = leader_pos + Vec2::from_angle(leader_heading).rotate(slot_offset);
    leader_vel + (slot - pos) / FORMATION_CLOSE_TIME
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

    // R101 S8 — `arrive_full_throttle_outside_ramps_down_inside_slow_radius`
    // RETIRED with the `arrive` primitive: nav arriving/parking is now the
    // controller's stoppable-speed property (covered by `controller_arrives_and_
    // parks_no_overshoot` in `control.rs` + `nav_arrives_and_parks_via_controller`
    // in `tests/ai.rs`).

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

    // R101 S8 — `waypoint_follow_advances_past_reached_waypoints_and_arrives_on_last`
    // RETIRED with the `waypoint_follow` primitive (the nav route walker is now
    // brain-arm logic feeding the controller; route-walk + arrive/park behavior is
    // covered by `nav_arrives_and_parks_via_controller` in `tests/ai.rs`).

    // R101 S8 — the two `formation_keep_*` tests RETIRED with the `formation_keep`
    // primitive. The velocity-matching slot math survives as `formation_desired_vel`
    // (tested via `formation_desired_vel_*` below) and the no-chatter / settle-and-
    // hold formation property is covered through the LIVE controller arm by
    // `formation_followers_settle_and_hold_without_chatter` +
    // `formation_follower_tracks_moving_leader_via_desired_velocity` in `tests/ai.rs`.

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
    fn formation_desired_vel_matches_velocity_on_slot_and_rotates_with_heading() {
        // (a) On the heading-rotated slot, moving exactly with the leader → the
        // closing term is 0, so `v_des == leader_vel` (the velocity-matching
        // keystone: a settled follower coasts WITH the leader). This is the
        // property the retired `formation_keep`-quiet test held.
        let leader_vel = Vec2::new(5.0, 0.0);
        let slot_offset = Vec2::new(-4.0, 2.0);
        let leader_heading = 0.3;
        let on_slot = Vec2::from_angle(leader_heading).rotate(slot_offset);
        let v_des =
            formation_desired_vel(on_slot, Vec2::ZERO, leader_vel, leader_heading, slot_offset);
        assert!(
            (v_des - leader_vel).length() < 1e-6,
            "on-slot + matched velocity → v_des matches the leader (got {v_des})"
        );
        // (b) Off-slot at rest with a stationary leader → `v_des` closes toward the
        // heading-rotated slot target. Leader heading +Y: an astern offset (0,-5) in
        // the leader frame rotates to (5,0), so the closing velocity points +X.
        let v_closing = formation_desired_vel(
            Vec2::ZERO,
            Vec2::ZERO,
            Vec2::ZERO,
            FRAC_PI_2,
            Vec2::new(0.0, -5.0),
        );
        assert!(
            v_closing.x > 0.0 && v_closing.normalize().distance(Vec2::X) < 1e-5,
            "closes toward the heading-rotated slot target (got {v_closing})"
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

    // R101 S8 — `reachability_bias_passthrough_when_slow_and_biases_at_speed`
    // RETIRED with the `reachability_bias` primitive. The unified controller
    // handles momentum/reachability intrinsically: `Facing::Free` points the nose
    // at the required acceleration and the body-frame allocation brakes along the
    // velocity, so there is no separate desire-toward-velocity blend to test
    // (covered by the controller's flip-and-burn property tests in `control.rs`).

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

    // R101 S8 — `compose_intent_turn_sign_matches_the_flight_convention` and
    // `steer_to_intent_emits_only_a_clamped_intent_composed_with_the_bias` RETIRED
    // with the `compose_intent`/`steer_to_intent` composer pair. The unified
    // `allocate_intent` controller is now the sole motion composer, and it carries
    // the same turn-sign convention (CCW = positive turn) + clamping + intent-only
    // (no fire) guarantees — exercised by the controller's own tests in `control.rs`
    // (e.g. `non_strafe_hull_rotates_body_for_lateral` for the CCW turn sign,
    // `parked_ship_does_not_spin` for the zero-command coast).

    #[test]
    fn wrap_angle_normalizes_across_the_pi_seam() {
        // The wrap helper (still public; used by the controller's turn channel and
        // the pursuit-lead tests) folds onto (-π, π].
        assert!((wrap_angle(PI + 0.1) + PI - 0.1).abs() < 1e-5);
        assert!(wrap_angle(0.0).abs() < 1e-6);
        assert!((wrap_angle(TAU + 0.3) - 0.3).abs() < 1e-5);
    }
}
