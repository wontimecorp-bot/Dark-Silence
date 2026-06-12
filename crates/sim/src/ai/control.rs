//! R101 unified motion controller: behaviors emit a desired velocity + facing;
//! `allocate_intent` turns that into a ShipIntent. Flip-and-burn / reverse-brake /
//! strafe-brake EMERGE from aligning thrust with the required acceleration.
//!
//! **Why this module exists** (R101 Stage S1): R96–R100 grew a thicket of
//! special-case braking primitives (`arrive_braked`, `brake_orientation`,
//! `compose_intent_aimed` — all retired and deleted in R101 S7) — each solving
//! one slice of "how do I stop / strafe / flip" with its own throttle sign
//! convention. This controller replaces the *decision* with a single law:
//! a behavior emits a DESIRED VELOCITY `v_des` and a FACING; the controller
//! computes the acceleration it needs to track that velocity (`a_req`), points
//! the available thrust channels at `a_req` in the ship's body frame, and lets
//! the named maneuvers fall out:
//! - **flip-and-burn** when `Facing::Free` lets the nose swing to retrograde so
//!   the strong forward drive does the braking;
//! - **reverse-brake** when `Facing::Aim` pins the nose forward, so the required
//!   retrograde accel lands on the (weaker) reverse channel;
//! - **strafe-brake** when the hull can strafe and the lateral accel demand
//!   lands on a strafe channel with the nose held.
//!
//! **Geometry/steering math — OUTSIDE the strict-f32 scoring fence.** This file
//! is allocation-free pure trigonometry (`normalize`/`length`/`dot`/`to_angle`/
//! `from_angle`) and carries NO utility scoring, so it is intentionally NOT
//! placed between the `STRICT-F32 SCORING BEGIN/END` markers in `brain.rs`. It
//! is still deterministic: pure `f32`, no RNG, no `HashMap`, entity-local (every
//! function depends only on its own ship's kinematics + stats — no cross-entity
//! or iteration-order coupling).
//!
//! **Stage S1 is PURELY ADDITIVE**: this module is built + unit-tested in
//! isolation; NOTHING on the execute path calls it yet (the wiring lands in
//! R101 S3/S5). Every existing test is therefore byte-unaffected.

use glam::Vec2;

use crate::ai::steering::wrap_angle;
use crate::ai::tuning::AiTuning;
use crate::broadphase::ObstacleField;
use crate::fitting::ShipStats;
use crate::intent::ShipIntent;

/// Floor for the tracking-time-constant denominator in [`allocate_intent`] so a
/// degenerate `tau_track == 0` can never divide-by-zero (never NaN/inf).
const EPS: f32 = 1e-4;

/// Minimum required-acceleration magnitude (world u/s²) for `Facing::Free` to
/// point the nose along `a_req`. Below it the controller falls down the parked
/// ladder (velocity dir, then current heading) so a settled ship does not spin
/// to chase normalize-noise on a near-zero accel.
const A_EPS: f32 = 1e-3;

/// Minimum speed (world u/s) for the `Facing::Free` fallback to point the nose
/// along the velocity vector once `a_req` is negligible. Below it the nose holds
/// the current heading (the parked-no-spin terminal of the ladder).
const V_EPS: f32 = 1e-3;

/// Floor on the turn-power factor used to PRE-DIVIDE the translational channels
/// so that, when a hard turn steals most of the drive power (`p → 0`), the
/// compensation cannot blow up to ±inf. The channels are clamped to ±1
/// afterward regardless, so this only bounds the intermediate.
const P_FLOOR: f32 = 0.05;

/// Proportional gain mapping a heading error (rad) to a turn input (clamped ±1).
/// **Replicated from** `ai::steering`'s private `TURN_GAIN` (= 2.5; the mining
/// transport's nav feel) so the controller's turn channel uses the SAME law the
/// retired legacy composers used (`compose_intent`/`steer_to_intent`, deleted in
/// R101 S8 once this controller became the sole motion composer). Kept as a
/// private const here (the steering one is not `pub`); the value MUST match.
const TURN_GAIN: f32 = 2.5;

