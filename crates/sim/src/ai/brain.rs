//! AI brain (T010–T014 OBJ2, T025–T027 OBJ4): deterministic utility-FSM
//! behavior selection (TR-004), the event-driven think scheduler (TR-005,
//! AD-003), fit-archetype classification (TR-006), the per-tick behavior
//! EXECUTION half driving steering through `ShipIntent` only (T013, TR-001),
//! combat behaviors + energy/heat fire gates + fire-group selection
//! (T025/T026, TR-011), the ram cost/benefit decision (T027, TR-012), and the
//! feature-gated score/transition capture seam (T014, TR-020, AD-006).
//!
//! **Enum-in-component** (HINT-003, research §ECS AI Scheduling): the behavior
//! state is a FIELD of the single [`AiBrain`] component — transitions mutate
//! the field, never add/remove per-state marker components (which would force
//! an archetype table move per transition and explode archetype count).
//!
//! **Strict-f32 scoring** (TR-004): every function on the scoring path
//! ([`curve_linear`] / [`curve_quadratic`] / [`curve_inv`] / [`curve_smooth`],
//! [`score_behavior`], [`select_behavior`]) uses ONLY `+ - * /`,
//! `min`/`max`/`clamp`, and comparisons — no `sin`/`cos`/`exp`/`powf`/`sqrt`/
//! `atan2`, no RNG, no HashMap iteration — so identical inputs yield
//! bit-identical scores and selections on every run.
//!
//! **Two-level tiebreak** (HINT-002, data-model §Behavior): within one ship's
//! selection, an EXACT score tie inside a priority bucket breaks by
//! behavior-enum ordinal (declaration order — level one, intra-ship); any
//! cross-entity ordering (the think loop, later target choice/fusion) breaks
//! by [`AiStableId`] (level two) — the scheduler iterates brains in stable-id
//! order (V-3).
//!
//! **Scheduler** (TR-005, AD-003): brains re-think on queued [`AiEvent`]s the
//! tick they are observed, with a phase-bucket fallback cadence
//! (`(now + phase_bucket) % cadence_for_tier == 0`) so calm ships incur ≈0
//! decision cost — an off-cadence brain with no event is one map lookup + one
//! modulo, then `continue`. Events COALESCE: at most ONE think per brain per
//! tick regardless of how many events queued (the [`RethinkQueue`] keeps one
//! entry per entity).

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::ident::{phase_bucket, AiStableId};
use crate::ai::lod::{AoiTier, Tier};
use crate::ai::perception::{nearest_contact, ContactList};
use crate::ai::role::{role_apply, RoleGoal, ScenarioRole};
use crate::ai::steering::{
    arrive, formation_keep, pursue_intercept, range_band_radial, steer_to_intent, waypoint_follow,
    ContextMap,
};
use crate::ai::tuning::AiTuning;
use crate::clock::CurrentTick;
use crate::collision::{RAM_CARVE_K, SHIP_MASS};
use crate::components::{
    AuthoredCells, Energy, Heading, Health, Heat, Position, Trigger, Velocity, WeaponGroups,
};
use crate::fitting::{FitLayout, ShipStats, ShipWeapons};
use crate::intent::ShipIntent;
use crate::tuning::SimTuning;

// ---------------------------------------------------------------------------
// T010 — Behavior + AiBrain
// ---------------------------------------------------------------------------

/// Behavior state of an [`AiBrain`] (data-model §Behavior state machine).
///
/// **Declaration order is load-bearing**: the derived `Ord` is the intra-ship
/// tiebreak ordinal — on an EXACT utility-score tie within one priority
/// bucket, [`select_behavior`] picks the LOWER ordinal (HINT-002 level one).
/// Cross-entity ties are the scheduler's concern and break by [`AiStableId`]
/// (level two). Reorder variants only with a determinism review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Behavior {
    /// Idle default: no goal — zero intent / station-keep (data-model `any →
    /// Hold` degrade row; also the derelict/no-power pin, T013).
    #[default]
    Hold,
    /// Follow a scripted patrol route (route data arrives with `ScenarioRole`,
    /// T032; until then [`AiBrain::home`] is the anchor placeholder).
    Patrol,
    /// Fly to [`AiBrain::waypoint`].
    Waypoint,
    /// Tail [`AiBrain::leader`] without a formation slot.
    Follow,
    /// Hold [`AiBrain::formation_slot`] relative to [`AiBrain::leader`].
    FormationKeep,
    /// Attack [`AiBrain::target`] (combat maneuvers arrive with T025).
    Engage,
    /// Break away from incoming threat (T025).
    Evade,
    /// Withdraw toward safety (T025).
    Retreat,
    /// Area coverage / recon (T035, TR-021): flies the `ScoutArea` role's
    /// boustrophedon route (movement identical to `Waypoint`); `Engage`/`Ram`
    /// candidacy is VETOED and a SUPERIOR perceived threat scores `Evade`.
    Scout,
    /// Search-and-destroy sweep of a region (T035, TR-021): flies the
    /// `SweepRegion` role's route; a perceived target's `Engage` outranks it
    /// (the [`RECON_BASELINE`] rule) — sweep, then prosecute.
    Sweep,
    /// Deliberate ramming attack (T027).
    Ram,
}

impl Behavior {
    /// Priority bucket of this behavior — buckets are evaluated HIGHEST-first
    /// (research §Utility-FSM: survival > tasks > idle/movement). A positive
    /// score in a higher bucket beats ANY score in a lower one; scores only
    /// compete within a bucket.
    ///
    /// - `2` survival: [`Evade`](Behavior::Evade) / [`Retreat`](Behavior::Retreat)
    ///   / [`Ram`](Behavior::Ram) (a ram is a terminal survival-bucket gambit —
    ///   it must outrank the task that spawned it, data-model `Engage → Ram`).
    /// - `1` tasks: [`Engage`](Behavior::Engage) / [`Scout`](Behavior::Scout)
    ///   / [`Sweep`](Behavior::Sweep).
    /// - `0` idle/movement: [`Hold`](Behavior::Hold) / [`Patrol`](Behavior::Patrol)
    ///   / [`Waypoint`](Behavior::Waypoint) / [`Follow`](Behavior::Follow)
    ///   / [`FormationKeep`](Behavior::FormationKeep).
    pub fn priority_bucket(self) -> u8 {
        match self {
            Behavior::Evade | Behavior::Retreat | Behavior::Ram => 2,
            Behavior::Engage | Behavior::Scout | Behavior::Sweep => 1,
            Behavior::Hold
            | Behavior::Patrol
            | Behavior::Waypoint
            | Behavior::Follow
            | Behavior::FormationKeep => 0,
        }
    }
}

/// Tactic archetype classified from a ship's derived [`ShipStats`] (TR-006).
///
/// Cached on [`AiBrain::archetype`]; recomputed ONLY on `Changed<ShipStats>`
/// (V-5) by [`archetype_refresh_system`] — per-think reads branch on the enum,
/// never re-derive. Default [`Generic`](FitArchetype::Generic): no distinctive
/// axis, plain all-rounder tactics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FitArchetype {
    /// Armed + tanky: closes in and slugs.
    Brawler,
    /// Armed + fast (and not tanky): keeps range, hit-and-run.
    Kiter,
    /// Armed, neither fast nor tanky: circles at its weapon envelope.
    Orbiter,
    /// Unarmed/under-gunned but tanky: its hull IS the weapon.
    Rammer,
    /// Unarmed + fast: screen/utility runner.
    Support,
    /// No distinctive axis — the default all-rounder.
    #[default]
    Generic,
}

/// The utility-FSM brain component — one per AI-controlled ship (data-model
/// §`AiBrain`; enum-in-component per HINT-003, never per-state markers).
///
/// `Clone + Debug`, no `Serialize` (V-9): all brain state is ephemeral and
/// re-derivable from sim state. `formation_slot` is the v1 standalone form — a
/// body-frame offset from `leader` (the squad-indexed slot of data-model
/// §`Squad` arrives with T016, which maps indices through `FormationDef`).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct AiBrain {
    /// Active behavior state. Transitions mutate this field only.
    pub behavior: Behavior,
    /// Current engage/follow target. Pruned by `ai_despawn_sweep_system` the
    /// tick the referent despawns (V-1) — never read dangling.
    pub target: Option<Entity>,
    /// Current nav goal (route step / squad slot goal).
    pub waypoint: Option<Vec2>,
    /// Patrol/return anchor placeholder — full routes arrive with
    /// `ScenarioRole` (T032).
    pub home: Option<Vec2>,
    /// Body-frame formation offset from `leader` (v1; see type docs).
    pub formation_slot: Option<Vec2>,
    /// Formation/follow leader. Pruned on despawn like `target` (V-1).
    pub leader: Option<Entity>,
    /// Commitment window (HINT-004): no re-selection before this tick unless
    /// an event that [`AiEvent::overrides_commit`] fires.
    pub commit_until_tick: u64,
    /// Last tick this brain completed a think.
    pub last_think_tick: u64,
    /// Cached tactic archetype (TR-006, V-5) — see [`FitArchetype`].
    pub archetype: FitArchetype,
    /// Mirror of the ship's [`AoiTier`] at its LAST think — drives the
    /// fallback cadence between thinks (a stale mirror self-corrects at the
    /// next think; promotion events wake brains faster in later tasks).
    pub think_tier: Tier,
    /// Fallback-cadence slot: `splitmix64(stable_id) % bucket_count` (V-4) —
    /// derived from [`AiStableId`], never `Entity` bits.
    pub phase_bucket: u16,
    /// Forward-intent throttle cap in `[0, 1]` (T017, TR-010): applied
    /// MULTIPLICATIVELY to the steered `ShipIntent::forward` by
    /// [`ai_execute_system`] — the squad pace seam. `squad_think_system` sets
    /// the formation leader's cap to `anchor_speed / leader_top_speed` so the
    /// formation never outruns its slowest essential member; everyone else
    /// (and every solo brain) keeps the default `1.0` (a `* 1.0` no-op —
    /// bit-identical to uncapped).
    pub throttle_cap: f32,
    /// Monotonic count of COMPLETED thinks over this brain's lifetime —
    /// incremented exactly once per completed think in [`ai_think_system`]
    /// (skipped/coalesced ticks never bump it). Deterministic bookkeeping that
    /// nothing on the decision path reads: T015's think-counter assertions
    /// observe it, and the T021 per-tier think counters aggregate it.
    pub thinks_total: u64,
}

impl Default for AiBrain {
    /// A goal-less brain: `Hold`, nothing referenced, `Dormant`-cadence mirror
    /// (matches `AoiTier::default`), bucket 0. Spawn paths use
    /// [`AiBrain::new`] for a real phase bucket.
    fn default() -> Self {
        Self {
            behavior: Behavior::Hold,
            target: None,
            waypoint: None,
            home: None,
            formation_slot: None,
            leader: None,
            commit_until_tick: 0,
            last_think_tick: 0,
            archetype: FitArchetype::Generic,
            think_tier: Tier::Dormant,
            phase_bucket: 0,
            throttle_cap: 1.0,
            thinks_total: 0,
        }
    }
}