/// A behavior's per-step motion command: a desired world-frame velocity and a
/// facing constraint. The controller ([`allocate_intent`]) turns it into a
/// [`ShipIntent`] by aligning thrust with the acceleration needed to track
/// `v_des`.
#[derive(Clone, Copy, Debug)]
pub struct MoveCmd {
    /// Desired world-frame velocity (world u/s). The controller drives the ship
    /// toward this; `Vec2::ZERO` is a "come to a stop" command.
    pub v_des: Vec2,
    /// Where the nose should point this step (see [`Facing`]).
    pub facing: Facing,
}

/// The facing constraint of a [`MoveCmd`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Facing {
    /// The controller is FREE to pick the nose direction — it points the nose at
    /// the required acceleration (so a strong forward drive does the work). This
    /// is what lets flip-and-burn EMERGE: a fast ship asked to stop has its
    /// required accel pointing retrograde, so `Free` swings the nose around.
    Free,
    /// Pin the NOSE to the unit direction `d` (e.g. a gunnery lead — keep the
    /// fixed-forward weapon bearing while translating any direction). A
    /// non-unit/zero `d` is normalized (and falls back to the current nose if
    /// degenerate), so braking happens on the reverse/strafe channels instead of
    /// by flipping.
    Aim(Vec2),
}

/// The acceleration authority of a ship's thrust geometry, in the body frame —
/// the controller's projection of [`ShipStats`] (or the unfitted `Tuning`
/// baseline) onto "how hard can I push along each axis". All accelerations are
/// `force / total_mass` (world u/s²), so they fold mass into the channel limits.
#[derive(Clone, Copy, Debug)]
pub struct ControlStats {
    /// Forward acceleration limit = `thrust_force / total_mass`.
    pub a_fwd: f32,
    /// Reverse (retro) acceleration limit = `reverse_force / total_mass`
    /// (typically ≈ ½·`a_fwd` — the retros are weaker, which is what makes the
    /// flip-and-burn pay off).
    pub a_rev: f32,
    /// Port (left, +90°/CCW) strafe acceleration limit = `strafe_port / total_mass`.
    pub a_strafe_port: f32,
    /// Starboard (right) strafe acceleration limit = `strafe_starboard / total_mass`.
    pub a_strafe_star: f32,
    /// Whether the hull has strafe authority (R93 `ShipStats::can_strafe`). When
    /// `false` the controller leaves `strafe = 0` and translates laterally by
    /// rotating the body (the non-holonomic hull).
    pub can_strafe: bool,
    /// Maximum turn rate (rad/s) = the stronger turn channel / angular drag
    /// ([`ShipStats::max_turn_rate`]). Reserved for callers sizing turn demand;
    /// the turn channel itself is the proportional [`TURN_GAIN`] law.
    pub max_turn_rate: f32,
    /// Fraction of translational thrust diverted at full turn input
    /// ([`ShipStats::turn_power_share`]); the controller compensates for it so
    /// the ship still achieves `a_req` while turning.
    pub turn_power_share: f32,
}

impl ControlStats {
    /// Project a fitted ship's [`ShipStats`] onto the body-frame acceleration
    /// limits. Mapping (the REAL field names, confirmed against
    /// `fitting/stats.rs`):
    /// - `a_fwd        = thrust_force    / total_mass`
    /// - `a_rev        = reverse_force   / total_mass`
    /// - `a_strafe_port= strafe_port     / total_mass`
    /// - `a_strafe_star= strafe_starboard/ total_mass`
    /// - `can_strafe   = ShipStats::can_strafe`
    /// - `max_turn_rate= ShipStats::max_turn_rate()` (= `max(turn_ccw, turn_cw)/angular_drag`)
    /// - `turn_power_share = ShipStats::turn_power_share`
    ///
    /// Every denominator is floored `> 0` ([`ShipStats`] already guarantees
    /// `total_mass > 0`, but the floor keeps the math total for any caller). The
    /// forces are already floored `> 0` by derivation, so the limits are finite.
    pub fn from_stats(s: &ShipStats) -> ControlStats {
        let m = s.total_mass.max(f32::MIN_POSITIVE);
        ControlStats {
            a_fwd: s.thrust_force / m,
            a_rev: s.reverse_force / m,
            a_strafe_port: s.strafe_port / m,
            a_strafe_star: s.strafe_starboard / m,
            can_strafe: s.can_strafe,
            max_turn_rate: s.max_turn_rate(),
            turn_power_share: s.turn_power_share,
        }
    }

    /// A sane projection for an UNFITTED ship (no [`ShipStats`] component) so the
    /// controller still produces sensible intent. Mirrors the global `Tuning`
    /// fallback the flight model uses for unfitted ships
    /// (`flight::FlightParams::from_tuning`): the seed-baseline fighter —
    /// `thrust_force 30`, `reverse_force 15`, `strafe_force 18`, `mass 1`,
    /// `turn_torque 12`, `angular_drag 4`, `turn_power_share 0.7`. That yields
    /// `a_fwd 30`, `a_rev 15` (≈ ½·a_fwd, so flip-and-burn still pays), strafe
    /// 18 each side, `max_turn_rate 3.0 rad/s`. `can_strafe = true` (the unfitted
    /// ship has full strafe authority, matching the un-gated flight path).
    pub fn fallback() -> ControlStats {
        // The `Tuning::default()` seed baseline (mass 1.0). Hardcoded here to keep
        // the controller free of a `Tuning` dependency on the no-stats path; the
        // numbers are the same the flight model falls back to.
        ControlStats {
            a_fwd: 30.0,
            a_rev: 15.0,
            a_strafe_port: 18.0,
            a_strafe_star: 18.0,
            can_strafe: true,
            max_turn_rate: 12.0 / 4.0, // turn_torque / angular_drag = 3.0 rad/s
            turn_power_share: 0.7,
        }
    }
}

/// The kinematic top speed (world u/s) a ship can still arrive at a goal `dist`
/// away from and brake to rest under a constant braking accel `a_brake`:
/// `v = sqrt(2·a_brake·dist)`. A behavior caps its desired closing speed at this
/// so the arrive is no-overshoot BY CONSTRUCTION (there is always enough room
/// left to stop). Both arguments are floored at `0`, so degenerate inputs yield
/// `0`, never NaN.
pub fn stoppable_speed(dist: f32, a_brake: f32) -> f32 {
    (2.0 * a_brake.max(0.0) * dist.max(0.0)).sqrt()
}

/// Replicated from `flight::turn_power_factor` (`flight.rs:110`): the available
/// translational-thrust scale under the shared power budget — hard turning
/// diverts drive power, so translational thrust is `1 − share·|turn|` (clamped
/// `0..=1`). The controller uses it to PRE-DIVIDE the translational channels so
/// the ship still achieves `a_req` while turning (and a `turn` of 0 → factor 1 →
/// a no-op). `flight::turn_power_factor` is `pub`, but it is replicated here with
/// this citation to keep the controller's body-frame allocation self-contained
/// and the formula explicit; the value MUST match the flight model.
fn turn_power_factor(turn_input: f32, share: f32) -> f32 {
    (1.0 - share * turn_input.abs()).clamp(0.0, 1.0)
}