impl AiBrain {
    /// A default brain with its `phase_bucket` derived from the ship's
    /// sim-stable id (V-4) over `bucket_count` scheduler buckets
    /// (`AiTuning::fallback_bucket_count`).
    pub fn new(id: AiStableId, bucket_count: u32) -> Self {
        Self {
            phase_bucket: phase_bucket(id, bucket_count),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// T010 — utility scoring core (strict f32: + - * / min max clamp ONLY)
// ---------------------------------------------------------------------------

// STRICT-F32 SCORING BEGIN (TR-004)
// Everything between this marker and the matching END marker is the
// deterministic scoring/curve/select region: `+ - * /`, `min`/`max`/`clamp`,
// and comparisons ONLY — no transcendentals (`sin`/`cos`/`exp`/`powf`/`sqrt`/
// `atan2`), no RNG, no HashMap iteration. The T015 CI grep
// (`strict_f32_scoring_grep` in `tests/ai.rs`) fails the build if one creeps
// in; keep the markers around this region when refactoring.

/// Linear response curve: the normalized input, clamped to `[0, 1]`.
pub fn curve_linear(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// Quadratic response curve `x²`: de-emphasizes low inputs.
pub fn curve_quadratic(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x
}

/// Inverted linear curve `1 − x`: high input → low consideration.
pub fn curve_inv(x: f32) -> f32 {
    1.0 - x.clamp(0.0, 1.0)
}

/// Smoothstep-LIKE polynomial `x²(3 − 2x)` — an S-curve built from `* - +`
/// only (the real smoothstep family is polynomial too; no transcendentals).
pub fn curve_smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Score one candidate behavior: the PRODUCT of its consideration curves
/// (each clamped to `[0, 1]` — multiplication preserves the zero-score veto:
/// any vetoing consideration zeroes the candidate), rescaled by Mark's
/// compensation factor so adding considerations doesn't starve the score
/// (research §Utility-FSM pitfall).
///
/// **Compensation formula (documented choice)**: the canonical geometric
/// rescale (`score^(1/n)`) needs `powf` — banned by TR-004 — so we use the
/// additive strict-f32 form:
///
/// ```text
/// comp(s, n) = s + (1 − s) · s · k · (n − 1) / n      (k = compensation_k)
/// ```
///
/// Properties: `n = 1` passes through unchanged (factor 0); `0` stays `0`
/// (veto intact) and `1` stays `1`; monotone in `s` for `k ∈ [0, 1]` and
/// bounded in `[0, 1]`; the boost grows with consideration count `n` toward
/// its `k/4` maximum near `s = 0.5` — exactly where multiplied mid-range
/// curves get starved. Empty `considerations` scores `0.0` (nothing to want).
pub fn score_behavior(considerations: &[f32], compensation_k: f32) -> f32 {
    let n = considerations.len();
    if n == 0 {
        return 0.0;
    }
    let mut score = 1.0_f32;
    for &c in considerations {
        score *= c.clamp(0.0, 1.0);
    }
    let frac = (n as f32 - 1.0) / n as f32;
    score + (1.0 - score) * score * compensation_k * frac
}

/// Select the winning behavior from scored candidates (T010, TR-004).
///
/// - **Momentum** (HINT-004, research ~25%): the incumbent's score is
///   multiplied by `1 + momentum_bonus` before comparison — hysteresis
///   against selection oscillation.
/// - **Priority buckets highest-first**: only the highest
///   [`Behavior::priority_bucket`] containing any score `> 0` competes — a
///   positive survival score beats any task score, any task beats idle.
/// - **Zero-score veto**: candidates scoring `≤ 0` are never selected. If NO
///   candidate scores `> 0`, the selection degrades to [`Behavior::Hold`]
///   (data-model `any → Hold`: "no valid behavior scores > 0").
/// - **Tiebreak**: an EXACT (`f32 ==`) score tie within the winning bucket
///   breaks by behavior ordinal — the lower declaration ordinal wins (the
///   intra-ship level of the two-level rule; cross-entity ordering keys off
///   [`AiStableId`] in the scheduler).
///
/// Strict f32 throughout: one multiply for momentum, comparisons otherwise.
pub fn select_behavior(
    candidates: &[(Behavior, f32)],
    incumbent: Behavior,
    momentum_bonus: f32,
) -> Behavior {
    let mut best: Option<(u8, f32, Behavior)> = None;
    for &(behavior, raw) in candidates {
        let score = if behavior == incumbent {
            raw * (1.0 + momentum_bonus)
        } else {
            raw
        };
        if score <= 0.0 {
            continue; // Zero-score veto: never selectable.
        }
        let bucket = behavior.priority_bucket();
        let wins = match best {
            None => true,
            Some((b_bucket, b_score, b_beh)) => {
                bucket > b_bucket
                    || (bucket == b_bucket && score > b_score)
                    || (bucket == b_bucket && score == b_score && behavior < b_beh)
            }
        };
        if wins {
            best = Some((bucket, score, behavior));
        }
    }
    best.map_or(Behavior::Hold, |(_, _, behavior)| behavior)
}

/// T027 (TR-012) — ram cost/benefit utility: score the `Ram` candidate for an
/// attacker considering a deliberate collision, via the collision system's
/// `RAM_CARVE_K · closing²` kinetic damage model. Strict f32 (inside the
/// TR-004 markers): the caller resolves all GEOMETRY (closing speed along the
/// line of sight, hull fraction, masses — which may use `normalize`/`length`)
/// and passes scalars; this function is `+ - * /` + comparisons only.
///
/// Three multiplied considerations, each a built-in zero-score VETO (a healthy
/// or stronger or uncatchable target can never be rammed):
///
/// 1. **Near-dead/disabled target** (`ram_target_hull_frac`, default 0.25):
///    `target_hull_frac > threshold` → `0` (veto). At/below it the score ramps
///    `1.0` (hulk) → `0.5` (exactly at the threshold) — a graded "finisher"
///    desire that stays POSITIVE on the data-model's "hull ≤ 25% = near-dead"
///    boundary instead of vanishing there.
/// 2. **Projected-damage advantage** (`ram_self_margin`, default 2.0):
///    projected damage through the collision model, scaled by the mass ratio
///    as the v1 relative-toughness APPROXIMATION (documented): the heavier
///    party delivers more of the impact energy into the lighter one —
///    `dealt = RAM_CARVE_K·closing²·(mₐ/mₜ)`, `taken = RAM_CARVE_K·closing²·(mₜ/mₐ)`.
///    `dealt/taken < margin` → `0` (veto); note the `RAM_CARVE_K·closing²`
///    factor CANCELS in the ratio (it scales both sides), so the margin is
///    effectively a mass-advantage test `(mₐ/mₜ)² ≥ margin` — closing speed
///    gates separately via (3). Above the margin the score ramps to `1.0` at
///    `2× margin`.
/// 3. **Closing speed** (`ram_min_closing_frac`, default 0.5): closing slower
///    than `frac · attacker_top_speed` → `0` (can't ram what you can't catch);
///    at/above it the score is `closing / top_speed` (a faster slam is
///    quadratically deadlier, so prefer it), clamped to `1`.
///
/// The considerations combine through [`score_behavior`] (same compensation as
/// every other candidate). Degenerate inputs (non-positive top speed/masses —
/// e.g. an unfitted attacker with no [`ShipStats`]) score `0`.
pub fn ram_utility(
    target_hull_frac: f32,
    closing_speed: f32,
    attacker_top_speed: f32,
    attacker_mass: f32,
    target_mass: f32,
    tuning: &AiTuning,
) -> f32 {
    if attacker_top_speed <= 0.0 || attacker_mass <= 0.0 || target_mass <= 0.0 {
        return 0.0; // Unknown/degenerate kinematics: never gamble on a ram.
    }
    // (3) Closing-speed gate.
    let min_closing = tuning.ram_min_closing_frac * attacker_top_speed;
    if closing_speed < min_closing {
        return 0.0;
    }
    let c_close = (closing_speed / attacker_top_speed).clamp(0.0, 1.0);
    // (1) Near-dead/disabled gate.
    let threshold = tuning.ram_target_hull_frac;
    if threshold <= 0.0 || target_hull_frac > threshold {
        return 0.0;
    }
    let c_hull = 1.0 - 0.5 * (target_hull_frac / threshold).clamp(0.0, 1.0);
    // (2) Projected-damage advantage through the RAM_CARVE_K·closing² model.
    let base = RAM_CARVE_K * closing_speed * closing_speed;
    let dealt = base * (attacker_mass / target_mass);
    let taken = base * (target_mass / attacker_mass);
    if taken <= 0.0 || dealt / taken < tuning.ram_self_margin {
        return 0.0;
    }
    let c_margin = (dealt / taken / (tuning.ram_self_margin * 2.0)).clamp(0.0, 1.0);
    score_behavior(&[c_hull, c_margin, c_close], tuning.compensation_k)
}

// STRICT-F32 SCORING END (TR-004)

// ---------------------------------------------------------------------------
// T012 — fit-archetype classification (TR-006)
// ---------------------------------------------------------------------------

/// Classify a ship's tactic archetype from its derived [`ShipStats`] — a pure
/// O(1) strict-f32 threshold function of the `AiTuning` `arch_*` cuts (TR-006;
/// the cuts are live-tunable, V-5 mass re-classification arrives with T038).
///
/// **Axes** (all `>=` threshold comparisons):
/// - *fast*: emergent top speed `thrust_force / linear_drag ≥ arch_speed_hi`
/// - *armed*: primary-weapon sustained DPS `damage · fire_rate ≥ arch_dps_hi`
///   (no weapon fitted → DPS 0)
/// - *tanky*: fitted `armor_value ≥ arch_armor_hi`
///
/// **Cuts (documented rules)**:
///
/// | armed | tanky | fast | → archetype |
/// |-------|-------|------|-------------|
/// | yes   | yes   | —    | `Brawler` (guns + armor: wade in)         |
/// | yes   | no    | yes  | `Kiter` (guns + speed, glass: keep range) |
/// | yes   | no    | no   | `Orbiter` (guns only: circle the envelope)|
/// | no    | yes   | —    | `Rammer` (mass without guns: hull weapon) |
/// | no    | no    | yes  | `Support` (fast utility/screen runner)    |
/// | no    | no    | no   | `Generic` (no distinctive axis)           |
pub fn classify_archetype(stats: &ShipStats, tuning: &AiTuning) -> FitArchetype {
    let fast = stats.top_speed() >= tuning.arch_speed_hi;
    let dps = match stats.weapon {
        Some(w) => w.damage * w.fire_rate,
        None => 0.0,
    };
    let armed = dps >= tuning.arch_dps_hi;
    let tanky = stats.armor_value >= tuning.arch_armor_hi;
    if armed {
        if tanky {
            FitArchetype::Brawler
        } else if fast {
            FitArchetype::Kiter
        } else {
            FitArchetype::Orbiter
        }
    } else if tanky {
        FitArchetype::Rammer
    } else if fast {
        FitArchetype::Support
    } else {
        FitArchetype::Generic
    }
}

/// Recompute + cache [`AiBrain::archetype`] for ships whose [`ShipStats`]
/// changed this tick (T012, TR-006/V-5: `Changed<ShipStats>` ONLY — a calm
/// fleet does zero classification work; per-think reads branch on the cached
/// enum). Mass re-classification (spawn wave / fleet refit) is the accepted
/// unbatched O(changed) case; the dev-panel threshold-edit path (forcing all
/// brains changed) arrives with T038.
///
/// Per-entity independent (reads its own `ShipStats`, writes its own brain —
/// no shared state), so query iteration order is immaterial here (V-3 applies
/// to loops mutating shared state). Registered in the gated AI set after the
/// AOI classify and before [`ai_think_system`], so a think always sees this
/// tick's archetype.
pub fn archetype_refresh_system(
    tuning: Res<AiTuning>,
    mut brains: Query<(&ShipStats, &mut AiBrain), Changed<ShipStats>>,
) {
    for (stats, mut brain) in &mut brains {
        let archetype = classify_archetype(stats, &tuning);
        if brain.archetype != archetype {
            brain.archetype = archetype; // Write only on change: no churn.
        }
    }
}

// ---------------------------------------------------------------------------
// T025/T026/T027 — combat helpers (TR-011/TR-012)
// ---------------------------------------------------------------------------

/// Fallback engagement standoff BASE (world units) for a ship with no usable
/// weapon profile (unarmed, or unfitted with no [`ShipStats`]): roughly half
/// the seed autocannon's reach — close enough to matter, far enough not to
/// blunder into a ram. Archetype standoff fractions scale it like a real range.
const FALLBACK_ENGAGE_RANGE: f32 = 100.0;
/// Brawler standoff as a fraction of weapon range: close to SHORT range and
/// hold — wade in and slug (TR-006 archetype tactics).
const BRAWLER_STANDOFF_FRAC: f32 = 0.3;
/// Kiter standoff fraction: a LONG standoff near the weapon envelope's edge —
/// thrust away inside the band, close only when the target slips out of reach.
const KITER_STANDOFF_FRAC: f32 = 0.85;
/// Orbiter/Generic (and every other archetype's) standoff fraction: a medium
/// ring inside the envelope.
const DEFAULT_STANDOFF_FRAC: f32 = 0.6;
/// Half-width of the engage range band as a fraction of the standoff distance
/// (see [`range_band_radial`]): the tolerance ring a ship "holds" within.
const RANGE_BAND_FRAC: f32 = 0.25;
/// T026 alignment gate: fire only when `cos(heading − aim) > this` — the
/// fixed-forward gun fires along the HEADING, so shooting while pointed away
/// from the lead solution just wastes energy/heat (TR-011 is about choosing
/// not to fire, not merely being blocked).
const FIRE_ALIGN_COS: f32 = 0.9;
/// [`hull_fraction`] baseline for FLAT-health targets (a bare [`Health`] with
/// no max recorded anywhere): the canonical demo/scenario ship spawn value
/// (`Health(100.0)` in the server spawn paths). A documented approximation —
/// flat-health entities are legacy/demo targets, and the ram decision only
/// needs "near-dead vs healthy", which this resolves correctly for them.
const FLAT_HULL_BASELINE: f32 = 100.0;

/// The ship's weapon REACH (world units) — `muzzle_speed · lifetime` of its
/// primary [`WeaponProfile`](crate::fitting::WeaponProfile) (`lifetime` is
/// itself derived `range_units / muzzle_speed`, so this recovers the authored
/// range). `None` when unarmed/unfitted or the profile degenerates to `≤ 0`.
pub fn weapon_range(stats: Option<&ShipStats>) -> Option<f32> {
    let w = stats?.weapon?;
    let range = w.muzzle_speed * w.lifetime;
    (range > 0.0).then_some(range)
}

/// Archetype-flavored standoff ring radius for the engage range-band
/// controller (T025): a fraction of `range` per the documented cuts —
/// Brawler [`BRAWLER_STANDOFF_FRAC`], Kiter [`KITER_STANDOFF_FRAC`], everyone
/// else [`DEFAULT_STANDOFF_FRAC`]. `range` is [`weapon_range`] or the
/// [`FALLBACK_ENGAGE_RANGE`] when unarmed.
pub fn standoff_distance(archetype: FitArchetype, range: f32) -> f32 {
    let frac = match archetype {
        FitArchetype::Brawler => BRAWLER_STANDOFF_FRAC,
        FitArchetype::Kiter => KITER_STANDOFF_FRAC,
        _ => DEFAULT_STANDOFF_FRAC,
    };
    frac * range
}

/// Remaining hull fraction of a target in `[0, 1]` (`1.0` = pristine), the
/// T027 "near-dead/disabled" input. **Documented fallback chain**:
///
/// 1. **Fitted ship with a spawn baseline** ([`FitLayout`] + [`AuthoredCells`]
///    `> 0`): `live_cells / authored_cells` — carving removes cells, the
///    baseline never shrinks (the HUD hull-bar formula).
/// 2. **Flat-health target** ([`Health`] only, or a fitted ship that never
///    recorded its baseline): `health / FLAT_HULL_BASELINE`, clamped.
/// 3. **No information** → `1.0` (assume healthy — never ram blind).
pub fn hull_fraction(
    health: Option<&Health>,
    layout: Option<&FitLayout>,
    authored: Option<&AuthoredCells>,
) -> f32 {
    if let (Some(layout), Some(authored)) = (layout, authored) {
        if authored.0 > 0 {
            return (layout.cells.len() as f32 / authored.0 as f32).clamp(0.0, 1.0);
        }
    }
    if let Some(h) = health {
        return (h.0 / FLAT_HULL_BASELINE).clamp(0.0, 1.0);
    }
    1.0
}

/// T026 fire-group selection — the v1 rule: choose the fire group with the
/// MOST weapons mapped to the [`Trigger::Primary`] trigger (the AI holds
/// primary fire only), breaking ties deterministically to the LOWEST group
/// index. No [`ShipWeapons`] list (legacy single-weapon ships, unarmed) or no
/// [`WeaponGroups`] component → the default group `0` (= group 1, the
/// fire-anything-on-Space convention the weapon system already honors).
pub fn primary_fire_group(weapons: Option<&ShipWeapons>, groups: Option<&WeaponGroups>) -> u8 {
    let Some(weapons) = weapons else { return 0 };
    let mut counts = [0u32; 6];
    for (slot, _) in &weapons.weapons {
        let map = groups.map(|g| g.for_slot(*slot)).unwrap_or_default();
        if map.trigger == Trigger::Primary {
            counts[(map.group as usize).min(5)] += 1;
        }
    }
    let mut best = 0usize;
    for (g, &count) in counts.iter().enumerate().skip(1) {
        if count > counts[best] {
            best = g; // Strict `>`: exact ties keep the lower index.
        }
    }
    best as u8
}

/// T026 — the AI's own fire DECISION for the Engage/Ram arms: `Some(group)`
/// when the ship should hold primary fire this tick, `None` otherwise. TR-011
/// requires the *decision* never to fire out-of-energy/overheated — the gates
/// here MIRROR `weapon_fire_system`'s own (`energy.current >= shot_cost`,
/// `heat.current < heat.max`, absent pool = ungated, exactly as there), so the
/// brain chooses not to pull the trigger rather than leaning on the weapon
/// system to block it.
///
/// Gate order: armed (`ShipStats::can_fire` + a profile) → in weapon range →
/// aligned to the gunnery lead within [`FIRE_ALIGN_COS`] (the L1 intercept
/// solve shared with `turret::aim_angle` via [`pursue_intercept`], IP-003;
/// shooter-velocity inheritance is ignored exactly as the turret solver does —
/// documented v1 approximation) → energy → heat. The energy gate uses the
/// CHEAPEST Primary-trigger shot in the selected group (if even that cannot
/// fire, nothing in the group can); legacy single-weapon ships gate on their
/// one profile.
#[allow(clippy::too_many_arguments)] // Mirrors the execute arm's locals 1:1.
fn fire_decision(
    pos: Vec2,
    heading: f32,
    stats: Option<&ShipStats>,
    weapons: Option<&ShipWeapons>,
    groups: Option<&WeaponGroups>,
    energy: Option<&Energy>,
    heat: Option<&Heat>,
    target_pos: Vec2,
    target_vel: Vec2,
    sim: &SimTuning,
) -> Option<u8> {
    let stats = stats?;
    let profile = stats.weapon?;
    if !stats.can_fire {
        return None; // Unarmed/unfitted: nothing to fire (TR-011).
    }
    let range = profile.muzzle_speed * profile.lifetime;
    if range <= 0.0 || (target_pos - pos).length() > range {
        return None; // Out of envelope: shots would expire short.
    }
    let aim_dir = pursue_intercept(pos, profile.muzzle_speed, target_pos, target_vel);
    if Vec2::from_angle(heading).dot(aim_dir) <= FIRE_ALIGN_COS {
        return None; // Not pointed at the lead solution: don't waste the shot.
    }
    let group = primary_fire_group(weapons, groups);
    let min_cost = match weapons {
        Some(w) if !w.weapons.is_empty() => {
            let mut cost: Option<f32> = None;
            for (slot, p) in &w.weapons {
                let map = groups.map(|g| g.for_slot(*slot)).unwrap_or_default();
                if map.group == group && map.trigger == Trigger::Primary {
                    let c = p.damage * sim.weapon_energy_per_damage;
                    cost = Some(cost.map_or(c, |b| b.min(c)));
                }
            }
            cost? // No Primary weapon in the chosen group: nothing would fire.
        }
        _ => profile.damage * sim.weapon_energy_per_damage,
    };
    // THE TR-011 GATES — mirror `weapon_fire_system` exactly (absent = ungated).
    if !energy.is_none_or(|e| e.current >= min_cost) {
        return None; // Out of energy: CHOOSE not to fire.
    }
    if !heat.is_none_or(|h| h.current < h.max) {
        return None; // Overheated: CHOOSE not to fire.
    }
    Some(group)
}

/// T025 — Engage MOVEMENT: the archetype-flavored range-band controller over
/// a small context map.
///
/// - `radial > 0` (outside the ring): interest toward the [`pursue_intercept`]
///   point at the ship's top speed — lead pursuit, weight = how far out.
/// - `radial < 0` (inside the ring): interest directly AWAY from the target;
///   when the target is actively closing on us its approach direction is
///   written as DANGER, so the masked resolve never flees *through* the
///   threat. (Additional `avoid` threat sources wire in with perception,
///   T029.)
/// - On the ring (map empty → `None`): hold position (zero throttle) and FACE
///   the gunnery lead so the fixed-forward gun connects — `compose_intent`
///   turns toward a direction even at zero throttle.
#[allow(clippy::too_many_arguments)] // Mirrors the execute arm's locals 1:1.
fn engage_motion(
    archetype: FitArchetype,
    pos: Vec2,
    vel: Vec2,
    heading: f32,
    turn_authority: f32,
    stats: Option<&ShipStats>,
    target_pos: Vec2,
    target_vel: Vec2,
    ai: &AiTuning,
) -> ShipIntent {
    let range = weapon_range(stats).unwrap_or(FALLBACK_ENGAGE_RANGE);
    let standoff = standoff_distance(archetype, range);
    let to = target_pos - pos;
    let dist = to.length();
    let dir_to = to.normalize_or_zero();
    if dir_to == Vec2::ZERO {
        return ShipIntent::default(); // Coincident: nothing sensible to steer.
    }
    let radial = range_band_radial(dist, standoff, RANGE_BAND_FRAC);
    let n = ai.slot_count as usize;
    let mut map = ContextMap::default();
    if radial > 0.0 {
        let top = stats.map_or(0.0, ShipStats::top_speed);
        map.add_interest_dir(
            pursue_intercept(pos, top, target_pos, target_vel),
            radial,
            n,
        );
    } else if radial < 0.0 {
        map.add_interest_dir(-dir_to, -radial, n);
        if (target_vel - vel).dot(-dir_to) > 0.0 {
            map.add_danger_dir(dir_to, 1.0, n); // The target's closing vector.
        }
    }
    match map.resolve(n, ai.danger_mask_floor) {
        Some((dir, strength)) => steer_to_intent(dir, strength, heading, vel, turn_authority),
        None => {
            let aim = match stats.and_then(|s| s.weapon) {
                Some(w) => pursue_intercept(pos, w.muzzle_speed, target_pos, target_vel),
                None => dir_to,
            };
            steer_to_intent(aim, 0.0, heading, vel, turn_authority)
        }
    }
}

/// T025 — Evade MOVEMENT: break-off at full throttle, directly away from the
/// threat, with the threat direction written as danger so the masked resolve
/// deflects around it rather than ever turning back in. (A last-threat-dir
/// memory for target-less evades arrives with perception, T029.)
fn evade_motion(
    pos: Vec2,
    vel: Vec2,
    heading: f32,
    turn_authority: f32,
    threat_pos: Vec2,
    ai: &AiTuning,
) -> ShipIntent {
    let dir_to = (threat_pos - pos).normalize_or_zero();
    if dir_to == Vec2::ZERO {
        return ShipIntent::default();
    }
    let n = ai.slot_count as usize;
    let mut map = ContextMap::default();
    map.add_interest_dir(-dir_to, 1.0, n);
    map.add_danger_dir(dir_to, 1.0, n);
    match map.resolve(n, ai.danger_mask_floor) {
        Some((dir, _)) => steer_to_intent(dir, 1.0, heading, vel, turn_authority),
        None => ShipIntent::default(),
    }
}

// ---------------------------------------------------------------------------
// T011 — event-driven scheduler (TR-005, AD-003)
// ---------------------------------------------------------------------------

/// A re-think trigger (TR-005): something happened that invalidates a brain's
/// standing decision, so it should think THIS tick instead of waiting for its
/// fallback cadence. Producers push these into the [`RethinkQueue`]; later
/// tasks wire the real producers (damage events T025, perception T029, squad
/// orders T017).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiEvent {
    /// The ship took damage (survival pressure).
    DamageTaken,
    /// The current target despawned or was lost from perception.
    TargetLost,
    /// A new contact entered the perception picture.
    NewContact,
    /// The current waypoint was reached.
    Arrived,
    /// The squad/scenario order changed.
    OrderChanged,
}

impl AiEvent {
    /// Whether this event breaks the [`AiBrain::commit_until_tick`] commitment
    /// window (HINT-004). **Documented rule**: `DamageTaken` (survival-bucket
    /// pressure must never be deferred) and `TargetLost` (the committed
    /// decision's premise is gone) override; `NewContact` / `Arrived` /
    /// `OrderChanged` wait the window out — that deferral IS the
    /// anti-oscillation hysteresis, and windows are at most one fallback
    /// cadence period long.
    pub fn overrides_commit(self) -> bool {
        matches!(self, AiEvent::DamageTaken | AiEvent::TargetLost)
    }
}

/// The pending re-think set (T011, AD-003): at most ONE entry per entity, so
/// any number of events in a tick coalesce into one think (the event-storm
/// worst case is bounded at one think/ship/tick — data-model §Behavior).
///
/// A `BTreeMap` keyed by `Entity` (V-3: no HashMap). The map is only ever
/// LOOKED UP per-entity and cleared — never iterated for decisions — so its
/// `Entity`-bits key order is never observable. Inserted at world construction
/// (`ServerApp::new`) like the other AI resources: inert until something
/// pushes into it.
#[derive(Resource, Clone, Debug, Default)]
pub struct RethinkQueue {
    /// Entity → strongest pending event this tick (see [`RethinkQueue::push`]).
    entries: BTreeMap<Entity, AiEvent>,
}

impl RethinkQueue {
    /// Queue a re-think for `entity`, coalescing with any event already
    /// pending: a commit-overriding event ([`AiEvent::overrides_commit`])
    /// upgrades a non-overriding one; otherwise the FIRST event of equal
    /// urgency stands (deterministic, since producers run in schedule order).
    pub fn push(&mut self, entity: Entity, event: AiEvent) {
        use std::collections::btree_map::Entry;
        match self.entries.entry(entity) {
            Entry::Vacant(slot) => {
                slot.insert(event);
            }
            Entry::Occupied(mut slot) => {
                if event.overrides_commit() && !slot.get().overrides_commit() {
                    slot.insert(event);
                }
            }
        }
    }

    /// The pending event for `entity`, if any (does not consume it — the
    /// think system drains the whole queue at the end of its run).
    pub fn get(&self, entity: Entity) -> Option<AiEvent> {
        self.entries.get(&entity).copied()
    }

    /// Whether no re-thinks are pending.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entities with a pending re-think.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Drop every pending entry (end-of-think drain).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Fallback think cadence (ticks) for an AOI tier, from `AiTuning`
/// (`think_ticks_active` / `_mid` / `_dormant`). A degenerate `0` cadence is
/// clamped to `1` (think every tick) rather than dividing by zero.
pub fn cadence_for_tier(tier: Tier, tuning: &AiTuning) -> u64 {
    let ticks = match tier {
        Tier::Active => tuning.think_ticks_active,
        Tier::Mid => tuning.think_ticks_mid,
        Tier::Dormant => tuning.think_ticks_dormant,
    };
    u64::from(ticks.max(1))
}

/// v1 Hold baseline consideration: always scoreable, so a brain with no goal
/// degrades to `Hold`, yet any live movement goal (presence score
/// [`MOVE_BASELINE`]) outcompetes it. Real considerations replace these
/// presence stubs in T013/T025.
const HOLD_BASELINE: f32 = 0.1;
/// v1 presence consideration for movement candidates (goal exists → fully
/// desirable). See [`HOLD_BASELINE`].
const MOVE_BASELINE: f32 = 1.0;
/// T035 presence consideration for the role-assigned recon tasks
/// (`Scout`/`Sweep`): deliberately BELOW [`MOVE_BASELINE`] so a perceived
/// target's `Engage` (scored at `MOVE_BASELINE`) outranks an INCUMBENT recon
/// task even through the momentum bonus (`1.0 > 0.7 × 1.25 = 0.875`) — the
/// TR-021 "engage targets once perceived" rule decided by SCORE, since
/// `Engage`/`Scout`/`Sweep` share the task priority bucket. Still far above
/// [`HOLD_BASELINE`] and in a higher bucket than every movement candidate, so
/// an un-threatened recon ship always flies its coverage route.
const RECON_BASELINE: f32 = 0.7;

/// The event-driven think scheduler (T011, TR-005, AD-003).
///
/// For each brain, in [`AiStableId`] order (V-3 — v1 thinks are
/// per-entity-local, but the stable order is the doctrine and becomes
/// load-bearing when target selection lands in T013/T025), think IF:
///
/// 1. the entity has a pending [`RethinkQueue`] event (same-tick reaction), OR
/// 2. its fallback cadence fires: `(now + phase_bucket) % cadence == 0`, with
///    the cadence taken from the brain's `think_tier` MIRROR (updated at each
///    think; phase buckets spread brains so each tick services ≈ N/buckets).
///
/// Calm ships (no event + off-cadence) cost one map lookup + one modulo, then
/// `continue` — ≈0 decision work (TR-005).
///
/// **Commitment window (HINT-004, documented rule)**: while
/// `now < commit_until_tick`, a due think is SKIPPED entirely (no scoring)
/// unless the pending event [`AiEvent::overrides_commit`] (survival-grade:
/// `DamageTaken`/`TargetLost`). On every completed think the window re-arms to
/// `now + cadence_for_tier(tier-at-this-think)` — exactly one fallback period,
/// so the next on-cadence think lands precisely when the window expires (the
/// guard is strict `<`) and only mid-window event thinks are damped.
///
/// **Candidates**: `Hold` always (baseline 0.1), `Waypoint` if a waypoint is
/// set, `FormationKeep` if leader + slot, `Follow` if leader only — presence
/// considerations (richer movement sets are later refinements). With a LIVE
/// `brain.target` (squad `Engage` orders until perception lands, T029):
/// `Engage` (task bucket — a presence consideration; perception-driven
/// considerations arrive with T029) and `Ram` scored by [`ram_utility`]
/// (T027, TR-012 — survival bucket per [`Behavior::priority_bucket`], the
/// T010 placement: a POSITIVE ram score on a near-dead target therefore
/// outranks Engage by bucket dominance, the data-model `Engage → Ram` row;
/// its triple zero-veto keeps healthy/strong/uncatchable targets unrammable).
/// `Retreat` has no candidate scoring yet — its inputs are damage pressure —
/// but its EXECUTION arm is live (T025), so scenario/squad-pinned survival
/// behaviors steer correctly. T035 (TR-021) recon roles add: `Sweep`/`Scout`
/// presence candidates at [`RECON_BASELINE`] (so a perceived target's `Engage`
/// outranks an incumbent sweep), the Scout `Engage`/`Ram` veto, and the
/// scout's superior-threat `Evade` candidate (survival bucket — wins while the
/// threat is perceived, releases with the contact). Each candidate runs
/// through [`score_behavior`] and [`select_behavior`] with the incumbent
/// momentum bonus.
///
/// The queue is drained at the end of the run (coalescing: at most one think
/// per ship per tick; events pushed by systems later in the tick are consumed
/// by the NEXT tick's think). Registered in the `ScenarioActive`-gated AI set
/// after [`archetype_refresh_system`], before `ship_motion_system`; no golden
/// world spawns an `AiBrain`, so the goldens stay bit-identical.
pub fn ai_think_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    mut queue: ResMut<RethinkQueue>,
    mut brains: Query<(
        Entity,
        &AiStableId,
        &mut AiBrain,
        Option<&AoiTier>,
        // T027: own kinematics/stats for the ram geometry (read-only; absent
        // on goal-only test brains → the Ram candidate is simply skipped).
        Option<&Position>,
        Option<&Velocity>,
        Option<&ShipStats>,
        // T032 (TR-015): the optional scenario-script overlay + the ship's own
        // perception memory it composes over — `role_apply` runs before
        // candidate scoring (script directs, brain fills tactics), and the
        // posture gates veto Engage/Ram candidacy.
        Option<&mut ScenarioRole>,
        Option<&ContactList>,
    )>,
    // T025/T027 target view (read-only, access-disjoint from `brains`' only
    // mutable component `AiBrain`): kinematics + the hull-state sources
    // `hull_fraction` documents.
    targets: Query<(
        &Position,
        &Velocity,
        Option<&Health>,
        Option<&FitLayout>,
        Option<&AuthoredCells>,
        Option<&ShipStats>,
    )>,
    // T014 (TR-020, AD-006): the capture seam exists ONLY under `ai_debug` —
    // with the feature off these params (and every capture statement below)
    // are compiled out, so headless/bench builds pay zero cost. The capture
    // query is disjoint from `brains` (no shared mutable component access).
    #[cfg(feature = "ai_debug")] mut captures: Query<&mut debug_capture::AiDebugCapture>,
    #[cfg(feature = "ai_debug")] mut commands: Commands,
) {
    let now = tick.0;

    // V-3 stable order: snapshot (stable id, entity) and sort. AiStableId is
    // unique per entity, so the sort is total — no Entity-bits tiebreak needed.
    let mut order: Vec<(AiStableId, Entity)> = brains.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();

    for (_, entity) in order {
        let Ok((_, _, mut brain, aoi, pos, vel, stats, mut role, contacts)) =
            brains.get_mut(entity)
        else {
            continue;
        };
        // Reads below go through `Deref` (no change-detection flag); the brain
        // is only marked changed when a think actually writes it.
        let event = queue.get(entity);
        let cadence = cadence_for_tier(brain.think_tier, &tuning);
        let cadence_due = (now + u64::from(brain.phase_bucket)).is_multiple_of(cadence);
        if event.is_none() && !cadence_due {
            continue; // Calm + off-cadence: zero decision work (TR-005).
        }
        if now < brain.commit_until_tick && !event.is_some_and(AiEvent::overrides_commit) {
            continue; // Committed (HINT-004); only survival-grade events break it.
        }

        // Mirror the AOI tier first so the commit window + next cadence derive
        // from the tier this think actually observed (absent component → keep
        // the previous mirror; aggregate/tier attachment is a later task).
        if let Some(aoi) = aoi {
            brain.think_tier = aoi.tier;
        }

        // T032 composition (TR-015): the scripted role DIRECTS first —
        // `role_apply` maintains waypoint/home/target upkeep from the goal —
        // then the ordinary utility selection below fills tactics WITHIN it.
        // The posture gates Engage/Ram candidacy (HoldFire vetoes always;
        // DefensiveOnly outside its fired-upon window).
        // T035 (TR-021): a recon goal additionally scores its task behavior —
        // and a ScoutArea role VETOES Engage/Ram outright (scouts avoid
        // combat; like the HoldFire candidacy veto, but the survival bucket
        // stays live — flee-permitted).
        let mut engage_allowed = true;
        let mut recon: Option<Behavior> = None;
        if let Some(role) = role.as_mut() {
            role_apply(
                role,
                &mut brain,
                pos.map(|p| p.0),
                contacts,
                tuning.base_sensor_range,
                now,
            );
            engage_allowed = role.allows_engage(now);
            match role.goal {
                RoleGoal::SweepRegion { .. } => recon = Some(Behavior::Sweep),
                RoleGoal::ScoutArea { .. } => {
                    recon = Some(Behavior::Scout);
                    engage_allowed = false; // The scout combat veto (TR-021).
                }
                _ => {}
            }
        }

        // Candidate set (see system docs): movement presence + combat.
        let k = tuning.compensation_k;
        let mut candidates: Vec<(Behavior, f32)> = Vec::with_capacity(6);
        candidates.push((Behavior::Hold, score_behavior(&[HOLD_BASELINE], k)));
        if brain.waypoint.is_some() {
            candidates.push((Behavior::Waypoint, score_behavior(&[MOVE_BASELINE], k)));
        }
        if brain.leader.is_some() {
            if brain.formation_slot.is_some() {
                candidates.push((Behavior::FormationKeep, score_behavior(&[MOVE_BASELINE], k)));
            } else {
                candidates.push((Behavior::Follow, score_behavior(&[MOVE_BASELINE], k)));
            }
        }
        // T035 (TR-021) — recon candidates. The task itself is a presence
        // consideration at RECON_BASELINE (see its docs for the
        // engage-once-perceived score interplay). A SCOUT additionally runs
        // the superior-threat test against its nearest perceived contact and
        // scores Evade (survival bucket → outranks the task bucket while the
        // threat is perceived; once the contact is released the candidate
        // vanishes and coverage resumes). "Report/maintain contacts" needs no
        // code here: the scout's own ContactList feeds sensor-network fusion.
        if let Some(task) = recon {
            candidates.push((task, score_behavior(&[RECON_BASELINE], k)));
            if task == Behavior::Scout {
                if let (Some(pos), Some(list)) = (pos, contacts) {
                    if let Some(threat) = nearest_contact(&list.contacts, pos.0) {
                        if let Ok((.., t_stats)) = targets.get(threat) {
                            // Superiority test v1 (documented, deterministic —
                            // pure component reads + comparisons): the threat
                            // is ARMED (`ShipStats::can_fire`) AND (self
                            // unarmed OR threat mass ≥ own mass) — mass as the
                            // v1 strength proxy (the ram-utility convention),
                            // flat SHIP_MASS fallback for unfitted parties.
                            let threat_armed = t_stats.is_some_and(|s| s.can_fire);
                            let self_armed = stats.is_some_and(|s| s.can_fire);
                            let own_mass = stats.map_or(SHIP_MASS, |s| s.total_mass);
                            let threat_mass = t_stats.map_or(SHIP_MASS, |s| s.total_mass);
                            if threat_armed && (!self_armed || threat_mass >= own_mass) {
                                // The Evade arm steers off brain.target; the
                                // Engage/Ram candidates stay vetoed above, so
                                // this reference is flee-only. Released by
                                // role_apply when no longer perceived (resume)
                                // or by the V-1 sweep on despawn.
                                brain.target = Some(threat);
                                candidates
                                    .push((Behavior::Evade, score_behavior(&[MOVE_BASELINE], k)));
                            }
                        }
                    }
                }
            }
        }
        // T025/T027 — combat candidates with a live target (the V-1 sweep
        // prunes despawned refs before this system, so the lookup is clean).
        // T032: the posture gate vetoes BOTH combat candidates (HoldFire
        // never selects Engage/Ram; DefensiveOnly only while fired-upon).
        if let Some((tpos, tvel, t_health, t_layout, t_authored, t_stats)) = engage_allowed
            .then_some(brain.target)
            .flatten()
            .and_then(|t| targets.get(t).ok())
        {
            candidates.push((Behavior::Engage, score_behavior(&[MOVE_BASELINE], k)));
            if let (Some(pos), Some(vel)) = (pos, vel) {
                // GEOMETRY (normalize/length) stays OUTSIDE the strict-f32
                // markers; `ram_utility` consumes pure scalars (TR-004).
                let dir = (tpos.0 - pos.0).normalize_or_zero();
                let closing = (vel.0 - tvel.0).dot(dir).max(0.0);
                // Mass fallback: unfitted parties use the flat collision-model
                // ship mass — the same body the ram impulse would move.
                let m_attacker = stats.map_or(SHIP_MASS, |s| s.total_mass);
                let m_target = t_stats.map_or(SHIP_MASS, |s| s.total_mass);
                let top = stats.map_or(0.0, ShipStats::top_speed);
                let frac = hull_fraction(t_health, t_layout, t_authored);
                let ram = ram_utility(frac, closing, top, m_attacker, m_target, &tuning);
                if ram > 0.0 {
                    candidates.push((Behavior::Ram, ram));
                }
            }
        }

        #[cfg(feature = "ai_debug")]
        let prev_behavior = brain.behavior;

        brain.behavior = select_behavior(&candidates, brain.behavior, tuning.momentum_bonus);
        brain.commit_until_tick = now + cadence_for_tier(brain.think_tier, &tuning);
        brain.last_think_tick = now;
        brain.thinks_total += 1; // One completed think (T015/T021 counter).

        // T014: record this think's final scores + any transition (AD-006).
        #[cfg(feature = "ai_debug")]
        debug_capture::capture_think(
            &mut captures,
            &mut commands,
            entity,
            now,
            prev_behavior,
            brain.behavior,
            &candidates,
            &tuning,
        );
    }

    // Drain: every queued entity got its chance this tick (despawned ones are
    // simply dropped). Guarded so an empty queue is never flagged as mutated —
    // the golden scenario worlds run this system with zero brains.
    if !queue.is_empty() {
        queue.clear();
    }
}

// ---------------------------------------------------------------------------
// T013 — behavior execution: brain → steering → ShipIntent (TR-001, V-6)
// ---------------------------------------------------------------------------

/// Arrive radius (world units) for `Waypoint`/`Patrol` goals: within this
/// range the goal counts as reached — the brain emits [`AiEvent::Arrived`] and
/// holds. A tuning-ish v1 const; `crate`-visible since T032, where the
/// `ScenarioRole` patrol cursor advances on the same radius (one shared
/// "arrived" definition). Matches the steering tests' canonical radius.
pub(crate) const ARRIVE_RADIUS: f32 = 10.0;

/// `Follow` arrive slow-radius (world units): mirrors the waypoint slow ramp
/// (4 × [`ARRIVE_RADIUS`], the `steering::WAYPOINT_SLOW_FACTOR` shape) so a
/// follower decelerates onto its leader instead of orbiting through it.
const FOLLOW_SLOW_RADIUS: f32 = 40.0;

/// The EXECUTION half of the brain (T013, TR-001): every tick, turn each
/// Active/Mid ship's selected [`Behavior`] into a [`ShipIntent`] via the
/// steering substrate. The think system SELECTS (event-driven, sparse); this
/// system EXECUTES (cheap per-tick steering math — a handful of vector ops per
/// ship), so a behavior switched mid-cadence steers the same tick.
///
/// **Output is intent-only (V-6)**: the system writes the ship's `ShipIntent`
/// component VALUE through [`steer_to_intent`]/[`compose_intent`]
/// (`crate::ai::steering`) and NEVER touches `Velocity`/`Heading`/`Position` —
/// the real flight model (`ship_motion_system`, registered right after this)
/// consumes the intent exactly as it would a player's.
///
/// **Graceful-degrade pins, checked FIRST** (TR-001 — completes the
/// data-model `any → Hold` degrade row "no live control source / no power"):
/// - **Derelict** (`stats.control_fitted && !stats.has_control`): the flight
///   model already ignores a derelict's intent (R93 free Newtonian coast), but
///   the brain must not thrash against dead controls — pin
///   `ShipIntent::default()` (zero intent) and skip steering entirely.
/// - **Dead reactor** (`stats.power_supply <= 0.0` on a fitted ship): no power
///   generation → the ship drifts; same zero-intent pin (documented choice —
///   stored capacitor charge may linger, but a brain flying on a dead reactor
///   would just burn it into an unrecoverable drift anyway).
///
/// **Tier policy**: `Dormant` ships are skipped entirely — the cheap-glide
/// aggregate owns them (T019); a ship with NO [`AoiTier`] component is treated
/// as Active (steered), matching the think system's absent-component rule.
///
/// **v1 behaviors** (movement set; combat/recon arms land with their tasks):
/// - [`Hold`](Behavior::Hold): coast — zero intent (documented v1 choice;
///   brake-to-stop is an acceptable later refinement).
/// - [`Waypoint`](Behavior::Waypoint): [`waypoint_follow`] toward
///   `brain.waypoint` (single waypoint v1). Within [`ARRIVE_RADIUS`]: hold +
///   push [`AiEvent::Arrived`] each tick ([`RethinkQueue`] coalesces to one
///   entry; the NEXT tick's think consumes it — the soft event respects the
///   commit window, so the re-think storm is bounded at one per cadence).
/// - [`Patrol`](Behavior::Patrol): v1 ping-pong — fly to `brain.waypoint`; on
///   arrive, SWAP `waypoint` ↔ `home` + `Arrived` (route vectors arrive with
///   `ScenarioRole`, T032). A home-less patrol degrades to hold-at-goal.
/// - [`Follow`](Behavior::Follow): [`arrive`] at the leader's position
///   ([`FOLLOW_SLOW_RADIUS`] ramp). Leader missing/despawned → zero intent
///   (the V-1 sweep clears the dangling ref; the next think degrades).
/// - [`FormationKeep`](Behavior::FormationKeep): [`formation_keep`] on the
///   leader's pos/vel/heading + `brain.formation_slot` (quiet on-slot).
///
/// **Combat behaviors (T025, TR-011)** — all keyed off `brain.target` looked
/// up in the same read-only kinematics query (a missing/despawned target →
/// zero intent; the V-1 sweep + next think degrade the behavior):
/// - [`Engage`](Behavior::Engage): [`engage_motion`] — the archetype-flavored
///   range-band standoff (Brawler close ring / Kiter long ring / medium
///   default) over a context map, facing the gunnery lead when on-ring.
/// - [`Evade`](Behavior::Evade): [`evade_motion`] — full-throttle break-off
///   away from the target with its direction danger-masked. Never fires
///   (documented v1 simplification; opportunistic aligned fire is a later
///   refinement).
/// - [`Retreat`](Behavior::Retreat): run HOME (`brain.home`) when set, else
///   directly away from the target. Never fires (per spec).
/// - [`Ram`](Behavior::Ram) (T027): full-throttle [`pursue_intercept`]
///   collision course; fire stays ALLOWED on the way in (a finisher, not a
///   ceasefire).
/// - [`Scout`](Behavior::Scout)/[`Sweep`](Behavior::Sweep) (T035, TR-021):
///   movement IDENTICAL to `Waypoint` — follow the role-asserted coverage leg
///   via [`waypoint_follow`], `Arrived` within the radius (the role cursor
///   advances at the next think). The recon difference is selection/veto, not
///   motion. Neither ever fires (not in the fire-overlay allowlist below).
///
/// **Fire control (T026, TR-011)**: after the movement arm, Engage/Ram run
/// [`fire_decision`] — in-range + aligned-to-lead + the energy/heat gates
/// MIRRORING `weapon_fire_system` (the AI *chooses* not to fire when gated) —
/// and on a yes set `fire_primary` + the [`primary_fire_group`]-selected
/// `active_group`. Every other behavior leaves the fire fields default
/// (false): Evade/Retreat never fire.
///
/// **Determinism (V-3)**: per-entity independent — each ship reads its own
/// brain + the leader's/target's kinematics (read-only) and writes its own
/// intent, so archetype iteration order is immaterial; `Arrived` pushes are
/// keyed per entity (BTreeMap, coalescing), never iterated. The
/// leader/target lookup query is access-disjoint from the mutable ship query
/// (it reads only `Position`/`Velocity`/`Heading`; the mutable accesses are
/// `AiBrain` + `ShipIntent`), so leaders/targets may themselves be AI ships.
/// `SimTuning`/`AiTuning` are read through `Option` with pinned-default
/// fallback (the graceful-degradation pattern `weapon_fire_system` uses), so
/// minimal test worlds run without them.
///
/// Registered in the `ScenarioActive`-gated AI set AFTER [`ai_think_system`]
/// and BEFORE `ship_motion_system`; no golden world spawns an `AiBrain`, so
/// the goldens stay bit-identical.
pub fn ai_execute_system(
    mut queue: ResMut<RethinkQueue>,
    // T026: shot-cost scale for the energy gate (Option → const defaults in
    // minimal worlds, the weapon_fire_system pattern).
    sim: Option<Res<SimTuning>>,
    // T025: context-map slot count + danger mask floor (Option → pinned
    // defaults; the system's run conditions predate AiTuning).
    tuning: Option<Res<AiTuning>>,
    // T032: the DefensiveOnly fired-upon window compares against the current
    // tick (Option → 0 in minimal worlds, which carry no roles anyway).
    tick: Option<Res<CurrentTick>>,
    mut ships: Query<(
        Entity,
        &mut AiBrain,
        &Position,
        &Velocity,
        &Heading,
        &mut ShipIntent,
        Option<&ShipStats>,
        Option<&AoiTier>,
        // T026 fire-control reads (all read-only; pools absent = ungated,
        // mirroring weapon_fire_system).
        Option<&Energy>,
        Option<&Heat>,
        Option<&WeaponGroups>,
        Option<&ShipWeapons>,
        // T032: the posture fire-gate overlay (read-only; absent = ungated).
        Option<&ScenarioRole>,
    )>,
    // Leader AND combat-target kinematics (read-only; see Determinism docs).
    others: Query<(&Position, &Velocity, &Heading)>,
) {
    let sim = sim.map(|s| *s).unwrap_or_default();
    let ai = tuning.map(|t| *t).unwrap_or_default();
    let now = tick.map_or(0, |t| t.0);
    for (
        entity,
        mut brain,
        pos,
        vel,
        heading,
        mut intent,
        stats,
        aoi,
        energy,
        heat,
        groups,
        weapons,
        role,
    ) in &mut ships
    {
        // Dormant: the glide aggregate owns it (T019) — leave its intent alone.
        if aoi.is_some_and(|a| a.tier == Tier::Dormant) {
            continue;
        }
        // TR-001 graceful-degrade pins (see system docs) — checked FIRST.
        if let Some(stats) = stats {
            let derelict = stats.control_fitted && !stats.has_control;
            if derelict || stats.power_supply <= 0.0 {
                intent.set_if_neq(ShipIntent::default());
                continue;
            }
        }
        // TR-003 turn authority for the reachability bias; an unfitted ship
        // (no ShipStats) passes 0 = "unknown → maximum caution" (the
        // documented `reachability_bias` convention).
        let turn_authority = stats.map_or(0.0, ShipStats::max_turn_rate);
        let fly_to = |goal: Vec2| {
            let (dir, throttle, _) = waypoint_follow(pos.0, vel.0, &[goal], 0, ARRIVE_RADIUS);
            steer_to_intent(dir, throttle, heading.0, vel.0, turn_authority)
        };

        let mut next = match brain.behavior {
            // Coast (v1 documented choice; brake-to-stop is a later refinement).
            Behavior::Hold => ShipIntent::default(),
            Behavior::Waypoint => match brain.waypoint {
                Some(goal) if (goal - pos.0).length() <= ARRIVE_RADIUS => {
                    queue.push(entity, AiEvent::Arrived);
                    ShipIntent::default()
                }
                Some(goal) => fly_to(goal),
                None => ShipIntent::default(), // Goal-less: the think degrades it.
            },
            Behavior::Patrol => match brain.waypoint {
                Some(goal) if (goal - pos.0).length() <= ARRIVE_RADIUS => {
                    // v1 ping-pong: swap the reached goal with the home anchor
                    // so the next leg flies back. Home-less → hold-at-goal.
                    if brain.home.is_some() {
                        let reached = brain.waypoint;
                        brain.waypoint = brain.home;
                        brain.home = reached;
                    }
                    queue.push(entity, AiEvent::Arrived);
                    ShipIntent::default()
                }
                Some(goal) => fly_to(goal),
                None => ShipIntent::default(),
            },
            Behavior::Follow => match brain.leader.and_then(|l| others.get(l).ok()) {
                Some((lpos, _, _)) => {
                    let (dir, throttle) = arrive(pos.0, vel.0, lpos.0, FOLLOW_SLOW_RADIUS);
                    steer_to_intent(dir, throttle, heading.0, vel.0, turn_authority)
                }
                None => ShipIntent::default(), // Leader gone: sweep/think clean up.
            },
            Behavior::FormationKeep => {
                match (
                    brain.leader.and_then(|l| others.get(l).ok()),
                    brain.formation_slot,
                ) {
                    (Some((lpos, lvel, lheading)), Some(slot)) => {
                        let (dir, throttle) =
                            formation_keep(pos.0, vel.0, lpos.0, lvel.0, lheading.0, slot);
                        steer_to_intent(dir, throttle, heading.0, vel.0, turn_authority)
                    }
                    _ => ShipIntent::default(),
                }
            }
            // T025 combat arms (see system docs); target gone → zero intent
            // (the V-1 sweep clears the ref; the next think degrades).
            Behavior::Engage => match brain.target.and_then(|t| others.get(t).ok()) {
                Some((tpos, tvel, _)) => engage_motion(
                    brain.archetype,
                    pos.0,
                    vel.0,
                    heading.0,
                    turn_authority,
                    stats,
                    tpos.0,
                    tvel.0,
                    &ai,
                ),
                None => ShipIntent::default(),
            },
            Behavior::Evade => match brain.target.and_then(|t| others.get(t).ok()) {
                Some((tpos, _, _)) => {
                    evade_motion(pos.0, vel.0, heading.0, turn_authority, tpos.0, &ai)
                }
                None => ShipIntent::default(),
            },
            Behavior::Retreat => match brain.home {
                // Run home: the waypoint arrive ramp stops the ship there.
                Some(home) => fly_to(home),
                // No home anchor: open the range directly away from the threat.
                None => match brain.target.and_then(|t| others.get(t).ok()) {
                    Some((tpos, _, _)) => {
                        let away = (pos.0 - tpos.0).normalize_or_zero();
                        steer_to_intent(away, 1.0, heading.0, vel.0, turn_authority)
                    }
                    None => ShipIntent::default(),
                },
            },
            // T027 Ram: full-throttle lead-pursuit collision course.
            Behavior::Ram => match brain.target.and_then(|t| others.get(t).ok()) {
                Some((tpos, tvel, _)) => {
                    let top = stats.map_or(0.0, ShipStats::top_speed);
                    let dir = pursue_intercept(pos.0, top, tpos.0, tvel.0);
                    steer_to_intent(dir, 1.0, heading.0, vel.0, turn_authority)
                }
                None => ShipIntent::default(),
            },
            // T035 recon arms (TR-021): Scout/Sweep MOVE exactly like
            // Waypoint — fly `brain.waypoint` (the role-asserted coverage
            // leg), hold + push `Arrived` within the radius so the role's
            // route cursor advances at the next think. The recon DIFFERENCE
            // lives entirely in SELECTION (the Scout combat veto +
            // superior-threat Evade; Sweep's engage-once-perceived baseline),
            // never in motion.
            Behavior::Scout | Behavior::Sweep => match brain.waypoint {
                Some(goal) if (goal - pos.0).length() <= ARRIVE_RADIUS => {
                    queue.push(entity, AiEvent::Arrived);
                    ShipIntent::default()
                }
                Some(goal) => fly_to(goal),
                None => ShipIntent::default(), // Goal-less: the think degrades it.
            },
        };
        // T026 fire-control overlay (TR-011): only Engage and Ram (a finisher
        // fires on the way in) ever pull the trigger; `fire_decision` owns the
        // in-range/alignment checks + the energy/heat gates. All other
        // behaviors keep the default fire fields — Evade/Retreat never fire.
        // T032 (TR-015): the scenario posture gates the trigger itself —
        // HoldFire NEVER fires, DefensiveOnly fires only inside its
        // fired-upon window — even when the behavior is pinned to Engage/Ram.
        if matches!(brain.behavior, Behavior::Engage | Behavior::Ram)
            && role.is_none_or(|r| r.allows_engage(now))
        {
            if let Some((tpos, tvel, _)) = brain.target.and_then(|t| others.get(t).ok()) {
                if let Some(group) = fire_decision(
                    pos.0, heading.0, stats, weapons, groups, energy, heat, tpos.0, tvel.0, &sim,
                ) {
                    next.active_group = group;
                    next.fire_primary = true;
                }
            }
        }
        // T017 squad pace seam: the throttle cap scales forward intent
        // multiplicatively (default 1.0 — `x * 1.0` is bit-identical to `x`,
        // so non-squad brains are untouched). Forward only: turn/strafe keep
        // full authority so a paced leader still maneuvers crisply.
        next.forward *= brain.throttle_cap;
        // `set_if_neq`: only flag the intent changed when the value moved —
        // a coasting ship's intent stays change-detection quiet.
        intent.set_if_neq(next);
    }
}

// ---------------------------------------------------------------------------
// T014 — feature-gated score/transition capture seam (TR-020, AD-006)
// ---------------------------------------------------------------------------

/// TR-020 / AD-006 capture seam — compiled ONLY under the `ai_debug` cargo
/// feature (OFF by default): headless server + bench builds contain none of
/// this code, so the measured TR-017 path pays zero cost. The windowed client
/// enables the feature and the dev panel (T038) reads the component.
#[cfg(feature = "ai_debug")]
pub mod debug_capture {
    use std::collections::VecDeque;

    use bevy_ecs::prelude::*;

    use super::{AiTuning, Behavior};

    /// Per-brain capture of the LAST completed think (component on the brain
    /// entity, inserted lazily by the first captured think): the dev panel's
    /// score-breakdown source (AD-006 — "without a score-breakdown view,
    /// tuning is blind"). Pure observability: nothing in the sim reads it.
    #[derive(Component, Clone, Debug, Default, PartialEq)]
    pub struct AiDebugCapture {
        /// Final per-candidate scores of the last think — the exact values
        /// `select_behavior` compared, momentum INCLUDED on the incumbent.
        pub last_scores: Vec<(Behavior, f32)>,
        /// The behavior the last think selected (the `Hold` degrade included).
        pub winner: Behavior,
        /// The momentum bonus applied to the incumbent's score at the last
        /// think (`0.0` when the incumbent was not among the candidates).
        pub momentum_applied: f32,
        /// Behavior-transition ring `(tick, from, to)` — recorded on CHANGE
        /// only, bounded by [`AiTuning::debug_history_len`] (oldest dropped).
        pub transitions: VecDeque<(u64, Behavior, Behavior)>,
    }

    impl AiDebugCapture {
        /// Fold one completed think into the capture.
        fn record(
            &mut self,
            tick: u64,
            from: Behavior,
            to: Behavior,
            scores: Vec<(Behavior, f32)>,
            momentum_applied: f32,
            history_len: usize,
        ) {
            self.last_scores = scores;
            self.winner = to;
            self.momentum_applied = momentum_applied;
            if from != to {
                self.transitions.push_back((tick, from, to));
                // A degenerate live-edited 0 keeps the newest entry (never an
                // empty ring right after a recorded transition).
                while self.transitions.len() > history_len.max(1) {
                    self.transitions.pop_front();
                }
            }
        }
    }

    /// Populate (insert-or-update) the brain entity's [`AiDebugCapture`] for
    /// one completed think — called by `ai_think_system` under the feature
    /// cfg. The first capture for an entity inserts the component via
    /// `Commands` (applied at the end of the schedule run); later thinks
    /// update it in place through the query.
    #[allow(clippy::too_many_arguments)] // Mirrors the think loop's locals 1:1.
    pub(super) fn capture_think(
        captures: &mut Query<&mut AiDebugCapture>,
        commands: &mut Commands,
        entity: Entity,
        tick: u64,
        from: Behavior,
        to: Behavior,
        candidates: &[(Behavior, f32)],
        tuning: &AiTuning,
    ) {
        // The FINAL scores `select_behavior` compared: the incumbent's raw
        // score times the momentum multiplier, everyone else as-is.
        let last_scores: Vec<(Behavior, f32)> = candidates
            .iter()
            .map(|&(behavior, raw)| {
                if behavior == from {
                    (behavior, raw * (1.0 + tuning.momentum_bonus))
                } else {
                    (behavior, raw)
                }
            })
            .collect();
        let momentum_applied = if candidates.iter().any(|&(b, _)| b == from) {
            tuning.momentum_bonus
        } else {
            0.0
        };
        let history_len = tuning.debug_history_len as usize;
        if let Ok(mut capture) = captures.get_mut(entity) {
            capture.record(tick, from, to, last_scores, momentum_applied, history_len);
        } else {
            let mut capture = AiDebugCapture::default();
            capture.record(tick, from, to, last_scores, momentum_applied, history_len);
            commands.entity(entity).insert(capture);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    // --- T010: enum ordering + buckets -----------------------------------

    /// Declaration order IS the intra-ship tiebreak ordinal (HINT-002 level
    /// one) and `Hold` is the default.
    #[test]
    fn behavior_ordinal_is_declaration_order() {
        assert_eq!(Behavior::default(), Behavior::Hold);
        assert!(Behavior::Hold < Behavior::Patrol);
        assert!(Behavior::Patrol < Behavior::Waypoint);
        assert!(Behavior::Waypoint < Behavior::Follow);
        assert!(Behavior::Follow < Behavior::FormationKeep);
        assert!(Behavior::FormationKeep < Behavior::Engage);
        assert!(Behavior::Engage < Behavior::Evade);
        assert!(Behavior::Evade < Behavior::Retreat);
        assert!(Behavior::Retreat < Behavior::Scout);
        assert!(Behavior::Scout < Behavior::Sweep);
        assert!(Behavior::Sweep < Behavior::Ram);
    }

    /// Survival > tasks > idle/movement (research priority buckets).
    #[test]
    fn priority_buckets_rank_survival_over_tasks_over_movement() {
        for survival in [Behavior::Evade, Behavior::Retreat, Behavior::Ram] {
            for task in [Behavior::Engage, Behavior::Scout, Behavior::Sweep] {
                assert!(survival.priority_bucket() > task.priority_bucket());
            }
        }
        for task in [Behavior::Engage, Behavior::Scout, Behavior::Sweep] {
            for idle in [
                Behavior::Hold,
                Behavior::Patrol,
                Behavior::Waypoint,
                Behavior::Follow,
                Behavior::FormationKeep,
            ] {
                assert!(task.priority_bucket() > idle.priority_bucket());
            }
        }
    }

    // --- T010: scoring ----------------------------------------------------

    /// Curves clamp out-of-range inputs and shape as documented.
    #[test]
    fn curves_clamp_and_shape() {
        assert_eq!(curve_linear(-1.0), 0.0);
        assert_eq!(curve_linear(2.0), 1.0);
        assert_eq!(curve_linear(0.25), 0.25);
        assert_eq!(curve_quadratic(0.5), 0.25);
        assert_eq!(curve_inv(0.25), 0.75);
        assert_eq!(curve_smooth(0.0), 0.0);
        assert_eq!(curve_smooth(1.0), 1.0);
        assert_eq!(curve_smooth(0.5), 0.5); // 0.25 · (3 − 1.0) = 0.5 exactly.
        assert!(curve_smooth(0.25) < 0.25, "S-curve suppresses low input");
        assert!(curve_smooth(0.75) > 0.75, "S-curve amplifies high input");
    }

    /// TR-004 strict-f32 determinism: the same inputs produce bit-identical
    /// scores, and the compensated score stays within scoring bounds.
    #[test]
    fn score_behavior_same_inputs_same_bits() {
        let inputs = [0.3_f32, 0.7, 0.9];
        let a = score_behavior(&inputs, 1.0);
        let b = score_behavior(&inputs, 1.0);
        assert_eq!(a.to_bits(), b.to_bits(), "bit-identical across calls");
        let product = 0.3_f32 * 0.7 * 0.9;
        assert!(a >= product, "compensation never lowers the product");
        assert!(a <= 1.0, "compensated score stays in [0, 1]");
    }

    /// The documented compensation formula `s + (1−s)·s·k·(n−1)/n`: n = 1
    /// passes through, zero stays zero (veto intact), one stays one.
    #[test]
    fn score_behavior_compensation_properties() {
        assert_eq!(score_behavior(&[0.5], 1.0), 0.5, "n = 1 passthrough");
        assert_eq!(score_behavior(&[0.0, 0.9], 1.0), 0.0, "veto survives");
        assert_eq!(score_behavior(&[1.0, 1.0, 1.0], 1.0), 1.0, "1 stays 1");
        assert_eq!(score_behavior(&[], 1.0), 0.0, "no considerations → 0");
        // Pinned arithmetic: s = 0.25, n = 2 → 0.25 + 0.75·0.25·0.5 = 0.34375.
        assert_eq!(score_behavior(&[0.5, 0.5], 1.0), 0.34375);
    }

    // --- T010: selection ---------------------------------------------------

    /// The ~25% momentum bonus keeps the incumbent on a near-tie…
    #[test]
    fn momentum_keeps_incumbent_on_near_tie() {
        let candidates = [(Behavior::Waypoint, 1.0), (Behavior::FormationKeep, 0.9)];
        let pick = select_behavior(&candidates, Behavior::FormationKeep, 0.25);
        assert_eq!(pick, Behavior::FormationKeep, "0.9 · 1.25 = 1.125 > 1.0");
    }

    /// …but a much better candidate still wins through it.
    #[test]
    fn much_better_candidate_beats_momentum() {
        let candidates = [(Behavior::Waypoint, 1.0), (Behavior::FormationKeep, 0.5)];
        let pick = select_behavior(&candidates, Behavior::FormationKeep, 0.25);
        assert_eq!(pick, Behavior::Waypoint, "0.5 · 1.25 = 0.625 < 1.0");
    }

    /// Buckets evaluate highest-first: any positive survival score beats any
    /// task score — even a maxed, momentum-boosted incumbent task.
    #[test]
    fn positive_survival_score_beats_any_task_score() {
        let candidates = [(Behavior::Engage, 1.0), (Behavior::Evade, 0.05)];
        let pick = select_behavior(&candidates, Behavior::Engage, 0.25);
        assert_eq!(pick, Behavior::Evade, "bucket dominance, not score size");
    }

    /// Exact (f32 ==) ties inside one bucket break by enum ordinal — the
    /// lower declaration ordinal wins, independent of candidate order.
    #[test]
    fn exact_tie_breaks_by_behavior_ordinal() {
        let forward = [(Behavior::Scout, 0.5), (Behavior::Sweep, 0.5)];
        let reverse = [(Behavior::Sweep, 0.5), (Behavior::Scout, 0.5)];
        assert_eq!(
            select_behavior(&forward, Behavior::Hold, 0.25),
            Behavior::Scout
        );
        assert_eq!(
            select_behavior(&reverse, Behavior::Hold, 0.25),
            Behavior::Scout
        );
    }

    /// Zero-score veto: when NOTHING scores > 0 the selection degrades to
    /// `Hold` (data-model "any → Hold" row) — momentum can't rescue a zero.
    #[test]
    fn all_zero_candidates_degrade_to_hold() {
        let candidates = [(Behavior::Engage, 0.0), (Behavior::Scout, 0.0)];
        assert_eq!(
            select_behavior(&candidates, Behavior::Engage, 0.25),
            Behavior::Hold
        );
        assert_eq!(select_behavior(&[], Behavior::Engage, 0.25), Behavior::Hold);
    }

    // --- T012: archetype classification ------------------------------------

    /// A real derived fighter fit (reactor + thruster + autocannon — the
    /// energy.rs pattern), used as the base for synthetic stat overrides.
    fn fighter_stats() -> ShipStats {
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{
            build_layout, derive_ship_stats, seed_catalogs, Fit, SlotId, HULL_FIGHTER,
        };
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let layout = build_layout(hull, &fit, &modules);
        derive_ship_stats(hull, &fit, &modules, &layout)
    }

    /// Synthetic stats pinning the three classification axes exactly:
    /// `top_speed` (drag normalized to 1), primary-weapon DPS, and armor.
    fn stats_with(top_speed: f32, dps: f32, armor: f32) -> ShipStats {
        let mut s = fighter_stats();
        s.linear_drag = 1.0;
        s.thrust_force = top_speed; // top_speed = thrust / drag = thrust.
        s.armor_value = armor;
        if dps > 0.0 {
            let mut w = s.weapon.expect("seed fighter carries a weapon");
            w.fire_rate = 1.0;
            w.damage = dps; // DPS = damage · fire_rate = damage.
            s.weapon = Some(w);
        } else {
            s.weapon = None;
        }
        s
    }

    /// The documented threshold cuts produce the expected archetypes
    /// (defaults: speed_hi 60, dps_hi 20, armor_hi 100).
    #[test]
    fn classify_archetype_cuts() {
        let t = AiTuning::default();
        let cases = [
            (stats_with(80.0, 30.0, 200.0), FitArchetype::Brawler),
            (stats_with(80.0, 30.0, 0.0), FitArchetype::Kiter),
            (stats_with(30.0, 30.0, 0.0), FitArchetype::Orbiter),
            (stats_with(30.0, 0.0, 200.0), FitArchetype::Rammer),
            (stats_with(80.0, 0.0, 0.0), FitArchetype::Support),
            (stats_with(30.0, 0.0, 0.0), FitArchetype::Generic),
        ];
        for (stats, expected) in cases {
            assert_eq!(classify_archetype(&stats, &t), expected);
        }
    }

    /// V-5: the refresh runs ONLY on `Changed<ShipStats>` — an untouched fit
    /// is never reclassified; touching the stats reclassifies that tick.
    #[test]
    fn archetype_refresh_only_on_changed_shipstats() {
        let mut world = World::new();
        world.insert_resource(AiTuning::default());
        let e = world
            .spawn((stats_with(80.0, 30.0, 200.0), AiBrain::default()))
            .id();
        let mut schedule = Schedule::default();
        schedule.add_systems(archetype_refresh_system);

        // Freshly-added ShipStats counts as Changed → classified.
        schedule.run(&mut world);
        assert_eq!(
            world.get::<AiBrain>(e).unwrap().archetype,
            FitArchetype::Brawler
        );

        // Sabotage the cache WITHOUT touching ShipStats: no reclassify.
        world.get_mut::<AiBrain>(e).unwrap().archetype = FitArchetype::Generic;
        schedule.run(&mut world);
        assert_eq!(
            world.get::<AiBrain>(e).unwrap().archetype,
            FitArchetype::Generic,
            "no Changed<ShipStats> → cache untouched (V-5)"
        );

        // A real stats change (armor stripped) reclassifies this tick.
        world.get_mut::<ShipStats>(e).unwrap().armor_value = 0.0;
        schedule.run(&mut world);
        assert_eq!(
            world.get::<AiBrain>(e).unwrap().archetype,
            FitArchetype::Kiter
        );
    }

    // --- T011: scheduler ----------------------------------------------------

    fn think_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick(0));
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(ai_think_system);
        (world, schedule)
    }

    fn step(world: &mut World, schedule: &mut Schedule, tick: u64) {
        world.resource_mut::<CurrentTick>().0 = tick;
        schedule.run(world);
    }

    fn brain_of(world: &World, e: Entity) -> AiBrain {
        *world.get::<AiBrain>(e).expect("entity carries AiBrain")
    }

    /// An Active-cadence brain (cadence 15) with a known phase bucket.
    fn active_brain(bucket: u16) -> AiBrain {
        AiBrain {
            think_tier: Tier::Active,
            phase_bucket: bucket,
            ..AiBrain::default()
        }
    }

    /// Cadence: a brain with bucket `b` thinks ONLY on ticks where
    /// `(now + b) % cadence == 0` — and the re-armed commit window (exactly
    /// one cadence period) never blocks the next on-cadence think.
    #[test]
    fn cadence_thinks_only_on_matching_ticks() {
        let (mut world, mut schedule) = think_world();
        let e = world.spawn((AiStableId(0), active_brain(3))).id();

        // Cadence 15, bucket 3 → due at ticks 12 and 27 within 0..=30.
        let mut thinks = Vec::new();
        let mut last = brain_of(&world, e).last_think_tick;
        for tick in 0..=30 {
            step(&mut world, &mut schedule, tick);
            let now = brain_of(&world, e).last_think_tick;
            if now != last {
                thinks.push(tick);
                last = now;
            }
        }
        assert_eq!(thinks, vec![12, 27], "only the bucket-matched ticks think");
        assert_eq!(
            brain_of(&world, e).commit_until_tick,
            27 + 15,
            "commit window re-armed to one cadence period at the last think"
        );
    }

    /// AD-003: an event forces an immediate (off-cadence) think, and multiple
    /// events for one entity coalesce into a single queue entry / think.
    #[test]
    fn event_forces_immediate_think_and_coalesces() {
        let (mut world, mut schedule) = think_world();
        let e = world.spawn((AiStableId(0), active_brain(3))).id();

        // Tick 5 is off-cadence for bucket 3 ((5 + 3) % 15 == 8).
        let mut queue = world.resource_mut::<RethinkQueue>();
        queue.push(e, AiEvent::NewContact);
        queue.push(e, AiEvent::DamageTaken);
        assert_eq!(queue.len(), 1, "two events, ONE coalesced entry");

        step(&mut world, &mut schedule, 5);
        assert_eq!(brain_of(&world, e).last_think_tick, 5, "thought same tick");
        assert!(
            world.resource::<RethinkQueue>().is_empty(),
            "queue drained at end of the think run"
        );
    }

    /// Coalescing keeps the strongest event: a commit-overriding event
    /// upgrades a pending soft one, and never downgrades back.
    #[test]
    fn rethink_queue_coalesces_to_strongest_event() {
        let mut world = World::new();
        let e = world.spawn_empty().id();
        let mut q = RethinkQueue::default();
        q.push(e, AiEvent::NewContact);
        q.push(e, AiEvent::DamageTaken);
        assert_eq!(q.get(e), Some(AiEvent::DamageTaken), "upgraded");
        q.push(e, AiEvent::Arrived);
        assert_eq!(q.get(e), Some(AiEvent::DamageTaken), "never downgraded");
        assert_eq!(q.len(), 1);
    }

    /// HINT-004: inside the commit window a due cadence think and a soft event
    /// are both skipped; a survival-grade event (DamageTaken) thinks anyway.
    #[test]
    fn commit_window_blocks_until_survival_event() {
        let (mut world, mut schedule) = think_world();
        let mut brain = active_brain(3);
        brain.commit_until_tick = 100;
        let e = world.spawn((AiStableId(0), brain)).id();

        // Tick 12 is cadence-due for bucket 3 — but committed until 100.
        step(&mut world, &mut schedule, 12);
        assert_eq!(brain_of(&world, e).last_think_tick, 0, "cadence blocked");

        // A soft event does not break the commitment.
        world
            .resource_mut::<RethinkQueue>()
            .push(e, AiEvent::Arrived);
        step(&mut world, &mut schedule, 13);
        assert_eq!(brain_of(&world, e).last_think_tick, 0, "soft event waits");

        // A survival-grade event does.
        world
            .resource_mut::<RethinkQueue>()
            .push(e, AiEvent::DamageTaken);
        step(&mut world, &mut schedule, 14);
        assert_eq!(brain_of(&world, e).last_think_tick, 14, "urgent overrides");
    }

    /// v1 presence selection: waypoint → `Waypoint`, leader + slot →
    /// `FormationKeep`, leader only → `Follow`, nothing → `Hold`; a completed
    /// think re-arms the commit window and mirrors the AOI tier.
    #[test]
    fn think_selects_movement_behavior_from_presence() {
        let (mut world, mut schedule) = think_world();
        let leader = world.spawn_empty().id();

        let idle = world.spawn((AiStableId(0), active_brain(0))).id();
        let way = world
            .spawn((
                AiStableId(1),
                AiBrain {
                    waypoint: Some(Vec2::new(10.0, 0.0)),
                    ..active_brain(0)
                },
            ))
            .id();
        let form = world
            .spawn((
                AiStableId(2),
                AiBrain {
                    leader: Some(leader),
                    formation_slot: Some(Vec2::new(0.0, 5.0)),
                    ..active_brain(0)
                },
            ))
            .id();
        let follow = world
            .spawn((
                AiStableId(3),
                AiBrain {
                    leader: Some(leader),
                    ..active_brain(0)
                },
            ))
            .id();
        // A Dormant-mirrored brain whose AoiTier says Active: the think
        // mirrors the tier and derives its commit window from it.
        let mirrored = world
            .spawn((
                AiStableId(4),
                AiBrain::default(), // think_tier Dormant, bucket 0 → due at 0.
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 0,
                },
            ))
            .id();

        step(&mut world, &mut schedule, 0); // bucket 0 → everyone is due.

        assert_eq!(brain_of(&world, idle).behavior, Behavior::Hold);
        assert_eq!(brain_of(&world, way).behavior, Behavior::Waypoint);
        assert_eq!(brain_of(&world, form).behavior, Behavior::FormationKeep);
        assert_eq!(brain_of(&world, follow).behavior, Behavior::Follow);

        let m = brain_of(&world, mirrored);
        assert_eq!(m.think_tier, Tier::Active, "AOI tier mirrored at think");
        assert_eq!(m.commit_until_tick, 15, "window from the MIRRORED tier");
        assert_eq!(brain_of(&world, way).commit_until_tick, 15);
        assert_eq!(brain_of(&world, way).last_think_tick, 0);
    }

    /// `cadence_for_tier` maps each tier to its tuned cadence and clamps a
    /// degenerate 0 to 1 (never a modulo-by-zero).
    #[test]
    fn cadence_for_tier_maps_and_guards_zero() {
        let t = AiTuning::default();
        assert_eq!(cadence_for_tier(Tier::Active, &t), 15);
        assert_eq!(cadence_for_tier(Tier::Mid, &t), 15);
        assert_eq!(cadence_for_tier(Tier::Dormant, &t), 90);
        let zero = AiTuning {
            think_ticks_active: 0,
            ..AiTuning::default()
        };
        assert_eq!(cadence_for_tier(Tier::Active, &zero), 1);
    }

    // --- T013: behavior execution -------------------------------------------

    /// A world + schedule running ONLY the execution half (no think — tests
    /// pin the behavior directly).
    fn exec_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(ai_execute_system);
        (world, schedule)
    }

    /// Kinematics + intent bundle for an executed ship at `pos`, heading +X.
    fn ship_bundle(brain: AiBrain, pos: Vec2) -> impl Bundle {
        (
            brain,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            ShipIntent::default(),
        )
    }

    fn intent_of(world: &World, e: Entity) -> ShipIntent {
        *world
            .get::<ShipIntent>(e)
            .expect("entity carries ShipIntent")
    }

    /// TR-001 derelict pin: a fitted ship whose control source is dead keeps a
    /// DEFAULT intent even with a live `Waypoint` goal — and a fitted ship
    /// with a dead reactor (`power_supply <= 0`) pins the same way.
    #[test]
    fn derelict_and_unpowered_fitted_ships_pin_zero_intent() {
        let (mut world, mut schedule) = exec_world();
        let brain = AiBrain {
            behavior: Behavior::Waypoint,
            waypoint: Some(Vec2::new(100.0, 0.0)),
            ..AiBrain::default()
        };

        // Derelict: control fitted, no live control source (R93).
        let mut derelict = fighter_stats();
        derelict.control_fitted = true;
        derelict.has_control = false;
        let d = world.spawn((ship_bundle(brain, Vec2::ZERO), derelict)).id();
        // Pre-dirty the intent: the pin must actively overwrite it.
        world.get_mut::<ShipIntent>(d).unwrap().forward = 1.0;

        // Dead reactor: zero power generation on a fitted ship → drift.
        let mut unpowered = fighter_stats();
        unpowered.power_supply = 0.0;
        let u = world
            .spawn((ship_bundle(brain, Vec2::ZERO), unpowered))
            .id();
        world.get_mut::<ShipIntent>(u).unwrap().forward = 1.0;

        schedule.run(&mut world);
        assert_eq!(
            intent_of(&world, d),
            ShipIntent::default(),
            "derelict → zero intent despite a live Waypoint goal (TR-001)"
        );
        assert_eq!(
            intent_of(&world, u),
            ShipIntent::default(),
            "dead reactor → drift (zero-intent pin)"
        );
        assert!(
            world.resource::<RethinkQueue>().is_empty(),
            "pinned ships never emit Arrived"
        );
    }

    /// `Waypoint` writes a nonzero forward intent toward the goal, and within
    /// the arrive radius it holds (zero intent) + queues `Arrived`.
    #[test]
    fn waypoint_behavior_steers_toward_goal_and_emits_arrived() {
        let (mut world, mut schedule) = exec_world();
        let goal = Vec2::new(100.0, 0.0);
        let brain = AiBrain {
            behavior: Behavior::Waypoint,
            waypoint: Some(goal),
            ..AiBrain::default()
        };
        let e = world.spawn(ship_bundle(brain, Vec2::ZERO)).id();

        schedule.run(&mut world);
        let intent = intent_of(&world, e);
        assert!(
            intent.forward > 0.9,
            "goal dead ahead → full burn (got {})",
            intent.forward
        );
        assert!(intent.turn.abs() < 1e-5, "no turn toward a dead-ahead goal");
        assert!(
            world.resource::<RethinkQueue>().is_empty(),
            "still en route → no Arrived"
        );

        // Inside ARRIVE_RADIUS: hold + Arrived queued for the next think.
        world.get_mut::<Position>(e).unwrap().0 = Vec2::new(95.0, 0.0);
        schedule.run(&mut world);
        assert_eq!(intent_of(&world, e), ShipIntent::default(), "arrive → hold");
        assert_eq!(
            world.resource::<RethinkQueue>().get(e),
            Some(AiEvent::Arrived)
        );
    }

    /// `Patrol` v1 ping-pong: on arrive the waypoint and home anchors swap
    /// (next leg flies back) and `Arrived` is queued.
    #[test]
    fn patrol_ping_pongs_waypoint_with_home_on_arrive() {
        let (mut world, mut schedule) = exec_world();
        let (goal, home) = (Vec2::new(100.0, 0.0), Vec2::new(-100.0, 0.0));
        let brain = AiBrain {
            behavior: Behavior::Patrol,
            waypoint: Some(goal),
            home: Some(home),
            ..AiBrain::default()
        };
        let e = world.spawn(ship_bundle(brain, Vec2::new(95.0, 0.0))).id();

        schedule.run(&mut world);
        let b = brain_of(&world, e);
        assert_eq!(b.waypoint, Some(home), "arrive → swapped onto the home leg");
        assert_eq!(b.home, Some(goal), "the reached goal becomes the anchor");
        assert_eq!(
            world.resource::<RethinkQueue>().get(e),
            Some(AiEvent::Arrived)
        );
        assert_eq!(
            intent_of(&world, e),
            ShipIntent::default(),
            "holds on the arrival tick; the next tick flies the swapped leg"
        );
    }

    /// `Follow` arrives at a live leader; a despawned leader (pruned by the
    /// V-1 sweep the same tick) degrades to zero intent.
    #[test]
    fn follow_with_despawned_leader_goes_quiet_after_sweep() {
        let mut world = World::new();
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        // The real registration order: sweep prunes BEFORE execution reads.
        schedule
            .add_systems((crate::ai::ident::ai_despawn_sweep_system, ai_execute_system).chain());

        let leader = world
            .spawn((
                Position(Vec2::new(100.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
            ))
            .id();
        let brain = AiBrain {
            behavior: Behavior::Follow,
            leader: Some(leader),
            ..AiBrain::default()
        };
        let e = world.spawn(ship_bundle(brain, Vec2::ZERO)).id();

        schedule.run(&mut world);
        assert!(
            intent_of(&world, e).forward > 0.9,
            "live leader ahead → follow burn"
        );

        world.despawn(leader);
        schedule.run(&mut world);
        assert_eq!(
            brain_of(&world, e).leader,
            None,
            "sweep pruned the dangling leader (V-1)"
        );
        assert_eq!(
            intent_of(&world, e),
            ShipIntent::default(),
            "leader gone → zero intent until the next think degrades the behavior"
        );
    }

    /// Dormant-tier ships are skipped entirely (the T019 glide owns them):
    /// their intent is never touched, even with a live goal.
    #[test]
    fn dormant_tier_ships_are_skipped_by_execution() {
        let (mut world, mut schedule) = exec_world();
        let brain = AiBrain {
            behavior: Behavior::Waypoint,
            waypoint: Some(Vec2::new(100.0, 0.0)),
            ..AiBrain::default()
        };
        let e = world
            .spawn((
                ship_bundle(brain, Vec2::ZERO),
                AoiTier {
                    tier: Tier::Dormant,
                    since_tick: 0,
                },
            ))
            .id();
        let pinned = ShipIntent {
            forward: 0.25,
            ..ShipIntent::default()
        };
        *world.get_mut::<ShipIntent>(e).unwrap() = pinned;

        schedule.run(&mut world);
        assert_eq!(
            intent_of(&world, e),
            pinned,
            "Dormant: execution leaves the intent untouched"
        );
    }

    // --- T025/T026/T027: combat behaviors, fire gates, ram decision ---------

    /// The freshly-built fighter [`FitLayout`] (the same fit as
    /// [`fighter_stats`]), for hull-fraction fixtures.
    fn fighter_layout() -> FitLayout {
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{build_layout, seed_catalogs, Fit, SlotId, HULL_FIGHTER};
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        build_layout(hull, &fit, &modules)
    }

    /// A full (never-blocking) energy pool for fire-gate fixtures.
    fn full_energy() -> Energy {
        Energy {
            current: 1_000.0,
            max: 1_000.0,
            regen: 0.0,
            rate: 0.0,
        }
    }

    /// A cold (never-blocking) heat pool for fire-gate fixtures.
    fn cold_heat() -> Heat {
        Heat {
            current: 0.0,
            max: 45.0,
            dissipation: 0.0,
        }
    }

    /// Run ONE execute tick of an Engage-behavior fighter at the origin
    /// against a static target, returning the emitted intent: the shared
    /// fixture for the T025 standoff + T026 fire-gate assertions.
    fn combat_case(
        archetype: FitArchetype,
        heading: f32,
        target_pos: Vec2,
        energy: Option<Energy>,
        heat: Option<Heat>,
    ) -> ShipIntent {
        let (mut world, mut schedule) = exec_world();
        let target = world
            .spawn((Position(target_pos), Velocity(Vec2::ZERO), Heading(0.0)))
            .id();
        let brain = AiBrain {
            behavior: Behavior::Engage,
            target: Some(target),
            archetype,
            ..AiBrain::default()
        };
        let e = world
            .spawn((ship_bundle(brain, Vec2::ZERO), fighter_stats()))
            .id();
        world.get_mut::<Heading>(e).unwrap().0 = heading;
        if let Some(energy) = energy {
            world.entity_mut(e).insert(energy);
        }
        if let Some(heat) = heat {
            world.entity_mut(e).insert(heat);
        }
        schedule.run(&mut world);
        intent_of(&world, e)
    }

    /// T027 (TR-012): the triple-veto ram utility — positive ONLY for a
    /// near-dead, much-weaker target being closed on fast; bit-identical
    /// across calls (strict f32).
    #[test]
    fn ram_utility_scores_finisher_and_vetoes_bad_rams() {
        let t = AiTuning::default(); // thresh 0.25 / margin 2.0 / closing 0.5.
                                     // Near-dead (0.1) + much weaker (mass 4 vs 2 → ratio (4/2)² = 4 ≥ 2)
                                     // + good closing (60 of top 80 ≥ 40) → a positive finisher score.
        let go = ram_utility(0.1, 60.0, 80.0, 4.0, 2.0, &t);
        assert!(go > 0.0, "advantageous ram scores positive (got {go})");
        assert_eq!(
            go.to_bits(),
            ram_utility(0.1, 60.0, 80.0, 4.0, 2.0, &t).to_bits(),
            "strict-f32: bit-identical across calls"
        );
        // Healthy stronger target → 0 (the OBJ4-VC2 no-ram side).
        assert_eq!(ram_utility(1.0, 60.0, 80.0, 2.0, 4.0, &t), 0.0);
        // Healthy WEAK target: hull veto alone still blocks.
        assert_eq!(ram_utility(0.9, 60.0, 80.0, 4.0, 2.0, &t), 0.0);
        // Near-dead but STRONGER target: the self-margin veto blocks.
        assert_eq!(ram_utility(0.1, 60.0, 80.0, 2.0, 4.0, &t), 0.0);
        // Too-slow closing (20 < 0.5·80): can't ram what you can't catch.
        assert_eq!(ram_utility(0.1, 20.0, 80.0, 4.0, 2.0, &t), 0.0);
        // Degenerate kinematics (no top speed / masses) never gamble.
        assert_eq!(ram_utility(0.1, 60.0, 0.0, 4.0, 2.0, &t), 0.0);
        assert_eq!(ram_utility(0.1, 60.0, 80.0, 0.0, 2.0, &t), 0.0);
    }

    /// `hull_fraction`'s documented fallback chain: authored-cells baseline →
    /// flat health → healthy default.
    #[test]
    fn hull_fraction_fallback_chain() {
        assert_eq!(hull_fraction(None, None, None), 1.0, "no info → healthy");
        assert_eq!(hull_fraction(Some(&Health(25.0)), None, None), 0.25);
        assert_eq!(
            hull_fraction(Some(&Health(500.0)), None, None),
            1.0,
            "flat health clamps to 1"
        );
        let mut layout = fighter_layout();
        let authored = AuthoredCells(layout.cells.len() as u32);
        assert_eq!(hull_fraction(None, Some(&layout), Some(&authored)), 1.0);
        // Carve half the cells off: the fraction tracks live/authored.
        let keep = layout.cells.len() / 2;
        while layout.cells.len() > keep {
            let key = *layout.cells.keys().next().unwrap();
            layout.cells.remove(&key);
        }
        let frac = hull_fraction(None, Some(&layout), Some(&authored));
        assert!((frac - keep as f32 / authored.0 as f32).abs() < 1e-6);
        // The cell baseline OUTRANKS flat health; a zero baseline falls back.
        assert!(hull_fraction(Some(&Health(100.0)), Some(&layout), Some(&authored)) < 1.0);
        assert_eq!(
            hull_fraction(Some(&Health(100.0)), Some(&layout), Some(&AuthoredCells(0))),
            1.0
        );
    }

    /// T026: most-Primaries-wins fire-group selection, lowest index on ties,
    /// default group 0 with no list/mapping.
    #[test]
    fn primary_fire_group_picks_most_primaries_lowest_on_tie() {
        use crate::components::FireMapping;
        use crate::fitting::SlotId;
        let profile = fighter_stats().weapon.expect("seed fighter is armed");
        let weapons = ShipWeapons {
            weapons: vec![
                (SlotId(3), profile),
                (SlotId(4), profile),
                (SlotId(5), profile),
            ],
        };
        assert_eq!(primary_fire_group(None, None), 0, "no list → default group");
        assert_eq!(
            primary_fire_group(Some(&weapons), None),
            0,
            "no mapping → everything defaults to group 0 / Primary"
        );
        // Two Primaries in group 1 vs one in group 0 → group 1.
        let mut groups = WeaponGroups::default();
        let map = |group, trigger| FireMapping { group, trigger };
        groups.mapping.insert(SlotId(3), map(0, Trigger::Primary));
        groups.mapping.insert(SlotId(4), map(1, Trigger::Primary));
        groups.mapping.insert(SlotId(5), map(1, Trigger::Primary));
        assert_eq!(primary_fire_group(Some(&weapons), Some(&groups)), 1);
        // Exact tie (one Primary each in groups 0 and 1) → lowest index.
        groups.mapping.insert(SlotId(5), map(1, Trigger::Off));
        assert_eq!(primary_fire_group(Some(&weapons), Some(&groups)), 0);
        // Secondary/Off never count toward the Primary tally.
        groups.mapping.insert(SlotId(3), map(0, Trigger::Secondary));
        assert_eq!(primary_fire_group(Some(&weapons), Some(&groups)), 1);
    }

    /// T026 (TR-011): the Engage fire DECISION respects every gate — energy,
    /// heat, lead alignment, weapon range — and absent pools are ungated
    /// (mirroring `weapon_fire_system`).
    #[test]
    fn engage_fire_respects_energy_heat_alignment_and_range_gates() {
        use std::f32::consts::PI;
        let range = weapon_range(Some(&fighter_stats())).expect("armed fighter");
        let near = Vec2::new(range * 0.3, 0.0);
        // All gates open: in range, aligned dead-ahead, charged, cold → FIRE.
        let go = combat_case(
            FitArchetype::Generic,
            0.0,
            near,
            Some(full_energy()),
            Some(cold_heat()),
        );
        assert!(go.fire_primary, "gates open → the brain holds primary fire");
        assert_eq!(go.active_group, 0, "default fire group selected");
        // Depleted energy → the brain CHOOSES not to fire (TR-011).
        let dry = combat_case(
            FitArchetype::Generic,
            0.0,
            near,
            Some(Energy {
                current: 0.0,
                ..full_energy()
            }),
            Some(cold_heat()),
        );
        assert!(!dry.fire_primary, "out of energy → never fires");
        // Overheated (heat == max) → no fire.
        let hot = combat_case(
            FitArchetype::Generic,
            0.0,
            near,
            Some(full_energy()),
            Some(Heat {
                current: 45.0,
                ..cold_heat()
            }),
        );
        assert!(!hot.fire_primary, "overheated → never fires");
        // Facing away from the lead solution → alignment gate blocks.
        let away = combat_case(
            FitArchetype::Generic,
            PI,
            near,
            Some(full_energy()),
            Some(cold_heat()),
        );
        assert!(!away.fire_primary, "misaligned → no fire");
        // Outside the weapon envelope → no fire, but full pursuit burn.
        let far = combat_case(
            FitArchetype::Generic,
            0.0,
            Vec2::new(range * 2.0, 0.0),
            Some(full_energy()),
            Some(cold_heat()),
        );
        assert!(!far.fire_primary, "out of range → no fire");
        assert!(far.forward > 0.9, "closes at full burn from outside");
        // Absent pools = ungated (the headless-world mirror).
        let bare = combat_case(FitArchetype::Generic, 0.0, near, None, None);
        assert!(bare.fire_primary);
    }

    /// T025 (TR-006/TR-011 archetype tactics): at one distance the Brawler's
    /// short standoff CLOSES while the Kiter's long standoff OPENS range —
    /// opposite radial signs, opposite intents.
    #[test]
    fn brawler_closes_where_kiter_opens_range() {
        let range = weapon_range(Some(&fighter_stats())).expect("armed fighter");
        let dist = range * 0.5; // Between the 0.3·R brawler and 0.85·R kiter rings.
        assert!(
            range_band_radial(
                dist,
                standoff_distance(FitArchetype::Brawler, range),
                RANGE_BAND_FRAC
            ) > 0.0,
            "brawler radial: too far → close in"
        );
        assert!(
            range_band_radial(
                dist,
                standoff_distance(FitArchetype::Kiter, range),
                RANGE_BAND_FRAC
            ) < 0.0,
            "kiter radial: too close → open range"
        );
        let target = Vec2::new(dist, 0.0);
        let brawler = combat_case(FitArchetype::Brawler, 0.0, target, None, None);
        assert!(
            brawler.forward > 0.9,
            "brawler burns toward the target (got {})",
            brawler.forward
        );
        assert!(brawler.turn.abs() < 1e-5, "target dead ahead: no turn");
        let kiter = combat_case(FitArchetype::Kiter, 0.0, target, None, None);
        assert_eq!(
            kiter.forward, 0.0,
            "kiter inside its band never burns toward the target"
        );
        assert_eq!(kiter.turn.abs(), 1.0, "kiter turns hard to flee the ring");
    }

    /// T025: Evade breaks off at full throttle; Retreat runs home (or directly
    /// away); NEITHER ever fires — even with every fire gate wide open.
    #[test]
    fn evade_and_retreat_break_off_and_never_fire() {
        let (mut world, mut schedule) = exec_world();
        let astern = world
            .spawn((
                Position(Vec2::new(-100.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
            ))
            .id();
        let ahead = world
            .spawn((
                Position(Vec2::new(100.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
            ))
            .id();
        // Evade a threat astern: straight-ahead escape at full burn.
        let evader = world
            .spawn((
                ship_bundle(
                    AiBrain {
                        behavior: Behavior::Evade,
                        target: Some(astern),
                        ..AiBrain::default()
                    },
                    Vec2::ZERO,
                ),
                fighter_stats(),
                full_energy(),
                cold_heat(),
            ))
            .id();
        // Retreat with a home anchor while a PERFECTLY firable target sits
        // dead ahead (in range, aligned, charged, cold): still no fire.
        let retreater = world
            .spawn((
                ship_bundle(
                    AiBrain {
                        behavior: Behavior::Retreat,
                        target: Some(ahead),
                        home: Some(Vec2::new(-500.0, 0.0)),
                        ..AiBrain::default()
                    },
                    Vec2::ZERO,
                ),
                fighter_stats(),
                full_energy(),
                cold_heat(),
            ))
            .id();
        // Retreat without a home: directly away from the threat.
        let anchorless = world
            .spawn((
                ship_bundle(
                    AiBrain {
                        behavior: Behavior::Retreat,
                        target: Some(ahead),
                        ..AiBrain::default()
                    },
                    Vec2::ZERO,
                ),
                fighter_stats(),
                full_energy(),
                cold_heat(),
            ))
            .id();
        schedule.run(&mut world);

        let e = intent_of(&world, evader);
        assert!(e.forward > 0.9, "evade burns away (got {})", e.forward);
        assert!(!e.fire_primary, "Evade never fires (v1 documented rule)");
        let r = intent_of(&world, retreater);
        assert!(!r.fire_primary, "Retreat NEVER fires, gates open or not");
        assert_eq!(r.forward, 0.0, "home is astern: turn first, then burn");
        assert_eq!(r.turn.abs(), 1.0, "turning hard toward home");
        let a = intent_of(&world, anchorless);
        assert!(!a.fire_primary);
        assert_eq!(a.forward, 0.0, "away-dir is astern: turn first");
        assert_eq!(a.turn.abs(), 1.0);
    }

    /// T027: Ram execution is a full-throttle collision course with finisher
    /// fire allowed on the way in.
    #[test]
    fn ram_is_full_throttle_collision_course_with_finisher_fire() {
        let (mut world, mut schedule) = exec_world();
        let target = world
            .spawn((
                Position(Vec2::new(200.0, 0.0)),
                Velocity(Vec2::ZERO),
                Heading(0.0),
            ))
            .id();
        let rammer = world
            .spawn((
                ship_bundle(
                    AiBrain {
                        behavior: Behavior::Ram,
                        target: Some(target),
                        ..AiBrain::default()
                    },
                    Vec2::ZERO,
                ),
                fighter_stats(),
                full_energy(),
                cold_heat(),
            ))
            .id();
        schedule.run(&mut world);
        let i = intent_of(&world, rammer);
        assert!(i.forward > 0.9, "full-throttle collision course");
        assert!(
            i.fire_primary,
            "fire stays allowed on the way in (finisher)"
        );
    }

    /// T025/T027 think-side: a live target yields an Engage selection (task
    /// bucket), and a near-dead much-weaker target being closed on fast
    /// escalates to Ram (survival bucket beats Engage by bucket dominance) —
    /// while a healthy target NEVER does (the OBJ4-VC2 decision pair).
    #[test]
    fn think_with_target_selects_engage_and_escalates_to_ram_when_advantageous() {
        let (mut world, mut schedule) = think_world();
        // A heavy fast attacker: top speed 80, mass pinned at 8 (the flat
        // SHIP_MASS-2 targets below are "much weaker": ratio (8/2)² = 16 ≥ 2).
        let mut stats = stats_with(80.0, 30.0, 0.0);
        stats.total_mass = 8.0;
        let near_dead = world
            .spawn((
                Position(Vec2::new(100.0, 0.0)),
                Velocity(Vec2::ZERO),
                Health(10.0),
            ))
            .id();
        let healthy = world
            .spawn((
                Position(Vec2::new(100.0, 0.0)),
                Velocity(Vec2::ZERO),
                Health(100.0),
            ))
            .id();
        let spawn_attacker = |world: &mut World, id: u64, target| {
            world
                .spawn((
                    AiStableId(id),
                    AiBrain {
                        target: Some(target),
                        ..active_brain(0)
                    },
                    Position(Vec2::ZERO),
                    Velocity(Vec2::new(80.0, 0.0)), // Closing 80 ≥ 0.5·80.
                    stats,
                ))
                .id()
        };
        let finisher = spawn_attacker(&mut world, 0, near_dead);
        let fighter = spawn_attacker(&mut world, 1, healthy);

        step(&mut world, &mut schedule, 0); // Bucket 0: everyone is due.

        assert_eq!(
            brain_of(&world, finisher).behavior,
            Behavior::Ram,
            "near-dead weak target + good closing → Ram wins the survival bucket"
        );
        assert_eq!(
            brain_of(&world, fighter).behavior,
            Behavior::Engage,
            "healthy target: Engage, never a self-destructive ram"
        );
    }

    // --- T014: feature-gated capture seam ------------------------------------

    /// TR-020/AD-006: under `ai_debug` a completed think populates the capture
    /// — final scores (momentum included on the incumbent), the winner, and
    /// the `(tick, from, to)` transition ring.
    #[cfg(feature = "ai_debug")]
    #[test]
    fn think_populates_debug_capture_scores_and_transition() {
        let (mut world, mut schedule) = think_world();
        let e = world
            .spawn((
                AiStableId(0),
                AiBrain {
                    waypoint: Some(Vec2::new(10.0, 0.0)),
                    ..active_brain(0)
                },
            ))
            .id();

        step(&mut world, &mut schedule, 0); // First think: Hold → Waypoint.

        let cap = world
            .get::<debug_capture::AiDebugCapture>(e)
            .expect("first captured think inserts the component via Commands");
        assert_eq!(cap.winner, Behavior::Waypoint);
        assert!(
            cap.last_scores
                .iter()
                .any(|&(b, s)| b == Behavior::Waypoint && s > 0.0),
            "candidate scores captured (got {:?})",
            cap.last_scores
        );
        let t = AiTuning::default();
        let hold_raw = score_behavior(&[HOLD_BASELINE], t.compensation_k);
        let hold_final = cap
            .last_scores
            .iter()
            .find(|&&(b, _)| b == Behavior::Hold)
            .expect("incumbent Hold among the captured candidates")
            .1;
        assert_eq!(
            hold_final.to_bits(),
            (hold_raw * (1.0 + t.momentum_bonus)).to_bits(),
            "incumbent's captured score includes the momentum multiplier"
        );
        assert_eq!(cap.momentum_applied, t.momentum_bonus);
        assert_eq!(
            cap.transitions.iter().copied().collect::<Vec<_>>(),
            vec![(0, Behavior::Hold, Behavior::Waypoint)],
            "the Hold → Waypoint transition is recorded with its tick"
        );
    }
}