/// THE KEYSTONE — convert a [`MoveCmd`] into a [`ShipIntent`] by aligning thrust
/// with the acceleration needed to track the desired velocity.
///
/// **The exact math** (entity-local, deterministic, no NaN):
/// 1. **Required accel** to close the velocity error over the tracking time
///    constant: `a_req = (v_des − vel) / max(tau_track, EPS)`.
/// 2. **Nose target `n_hat`** from the facing:
///    - `Aim(d)` → unit `d` (falls back to the current nose if `d` is degenerate),
///    - `Free` → the parked-no-spin ladder: point at `a_req` if it is non-trivial
///      (`|a_req| > A_EPS`), else along the velocity if moving (`|vel| > V_EPS`),
///      else hold the current heading (no spin).
/// 3. **Turn channel**: the SAME proportional law as the steering substrate —
///    `turn = (wrap_angle(n_hat.to_angle() − heading) · TURN_GAIN).clamp(±1)`.
/// 4. **Body-frame allocation**: project `a_req` onto the nose / left axes,
///    divide each component by the matching acceleration limit (forward vs
///    reverse by sign; port vs starboard by sign), clamp to ±1, then PRE-DIVIDE
///    by the turn-power factor so the achieved accel still hits `a_req` while
///    turning. A non-strafe hull keeps `strafe = 0`.
///
/// **Why the named maneuvers emerge**: `Free` braking a fast ship has
/// `a_req ≈ −vel`, so `n_hat` points retrograde — the nose swings around
/// (flip-and-burn) and the strong FORWARD channel brakes. `Aim`-pinned braking
/// keeps the nose forward, so the same retrograde `a_req` projects NEGATIVE onto
/// the nose → the REVERSE channel (reverse-brake), or onto the LEFT axis →
/// the STRAFE channel for a strafe-capable hull (strafe-brake).
///
/// All denominators are floored (`tau_track`, the accel limits via
/// [`ControlStats`], the turn-power factor via [`P_FLOOR`]) and every direction
/// uses `normalize_or_zero`, so degenerate inputs yield finite intent.
pub fn allocate_intent(
    cmd: MoveCmd,
    vel: Vec2,
    heading: f32,
    stats: ControlStats,
    tau_track: f32,
) -> ShipIntent {
    // 1. The acceleration that closes the velocity error over `tau_track`.
    let a_req = (cmd.v_des - vel) / tau_track.max(EPS);

    // The current nose, reused as the body frame AND the parked-fallback heading.
    let nose = Vec2::from_angle(heading);

    // 2. The desired nose direction (the parked-no-spin fallback ladder for Free).
    let n_hat = match cmd.facing {
        Facing::Aim(d) => {
            let u = d.normalize_or_zero();
            if u == Vec2::ZERO {
                nose
            } else {
                u
            }
        }
        Facing::Free => {
            if a_req.length() > A_EPS {
                a_req.normalize_or_zero()
            } else if vel.length() > V_EPS {
                vel.normalize_or_zero()
            } else {
                nose
            }
        }
    };
    // `normalize_or_zero` above can only zero on an already-degenerate input; in
    // that case hold the current nose so the turn error is 0 (no spin).
    let n_hat = if n_hat == Vec2::ZERO { nose } else { n_hat };

    // 3. Turn = proportional control on the wrapped heading error (the steering
    //    TURN_GAIN convention — positive error = target CCW = positive turn).
    let err = wrap_angle(n_hat.to_angle() - heading);
    let turn = (err * TURN_GAIN).clamp(-1.0, 1.0);

    // 4. Body-frame projection of the required accel (left = +90°/CCW perp of nose,
    //    matching `flight::step_ship_motion` and the steering substrate).
    let left = Vec2::new(-nose.y, nose.x);
    let f_comp = nose.dot(a_req);
    let l_comp = left.dot(a_req);

    // Forward/reverse channel by sign; strafe port/starboard by sign. Denominators
    // are the floored accel limits (never zero) from `ControlStats`.
    let mut forward = if f_comp >= 0.0 {
        f_comp / stats.a_fwd.max(f32::MIN_POSITIVE)
    } else {
        f_comp / stats.a_rev.max(f32::MIN_POSITIVE)
    }
    .clamp(-1.0, 1.0);
    let mut strafe = if stats.can_strafe {
        if l_comp >= 0.0 {
            l_comp / stats.a_strafe_port.max(f32::MIN_POSITIVE)
        } else {
            l_comp / stats.a_strafe_star.max(f32::MIN_POSITIVE)
        }
        .clamp(-1.0, 1.0)
    } else {
        0.0
    };

    // Turn-power compensation: a hard turn steals translational thrust (the flight
    // model multiplies it by `p = turn_power_factor`), so PRE-DIVIDE the channels
    // by `p` to still achieve `a_req`. Aligned/parked ⇒ turn 0 ⇒ p == 1 ⇒ a no-op.
    let p = turn_power_factor(turn, stats.turn_power_share).max(P_FLOOR);
    forward = (forward / p).clamp(-1.0, 1.0);
    strafe = (strafe / p).clamp(-1.0, 1.0);

    ShipIntent {
        forward,
        strafe,
        turn,
        ..Default::default()
    }
}

/// R101 — the UNIFIED obstacle deflection over a desired velocity, and the SOLE
/// obstacle-avoidance path on the AI execute arm (R101 S7 deleted the old
/// private `brain.rs` context-map helpers it superseded). Bends `v_des` around
/// in-range large neutral bodies: an in-range gate, an interest write toward the
/// desired direction, an [`add_explore_floor`](crate::ai::steering::ContextMap::add_explore_floor)
/// baseline, the obstacle danger, then a masked
/// [`resolve`](crate::ai::steering::ContextMap::resolve) — re-scaling the
/// resolved unit direction back to the original `|v_des|`.
///
/// **Empty-field parity keystone**: with ZERO in-range obstacles the gate is
/// `false` and this returns `v_des` UNCHANGED — so a wired caller in an
/// obstacle-free world (the golden trio) is byte-identical to not calling it at
/// all.
///
/// **The geometry**: the danger DIRECTION is `obstacle → ship`, the closeness
/// gate uses the smaller of the current and the `pos + vel·lookahead` range
/// (matching `steering::avoid`), and the avoid radius is `obstacle_radius +
/// own_radius + obstacle_clearance_pad`. Pure + deterministic: the field is
/// pre-sorted by position bits at build and `add_danger_*` combines per-slot by
/// `max`, so the result is independent of obstacle order.
pub fn deflect_v_des(
    v_des: Vec2,
    field: &ObstacleField,
    pos: Vec2,
    vel: Vec2,
    own_radius: f32,
    ai: &AiTuning,
) -> Vec2 {
    // The R101 explore-floor baseline (matches `brain::EXPLORE_FLOOR`): a small
    // uniform interest so a goal direction fully masked by a head-on obstacle
    // still resolves to a way AROUND instead of stalling.
    const EXPLORE_FLOOR: f32 = 0.25;

    let speed = v_des.length();
    let dir = v_des.normalize_or_zero();
    // No demand to deflect (zero command) → unchanged.
    if dir == Vec2::ZERO {
        return v_des;
    }

    // THE EMPTY-FIELD GATE (replicated from `brain::obstacle_in_range`): no
    // in-range obstacle → return `v_des` UNCHANGED (the parity keystone).
    let probe = pos + vel * ai.obstacle_lookahead_s;
    let in_range = field.obstacles.iter().any(|&(obs_pos, obs_radius)| {
        let near = (obs_pos - pos).length().min((obs_pos - probe).length());
        let avoid_radius = obs_radius + own_radius + ai.obstacle_clearance_pad;
        near <= ai.obstacle_query_radius && near < avoid_radius
    });
    if !in_range {
        return v_des;
    }

    // In range → build the context map: interest toward the desired direction,
    // an explore floor, then the obstacle danger. Resolve under the danger mask.
    let n = ai
        .slot_count
        .clamp(1, crate::ai::steering::MAX_SLOTS as u32) as usize;
    let mut map = crate::ai::steering::ContextMap::default();
    map.add_interest_dir(dir, 1.0, n);
    map.add_explore_floor(EXPLORE_FLOOR, n);

    // Obstacle danger (replicated from `brain::add_obstacle_danger`): danger
    // toward each in-range body (direction = obstacle → ship), closeness via the
    // predictive probe; the look-ahead position is also reckoned so a ship flying
    // INTO a currently-clear obstacle still gets the danger written.
    let danger_weight = ai.obstacle_danger_weight;
    for &(obs_pos, obs_radius) in &field.obstacles {
        let near = (obs_pos - pos).length().min((obs_pos - probe).length());
        if near > ai.obstacle_query_radius {
            continue;
        }
        let avoid_radius = obs_radius + own_radius + ai.obstacle_clearance_pad;
        map.add_danger_threat(obs_pos, pos, avoid_radius, danger_weight, n);
        if probe != pos {
            let to = obs_pos - pos;
            let dist = to.length();
            let probe_dist = (obs_pos - probe).length();
            if dist >= avoid_radius && probe_dist < avoid_radius && dist > f32::EPSILON {
                let w = danger_weight * (1.0 - probe_dist / avoid_radius);
                map.add_danger_dir(to / dist, w, n);
            }
        }
    }

    match map.resolve(n, ai.danger_mask_floor) {
        // Re-scale the resolved unit direction back to the original speed.
        Some((resolved, _)) => resolved * speed,
        // Fully masked (surrounded) → leave the command unchanged (the caller's
        // braking/arrive logic still applies); never NaN.
        None => v_des,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// A representative fitted FIGHTER: forward ≈ 2× reverse (so flip-and-burn
    /// pays), strafe between the two, a 3 rad/s turn rate, the default 0.7 power
    /// share. `can_strafe` is the variant under test.
    fn fighter(can_strafe: bool) -> ControlStats {
        ControlStats {
            a_fwd: 30.0,
            a_rev: 15.0,
            a_strafe_port: 18.0,
            a_strafe_star: 18.0,
            can_strafe,
            max_turn_rate: 3.0,
            turn_power_share: 0.7,
        }
    }

    /// A faithful mini-integrator mirroring `flight::step_ship_motion`'s assisted
    /// branch for a ship ALREADY aligned on `heading` (turn handled separately):
    /// `accel = (thrust − vel·drag)/mass`, `thrust` = the per-channel force the
    /// intent commands. Uses the fighter's drag/mass so the emergent top speed +
    /// braking match the real model. The controller test loop keeps the nose on
    /// the +X axis (1-D), so we integrate the forward/reverse channel only.
    fn step_1d(vel: f32, intent_forward: f32, a_fwd: f32, a_rev: f32, dt: f32) -> f32 {
        // Map the normalized channel back to an acceleration limit (force/mass),
        // exactly the inverse of `allocate_intent`'s channel division.
        let a_cmd = if intent_forward >= 0.0 {
            intent_forward * a_fwd
        } else {
            intent_forward * a_rev
        };
        // Drag from the seed fighter (top speed 80 at a_fwd 30 ⇒ drag/mass 0.375).
        const DRAG_OVER_MASS: f32 = 0.375;
        let accel = a_cmd - vel * DRAG_OVER_MASS;
        vel + accel * dt
    }

    #[test]
    fn controller_arrives_and_parks_no_overshoot() {
        // 1-D arrive: start far on +X, each step command v_des toward the goal,
        // capped at stoppable_speed so the brake distance is always sufficient.
        // The no-overshoot-by-construction proof: position must NEVER pass the
        // goal (beyond a tiny epsilon) and must settle at rest on the goal.
        let stats = fighter(false);
        let dt = 1.0 / 30.0;
        let goal = 1000.0_f32;
        let mut pos = 0.0_f32;
        let mut vel = 0.0_f32;
        let v_max = 80.0_f32; // the fighter's top speed.
        let mut max_pos = 0.0_f32;
        // Conservative brake authority: a real behavior caps its desired closing
        // speed at `stoppable_speed` under a brake accel below the true limit (the
        // R96 `brake_aggression` margin), so the discrete integrator has room to
        // stop without the half-step + tracking lag carrying it past the goal. Here
        // the nose is pinned +X → the brake is the reverse channel (`a_rev`), and a
        // 0.5 margin gives the velocity-tracking loop (tau 0.3 s) ample stop room.
        let a_brake = 0.5 * stats.a_rev;
        for _ in 0..4000 {
            let dist = goal - pos;
            let v_des_speed = v_max.min(stoppable_speed(dist, a_brake));
            let cmd = MoveCmd {
                v_des: Vec2::new(v_des_speed, 0.0),
                facing: Facing::Aim(Vec2::X),
            };
            let intent = allocate_intent(cmd, Vec2::new(vel, 0.0), 0.0, stats, 0.3);
            vel = step_1d(vel, intent.forward, stats.a_fwd, stats.a_rev, dt);
            pos += vel * dt;
            max_pos = max_pos.max(pos);
        }
        assert!(
            (pos - goal).abs() < 1.0,
            "converges onto the goal (pos {pos}, goal {goal})"
        );
        assert!(vel.abs() < 0.5, "settles at rest (final speed {vel})");
        // The kinematic `stoppable_speed` cap makes the arrive no-overshoot BY
        // CONSTRUCTION; the only residual is one forward-Euler step's worth of
        // tracking-lag velocity near the goal — bounded here to < 1 u, i.e. under
        // 0.1% of the 1000 u traverse (vanishing as dt → 0).
        assert!(
            max_pos <= goal + 1.0,
            "never meaningfully overshoots the goal (peak {max_pos}, goal {goal})"
        );
    }

    #[test]
    fn free_facing_flips_to_retrograde_to_brake() {
        // Moving fast along +X, asked to STOP (v_des = 0), Free facing. The
        // required accel is retrograde (−X), so the nose target flips to point
        // retrograde and the ship burns FORWARD along that retro nose.
        let stats = fighter(false);
        let vel = Vec2::new(70.0, 0.0);
        let cmd = MoveCmd {
            v_des: Vec2::ZERO,
            facing: Facing::Free,
        };
        // Heading currently +X (nose along the velocity).
        let intent = allocate_intent(cmd, vel, 0.0, stats, 0.3);
        // The commanded turn must drive the nose toward retrograde (−X). Apply the
        // turn law's target: n_hat is along a_req = (0 − vel)/tau = −X.
        let n_hat = (-vel).normalize();
        assert!(
            n_hat.dot((-vel).normalize()) > 0.9,
            "nose target is retrograde (the flip)"
        );
        // The turn is non-zero (heading +X, target −X = max error) and forward is
        // POSITIVE once aligned: re-evaluate with the nose ALREADY retrograde to
        // confirm the burn is forward (the flip-and-burn brake).
        assert!(intent.turn.abs() > 0.0, "commands a turn toward retrograde");
        let aligned = allocate_intent(cmd, vel, PI, stats, 0.3);
        assert!(
            aligned.forward > 0.0,
            "burns FORWARD along the retrograde nose (got {})",
            aligned.forward
        );
    }

    #[test]
    fn aim_constrained_brake_uses_bearing_thrust() {
        // Same braking need, but the nose is PINNED forward (Aim along +X = the
        // goal/bearing). The ship must NOT flip: it keeps the nose on the bearing
        // (small turn error) and brakes via REVERSE thrust (forward < 0).
        let stats = fighter(false); // non-strafe hull → reverse-only brake.
        let vel = Vec2::new(70.0, 0.0);
        let cmd = MoveCmd {
            v_des: Vec2::ZERO,
            facing: Facing::Aim(Vec2::X),
        };
        let intent = allocate_intent(cmd, vel, 0.0, stats, 0.3);
        assert!(
            intent.turn.abs() < 1e-3,
            "keeps the nose on the bearing (turn ≈ 0, got {})",
            intent.turn
        );
        assert!(
            intent.forward < 0.0,
            "brakes via REVERSE thrust, not a flip (got {})",
            intent.forward
        );
    }

    #[test]
    fn strafe_brake_uses_lateral_for_can_strafe() {
        // A strafe-capable hull with the nose pinned +X, asked for a LATERAL
        // velocity change (v_des along +Y): the lateral demand lands on the strafe
        // channel (strafe ≠ 0) while the nose stays on the bearing.
        let stats = fighter(true);
        let vel = Vec2::ZERO;
        let cmd = MoveCmd {
            v_des: Vec2::new(0.0, 40.0), // wants +Y velocity (port/left).
            facing: Facing::Aim(Vec2::X),
        };
        let intent = allocate_intent(cmd, vel, 0.0, stats, 0.3);
        assert!(
            intent.turn.abs() < 1e-3,
            "nose unchanged on the +X bearing (turn {})",
            intent.turn
        );
        assert!(
            intent.strafe > 0.0,
            "the lateral demand is met on the strafe channel (got {})",
            intent.strafe
        );
    }

    #[test]
    fn parked_ship_does_not_spin() {
        // v_des = 0, vel = 0, Free facing: nothing to do. The parked-no-spin
        // ladder must hold the heading and command no thrust/turn (no spin from
        // normalize-noise on a zero accel).
        let stats = fighter(true);
        let cmd = MoveCmd {
            v_des: Vec2::ZERO,
            facing: Facing::Free,
        };
        let intent = allocate_intent(cmd, Vec2::ZERO, 0.9, stats, 0.3);
        assert!(intent.turn.abs() < 1e-6, "no spin (turn {})", intent.turn);
        assert!(
            intent.forward.abs() < 1e-6,
            "no burn (forward {})",
            intent.forward
        );
        assert!(
            intent.strafe.abs() < 1e-6,
            "no strafe (strafe {})",
            intent.strafe
        );
    }

    #[test]
    fn non_strafe_hull_rotates_body_for_lateral() {
        // can_strafe = false: a lateral v_des with Free facing cannot use the
        // strafe channel, so strafe stays 0 and the body ROTATES (turn ≠ 0) to
        // point the thrust at the lateral demand.
        let stats = fighter(false);
        let vel = Vec2::ZERO;
        let cmd = MoveCmd {
            v_des: Vec2::new(0.0, 40.0), // +Y lateral demand.
            facing: Facing::Free,
        };
        let intent = allocate_intent(cmd, vel, 0.0, stats, 0.3);
        assert_eq!(intent.strafe, 0.0, "no strafe authority → strafe stays 0");
        assert!(
            intent.turn.abs() > 0.0,
            "rotates the body toward the lateral demand (turn {})",
            intent.turn
        );
        // The turn is toward +Y (positive/CCW from a +X heading).
        assert!(
            intent.turn > 0.0,
            "turns CCW toward +Y (got {})",
            intent.turn
        );
    }

    #[test]
    fn turn_power_share_compensation_under_hard_turn() {
        // A large heading error (hard turn ⇒ p < 1) plus a forward accel demand:
        // the turn-power compensation must PRE-DIVIDE forward UP so the ship still
        // achieves the demanded accel despite the turn stealing thrust — larger
        // than the naive uncompensated channel value — while staying clamped ≤ 1.
        let stats = fighter(false);
        // A MODERATE forward accel demand (so the naive channel is well below 1 and
        // there is headroom to compensate) plus an off-axis Aim (a hard turn, p < 1)
        // that still leaves a forward component of a_req on the current nose.
        let vel = Vec2::new(5.0, 0.0);
        let cmd = MoveCmd {
            v_des: Vec2::new(7.0, 0.0), // a_req_x = (7−5)/0.3 ≈ 6.7 → naive ≈ 0.22.
            facing: Facing::Aim(Vec2::from_angle(0.3)), // ~17° off → a real turn.
        };
        let heading = 0.0_f32;
        let intent = allocate_intent(cmd, vel, heading, stats, 0.3);

        // Recompute the NAIVE (uncompensated) forward channel for comparison.
        let a_req = (cmd.v_des - vel) / 0.3_f32.max(EPS);
        let nose = Vec2::from_angle(heading);
        let f_comp = nose.dot(a_req);
        let naive = (f_comp / stats.a_fwd).clamp(-1.0, 1.0);
        let p = turn_power_factor(intent.turn, stats.turn_power_share).max(P_FLOOR);
        assert!(p < 1.0, "the hard turn steals thrust (p {p} < 1)");
        assert!(
            intent.forward > naive,
            "compensated forward ({}) exceeds the naive value ({naive})",
            intent.forward
        );
        assert!(
            intent.forward <= 1.0 + 1e-6,
            "stays clamped ≤ 1 (got {})",
            intent.forward
        );
    }

    #[test]
    fn from_stats_maps_real_fields_and_fallback_is_sane() {
        // `from_stats` reads the real ShipStats fields; the fallback is the seed
        // baseline (a_fwd ≈ 2·a_rev so flip-and-burn pays).
        let fb = ControlStats::fallback();
        assert!((fb.a_fwd - 30.0).abs() < 1e-6 && (fb.a_rev - 15.0).abs() < 1e-6);
        assert!(fb.a_fwd > 1.9 * fb.a_rev, "forward ≈ 2× reverse");
        assert!(fb.can_strafe && (fb.max_turn_rate - 3.0).abs() < 1e-6);
    }

    #[test]
    fn deflect_v_des_empty_field_is_unchanged() {
        // The empty-field parity keystone: no obstacles → v_des returned EXACTLY.
        let ai = AiTuning::default();
        let field = ObstacleField::default();
        let v = Vec2::new(40.0, 10.0);
        let out = deflect_v_des(v, &field, Vec2::ZERO, Vec2::new(40.0, 10.0), 2.0, &ai);
        assert_eq!(out, v, "no in-range obstacle → unchanged");
        // A zero command is also unchanged (no direction to deflect).
        let z = deflect_v_des(Vec2::ZERO, &field, Vec2::ZERO, Vec2::ZERO, 2.0, &ai);
        assert_eq!(z, Vec2::ZERO);
    }

    #[test]
    fn deflect_v_des_bends_around_a_blocking_obstacle() {
        // A large body dead-ahead on the goal path deflects the desired velocity
        // off the head-on heading (still roughly forward, but not straight into
        // the obstacle), preserving the original speed magnitude.
        let ai = AiTuning::default();
        // Obstacle ahead at +X, radius 30 (≥ obstacle_min_radius 20 so it enters).
        let field = ObstacleField::build(
            std::iter::once((Vec2::new(60.0, 0.0), 30.0)),
            ai.obstacle_min_radius,
        );
        let v_des = Vec2::new(50.0, 0.0); // straight at the obstacle.
        let out = deflect_v_des(v_des, &field, Vec2::ZERO, Vec2::new(50.0, 0.0), 2.0, &ai);
        assert!(
            out.length() > 0.0 && (out.length() - v_des.length()).abs() < 1e-3,
            "speed magnitude preserved (got {})",
            out.length()
        );
        assert!(
            out.x < v_des.x,
            "deflected off the head-on heading (x {} < {})",
            out.x,
            v_des.x
        );
    }
}
