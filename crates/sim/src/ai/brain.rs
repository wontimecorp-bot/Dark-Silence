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

use crate::ai::command::PlayerOrder;
use crate::ai::control::{
    allocate_intent, deflect_v_des, stoppable_speed, ControlStats, Facing, MoveCmd,
};
use crate::ai::ident::{phase_bucket, AiStableId};
use crate::ai::lod::{AoiTier, Tier};
use crate::ai::perception::{nearest_contact, ContactList};
use crate::ai::role::{role_apply, Posture, RoleGoal, ScenarioRole};
use crate::ai::steering::{formation_desired_vel, pursue_intercept, range_band_radial, ContextMap};
use crate::ai::tuning::AiTuning;
use crate::broadphase::ObstacleField;
use crate::clock::CurrentTick;
use crate::collision::{RAM_CARVE_K, SHIP_MASS};
use crate::components::{
    AuthoredCells, CollisionRadius, Energy, Heading, Health, Heat, MeshAnchor, Position, Trigger,
    Velocity, WeaponGroups,
};
use crate::fitting::{FitLayout, HullCatalog, ShipStats, ShipWeapons, CELL_WORLD_SIZE};
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

/// Combat steering STYLE (R96 Part C): how a ship paces and shapes its motion
/// while in the [`Engage`](Behavior::Engage) task. Cached on
/// [`AiBrain::combat_stance`]; the EXECUTE-time [`engage_motion`] reads it and
/// composes a per-stance context map over the SAME range-band / standoff /
/// lead-pursuit primitives.
///
/// **Charge is the parity default** (the determinism keystone): a
/// [`Charge`](CombatStance::Charge) ship flies the EXACT pre-R96-C
/// `engage_motion` body — the range-band radial controller that closes toward
/// the gunnery lead when outside the standoff ring, opens (danger-masked)
/// inside it, and holds facing the lead on-ring — with NO new math, so
/// `AiBrain::default()` and every existing combat fixture stay byte-for-byte
/// identical. Only the other three stances add new interest/danger terms.
///
/// **Perp sign convention** (load-bearing for [`Orbit`](CombatStance::Orbit)):
/// `perp(v) = Vec2::new(-v.y, v.x)` is the +90° (CCW) rotation of `v` in the
/// heading convention `0 = +X`, increasing CCW (matching `steering.rs` /
/// `flight.rs`). The orbit tangent is `±perp` of the OUTWARD radial (target →
/// ship, i.e. `-dir_to_target`): a CCW orbit (bearing increasing) moves along
/// `perp(outward) = perp(-dir_to) = -perp(dir_to)`. So `ccw = true` uses
/// tangent `= -perp(dir_to)` and `intent.strafe = +strafe_stance_lateral`
/// (strafe LEFT/port — the inside of a left-hand turn); `ccw = false` (CW) uses
/// `+perp(dir_to)` and `intent.strafe = -...` (strafe RIGHT/starboard).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CombatStance {
    /// PARITY default: the pre-R96-C range-band controller — close toward the
    /// gunnery lead outside the standoff ring, open (danger-masked) inside it,
    /// hold facing the lead on-ring. Byte-identical to the legacy `engage_motion`.
    #[default]
    Charge,
    /// Bank/strafe AROUND the target at the standoff ring (`orbit_radius_frac ×
    /// standoff`): a radial correction onto the ring PLUS a tangential interest
    /// (signed by `ccw`) that DOMINATES on-ring, so forward-only fighters orbit
    /// by turning and strafe-capable hulls sidle. See the perp-sign convention.
    Orbit {
        /// `true` = counter-clockwise (tangent `+perp`, strafe left); `false` =
        /// clockwise (tangent `-perp`, strafe right).
        ccw: bool,
    },
    /// HOLD the ring: face the gunnery lead at zero throttle within the band,
    /// closing only when well outside — the explicit "hold position, guns on
    /// target" stance (Charge's on-ring branch, applied throughout the band).
    Standoff,
    /// KITE: keep the target at arm's length (`kite_range_frac × weapon_range`)
    /// — full interest AWAY plus the closing-vector danger when the target is
    /// inside that range, face-and-hold (gun bears) at or beyond it.
    Kite,
}

/// Movement pacing profile (R96 Part A): how aggressively a ship paces and
/// brakes onto its nav goals. Cached on [`AiBrain::movement_profile`].
///
/// **R101 S3 — the controller pace:** the nav path (`ai_execute_system`'s
/// `nav_intent`) now reads the profile's `(v_max, tau)` from
/// [`AiTuning::control_params`](crate::ai::tuning::AiTuning::control_params) and
/// drives the unified controller ([`allocate_intent`](crate::ai::control::allocate_intent)):
/// `v_max` caps the desired closing speed, `tau` sets the velocity-tracking time
/// constant. Active braking / flip-and-burn EMERGE from the controller — the
/// R96–R100 `(cap, brake_aggression, slow_factor)` brake triple is no longer on
/// the nav path (it lives on for combat/survival until S5/S6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MovementProfile {
    /// Hot pace: the highest `v_max` and shortest tracking `tau` — closes fast
    /// and parks tight (no overshoot, by the controller's stoppable-speed cap).
    Rush,
    /// Medium pace: a moderate `v_max`/`tau`. The default profile.
    #[default]
    Cruise,
    /// Lazy pace: the lowest `v_max` and longest `tau` — an unhurried,
    /// energy-saving approach that settles gently.
    Leisurely,
}

/// R96 precedence BASE — the archetype's default [`MovementProfile`] (the
/// lowest-priority link in the resolved chain `squad ← role ← archetype`).
/// Pure, deterministic match — no RNG, no allocation, no state. The table is a
/// documented v1 tuning choice (overridable per-role / per-squad; tunable here
/// later):
///
/// | archetype | profile  | rationale                                     |
/// |-----------|----------|-----------------------------------------------|
/// | `Brawler` | `Rush`   | wade in and slug — arrive on-station, settled |
/// | `Rammer`  | `Rush`   | the hull IS the weapon — close hard, no coast |
/// | `Kiter`   | `Cruise` | hit-and-run — the legacy coast (kite STYLE is the stance) |
/// | `Orbiter` | `Cruise` | circle the envelope at the parity coast       |
/// | `Support` | `Cruise` | screen/utility runner — the parity coast      |
/// | `Generic` | `Cruise` | the parity default (byte-identical baseline)  |
///
/// **Determinism keystone**: every archetype except the two `Rush` brawlers/
/// rammers maps to [`Cruise`](MovementProfile::Cruise) — the pre-R96 drag-braked
/// coast — so a `Generic`/`Kiter`/`Orbiter`/`Support` ship with no role/squad
/// style resolves to `Cruise` and emits today's intents exactly. Only the
/// hard-charging brawler/rammer change pace; combat ships in golden worlds carry
/// no `AiBrain`, so nothing observable shifts there.
pub fn default_movement_profile(a: FitArchetype) -> MovementProfile {
    match a {
        FitArchetype::Brawler | FitArchetype::Rammer => MovementProfile::Rush,
        FitArchetype::Kiter
        | FitArchetype::Orbiter
        | FitArchetype::Support
        | FitArchetype::Generic => MovementProfile::Cruise,
    }
}

/// R96 precedence BASE — the archetype's default [`CombatStance`] (the
/// lowest-priority link in the resolved chain `squad ← role ← archetype`).
/// Pure, deterministic match. The table is a documented v1 tuning choice
/// (overridable per-role / per-squad):
///
/// | archetype | stance       | rationale                                    |
/// |-----------|--------------|----------------------------------------------|
/// | `Brawler` | `Charge`     | close to the short standoff and slug         |
/// | `Rammer`  | `Charge`     | bear straight in (the ram arm finishes)      |
/// | `Kiter`   | `Kite`       | keep the target at arm's length, open inside |
/// | `Orbiter` | `Orbit{ccw}` | circle the weapon envelope                   |
/// | `Support` | `Standoff`   | hold the ring, guns on target (no chase)     |
/// | `Generic` | `Charge`     | the parity default — the legacy range-band controller |
///
/// **Parity keystone**: `Generic` → [`Charge`](CombatStance::Charge) (the
/// verbatim legacy `engage_motion`), so a no-role/no-squad `Generic` ship is
/// byte-identical to pre-R96-C. The other archetypes' stances formalize tactics
/// the range-band controller already approximated (a `Kiter`'s long standoff
/// ring IS a kite, a `Brawler`'s short ring IS a charge), so the
/// `archetype_range_bands_differ` fixture (brawler-vs-kiter band occupancy)
/// holds: `Charge` still charges the brawler to its short standoff, and `Kite`
/// holds the kiter just past the envelope edge (`kite_range_frac · range`),
/// which sits inside the ±40% tolerance of the kiter's `0.85 · range` standoff
/// ring — both satisfy their range-band assertions.
pub fn default_combat_stance(a: FitArchetype) -> CombatStance {
    match a {
        FitArchetype::Brawler | FitArchetype::Rammer | FitArchetype::Generic => {
            CombatStance::Charge
        }
        FitArchetype::Kiter => CombatStance::Kite,
        FitArchetype::Orbiter => CombatStance::Orbit { ccw: true },
        FitArchetype::Support => CombatStance::Standoff,
    }
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
    /// Movement pacing profile (R96 Part A) — see [`MovementProfile`]. Default
    /// [`Cruise`](MovementProfile::Cruise) keeps `AiBrain::default()` (and every
    /// constructor that spreads `..default()`) byte-identical to pre-R96: only
    /// `Rush`/`Leisurely` route through the active-braking path.
    pub movement_profile: MovementProfile,
    /// Combat steering style (R96 Part C) — see [`CombatStance`]. Default
    /// [`Charge`](CombatStance::Charge) keeps `AiBrain::default()` (and every
    /// constructor that spreads `..default()`) byte-identical to pre-R96-C: the
    /// `Engage` arm flies the legacy range-band controller; only
    /// `Orbit`/`Standoff`/`Kite` add new context-map terms.
    ///
    /// RESOLVED FIELD (R96 precedence chain): `ai_think_system` overwrites this
    /// every think from `squad_stance.or(role stance).unwrap_or(archetype
    /// default)` — never read it as authoritative input, set
    /// [`AiBrain::squad_stance`] / the role's `combat_stance` to influence it.
    pub combat_stance: CombatStance,
    /// R96 precedence CHANNEL (squad → brain): a squad whose
    /// [`Squad::movement_profile`](crate::ai::squad::Squad::movement_profile)
    /// is `Some(...)` writes it here (via `apply_assignment`) onto each NON-roled
    /// member; `ai_think_system` then reads it as the HIGHEST-priority override
    /// in the resolved [`AiBrain::movement_profile`] chain (squad ← role ←
    /// archetype default). `None` = the squad imposes no pace style (the default;
    /// roled members are squad-exempt so this stays `None` for them and the role
    /// override wins). Stored locally by its writer so the precedence is a single
    /// `.or().unwrap_or()` in the think.
    pub squad_profile: Option<MovementProfile>,
    /// R96 precedence CHANNEL (squad → brain): the [`CombatStance`] twin of
    /// [`AiBrain::squad_profile`] — a squad's
    /// [`Squad::combat_stance`](crate::ai::squad::Squad::combat_stance) written
    /// onto each non-roled member, read as the highest-priority override in the
    /// resolved [`AiBrain::combat_stance`] chain. `None` = no squad stance override.
    pub squad_stance: Option<CombatStance>,
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
    /// R97 Phase 1 Stage A — the tick this brain last TOOK damage, stamped by
    /// the gated damage-recency producer in `fitted_damage_system` /
    /// `collision_detect_system` when a projectile (or ram) carves a target that
    /// carries this component. `0` = never hit (the default; also the
    /// `CurrentTick`-absent fallback). Read by survival-pressure considerations
    /// (Stage B/C) as a recency window (`now − last_damaged_tick < window`).
    /// Determinism-safe: golden worlds spawn no `AiBrain`, so this stays `0`
    /// there and the producer branch is never taken.
    pub last_damaged_tick: u64,
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
            movement_profile: MovementProfile::Cruise,
            combat_stance: CombatStance::Charge,
            squad_profile: None,
            squad_stance: None,
            think_tier: Tier::Dormant,
            phase_bucket: 0,
            throttle_cap: 1.0,
            thinks_total: 0,
            last_damaged_tick: 0,
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

// ---------------------------------------------------------------------------
// R97 Phase 1 Stage A — consideration library (TR-004, strict f32)
// ---------------------------------------------------------------------------
//
// A small, documented library of single-input consideration curves that later
// stages (B/C/D) combine through `score_behavior`. Each takes a NORMALIZED
// `[0, 1]` input (the GEOMETRY that produces it — sqrt/normalize/length — lives
// OUTSIDE these markers in the physics-scalar prep helpers below, exactly like
// `ram_utility`'s "geometry outside, scalars in" pattern) and returns a
// consideration in `[0, 1]`. NOTHING calls these yet — they are the primitives
// Stage B/C/D consume. Strict f32: `+ - * /`, `min`/`max`/`clamp`, comparisons.

/// Self-health consideration: rises with remaining hull fraction `frac` — a
/// healthy ship is MORE willing to commit to a task (linear pass-through). The
/// "I'm fine, press the attack" curve.
pub fn con_health(frac: f32) -> f32 {
    curve_linear(frac)
}

/// Low-health consideration: rises as the ship gets HURT — `curve_quadratic` of
/// the inverted fraction, so it stays near zero while healthy and ramps up
/// sharply as hull bleeds away (the survival-pressure / "break off" curve).
pub fn con_health_low(frac: f32) -> f32 {
    curve_quadratic(curve_inv(frac))
}

/// Energy consideration — **FLOORED** (the parity-plan keystone): a smoothstep
/// of the energy fraction lifted onto `[0.3, 1.0]` (`0.3 + 0.7·curve_smooth`),
/// so a low-energy ship still scores a non-zero willingness (it does not VETO a
/// behavior — the fire/throttle gates downstream decide what it can actually
/// do) while a charged ship scores full. Floored so a gated ship still ranks
/// candidates instead of zeroing them all.
pub fn con_energy(frac: f32) -> f32 {
    0.3 + 0.7 * curve_smooth(frac)
}

/// Heat-headroom consideration — **FLOORED** inverse: `curve_inv` of the heat
/// fraction (high heat → low score) but never a hard veto on its own; a cool
/// ship scores ~1, an overheating one scores low-but-positive so heat shades a
/// preference rather than nulling every option (parity with `con_energy`).
pub fn con_heat_ok(frac: f32) -> f32 {
    curve_inv(frac)
}

/// In-band consideration: a tent peaked at `range_norm == 1` (the ship is
/// exactly on its standoff ring) falling to 0 a full band-width to either side
/// — `1 − |range_norm − 1|` clamped to `[0, 1]`. `range_norm` is the caller's
/// `distance / standoff` (the geometry is a single divide, done outside). High
/// when at the preferred range, low when too close OR too far.
pub fn con_in_band(range_norm: f32) -> f32 {
    (1.0 - (range_norm - 1.0).abs()).clamp(0.0, 1.0)
}

/// Target-vulnerability consideration: rises as the target's hull `frac` falls
/// (`curve_inv`) — a near-dead target is a more attractive finisher. The combat
/// twin of `con_health_low`, read on the TARGET instead of self.
pub fn con_target_vulnerable(frac: f32) -> f32 {
    curve_inv(frac)
}

/// Incoming-threat consideration: an S-curve (`curve_smooth`) of a normalized
/// threat scalar `[0, 1]` (recency × proximity × firepower, resolved outside),
/// so a small threat is shrugged off and a building one ramps in smoothly — the
/// pressure term that lifts `Evade`/`Retreat` as danger mounts.
pub fn con_incoming_threat(threat_norm: f32) -> f32 {
    curve_smooth(threat_norm)
}

/// Collision-imminence consideration: high when the normalized time-to-collision
/// `ttc_norm` is SMALL (impact is near). `curve_quadratic(curve_inv(ttc_norm))`
/// — `ttc_norm = 1` (a full horizon away) → 0, `ttc_norm = 0` (about to hit) →
/// 1, ramping quadratically so only an imminent collision dominates. `ttc_norm`
/// is `time_to_collision / horizon` clamped, computed outside via
/// [`time_to_closest_approach`] / [`closest_approach_dist`].
pub fn con_collision_imminence(ttc_norm: f32) -> f32 {
    curve_quadratic(curve_inv(ttc_norm))
}

/// Weapon-ready consideration: the PRODUCT of three already-normalized gates —
/// `in_arc` (the gunnery lead is within the firing arc, `[0, 1]`), `e_ok`
/// (energy headroom, e.g. [`con_energy`]) and `h_ok` (heat headroom, e.g.
/// [`con_heat_ok`]). A product so any one near-zero gate drags the readiness
/// down (you can't usefully fire mis-aimed, drained, or overheated), without a
/// hard veto when the floored energy/heat curves keep it positive.
pub fn con_weapon_ready(in_arc: f32, e_ok: f32, h_ok: f32) -> f32 {
    in_arc.clamp(0.0, 1.0) * e_ok.clamp(0.0, 1.0) * h_ok.clamp(0.0, 1.0)
}

/// Momentum-alignment consideration: an S-curve (`curve_smooth`) of a normalized
/// alignment scalar `[0, 1]` (how well the ship's velocity already points where
/// it wants to go — `(1 + v̂·d̂)/2` resolved outside). High when momentum is
/// already carrying the ship toward its goal (cheap, reachable), low when it
/// would have to fight its own inertia — the "is this maneuver in tune with my
/// drift?" term.
pub fn con_momentum_align(align_norm: f32) -> f32 {
    curve_smooth(align_norm)
}

// STRICT-F32 SCORING END (TR-004)

// ---------------------------------------------------------------------------
// R97 Phase 1 Stage A — physics-scalar PREP helpers (OUTSIDE the markers)
// ---------------------------------------------------------------------------
//
// The geometry that FEEDS the consideration library: closed-form, O(1)
// kinematics that use sqrt/length/normalize (which are BANNED inside the
// strict-f32 markers), then hand normalized scalars to the considerations —
// exactly the pattern `ram_utility`'s caller uses. These are computed at THINK
// time for the FEW relevant entities only (the current target, the nearest
// obstacle), never per-tick over the whole world, so the cost is negligible.

/// Time (s) to the closest approach of two bodies with relative position `r`
/// (other − self) and relative velocity `v` (other − self), or `f32::INFINITY`
/// when they are not converging.
///
/// **Model**: the squared separation `|r + v·t|²` is minimized at
/// `t* = −(r·v)/(v·v)`. A non-positive `t*` (or a vanishing relative velocity)
/// means the bodies are diverging / parallel / coincident-velocity — there is
/// no future closest approach to react to, so this returns `INFINITY` (a clean
/// "no collision pressure" sentinel the imminence curve maps to 0). O(1), no
/// transcendentals beyond the dot products; never NaN (the `v·v` denominator is
/// guarded by the `eps` check).
pub fn time_to_closest_approach(r: Vec2, v: Vec2) -> f32 {
    let vv = v.length_squared();
    if vv < f32::EPSILON {
        return f32::INFINITY; // No relative motion → never closes.
    }
    let t = -(r.dot(v)) / vv;
    if t <= 0.0 {
        f32::INFINITY // Diverging (or already at closest approach) → no pressure.
    } else {
        t
    }
}

/// The closest-approach DISTANCE (world units) of two bodies with relative
/// position `r` and relative velocity `v` — the separation at
/// [`time_to_closest_approach`] (clamped at `t = 0` so a diverging pair reports
/// its CURRENT separation, never a fictitious past one). The miss distance an
/// avoidance check compares against the combined radii. O(1); the single
/// `length()` is why this lives OUTSIDE the strict-f32 markers.
pub fn closest_approach_dist(r: Vec2, v: Vec2) -> f32 {
    let tca = time_to_closest_approach(r, v);
    // A finite tca is already > 0; INFINITY (diverging) clamps to the present.
    let t = if tca.is_finite() { tca } else { 0.0 };
    (r + v * t).length()
}

/// Kinematic stopping distance (world units) for a body moving at `speed` under
/// a constant deceleration `decel`: `speed² / (2·decel)`. The denominator is
/// floored (`decel.max(eps)`) so a zero/negative decel never divides by zero
/// (no NaN, no panic) — it yields `+∞`, the well-defined "can never stop"
/// answer a brake check naturally treats as "too far to ever halt". Finite for
/// any positive decel and monotonically increasing in `speed` — the "how far
/// before I can stop" scalar an active-brake / collision-preempt check compares
/// against the range to a body. O(1), no transcendentals.
pub fn stopping_distance(speed: f32, decel: f32) -> f32 {
    speed * speed / (2.0 * decel.max(f32::MIN_POSITIVE))
}

/// R97 Phase 1 Stage D — the DOMINANT collision-imminence scalar over the
/// in-range obstacles: the [`con_collision_imminence`] consideration evaluated
/// for the MOST imminent obstacle the ship is closing on (`[0, 1]`, `0` = no
/// imminent collision). The two-layer split's decision half: a high value
/// scales the obstacle danger weight so an IMMINENT crash DOMINATES the MOVE map
/// (the ship breaks off even an attack run to avoid it), while a distant or
/// non-closing obstacle yields `0` and the gentle R96 reactive weight stands.
///
/// **TTC units (documented, load-bearing)**: the flight model integrates
/// `pos += vel·dt` with `dt` in SECONDS (`FixedDt`, [`crate::clock::FixedDt`]),
/// so `Velocity` is world-units-PER-SECOND. The relative velocity an obstacle
/// approaches the ship at is therefore `-ship_vel` in u/s (the obstacle is
/// stationary in the [`ObstacleField`]; it carries no velocity), and
/// [`time_to_closest_approach`] `(obs_pos − ship_pos, −ship_vel)` returns the
/// time-to-closest-approach DIRECTLY in SECONDS — no tick conversion. It is
/// normalized by [`AiTuning::collision_horizon_s`] (also seconds): `ttc_norm =
/// (ttc / horizon).clamp(0, 1)`, so a collision beyond the horizon scores ~0
/// imminence and one inside it ramps quadratically toward 1.
///
/// **Imminent test**: an obstacle contributes only when it is BOTH closing soon
/// (finite ttc within the horizon) AND on a near-miss course — the
/// [`closest_approach_dist`] is inside `obs_radius + own_radius + clearance_pad`
/// (the same avoid ring `control::deflect_v_des` writes danger for). A glancing
/// pass that clears the ring, or a diverging/parallel obstacle (`ttc = ∞`),
/// contributes 0. Returns the MAXIMUM imminence over all qualifying in-range
/// obstacles — the single most-urgent crash dominates the response. O(field);
/// the field is a handful of bodies, scanned at think/execute time only.
///
/// Pure + deterministic: a linear `max`-fold over the position-sorted field
/// (order-independent — `max` is associative/commutative), the geometry
/// (`length`/dot) OUTSIDE the strict-f32 markers, then the strict-f32
/// `con_collision_imminence` curve. No RNG, no HashMap, no transcendentals.
pub fn obstacle_imminence(
    field: &ObstacleField,
    ship_pos: Vec2,
    ship_vel: Vec2,
    own_radius: f32,
    tuning: &AiTuning,
) -> f32 {
    // Relative velocity of every (stationary) obstacle toward the ship.
    let rel_vel = -ship_vel;
    let horizon = tuning.collision_horizon_s.max(f32::MIN_POSITIVE);
    let mut imm = 0.0_f32;
    for &(obs_pos, obs_radius) in &field.obstacles {
        let rel_pos = obs_pos - ship_pos;
        // Only obstacles inside the query scope contribute (cheap linear gate,
        // mirroring `control::deflect_v_des`'s in-range test).
        if rel_pos.length() > tuning.obstacle_query_radius {
            continue;
        }
        let ttc = time_to_closest_approach(rel_pos, rel_vel);
        if !ttc.is_finite() {
            continue; // Diverging / parallel / not closing → no imminence.
        }
        // Near-miss test: the closest approach must breach the avoid ring, else
        // a glancing pass that clears the body contributes nothing.
        let miss = closest_approach_dist(rel_pos, rel_vel);
        let avoid_radius = obs_radius + own_radius + tuning.obstacle_clearance_pad;
        if miss >= avoid_radius {
            continue;
        }
        // Normalize TTC by the (seconds) horizon and take the consideration.
        let ttc_norm = (ttc / horizon).clamp(0.0, 1.0);
        imm = imm.max(con_collision_imminence(ttc_norm));
    }
    imm
}

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

/// R97 Phase 1 Stage D (ai_debug only) — the diagnostic twin of
/// [`fire_decision`]: it walks the SAME gate ladder in the SAME order but
/// returns WHICH gate decided, so the dev panel can explain a non-firing ship
/// ("not aligned", "out of energy", …). Compiled only under the feature, so the
/// non-debug fire path is untouched; mirrors `fire_decision` line-for-line to
/// stay in sync (a divergence would mislabel, never misfire — this never gates).
#[cfg(feature = "ai_debug")]
#[allow(clippy::too_many_arguments)] // Mirrors `fire_decision` 1:1.
fn fire_reason(
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
) -> debug_capture::FireReason {
    use debug_capture::FireReason;
    let Some(stats) = stats else {
        return FireReason::Unarmed;
    };
    let Some(profile) = stats.weapon else {
        return FireReason::Unarmed;
    };
    if !stats.can_fire {
        return FireReason::Unarmed;
    }
    let range = profile.muzzle_speed * profile.lifetime;
    if range <= 0.0 || (target_pos - pos).length() > range {
        return FireReason::OutOfRange;
    }
    let aim_dir = pursue_intercept(pos, profile.muzzle_speed, target_pos, target_vel);
    if Vec2::from_angle(heading).dot(aim_dir) <= FIRE_ALIGN_COS {
        return FireReason::NotAligned;
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
            match cost {
                Some(c) => c,
                None => return FireReason::Unarmed, // No Primary weapon in the group.
            }
        }
        _ => profile.damage * sim.weapon_energy_per_damage,
    };
    if !energy.is_none_or(|e| e.current >= min_cost) {
        return FireReason::NoEnergy;
    }
    if !heat.is_none_or(|h| h.current < h.max) {
        return FireReason::Overheated;
    }
    FireReason::Fired
}

/// +90° (counter-clockwise) rotation of `v` in the `0 = +X`, CCW-increasing
/// heading convention (matching `steering.rs`/`flight.rs`): `perp(v) =
/// (-v.y, v.x)`. The [`CombatStance::Orbit`] tangent rotates the OUTWARD radial
/// (`-dir_to_target`) by ±90° — a CCW orbit moves along `perp(outward) =
/// -perp(dir_to)`, a CW orbit along `+perp(dir_to)` (see [`CombatStance`]'s sign
/// convention). Execute-time geometry, OUTSIDE the strict-f32 scoring markers.
fn perp(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

/// R97 Phase 1 Stage B — the combat AIM channel: the gunnery-lead facing the
/// fixed-forward gun wants so it bears on the target. Muzzle-speed
/// [`pursue_intercept`] lead for an armed ship; the plain direction-to-target
/// for an unarmed one (matching the on-ring "face the target" fallback). This is
/// the SAME value [`engage_channels`] resolves as its `aim_dir` — extracted so
/// the channel core and the live `Engage` arm (which composes MOVE via
/// [`engage_motion`] and records AIM via this) never diverge. Stage B records
/// it; Stage C composes the turn channel from it. Returns `Vec2::ZERO` only for
/// a coincident target (no sensible facing).
fn engage_aim_dir(
    pos: Vec2,
    stats: Option<&ShipStats>,
    target_pos: Vec2,
    target_vel: Vec2,
) -> Vec2 {
    let dir_to = (target_pos - pos).normalize_or_zero();
    if dir_to == Vec2::ZERO {
        return Vec2::ZERO; // Coincident: no facing.
    }
    match stats.and_then(|s| s.weapon) {
        Some(w) => pursue_intercept(pos, w.muzzle_speed, target_pos, target_vel),
        None => dir_to,
    }
}

/// R101 S5 — the WORLD-space live-cell **CENTROID** of a target's [`FitLayout`]:
/// the mean of its remaining cell coords, mapped to world. This is the BULK the
/// gun should bore — instead of the bare `Position` (= the grid CENTRE, which an
/// empty fighter fit's off-centre silhouette misses). The caller projects this
/// onto the line-of-sight to build the KILL aim point ([`combat_aim_pos`]).
///
/// The cell→world transform is the SAME machinery [`sever_chunk`](crate::damage::sever::sever_chunk)
/// uses (single-sourced so the aim point and the carve geometry can never drift):
/// the cell-space offset `r_local = centroid − grid_centre` (centroid over
/// `cell_center = (col+0.5, row+0.5)`) is mapped into the ship's LOCAL WORLD frame
/// the render way (forward `+X` ← row, lateral `+Y` ← col, × [`CELL_WORLD_SIZE`]),
/// rotated into world by the target's `heading`, and offset from `target_pos`.
///
/// `grid_centre` is the cell-space point whose world location IS `target_pos` —
/// the live ship's grid CENTRE `(cols·0.5, rows·0.5)`, or a frozen
/// [`MeshAnchor`](crate::components::MeshAnchor) for a wreck — resolved by the
/// caller exactly as `sever_chunk` resolves it. With NO cells (empty layout — the
/// ship is already gone) this returns `target_pos` (nothing left to aim at).
///
/// Execute-time geometry, OUTSIDE the strict-f32 scoring markers (it uses
/// `Vec2::from_angle`/`rotate`); deterministic — the centroid is a sorted
/// `BTreeMap`-keys fold, no RNG/HashMap.
fn target_centroid_world(
    layout: &FitLayout,
    heading: f32,
    target_pos: Vec2,
    grid_centre: Vec2,
) -> Vec2 {
    let n = layout.cells.len();
    if n == 0 {
        return target_pos; // No cells left → nothing to aim at; hold the centre.
    }
    // Live-cell centroid in cell space (mean of the unit-cell mid-points). Sorted
    // BTreeMap iteration → deterministic regardless of carve order.
    let sum = layout.cells.keys().fold(Vec2::ZERO, |acc, &(col, row)| {
        acc + Vec2::new(col as f32 + 0.5, row as f32 + 0.5)
    });
    let centroid = sum / n as f32;
    // Cell-space offset `(Δcol, Δrow)` of the centroid from the render/carve anchor.
    let r_local = centroid - grid_centre;
    // Map into the ship's LOCAL WORLD frame the render way (forward +X ← row,
    // lateral +Y ← col, × CELL_WORLD_SIZE) — i.e. `X = Δrow, Y = Δcol`.
    let r_local_world = Vec2::new(r_local.y, r_local.x) * CELL_WORLD_SIZE;
    // Rotate into world by the target heading and offset from its Position.
    target_pos + Vec2::from_angle(heading).rotate(r_local_world)
}

/// R101 S5 — the WORLD-space KILL aim point a combat ship's gun bores for: the
/// target's live-cell [`target_centroid_world`] **projected to the body's RANGE**
/// (its along–line-of-sight depth held to the body centre) plus a small RAKE
/// JINK across the hull, perpendicular to the line of sight.
///
/// **Why project + jink (the stationary-target fix, the half that actually
/// kills)**: aiming the gun at the bare centroid is not enough on its own. (a) A
/// forward-only hull pins its nose to the aim, so its forward thrust chases the
/// aim — and as the near cells carve the centroid recedes ALONG the shot line, so
/// the ship would creep THROUGH the target (the runaway). Projecting the aim back
/// to the body's depth removes that along-LoS pull: the nose holds the body range
/// while still centred on the bulk's LATERAL offset, so the ring brake parks the
/// ship cleanly. (b) A gun bored at one fixed point carves ONE narrow tunnel then
/// the shots pass down the empty channel — a single-tunnel STALL that never
/// disconnects an off-centre core. The RAKE JINK sweeps the bore ±`combat_rake_frac
/// · hull_half_width` across the hull (a deterministic triangle wave in the target
/// tick `now`), so the gun carves fresh cells across the full WIDTH until the ship
/// is hollowed and its core disconnects. The jink is a few-degrees nose sweep at
/// standoff range, far too small to perturb the ring-hold.
///
/// `shooter_pos`/`body_pos` define the line of sight; `centroid` is
/// [`target_centroid_world`]; `now` is the current tick (the deterministic jink
/// phase); `rake_amp` is the jink half-amplitude in world units (the caller sizes
/// it from the hull footprint × `combat_rake_frac`). Pure geometry, OUTSIDE the
/// strict-f32 markers; deterministic (the triangle wave is integer-tick `% / -`,
/// no RNG/trig table).
fn combat_aim_point(
    shooter_pos: Vec2,
    body_pos: Vec2,
    centroid: Vec2,
    now: u64,
    rake_amp: f32,
) -> Vec2 {
    let los = body_pos - shooter_pos;
    let dir = los.normalize_or_zero();
    if dir == Vec2::ZERO {
        return centroid; // Coincident → no LoS; aim at the bulk.
    }
    // Perpendicular to the line of sight (the rake axis).
    let perp_los = Vec2::new(-dir.y, dir.x);
    // Hold the aim at the BODY's depth (drop the centroid's along-LoS component so
    // the nose never chases the receding bulk), keep its LATERAL offset (so the
    // bore is centred on the actual cells, not the empty grid centre).
    let lateral = (centroid - body_pos).dot(perp_los);
    // Deterministic RAKE: a triangle wave in `now` over a fixed period, in
    // `(-1, 1]`, scaled by the rake half-amplitude — sweeps the bore across the
    // hull WIDTH. Integer/float arithmetic only (no trig), so it is
    // bit-deterministic. `2·phase − 1` ∈ [−1, 1); its abs ∈ [0, 1); `1 − 2·abs`
    // peaks +1 at phase 0 and dips to −1 at phase 0.5 — a full ± sweep per period.
    const RAKE_PERIOD: u64 = 90; // ~3 s at 30 Hz — a slow, full-width sweep.
    let phase = (now % RAKE_PERIOD) as f32 / RAKE_PERIOD as f32; // [0, 1)
    let sweep = 1.0 - 2.0 * (2.0 * phase - 1.0).abs(); // (-1, 1]
    body_pos + perp_los * (lateral + rake_amp * sweep)
}

// R101 S5 — RETIRED the legacy combat MOTION generator (`EngageChannels`,
// `engage_channels`, `engage_motion`): the per-stance ContextMap range-band
// controller. The Engage arm now drives the unified `combat_intent`
// desired-velocity controller (`ai_execute_system`), expressing each
// [`CombatStance`] as a `v_des` field with the nose pinned to the gunnery lead
// (`Facing::Aim`). Combat motion is now O(1) (no per-stance map build). The
// stance geometry primitives it used ([`range_band_radial`],
// [`standoff_distance`], [`weapon_range`], [`pursue_intercept`], [`perp`]) live
// on, now consumed directly by `combat_intent`; the obstacle avoidance is the
// shared `control::deflect_v_des` over `v_des`. The AIM channel facing is still
// [`engage_aim_dir`].

/// T025 / R97 Phase 1 Stage C — the Evade MOVE channel: the away-deflected
/// break-off DIRECTION (full throttle), directly away from the threat with the
/// threat direction written as danger so the masked resolve deflects around it
/// rather than ever turning back in. `None` for a coincident threat (no sensible
/// flee vector). Stage C composes this MOVE direction against the SEPARATE AIM
/// channel (the threat facing) via the unified controller (`Facing::Aim`), so an
/// armed evader keeps its guns on the pursuer while running — the fighting-retreat
/// emergence.
/// (A last-threat-dir memory for target-less evades arrives with perception,
/// T029.)
fn evade_move_dir(pos: Vec2, threat_pos: Vec2, ai: &AiTuning) -> Option<Vec2> {
    let dir_to = (threat_pos - pos).normalize_or_zero();
    if dir_to == Vec2::ZERO {
        return None;
    }
    let n = ai.slot_count as usize;
    let mut map = ContextMap::default();
    map.add_interest_dir(-dir_to, 1.0, n);
    map.add_danger_dir(dir_to, 1.0, n);
    map.resolve(n, ai.danger_mask_floor).map(|(dir, _)| dir)
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

/// R99 Phase A — whether `posture` permits selecting `Engage`/`Ram` and firing
/// at `now`, for a PLAYER-overridden posture. Mirrors
/// [`ScenarioRole::allows_engage`](crate::ai::role::ScenarioRole::allows_engage)
/// exactly, but the `DefensiveOnly` fired-upon window is read from the ship's
/// role (if any) — a player can flip a ship to `DefensiveOnly` and the role's
/// `fired_upon_until` (armed by the trigger pass on a damage event) still gates
/// it; a ship with NO role under a player `DefensiveOnly` posture has no armed
/// window, so it stays weapons-tight (deadline `0`, strict `<` → never).
fn posture_allows_engage(posture: Posture, role: Option<&ScenarioRole>, now: u64) -> bool {
    match posture {
        Posture::FreeEngage => true,
        Posture::DefensiveOnly => now < role.map_or(0, |r| r.fired_upon_until),
        Posture::HoldFire => false,
    }
}

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
        // R99 Phase A: the user's DIRECT command override (mutable for the
        // Patrol cursor advance). Applied at HIGHEST precedence — its nav goal
        // overwrites the role's, and its style/posture win the resolution.
        Option<&mut PlayerOrder>,
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
        let Ok((_, _, mut brain, aoi, pos, vel, stats, mut role, contacts, mut player_order)) =
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
        // R96 precedence — the ROLE link of the resolved style chain (the squad
        // link rides `brain.squad_profile`/`_stance`, the base is the archetype
        // default). Captured from the role here, folded into the final
        // `.or().unwrap_or()` below. `None` when the ship carries no role.
        let mut role_profile: Option<MovementProfile> = None;
        let mut role_stance: Option<CombatStance> = None;
        if let Some(role) = role.as_mut() {
            role_apply(
                role,
                &mut brain,
                pos.map(|p| p.0),
                contacts,
                tuning.base_sensor_range,
                now,
            );
            role_profile = role.movement_profile;
            role_stance = role.combat_stance;
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

        // R99 Phase A — PLAYER OVERRIDE (HIGHEST precedence). A present
        // `PlayerOrder` with a `Some(kind)` nav command is applied AFTER
        // `role_apply`, so its waypoint/home/target OVERWRITE the role's writes
        // (the player wins the nav goal; a settings-only order leaves the goal
        // to the role/squad). Its `profile`/`stance`/`posture` overrides are the
        // TOP link of the resolved chains below. A commanded ship is also
        // squad-exempt + planner-skipped (squad.rs / strategy.rs), so the order
        // layer never stomps the command between thinks.
        let mut player_profile: Option<MovementProfile> = None;
        let mut player_stance: Option<CombatStance> = None;
        if let Some(order) = player_order.as_mut() {
            order.apply(&mut brain, pos.map_or(Vec2::ZERO, |p| p.0), now);
            // R100 — a commanded POSITIONAL move (MoveTo/HoldAt/Patrol) PARKS:
            // it defaults to `Rush` (active-braking `arrive_braked`) when the
            // user pinned no profile, instead of inheriting the archetype
            // default (`Cruise` — a drag-braked coast that overshoots). A pinned
            // profile, an `Attack`, and a settings-only order are unchanged.
            player_profile = order.resolved_move_profile();
            player_stance = order.stance;
            // The player posture (if set) wins over the role posture; the engage
            // gate is re-derived from the resolved posture.
            if let Some(posture) = order.posture {
                let role_for_window = role.as_deref();
                engage_allowed = posture_allows_engage(posture, role_for_window, now);
            }
        }

        // R96 STYLE RESOLUTION (do it ONCE per think, after `role_apply`): the
        // documented precedence chain `squad ← role ← archetype default`. Each
        // writer stores its `Option` LOCALLY — the squad's onto
        // `brain.squad_profile`/`_stance` (via `apply_assignment`; `None` for
        // roled members, who are squad-exempt), the role's read out into
        // `role_profile`/`_stance` above, the base from `default_*` — so the
        // precedence is exactly one `.or().or().unwrap_or()`. Roled members get
        // role-or-archetype (squad channel `None`); squad non-roled members get
        // squad-or-archetype (role `None`); lone ships get the archetype default.
        // Parity: a `Generic` archetype with no role/squad style resolves to
        // `Cruise`/`Charge` — today's intents, byte-for-byte.
        //
        // R99 Phase A — the PLAYER link sits ABOVE the squad link: a
        // `PlayerOrder.profile`/`.stance` override wins the whole chain
        // (`player ← squad ← role ← archetype default`). `None` defers to the
        // next link, so a settings-only/style-less order is transparent here.
        brain.movement_profile = player_profile
            .or(brain.squad_profile)
            .or(role_profile)
            .unwrap_or_else(|| default_movement_profile(brain.archetype));
        brain.combat_stance = player_stance
            .or(brain.squad_stance)
            .or(role_stance)
            .unwrap_or_else(|| default_combat_stance(brain.archetype));

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

/// R97 Phase 1 Stage B/C — the per-behavior MOVE/AIM/FIRE channel split of the
/// execution arm. Each [`Behavior`] arm of [`ai_execute_system`] resolves into
/// these three channels:
///
/// - **MOVE** (`intent`): WHERE the ship translates + how it turns. After R101 S8
///   EVERY arm drives the SAME unified controller
///   ([`allocate_intent`](crate::ai::control::allocate_intent)) — the nav arms
///   (Waypoint / Patrol / Scout|Sweep / Follow) via `nav_intent` (S3),
///   FormationKeep via `formation_desired_vel` (S4), the combat stances via
///   `combat_intent` (S5), the survival arms via `survival_intent` (S6), and the
///   ram lead-pursuit emits `v_des` toward the intercept at top speed (S8) — so
///   there is no longer any legacy composer left. **Stage C** is where the
///   SURVIVAL arms
///   intentionally diverge: [`Evade`](Behavior::Evade)/[`Retreat`](Behavior::Retreat)
///   compose MOVE (flee) against a SEPARATE AIM (the threat facing) via the
///   unified controller (`Facing::Aim`), so the nose tracks the pursuer while the
///   ship runs.
/// - **AIM** (`aim_dir`): WHERE the ship points. For the survival arms this is the
///   nearest-hostile facing — DECOUPLED from MOVE and fed into the composition
///   above; for every other arm it is the recorded move interest direction.
///   `Vec2::ZERO` = "no aim".
/// - **FIRE** (`fire`): WHETHER this arm has a hostile to bear its gun on — the
///   WEAPONS-FREE allowlist (Stage C): `Engage`/`Ram` AND `Evade`/`Retreat` when a
///   threat exists. The overlay below runs `fire_decision` (armed + in-range +
///   aligned + energy/heat) and the posture gate; this flag records WHO may fire.
struct ChannelIntent {
    /// MOVE channel — the composed intent from this arm. Every arm now produces
    /// it through the unified `allocate_intent` controller (R101 S8); the survival
    /// arms decouple AIM from MOVE via `Facing::Aim` — see the struct docs.
    intent: ShipIntent,
    /// AIM channel — the recorded facing. For the survival arms it is the
    /// nearest-hostile facing already consumed by the composition; for the others
    /// the move interest direction. `Vec2::ZERO` = "no aim".
    aim_dir: Vec2,
    /// FIRE channel — Stage C weapons-free: `true` for `Engage`/`Ram`, and for
    /// `Evade`/`Retreat` when a hostile AIM target exists (fighting retreat).
    fire: bool,
}

/// Arrive radius (world units) for `Waypoint`/`Patrol` goals: within this
/// range the goal counts as reached — the brain emits [`AiEvent::Arrived`] and
/// holds. A tuning-ish v1 const; `crate`-visible since T032, where the
/// `ScenarioRole` patrol cursor advances on the same radius (one shared
/// "arrived" definition). Matches the steering tests' canonical radius.
pub(crate) const ARRIVE_RADIUS: f32 = 10.0;

/// The EXECUTION half of the brain (T013, TR-001): every tick, turn each
/// Active/Mid ship's selected [`Behavior`] into a [`ShipIntent`] via the
/// steering substrate. The think system SELECTS (event-driven, sparse); this
/// system EXECUTES (cheap per-tick steering math — a handful of vector ops per
/// ship), so a behavior switched mid-cadence steers the same tick.
///
/// **Output is intent-only (V-6)**: the system writes the ship's `ShipIntent`
/// component VALUE through the unified
/// [`allocate_intent`](crate::ai::control::allocate_intent) controller (R101 S8 —
/// the SOLE motion composer) and NEVER touches `Velocity`/`Heading`/`Position` —
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
/// **Navigation behaviors (R101 S3 — the unified `nav_intent` controller path)**:
/// each computes its GOAL, then drives the controller — desired velocity toward
/// the goal (capped at the kinematic stoppable speed so the arrive is
/// no-overshoot), `deflect_v_des` obstacle avoidance, then
/// [`allocate_intent`](crate::ai::control::allocate_intent) (flip-and-burn /
/// reverse-brake EMERGE):
/// - [`Hold`](Behavior::Hold): ACTIVE station-keep — `v_des = 0` through the
///   controller (brakes a drifting ship to rest; a ship already at rest emits
///   zero intent via the parked-no-spin ladder). FLAGGED change from the v1 coast.
/// - [`Waypoint`](Behavior::Waypoint): `nav_intent(brain.waypoint)` (single
///   waypoint v1). Within [`ARRIVE_RADIUS`]: hold + push [`AiEvent::Arrived`]
///   each tick ([`RethinkQueue`] coalesces to one entry; the NEXT tick's think
///   consumes it — the re-think storm is bounded at one per cadence).
/// - [`Patrol`](Behavior::Patrol): v1 ping-pong — `nav_intent(brain.waypoint)`;
///   on arrive, SWAP `waypoint` ↔ `home` + `Arrived` (route vectors arrive with
///   `ScenarioRole`, T032). A home-less patrol degrades to hold-at-goal.
/// - [`Follow`](Behavior::Follow): `nav_intent(leader_pos)` — arrives at the
///   leader's position; within [`ARRIVE_RADIUS`] of the leader it station-keeps
///   (controller brakes to rest). Leader missing/despawned → zero intent (the
///   V-1 sweep clears the dangling ref; the next think degrades).
/// - [`FormationKeep`](Behavior::FormationKeep) (R101 S4 — now on the unified
///   controller path): emit `v_des = formation_desired_vel(...)` = leader vel +
///   slot-error closing term (the EXACT velocity-matching math the legacy
///   `formation_keep` computed), capped at the pace `v_max`, then
///   [`allocate_intent`](crate::ai::control::allocate_intent) with `Facing::Free`
///   (point where you're going / brake naturally onto the slot). On-slot with
///   matched velocity `v_des == leader_vel`, so the follower coasts WITH the
///   leader (quiet, no chatter). Leader/slot missing → default coast.
///
/// **Combat behaviors (T025, TR-011)** — all keyed off `brain.target` looked
/// up in the same read-only kinematics query (a missing/despawned target →
/// zero intent; the V-1 sweep + next think degrade the behavior):
/// - [`Engage`](Behavior::Engage): [`engage_motion`] — the archetype-flavored
///   range-band standoff (Brawler close ring / Kiter long ring / medium
///   default) over a context map, facing the gunnery lead when on-ring.
/// - [`Evade`](Behavior::Evade) / [`Retreat`](Behavior::Retreat) (R97 Phase 1
///   Stage C): the SURVIVAL arms. MOVE = the flee vector (Evade: the
///   danger-masked break-off via [`evade_move_dir`]; Retreat: toward `brain.home`
///   when set, else directly away). AIM = the nearest-hostile facing, composed
///   DECOUPLED from MOVE via the unified controller (`Facing::Aim`) — a
///   forward-only hull reverse-drifts (retro nose-on) while the gun bears, a
///   `can_strafe` hull
///   sidles. **They fire when aimed at a hostile and aligned** (the weapons-free
///   rule below): a FIGHTING RETREAT emerges — open range while facing and firing
///   on the pursuer — with NO dedicated FightingRetreat behavior. An unarmed or
///   unaligned survival ship just runs (and survives) — no fire.
/// - [`Ram`](Behavior::Ram) (T027; R101 S8 — onto the unified controller): a
///   full-throttle collision course — emit `v_des` toward the [`pursue_intercept`]
///   lead at the ship's TOP speed with `Facing::Free` (nose along the thrust = at
///   the intercept = into the target), NO stoppable-speed cap and NO obstacle
///   deflection (a ram must not brake before impact or swerve around bodies).
///   Fire stays ALLOWED on the way in (a finisher, not a ceasefire).
/// - [`Scout`](Behavior::Scout)/[`Sweep`](Behavior::Sweep) (T035, TR-021):
///   movement IDENTICAL to `Waypoint` — `nav_intent` toward the role-asserted
///   coverage leg, `Arrived` within the radius (the role cursor advances at the
///   next think). The recon difference is selection/veto, not motion. Neither
///   fires (no hostile AIM target → `fire == false`).
///
/// **Fire control (T026, TR-011; R97 Stage C weapons-free)**: after the movement
/// arm, any arm with a hostile AIM target (Engage/Ram AND Evade/Retreat) runs
/// [`fire_decision`] — armed + in-range + aligned-to-lead + the energy/heat gates
/// MIRRORING `weapon_fire_system` (the AI *chooses* not to fire when gated) —
/// and on a yes set `fire_primary` + the [`primary_fire_group`]-selected
/// `active_group`. Survival modes fire when aimed at a hostile and aligned
/// (fighting retreat emerges). The arms with no hostile target
/// (Hold/Patrol/Waypoint/Follow/FormationKeep/Scout/Sweep) leave the fire fields
/// default (false). The posture gate still suppresses (HoldFire never fires).
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
// The cfg'd `ai_debug` capture seam (the `captures` query + `commands`) tips the
// already-wide system over clippy's arg limit ONLY under the feature; each param
// is a distinct ECS read the system genuinely needs (the `weapon_fire_system`
// query-width pattern). Bevy injects them — there is no caller to simplify.
#[allow(clippy::too_many_arguments)]
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
    // R96 Part D: the per-tick obstacle field the move + combat arms steer
    // around (Option → no avoidance in worlds that never inserted it; the
    // empty-field gate keeps that path byte-identical to pre-R96-D anyway).
    obstacles: Option<Res<ObstacleField>>,
    // R101 S5 — the hull catalog the combat aim path resolves a fitted target's
    // grid dims through (→ its grid CENTRE, the render/carve anchor the
    // cell→world core-aim transform offsets from). Option → a minimal world (the
    // stance fixtures spawn bare `Position`-only targets, no `FitLayout`) keeps
    // the legacy aim-at-`Position` path: with no resolvable grid centre the aim
    // falls back to the target centre, byte-identical to pre-S5.
    hulls: Option<Res<HullCatalog>>,
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
        // mirroring weapon_fire_system). Grouped into a nested tuple so the
        // top-level query arity stays within Bevy's `QueryData` tuple limit.
        (
            Option<&Energy>,
            Option<&Heat>,
            Option<&WeaponGroups>,
            Option<&ShipWeapons>,
        ),
        // T032: the posture fire-gate overlay (read-only; absent = ungated).
        Option<&ScenarioRole>,
        // R96 Part D: the ship's own collision radius sizes the avoid clearance
        // (absent = 0, so an unfitted/test ship still steers around with the pad).
        Option<&CollisionRadius>,
        // R97 Phase 1 Stage C: the perception memory the survival arms read to
        // face the NEAREST hostile when `brain.target` is unset (absent = no
        // contact picture, so the arms fall back to `brain.target` alone).
        Option<&ContactList>,
    )>,
    // Leader AND combat-target kinematics (read-only; see Determinism docs).
    others: Query<(&Position, &Velocity, &Heading)>,
    // R101 S5 — the combat-target hull geometry the core-aim path reads (read-only,
    // access-disjoint from `ships`: `ships` mutates only `AiBrain`/`ShipIntent`,
    // never `FitLayout`/`MeshAnchor`). The target's `FitLayout` yields its
    // `core_cell` (the kill cell) + its `hull` id (→ grid dims → grid centre); the
    // frozen `MeshAnchor`, when present (a wreck), overrides the grid-centre anchor
    // exactly as `sever_chunk` does. A target with NO `FitLayout` (a flat-health
    // dummy) is absent here → the aim falls back to its `Position` (legacy).
    target_geom: Query<(&FitLayout, Option<&MeshAnchor>)>,
    // R97 Phase 1 Stage D (TR-020, AD-006): the per-channel capture seam exists
    // ONLY under `ai_debug` — with the feature off these params (and every
    // capture statement below) are compiled out, so headless/bench builds pay
    // zero cost. The query is access-disjoint from `ships` (the brain query's
    // only mutable component is `AiBrain` + `ShipIntent`; this is `AiDebugCapture`).
    #[cfg(feature = "ai_debug")] mut captures: Query<&mut debug_capture::AiDebugCapture>,
    #[cfg(feature = "ai_debug")] mut commands: Commands,
) {
    let sim = sim.map(|s| *s).unwrap_or_default();
    let ai = tuning.map(|t| *t).unwrap_or_default();
    let now = tick.map_or(0, |t| t.0);
    let field = obstacles.as_deref();
    // R101 S5 — the hull catalog the combat aim resolves a target's grid centre
    // through (None in minimal worlds → the aim falls back to the target centre).
    let hulls = hulls.as_deref();
    for (
        entity,
        mut brain,
        pos,
        vel,
        heading,
        mut intent,
        stats,
        aoi,
        (energy, heat, groups, weapons),
        role,
        collision_radius,
        contacts,
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
        // R96 Part D — the ship's own collision radius sizes the obstacle avoid
        // clearance; absent (unfitted/test ship) → 0, so it still steers around
        // with the configured pad.
        let own_radius = collision_radius.map_or(0.0, |r| r.0);
        // R97 Phase 1 Stage C — the AIM channel's threat picture for the survival
        // arms (Retreat/Evade): the NEAREST hostile to point the fixed-forward gun
        // at while fleeing, so a FIGHTING RETREAT emerges (open range while facing
        // + firing on the pursuer) without any dedicated FightingRetreat behavior.
        // Priority (documented): `brain.target` (the committed/perceived hostile,
        // with full kinematics for the lead/fire solve), else the nearest
        // `ContactList` contact's last-known position (zero velocity — a static
        // facing the gun still bears on). `None` = no hostile to watch → the
        // survival arms fall back to aim == move (run blind, never fire).
        let threat = brain
            .target
            .and_then(|t| others.get(t).ok().map(|(p, v, _)| (p.0, v.0)))
            .or_else(|| {
                let list = contacts?;
                let c = nearest_contact(&list.contacts, pos.0)?;
                list.contacts
                    .iter()
                    .find(|x| x.target == c)
                    .map(|x| (x.last_pos, Vec2::ZERO))
            });
        let threat_aim = threat.map(|(tpos, _)| (tpos - pos.0).normalize_or_zero());
        // R101 S3 — the movement-profile pace for this ship, copied out so the
        // `nav_intent` closure never borrows `brain` (the Patrol arm mutates
        // `brain.waypoint`/`brain.home` while a closure is live).
        // `control_params(profile)` yields the controller's `(v_max, tau)` — the
        // unified controller's pace projection (R101 S3 retired the R96–R100
        // `arrive_braked`/`brake_orientation` brake triple entirely).
        let profile = brain.movement_profile;
        // R101 S5 — the combat archetype + resolved stance, copied out so the
        // `combat_intent` closure never borrows `brain` (same reason as `profile`:
        // the Patrol arm mutates `brain` while a closure is live).
        let combat_archetype = brain.archetype;
        let combat_stance = brain.combat_stance;

        // R101 S3 — THE UNIFIED NAV PATH. Every navigation behavior with a goal
        // (Waypoint / Patrol leg / Scout|Sweep coverage leg / Follow-leader /
        // commanded MoveTo/HoldAt) drives through the same controller: project a
        // desired velocity toward the goal (capped at the kinematic stoppable
        // speed so the arrive is no-overshoot BY CONSTRUCTION), deflect it around
        // in-range obstacles (`deflect_v_des` — the empty-field gate leaves it
        // unchanged), then `allocate_intent` aligns thrust with the accel needed
        // to track it (flip-and-burn / reverse-brake EMERGE from `Facing::Free`).
        //
        // S3 generalizes the S2 commanded-only early-dispatch: it is no longer
        // gated on a PlayerOrder — scenario nav ships now share this path, so a
        // Rush/Cruise/Leisurely waypoint ship parks via the controller rather than
        // the R96–R100 `arrive_braked`/`brake_orientation` brake. That brake (the
        // old `fly_to` closure) is RETIRED from the nav arms; the underlying
        // `steering.rs` primitives stay defined for combat/survival until S5/S6.
        //
        // Per-arm goal logic (Patrol leg swap, Scout/Sweep cursor, Follow lookup)
        // is UNCHANGED — only this MOTION generation is shared. The arms call
        // `nav_intent(goal)` for the non-arrived case; arrival (push `Arrived` +
        // any leg swap) stays in each arm exactly as before, but the arrived
        // motion is now `park_intent()` (active station-keep: `v_des = 0` through
        // the controller) instead of a coast — so a ship that enters the arrive
        // radius still MOVING brakes to rest on the point (matching the S2
        // commanded-mover park). The uniform `throttle_cap` multiply + the
        // `ai_debug` capture below this match apply to the result, so no per-arm
        // cap/capture is needed here.
        let cstats = stats
            .map(ControlStats::from_stats)
            .unwrap_or_else(ControlStats::fallback);
        // The controller's velocity-tracking time constant for this pace.
        let (nav_v_max, nav_tau) = ai.control_params(profile);
        // ACTIVE station-keep: `v_des = 0` through the controller brakes a drifting
        // ship to rest; `Facing::Free` + a zero command hits the parked-no-spin
        // ladder, so a ship already at rest emits zero intent (quiet on station).
        let park_intent = || {
            allocate_intent(
                MoveCmd {
                    v_des: Vec2::ZERO,
                    facing: Facing::Free,
                },
                vel.0,
                heading.0,
                cstats,
                nav_tau,
            )
        };
        let nav_intent = |goal: Vec2| -> ShipIntent {
            let to = goal - pos.0;
            let dist = to.length();
            // Cap the closing speed at the kinematic stoppable speed (the reverse
            // channel as the brake) so the controller arrives no-overshoot. The
            // arms only call this OUTSIDE the arrive radius, so `v_des` is non-zero;
            // a degenerate `to` still yields a finite (zero) command. `nav_v_max`/
            // `cstats` are hoisted above (the Patrol arm mutates `brain` while this
            // closure is live, so it borrows neither `brain` nor recomputes them).
            let speed_cap = nav_v_max.min(stoppable_speed(dist, cstats.a_rev));
            let v_des = to.normalize_or_zero() * speed_cap;
            // Obstacle avoidance: bend `v_des` around in-range large bodies (the
            // empty-field gate inside `deflect_v_des` returns it unchanged, so an
            // obstacle-free world keeps the raw controller path).
            let v_des = match field {
                Some(f) => deflect_v_des(v_des, f, pos.0, vel.0, own_radius, &ai),
                None => v_des,
            };
            allocate_intent(
                MoveCmd {
                    v_des,
                    facing: Facing::Free,
                },
                vel.0,
                heading.0,
                cstats,
                nav_tau,
            )
        };

        // R101 S5 — THE UNIFIED COMBAT PATH. Each [`CombatStance`] is expressed as
        // a DESIRED-VELOCITY field with the nose PINNED to the gunnery lead
        // (`Facing::Aim`), then run through the SAME controller as nav/formation:
        // build `v_des` from the range-band geometry, deflect it around in-range
        // obstacles (`deflect_v_des` — empty-field gate leaves it unchanged), then
        // `allocate_intent` aligns thrust with the accel needed to track it. The
        // fixed-forward gun stays on the lead via `Facing::Aim(lead)`; because the
        // nose is pinned, the lateral component of `v_des` lands on the strafe
        // channel for a `can_strafe` hull (orbit sidle EMERGES) or rotates the body
        // for a forward-only hull — no hand-injected strafe. The FIRE decision is
        // computed AFTER the match (the shared `fire_decision` overlay), reading the
        // ACTUAL heading the controller turned to — so the alignment gate is honest.
        //
        // Per-stance `v_des` (`dir_t` = unit toward target; `radial` =
        // `range_band_radial(dist, standoff, RANGE_BAND_FRAC)` ∈ [-1,1], > 0 = too
        // far/close in, < 0 = too close/open out; `stoppable_speed(.., a_rev)` caps
        // the closing speed so the arrive-onto-ring is no-overshoot BY
        // CONSTRUCTION). `combat_v_max` is the profile pace cap; `a_rev` is the
        // brake authority (the nose is pinned, so reverse — not a flip — brakes).
        let (combat_v_max, combat_tau) = ai.control_params(profile);
        // Speed floor below which a `v_des` is "no demand" — the forward-only
        // facing test then holds `Aim(lead)` (a near-zero command has no maneuver
        // to steer the body toward, so keep the gun on target).
        const V_DES_EPS: f32 = 1e-3;
        // R101 S5 — BIAS THE AIM TOWARD THE KILL: the world point on `target`'s
        // hull the gun should bore — its live-cell BULK (`target_centroid_world`)
        // PROJECTED to the body range + a RAKE jink across the hull
        // (`combat_aim_point`), NOT the bare `Position` (= grid CENTRE). A STATIONARY
        // target whose occupied cells sit off the grid centre would otherwise get one
        // fixed central tunnel that misses the bulk and never dies; the projection
        // stops the forward-only nose from chasing the receding bulk THROUGH the
        // target, and the rake sweeps the bore across the full width so the carve
        // disconnects an off-centre core. Re-solved each tick. Falls back to `tpos`
        // for a target with no `FitLayout`/no resolvable grid centre (a flat-health
        // dummy), byte-identical to the legacy aim-at-`Position`.
        let combat_aim_pos = |target: Entity, tpos: Vec2, theading: f32| -> Vec2 {
            let Ok((layout, anchor)) = target_geom.get(target) else {
                return tpos; // No hull geometry → aim at the body centre (legacy).
            };
            // Resolve the target's grid dims (→ grid centre + hull half-extent for
            // the rake) from `layout.hull` via the catalog. No catalog / unresolved
            // hull → aim at the body centre (legacy).
            let Some(grid_dims) = hulls
                .and_then(|h| h.get(layout.hull))
                .map(|hull| hull.grid_dims)
            else {
                return tpos;
            };
            // The render/carve anchor whose world location IS `tpos`: a frozen
            // `MeshAnchor` (a wreck), else the live ship's grid CENTRE
            // `(cols·0.5, rows·0.5)`. (Single-sourced with `sever_chunk`.)
            let grid_centre = match anchor {
                Some(a) => a.0,
                None => Vec2::new(grid_dims.0 as f32 * 0.5, grid_dims.1 as f32 * 0.5),
            };
            // The live-cell BULK (centroid), then the projected + raked KILL aim:
            // hold the body range (no along-LoS chase) + sweep the bore across the
            // hull. The rake half-amplitude is the hull half-extent × the knob.
            let centroid = target_centroid_world(layout, theading, tpos, grid_centre);
            let half_extent =
                grid_dims.0.max(grid_dims.1) as f32 * 0.5 * crate::fitting::CELL_WORLD_SIZE;
            let rake_amp = ai.combat_rake_frac * half_extent;
            combat_aim_point(pos.0, tpos, centroid, now, rake_amp)
        };
        // R101 S5 — `body_pos` is the target's `Position` (the BODY CENTRE) — the
        // RING geometry (dist/standoff/radial/weave) is measured to it, so the ship
        // holds a STABLE ring as the hull carves. `aim_pos` is the kill point (the
        // live-cell centroid) — only the gunnery LEAD + facing track it, so the gun
        // bores the bulk while the ship parks at range. Decoupling them is what
        // stops the runaway: aiming the RING at the receding centroid would chase
        // the carved-away mass and fly the ship past the target.
        let combat_intent =
            |stance: CombatStance, body_pos: Vec2, aim_pos: Vec2, target_vel: Vec2| -> ShipIntent {
                let to = body_pos - pos.0;
                let dist = to.length();
                let dir_t = to.normalize_or_zero();
                if dir_t == Vec2::ZERO {
                    // Coincident target: nothing sensible to steer — park (brake to
                    // rest, no spin), exactly as the nav arms do for a degenerate goal.
                    return park_intent();
                }
                // The gunnery lead the fixed-forward gun bears on — the KILL point.
                let lead = engage_aim_dir(pos.0, stats, aim_pos, target_vel);
                // The standoff ring this stance holds (the SAME per-stance radius rule
                // the legacy `engage_channels` used, so the archetype rings are
                // unchanged): Orbit at `orbit_radius_frac × archetype standoff`, Kite at
                // `kite_range_frac × weapon_range`, everyone else the archetype standoff.
                let range = weapon_range(stats).unwrap_or(FALLBACK_ENGAGE_RANGE);
                let standoff = match stance {
                    CombatStance::Orbit { .. } => {
                        ai.orbit_radius_frac * standoff_distance(combat_archetype, range)
                    }
                    CombatStance::Kite => ai.kite_range_frac * range,
                    _ => standoff_distance(combat_archetype, range),
                };
                let radial = range_band_radial(dist, standoff, RANGE_BAND_FRAC);

                // The brake authority sizing the radial close/open speed (the
                // `stoppable_speed` cap). The nose is PINNED to the lead, so the
                // realizable brake is technically the (often weak) reverse channel —
                // but a hull whose reverse is a small fraction of forward (a_rev ≪
                // a_fwd) would then CRAWL onto its ring and asymptotically stall far
                // out. We size the cap with the STRONGER channel (`a_fwd`) so the ship
                // keeps closing momentum until it is genuinely near the ring; the
                // band's open-out arm (`radial < 0`) plus the ship's own drag bound any
                // overshoot to a soft slug-close (a brawler is meant to wade in), and
                // the controller still brakes with whatever channel it actually has.
                let close_brake = cstats.a_fwd.max(cstats.a_rev);
                // The radial velocity onto the ring: close toward it when outside
                // (`radial > 0`), open back out when inside (`radial < 0`), zero
                // on-band. Capped at `stoppable_speed(gap, close_brake)` (no-overshoot
                // sizing) and the profile pace `combat_v_max`.
                let radial_v = |toward_target: bool, gap: f32| -> Vec2 {
                    let speed = combat_v_max.min(stoppable_speed(gap, close_brake));
                    if toward_target {
                        dir_t * speed
                    } else {
                        -dir_t * speed
                    }
                };

                // R101 S5 — ON-BAND COMBAT WEAVE: a gentle TANGENTIAL drift the
                // Charge/Standoff stances take IN-BAND instead of a dead-stop
                // (`v_des = 0`). Tangential (`-perp(dir_t)` — the CCW orbit
                // convention) so it HOLDS the ring (RANGE ~constant) while the ship
                // slowly circles — never a sitting duck, and the gun RAKES a swath
                // across the hull as the bearing sweeps, disconnecting an off-centre
                // core. Amplitude `combat_weave_frac · v_max` (≪ the orbit speed, so
                // the range stays in the band); `0.0` restores the legacy dead-stop.
                // It is scaled by `(1 − |radial|)` (the SAME blend Orbit uses), so it
                // DOMINATES in-band (`radial ≈ 0`) and FADES OUT off-band where the
                // radial close/open correction takes over — a bare `radial == 0`
                // gate would be dead code under float arithmetic (the ship almost
                // never lands exactly on-band), so the blend is what makes the weave
                // real. With `combat_weave_frac == 0` the term is zero everywhere →
                // byte-identical to the legacy radial-only controller.
                let weave =
                    -perp(dir_t) * (ai.combat_weave_frac * combat_v_max * (1.0 - radial.abs()));

                // Per-stance desired velocity (world frame).
                let v_des = match stance {
                    // CHARGE: close to the ring when outside, back off when inside, weave
                    // in-band. The arrive target is the RING (`dist − standoff`), not the
                    // target, so it parks at range instead of ramming; in-band the radial
                    // correction fades and the tangential weave dominates (a gentle circle
                    // that rakes the gun across the hull) rather than dead-stopping.
                    CombatStance::Charge => {
                        let correction = if radial > 0.0 {
                            radial_v(true, dist - standoff)
                        } else if radial < 0.0 {
                            radial_v(false, standoff - dist)
                        } else {
                            Vec2::ZERO
                        };
                        correction + weave
                    }
                    // ORBIT: a dominating-on-ring TANGENT plus an off-ring radial
                    // correction onto the ring. The tangent sign matches the legacy perp
                    // convention (CCW = `-perp(dir_t)`). On-ring (`radial ≈ 0`) the
                    // tangent is full and the correction zero; off-ring the tangent
                    // fades (`× (1 − |radial|)`) and the correction eases it back on.
                    CombatStance::Orbit { ccw } => {
                        let tangent = if ccw { -perp(dir_t) } else { perp(dir_t) };
                        let v_orbit = ai.orbit_speed_frac * combat_v_max * (1.0 - radial.abs());
                        let correction = if radial > 0.0 {
                            radial_v(true, dist - standoff)
                        } else if radial < 0.0 {
                            radial_v(false, standoff - dist)
                        } else {
                            Vec2::ZERO
                        };
                        tangent * v_orbit + correction
                    }
                    // STANDOFF: close to the ring when outside, else HOLD it (hold
                    // range, guns on the lead) — but with the gentle in-band weave so
                    // the ship isn't a sitting duck and the gun rakes the hull. The
                    // weave is tangential (`× (1 − |radial|)`), so it still HOLDS THE
                    // RING (range ~constant) while slowly circling; the radial close
                    // term only acts when OUTSIDE the ring.
                    CombatStance::Standoff => {
                        let correction = if radial > 0.0 {
                            radial_v(true, dist - standoff)
                        } else {
                            Vec2::ZERO
                        };
                        correction + weave
                    }
                    // KITE: open AWAY while INSIDE the kite ring; hold at/beyond it (the
                    // gun bears as the target chases). Open at the pace cap so the kiter
                    // outruns a closing brawler.
                    CombatStance::Kite => {
                        if radial < 0.0 {
                            -dir_t * combat_v_max
                        } else {
                            Vec2::ZERO // At/beyond the kite ring: hold and shoot.
                        }
                    }
                };

                // Obstacle avoidance: bend `v_des` around in-range large bodies (the
                // empty-field gate inside `deflect_v_des` returns it unchanged, so an
                // obstacle-free engagement keeps the raw controller path). Done BEFORE
                // the facing choice so a detour redirects the nose for a forward-only
                // hull (see below).
                let v_des = match field {
                    Some(f) => deflect_v_des(v_des, f, pos.0, vel.0, own_radius, &ai),
                    None => v_des,
                };

                // FACING: pin the nose to the gunnery lead so the fixed-forward gun
                // bears while the ship translates (`Aim(lead)`). A `can_strafe` hull
                // ALWAYS pins (its strafe channel does any lateral work — the orbit
                // sidle EMERGES, and obstacle detours sidle too, gun on target). A
                // FORWARD-ONLY hull cannot translate PERPENDICULAR to its nose, so when
                // the (post-deflection) `v_des` has a large LATERAL component relative
                // to the lead — an Orbit tangent, or an obstacle-deflected detour — it
                // must point the nose along `v_des` to move there (`Facing::Free`),
                // turning the body and trading gun-on-target for the maneuver; radial
                // motion (Charge/Standoff close, Kite open — `v_des` ∥ or anti-∥ the
                // lead, handled by the fwd/reverse channel) keeps `Aim(lead)` so the
                // gun bears. The threshold is half the desired speed (a v_des more than
                // ~30° off the lead axis steers the body).
                let facing = if cstats.can_strafe {
                    Facing::Aim(lead)
                } else {
                    let speed = v_des.length();
                    let along = v_des.dot(lead); // signed component on the lead axis.
                    let lateral = (speed * speed - along * along).max(0.0).sqrt();
                    if speed > V_DES_EPS && lateral > 0.5 * speed {
                        Facing::Free
                    } else {
                        Facing::Aim(lead)
                    }
                };

                allocate_intent(
                    MoveCmd { v_des, facing },
                    vel.0,
                    heading.0,
                    cstats,
                    combat_tau,
                )
            };

        // R101 S6 — THE UNIFIED SURVIVAL PATH. Evade/Retreat now drive the SAME
        // controller as nav/formation/combat: a behavior emits a FLEE direction
        // (unit) and a FACING, this scales the flee dir to the profile pace
        // (`v_des = flee_dir · v_max`), deflects it around in-range obstacles
        // (`deflect_v_des` — the empty-field gate leaves it unchanged, replacing
        // the old `mask_move_dir` ContextMap with the unified path), then
        // `allocate_intent` aligns thrust with the accel needed to track it.
        //
        // **Why a FIGHTING RETREAT emerges** (no dedicated behavior): the flee
        // dir opens range (`v_des` points AWAY from the threat / toward `home`)
        // while `Facing::Aim(threat)` PINS the nose on the pursuer. The
        // controller, asked to translate one way while the nose points another,
        // reverse-brakes / strafes to realize the opening `v_des` — exactly the
        // reverse-thrust withdrawal (forward-only hull) or lateral sidle
        // (`can_strafe` hull) the old `compose_intent_aimed` produced, but as an
        // EMERGENT allocation rather than a special-case primitive. With the
        // nose held on the threat the FIRE overlay (below the match, unchanged)
        // reads the ACTUAL heading and fires when the gun bears.
        //
        // `Facing::Free` (no hostile to watch) runs blind: the controller points
        // the nose along the flee `v_des` and burns forward at full pace.
        let survival_v_max = nav_v_max;
        let survival_intent = |flee_dir: Vec2, facing: Facing| -> ShipIntent {
            let v_des = flee_dir.normalize_or_zero() * survival_v_max;
            // Obstacle avoidance: bend the flee velocity around in-range large
            // bodies (the empty-field gate inside `deflect_v_des` returns it
            // unchanged) — a forward-only hull otherwise reverses STRAIGHT into a
            // body behind it. This replaces the R98 `mask_move_dir` ContextMap
            // with the unified desired-velocity deflection nav/combat use.
            let v_des = match field {
                Some(f) => deflect_v_des(v_des, f, pos.0, vel.0, own_radius, &ai),
                None => v_des,
            };
            allocate_intent(MoveCmd { v_des, facing }, vel.0, heading.0, cstats, nav_tau)
        };

        // R97 Phase 1 Stage B — resolve the behavior into MOVE/AIM/FIRE channels.
        // The MOVE channel (`intent`) is composed via each arm's TODAY-EXACT call
        // path (so the output is byte-identical); AIM (`aim_dir`, Stage B
        // invariant: aim == move interest direction) and FIRE (the `Engage`/`Ram`
        // allowlist) are RECORDED but only consumed in Stage C. `aim_dir ==
        // Vec2::ZERO` records "no aim" (a held/coasting arm). The composition is
        // unchanged from pre-Stage-B; only the channel threading is new.
        let channel = match brain.behavior {
            // R101 S3 — ACTIVE station-keep (FLAGGED change from the v1 coast):
            // Hold now drives `v_des = 0` through the controller (`park_intent`),
            // so a drifting ship brakes to rest instead of coasting forever; a ship
            // already at rest emits zero intent (the parked-no-spin ladder), quiet
            // on station. (Determinism is unaffected: the controller is pure f32;
            // the bit-identity tests compare fresh rebuilds, not a golden.)
            Behavior::Hold => ChannelIntent {
                intent: park_intent(),
                aim_dir: Vec2::ZERO,
                fire: false,
            },
            Behavior::Waypoint => match brain.waypoint {
                Some(goal) if (goal - pos.0).length() <= ARRIVE_RADIUS => {
                    queue.push(entity, AiEvent::Arrived);
                    ChannelIntent {
                        intent: park_intent(), // brake to rest ON the point
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    }
                }
                Some(goal) => ChannelIntent {
                    intent: nav_intent(goal),
                    aim_dir: (goal - pos.0).normalize_or_zero(),
                    fire: false,
                },
                None => ChannelIntent {
                    intent: ShipIntent::default(), // Goal-less: the think degrades it.
                    aim_dir: Vec2::ZERO,
                    fire: false,
                },
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
                    ChannelIntent {
                        intent: park_intent(), // brake to rest ON the leg point
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    }
                }
                Some(goal) => ChannelIntent {
                    intent: nav_intent(goal),
                    aim_dir: (goal - pos.0).normalize_or_zero(),
                    fire: false,
                },
                None => ChannelIntent {
                    intent: ShipIntent::default(),
                    aim_dir: Vec2::ZERO,
                    fire: false,
                },
            },
            // R101 S3 — Follow arrives at the leader's position via the controller
            // (the goal is the leader pos; no formation slot — that is
            // FormationKeep). Within ARRIVE_RADIUS the controller parks (v_des = 0),
            // so a follower settles onto its leader instead of orbiting through it.
            // No `Arrived` event: a follow goal is the moving leader, never
            // "reached" the way a static waypoint is.
            Behavior::Follow => match brain.leader.and_then(|l| others.get(l).ok()) {
                Some((lpos, _, _)) => {
                    let to_leader = lpos.0 - pos.0;
                    let intent = if to_leader.length() <= ARRIVE_RADIUS {
                        park_intent() // on the leader: station-keep (brake to rest)
                    } else {
                        nav_intent(lpos.0)
                    };
                    ChannelIntent {
                        intent,
                        aim_dir: to_leader.normalize_or_zero(),
                        fire: false,
                    }
                }
                None => ChannelIntent {
                    intent: ShipIntent::default(), // Leader gone: sweep/think clean up.
                    aim_dir: Vec2::ZERO,
                    fire: false,
                },
            },
            // R101 S4 — FormationKeep now drives through the unified controller.
            // The velocity-matching slot math is unchanged (`formation_desired_vel`
            // = the EXACT `desired_vel` the legacy `formation_keep` computed
            // internally): emit `v_des` = leader vel + slot-error closing term,
            // capped at the pace `v_max` so a far-off follower closes no faster
            // than nav, then `allocate_intent` aligns thrust with the accel needed
            // to track it. `Facing::Free` (consistent with the nav arms) lets the
            // follower point where it's GOING and brake naturally onto its slot
            // (flip-and-burn emerges if it overshoots) — leader-facing is NOT
            // required for slot-holding here, and Free keeps the parked-no-spin
            // settle that the no-chatter VC2 test depends on. On-slot with matched
            // velocity `v_des == leader_vel` and the controller emits a quiet,
            // velocity-matched intent (no oscillation). The uniform `throttle_cap`
            // multiply + the `ai_debug` capture below the match apply to the
            // result, so no per-arm cap/capture is needed. Leader/slot missing →
            // default coast, exactly as before (the sweep/think clean up).
            Behavior::FormationKeep => {
                match (
                    brain.leader.and_then(|l| others.get(l).ok()),
                    brain.formation_slot,
                ) {
                    (Some((lpos, lvel, lheading)), Some(slot)) => {
                        let mut v_des =
                            formation_desired_vel(pos.0, lpos.0, lvel.0, lheading.0, slot);
                        if v_des.length() > nav_v_max {
                            v_des = v_des.normalize_or_zero() * nav_v_max;
                        }
                        ChannelIntent {
                            intent: allocate_intent(
                                MoveCmd {
                                    v_des,
                                    facing: Facing::Free,
                                },
                                vel.0,
                                heading.0,
                                cstats,
                                nav_tau,
                            ),
                            aim_dir: v_des.normalize_or_zero(),
                            fire: false,
                        }
                    }
                    _ => ChannelIntent {
                        intent: ShipIntent::default(),
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    },
                }
            }
            // T025 combat arms (see system docs); target gone → zero intent
            // (the V-1 sweep clears the ref; the next think degrades).
            Behavior::Engage => {
                match brain
                    .target
                    .and_then(|t| others.get(t).ok().map(|(p, v, h)| (t, p, v, h)))
                {
                    Some((tentity, tpos, tvel, theading)) => {
                        // R101 S5 — the combat arm drives the UNIFIED controller:
                        // `combat_intent` expresses the resolved stance as a desired
                        // velocity with the nose pinned to the gunnery lead
                        // (`Facing::Aim`), so the same `allocate_intent` law that
                        // powers nav/formation also powers combat (combat motion is
                        // O(1) — no per-stance ContextMap build). The aim point is the
                        // target's core-cell KILL region (`combat_aim_pos`), not the
                        // bare `Position`, so the gun bores toward what kills it (the
                        // stationary-target fix). FIRE is decided by the shared
                        // `fire_decision` overlay after the match, reading the ACTUAL
                        // heading the controller turned to.
                        let aim_pos = combat_aim_pos(tentity, tpos.0, theading.0);
                        let intent = combat_intent(combat_stance, tpos.0, aim_pos, tvel.0);
                        ChannelIntent {
                            intent,
                            aim_dir: engage_aim_dir(pos.0, stats, aim_pos, tvel.0),
                            fire: true, // Engage is in the fire allowlist.
                        }
                    }
                    None => ChannelIntent {
                        intent: ShipIntent::default(),
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    },
                }
            }
            // R101 S6 — Evade through the UNIFIED controller. MOVE = the
            // away-deflected break-off flee direction (`evade_move_dir`), scaled to
            // the profile pace by `survival_intent`. FACING = `Aim(threat)` when a
            // hostile exists AND the ship is ARMED, so the nose tracks the pursuer
            // and the gun can fire WHILE juking (a forward-only hull retro-drifts
            // facing the threat, a `can_strafe` hull sidles — both EMERGE from the
            // controller). An UNARMED evader (the Scout-superior-threat case) has
            // no gun to bear, so it runs BLIND (`Facing::Free`: nose along the
            // flee `v_des`) — it still opens range + survives. FIRE is weapons-free
            // (the overlay below decides armed + posture + in-range + aligned).
            // Threat-less → default coast.
            Behavior::Evade => {
                let self_armed = stats.is_some_and(|s| s.can_fire);
                match threat.and_then(|(tpos, _)| evade_move_dir(pos.0, tpos, &ai)) {
                    Some(flee_dir) => {
                        let facing = match threat_aim {
                            Some(aim) if self_armed => Facing::Aim(aim),
                            _ => Facing::Free,
                        };
                        let aim_dir = match facing {
                            Facing::Aim(d) => d,
                            Facing::Free => flee_dir.normalize_or_zero(),
                        };
                        ChannelIntent {
                            intent: survival_intent(flee_dir, facing),
                            aim_dir,
                            fire: true,
                        }
                    }
                    None => ChannelIntent {
                        intent: ShipIntent::default(),
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    },
                }
            }
            // R101 S6 — Retreat through the UNIFIED controller. MOVE = the flee
            // direction (toward `brain.home` when anchored, else directly AWAY
            // from the nearest hostile), scaled to the profile pace by
            // `survival_intent`. FACING = `Aim(threat)` so the nose STAYS ON the
            // pursuer while the ship withdraws — this is the FIGHTING RETREAT: the
            // controller, asked to open range (`v_des` away/home) while the nose
            // holds the threat, reverse-brakes (forward-only hull, retro nose-on)
            // or strafes (`can_strafe` hull) to realize the opening velocity, so
            // the range grows WHILE the gun bears and fires — no dedicated
            // FightingRetreat behavior. With no threat the withdrawal runs blind
            // (`Facing::Free`, nose along the flee `v_des`) and never fires (no AIM
            // target). With no threat AND no home → default coast (think degrades).
            Behavior::Retreat => {
                let flee_dir = match brain.home {
                    Some(home) => (home - pos.0).normalize_or_zero(),
                    None => threat
                        .map(|(tpos, _)| (pos.0 - tpos).normalize_or_zero())
                        .unwrap_or(Vec2::ZERO),
                };
                if flee_dir == Vec2::ZERO {
                    ChannelIntent {
                        intent: ShipIntent::default(),
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    }
                } else {
                    let facing = match threat_aim {
                        Some(aim) => Facing::Aim(aim),
                        None => Facing::Free,
                    };
                    let aim_dir = match facing {
                        Facing::Aim(d) => d,
                        Facing::Free => flee_dir.normalize_or_zero(),
                    };
                    ChannelIntent {
                        intent: survival_intent(flee_dir, facing),
                        aim_dir,
                        // Weapons-free only when a hostile exists to bear on; a
                        // pure run-home with no threat never fires (no AIM target).
                        fire: threat.is_some(),
                    }
                }
            }
            // T027 Ram (R101 S8 — THE FINAL ARM onto the unified controller; with
            // this `allocate_intent` is the SOLE motion composer). A ram is a
            // full-throttle lead-pursuit COLLISION course: emit `v_des` toward the
            // target's intercept point (the SAME L1 lead `pursue_intercept` gives,
            // shared with `turret::aim_angle`), scaled to the ship's TOP speed —
            // a ram WANTS maximum closing speed. `Facing::Free` lets the nose point
            // where the thrust goes (along `v_des` = at the intercept = INTO the
            // target), so the strong forward drive does the closing.
            //
            // NO `stoppable_speed` cap and NO `deflect_v_des`: a ram is a DELIBERATE
            // collision — it must NOT brake before impact (the cap would arrive-and-
            // park short of the target) and it must NOT swerve around bodies between
            // it and its target (the legacy arm avoided neither, matched here). FIRE
            // is preserved (Ram is in the allowlist — a rammer also fires as it
            // closes). The uniform `throttle_cap` multiply + `ai_debug` capture
            // below the match apply to the result, as for every other arm.
            Behavior::Ram => match brain.target.and_then(|t| others.get(t).ok()) {
                Some((tpos, tvel, _)) => {
                    let top = stats.map_or(0.0, ShipStats::top_speed);
                    let dir = pursue_intercept(pos.0, top, tpos.0, tvel.0);
                    let v_des = dir * top;
                    ChannelIntent {
                        intent: allocate_intent(
                            MoveCmd {
                                v_des,
                                facing: Facing::Free,
                            },
                            vel.0,
                            heading.0,
                            cstats,
                            nav_tau,
                        ),
                        aim_dir: dir,
                        fire: true, // Ram is in the fire allowlist (a finisher).
                    }
                }
                None => ChannelIntent {
                    intent: ShipIntent::default(),
                    aim_dir: Vec2::ZERO,
                    fire: false,
                },
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
                    ChannelIntent {
                        intent: park_intent(), // brake to rest ON the coverage leg
                        aim_dir: Vec2::ZERO,
                        fire: false,
                    }
                }
                Some(goal) => ChannelIntent {
                    intent: nav_intent(goal),
                    aim_dir: (goal - pos.0).normalize_or_zero(),
                    fire: false,
                },
                None => ChannelIntent {
                    intent: ShipIntent::default(), // Goal-less: the think degrades it.
                    aim_dir: Vec2::ZERO,
                    fire: false,
                },
            },
        };
        // Stage C: each arm now composes its MOVE+AIM inline through the unified
        // `allocate_intent` controller (R101 S8 — every arm, including Ram; the
        // survival arms decouple the SEPARATE aim via `Facing::Aim`), so `aim_dir`
        // is the recorded facing — bound to `_` (the composition already consumed it).
        let ChannelIntent {
            intent: mut next,
            aim_dir: _aim_dir,
            fire: fire_channel,
        } = channel;
        // R97 Phase 1 Stage C — WEAPONS-FREE fire (TR-011): the FIRE channel is
        // set by any arm that has a hostile AIM target — Engage/Ram (allowlist),
        // AND Retreat/Evade (the survival arms now bear their gun on the nearest
        // hostile while fleeing). `fire_decision` owns the rest: armed (unarmed →
        // None), in weapon range, aligned to the lead (`cos > FIRE_ALIGN_COS`),
        // and the energy/heat gates MIRRORING `weapon_fire_system` (so a gated
        // ship still CHOOSES not to fire — `gated_ship_never_fires` holds). This
        // is what makes a FIGHTING RETREAT emerge: a Retreat/Evade ship fires on
        // the ticks its nose tracks onto the pursuer. Hold/Patrol/Waypoint/Follow/
        // FormationKeep/Scout/Sweep set `fire == false` (no hostile AIM target).
        // T032 (TR-015): the scenario posture STILL gates the trigger —
        // HoldFire NEVER fires, DefensiveOnly only inside its fired-upon window —
        // unchanged. The fire SOLVE uses the SAME resolved `threat` the AIM
        // channel faces (`brain.target` kinematics, else the nearest contact's
        // last position at zero velocity), so the gun fires where the nose points.
        let posture_ok = role.is_none_or(|r| r.allows_engage(now));
        if fire_channel && posture_ok {
            if let Some((tpos, tvel)) = threat {
                // R101 S5 — fire at the KILL aim point too: for an Engage target with
                // hull geometry the gun SOLVES on the SAME projected + raked kill
                // point the nose tracks (`combat_aim_pos`), so the alignment gate is
                // honest and the bored line sweeps where it kills. Survival arms (and
                // an Engage target with no `FitLayout`) keep the body-centre solve.
                let fire_pos = if matches!(brain.behavior, Behavior::Engage) {
                    brain
                        .target
                        .and_then(|t| {
                            others
                                .get(t)
                                .ok()
                                .map(|(_, _, h)| combat_aim_pos(t, tpos, h.0))
                        })
                        .unwrap_or(tpos)
                } else {
                    tpos
                };
                if let Some(group) = fire_decision(
                    pos.0, heading.0, stats, weapons, groups, energy, heat, fire_pos, tvel, &sim,
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

        // R97 Phase 1 Stage D — per-channel capture (ai_debug only): record this
        // tick's MOVE/AIM/FIRE resolution + the collision imminence so a
        // playtester can SEE why the ship chose its action. Compiled OUT of
        // headless/bench (zero TR-017-path cost). Computed AFTER the fire overlay
        // so the captured `fire`/reason match the intent actually emitted.
        #[cfg(feature = "ai_debug")]
        {
            use debug_capture::{AimDrive, ChannelDebug, FireReason};
            // MOVE channel (world frame): the flight model applies body-frame
            // thrust `(forward, strafe)` rotated by heading; capture that as the
            // resolved travel direction, and `forward` (pre-cap value is folded
            // into `next.forward` already) as the throttle.
            let move_throttle = next.forward;
            let move_dir = Vec2::from_angle(heading.0).rotate(Vec2::new(next.forward, next.strafe));
            // AIM drive: by arm — combat aims at the target lead, survival arms
            // at the threat, movement arms along the move interest. `Vec2::ZERO`
            // recorded aim = a held/coasting arm (None).
            let aim_drive = if _aim_dir == Vec2::ZERO {
                AimDrive::None
            } else {
                match brain.behavior {
                    Behavior::Engage | Behavior::Ram => AimDrive::Target,
                    Behavior::Evade | Behavior::Retreat => {
                        // R101 S6 — the survival arms record `Threat` ONLY when the
                        // nose was actually PINNED to the threat (`Facing::Aim`): a
                        // Retreat with a hostile, or an ARMED Evade with a hostile.
                        // An UNARMED Evade (or a threat-less withdrawal) runs
                        // `Facing::Free` with the nose along the flee `v_des`, so its
                        // aim is the MOVE direction — label it honestly. `threat_aim`
                        // is the pinned direction; the recorded `_aim_dir` equals it
                        // exactly on the `Aim` path.
                        if threat_aim.is_some_and(|a| a == _aim_dir) {
                            AimDrive::Threat
                        } else {
                            AimDrive::Move
                        }
                    }
                    _ => AimDrive::Move,
                }
            };
            // FIRE reason: walk the same gate ladder `fire_decision` did.
            let reason = if !fire_channel {
                FireReason::NoTarget
            } else if !posture_ok {
                FireReason::PostureBlocked
            } else if let Some((tpos, tvel)) = threat {
                fire_reason(
                    pos.0, heading.0, stats, weapons, groups, energy, heat, tpos, tvel, &sim,
                )
            } else {
                FireReason::NoTarget
            };
            // Collision imminence: the dominant value over the in-range obstacles
            // (the Stage-D move-drive override's input); 0 when no field/obstacle.
            let collision_imminence = field.map_or(0.0, |f| {
                obstacle_imminence(f, pos.0, vel.0, own_radius, &ai)
            });
            debug_capture::capture_channels(
                &mut captures,
                &mut commands,
                entity,
                ChannelDebug {
                    move_dir,
                    move_throttle,
                    aim_dir: _aim_dir,
                    aim_drive,
                    fire: next.fire_primary,
                    fire_reason: reason,
                    collision_imminence,
                },
            );
        }

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
    use glam::Vec2;

    use super::{AiTuning, Behavior};

    /// R97 Phase 1 Stage D — which aim-drive won the AIM channel (so a
    /// playtester can SEE where a ship is pointing its gun and WHY). Recorded by
    /// `ai_execute_system` alongside the resolved aim direction.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum AimDrive {
        /// No aim this tick — a held/coasting arm (`aim_dir == Vec2::ZERO`).
        #[default]
        None,
        /// The aim follows the MOVE interest direction (movement arms:
        /// Waypoint/Patrol/Follow/FormationKeep/Scout/Sweep).
        Move,
        /// The aim is the combat gunnery LEAD on `brain.target` (Engage/Ram).
        Target,
        /// The aim is the nearest-hostile THREAT facing (the survival arms
        /// Evade/Retreat bear their gun on the pursuer while fleeing).
        Threat,
    }

    /// R97 Phase 1 Stage D — why the FIRE channel did (or did not) pull the
    /// trigger this tick, mirroring `fire_decision`'s gate ladder so the dev
    /// panel can explain a non-firing ship. Recorded by `ai_execute_system`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum FireReason {
        /// The arm has no hostile AIM target to bear on (Hold/Patrol/Waypoint/
        /// Follow/FormationKeep/Scout/Sweep, or a threat-less Retreat).
        #[default]
        NoTarget,
        /// The scenario posture suppressed fire (HoldFire always; DefensiveOnly
        /// outside its fired-upon window).
        PostureBlocked,
        /// Unarmed / unfitted — nothing to fire (`fire_decision` armed gate).
        Unarmed,
        /// The target is outside the weapon envelope (range gate).
        OutOfRange,
        /// The gun is not pointed at the lead solution (`cos ≤ FIRE_ALIGN_COS`).
        NotAligned,
        /// Out of energy: the AI CHOSE not to fire (mirrors `weapon_fire_system`).
        NoEnergy,
        /// Overheated: the AI CHOSE not to fire (mirrors `weapon_fire_system`).
        Overheated,
        /// All gates passed — the trigger was pulled (the selected fire group).
        Fired,
    }

    /// Per-brain capture of the LAST completed think (component on the brain
    /// entity, inserted lazily by the first captured think): the dev panel's
    /// score-breakdown source (AD-006 — "without a score-breakdown view,
    /// tuning is blind"). Pure observability: nothing in the sim reads it.
    ///
    /// **Two writers, disjoint fields** (R97 Phase 1 Stage D): `ai_think_system`
    /// writes the SELECTION fields ([`Self::last_scores`]/[`Self::winner`]/
    /// [`Self::momentum_applied`]/[`Self::transitions`]) via [`Self::record`];
    /// `ai_execute_system` writes the per-CHANNEL fields ([`Self::move_dir`] …
    /// [`Self::collision_imminence`]) via [`Self::record_channels`] EVERY tick
    /// (the execute arm runs per-tick, not on the think cadence), so a
    /// playtester sees the live action a ship chose even between thinks. Each
    /// writer touches only its own fields, so the two never race.
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
        // --- Per-channel capture (Stage D; written by `ai_execute_system`) ---
        /// MOVE channel: the resolved world-frame translate direction this tick.
        pub move_dir: Vec2,
        /// MOVE channel: the resolved throttle this tick (post-cap, the value
        /// composed into the intent's forward magnitude before the squad cap).
        pub move_throttle: f32,
        /// AIM channel: the resolved facing direction this tick (`Vec2::ZERO` =
        /// no aim — a held/coasting arm).
        pub aim_dir: Vec2,
        /// AIM channel: which drive won the facing (target / threat / move).
        pub aim_drive: AimDrive,
        /// FIRE channel: whether the trigger was pulled this tick.
        pub fire: bool,
        /// FIRE channel: WHY fire was / was not taken (the gate that decided).
        pub fire_reason: FireReason,
        /// The dominant collision-imminence value `[0, 1]` over the in-range
        /// obstacles this tick (`obstacle_imminence`): `0` = no imminent crash,
        /// `1` = collision now. The Stage-D move-drive override's input.
        pub collision_imminence: f32,
    }

    impl AiDebugCapture {
        /// Fold one completed think into the capture (SELECTION fields only).
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

        /// R97 Phase 1 Stage D — fold one tick's per-CHANNEL resolution into the
        /// capture (the MOVE/AIM/FIRE fields + the collision imminence). Called
        /// by `ai_execute_system` every tick; touches none of the selection
        /// fields `record` owns.
        #[allow(clippy::too_many_arguments)] // Mirrors the execute arm's channels 1:1.
        fn record_channels(
            &mut self,
            move_dir: Vec2,
            move_throttle: f32,
            aim_dir: Vec2,
            aim_drive: AimDrive,
            fire: bool,
            fire_reason: FireReason,
            collision_imminence: f32,
        ) {
            self.move_dir = move_dir;
            self.move_throttle = move_throttle;
            self.aim_dir = aim_dir;
            self.aim_drive = aim_drive;
            self.fire = fire;
            self.fire_reason = fire_reason;
            self.collision_imminence = collision_imminence;
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

    /// R97 Phase 1 Stage D — the per-tick MOVE/AIM/FIRE + collision-imminence
    /// resolution `ai_execute_system` hands to [`AiDebugCapture::record_channels`]
    /// (bundled so the capture call stays one argument, not seven).
    #[derive(Clone, Copy, Debug, Default)]
    pub(super) struct ChannelDebug {
        pub move_dir: Vec2,
        pub move_throttle: f32,
        pub aim_dir: Vec2,
        pub aim_drive: AimDrive,
        pub fire: bool,
        pub fire_reason: FireReason,
        pub collision_imminence: f32,
    }

    /// Populate (insert-or-update) the brain entity's [`AiDebugCapture`] with one
    /// tick's per-channel resolution — called by `ai_execute_system` under the
    /// feature cfg, EVERY tick (the execute arm is per-tick, not on the think
    /// cadence). The first capture for an entity that has never thought inserts
    /// the component via `Commands`; otherwise it updates the channel fields in
    /// place, leaving the selection fields (the think writer's) untouched.
    pub(super) fn capture_channels(
        captures: &mut Query<&mut AiDebugCapture>,
        commands: &mut Commands,
        entity: Entity,
        ch: ChannelDebug,
    ) {
        if let Ok(mut capture) = captures.get_mut(entity) {
            capture.record_channels(
                ch.move_dir,
                ch.move_throttle,
                ch.aim_dir,
                ch.aim_drive,
                ch.fire,
                ch.fire_reason,
                ch.collision_imminence,
            );
        } else {
            let mut capture = AiDebugCapture::default();
            capture.record_channels(
                ch.move_dir,
                ch.move_throttle,
                ch.aim_dir,
                ch.aim_drive,
                ch.fire,
                ch.fire_reason,
                ch.collision_imminence,
            );
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

    /// R97 Phase 1 Stage D — the collision-imminence consideration is bounded
    /// `[0, 1]`, MONOTONE DECREASING in `ttc_norm` (a nearer collision scores
    /// higher), pinned at the endpoints (`ttc_norm = 0` → 1 "impact now",
    /// `ttc_norm = 1` → 0 "a horizon away"), and never NaN — for in-range AND
    /// out-of-range inputs (the curve clamps).
    #[test]
    fn collision_imminence_curve_is_bounded_monotone_and_finite() {
        // Endpoints: about-to-hit scores maximal, a full horizon away scores 0.
        assert_eq!(con_collision_imminence(0.0), 1.0, "impact now → 1");
        assert_eq!(con_collision_imminence(1.0), 0.0, "a horizon away → 0");
        // Bounded + finite + monotone decreasing across a dense sweep, including
        // out-of-range inputs (which clamp, never NaN).
        let mut prev = f32::INFINITY;
        for i in -2..=12 {
            let x = i as f32 / 10.0; // −0.2 … 1.2 (spans the clamp edges).
            let y = con_collision_imminence(x);
            assert!(y.is_finite(), "imminence is finite at ttc_norm {x}");
            assert!(
                (0.0..=1.0).contains(&y),
                "imminence in [0,1] at {x} (got {y})"
            );
            assert!(
                y <= prev + 1e-7,
                "monotone non-increasing in ttc_norm at {x}"
            );
            prev = y;
        }
        // A near-impact scores strictly higher than a distant one.
        assert!(
            con_collision_imminence(0.1) > con_collision_imminence(0.9),
            "a nearer collision is more imminent"
        );
    }

    /// R97 Phase 1 Stage D — the TTC prep helpers feeding the curve: a ship
    /// closing head-on gets a finite, correctly-signed time-to-closest-approach
    /// and a closest-approach distance under the avoid ring; a diverging /
    /// non-closing pair gets the `INFINITY` sentinel (mapped to zero imminence),
    /// and no path produces a NaN.
    #[test]
    fn ttc_helpers_close_in_finite_and_diverge_to_infinity() {
        // A stationary obstacle 100 u dead-ahead (+X); the ship flies at +X 50 u/s
        // → relative velocity of the obstacle toward the ship is −50 u/s in X.
        let rel_pos = Vec2::new(100.0, 0.0);
        let rel_vel = Vec2::new(-50.0, 0.0); // = -ship_vel for ship_vel = (50, 0).
        let ttc = time_to_closest_approach(rel_pos, rel_vel);
        assert!(
            ttc.is_finite() && (ttc - 2.0).abs() < 1e-4,
            "head-on closes in 2 s"
        );
        assert!(
            closest_approach_dist(rel_pos, rel_vel) < 1e-3,
            "a dead-ahead head-on passes through the centre (miss ≈ 0)"
        );
        // Diverging (ship moving AWAY from the obstacle) → no future approach.
        let diverging = time_to_closest_approach(rel_pos, Vec2::new(50.0, 0.0));
        assert!(diverging.is_infinite(), "a diverging pair never closes");
        // The diverging closest-approach clamps to the present separation (no NaN,
        // no fictitious past approach).
        let miss = closest_approach_dist(rel_pos, Vec2::new(50.0, 0.0));
        assert!(miss.is_finite() && (miss - 100.0).abs() < 1e-4);
        // Zero relative velocity → never closes (guarded denominator, no NaN).
        assert!(time_to_closest_approach(rel_pos, Vec2::ZERO).is_infinite());
    }

    /// R97 Phase 1 Stage D — `obstacle_imminence` over the field: a stationary
    /// obstacle the ship is flying STRAIGHT INTO scores high imminence as the
    /// time-to-impact falls inside the horizon; the SAME obstacle when the ship
    /// is diverging (or parked) scores ZERO (the two-layer split's input — only
    /// a closing, near-miss obstacle drives the preemptive override). Pure, no
    /// world needed.
    #[test]
    fn obstacle_imminence_rises_closing_and_zero_diverging() {
        let tuning = AiTuning::default();
        let obs_radius = 50.0;
        let own_radius = 4.0;
        // The obstacle sits within the horizon at the ship's speed: ship at the
        // origin, obstacle ahead at +X, distance chosen so ttc ≈ 0.5·horizon.
        let speed = 80.0;
        let dist = speed * tuning.collision_horizon_s * 0.5; // ttc ≈ 0.5·horizon
        let field = ObstacleField {
            obstacles: vec![(Vec2::new(dist, 0.0), obs_radius)],
        };
        // Flying STRAIGHT into it → a positive, dominant imminence.
        let closing = obstacle_imminence(
            &field,
            Vec2::ZERO,
            Vec2::new(speed, 0.0),
            own_radius,
            &tuning,
        );
        assert!(
            closing > 0.0,
            "a ship closing on an obstacle is imminent (got {closing})"
        );
        // Parked (zero velocity) → never closes → zero.
        assert_eq!(
            obstacle_imminence(&field, Vec2::ZERO, Vec2::ZERO, own_radius, &tuning),
            0.0,
            "a stationary ship has no collision pressure"
        );
        // Flying AWAY (diverging) → zero.
        assert_eq!(
            obstacle_imminence(
                &field,
                Vec2::ZERO,
                Vec2::new(-speed, 0.0),
                own_radius,
                &tuning
            ),
            0.0,
            "a diverging ship has no collision pressure"
        );
        // A glancing pass that clears the avoid ring (offset far on +Y, moving +X)
        // → the closest approach exceeds the ring → zero imminence.
        let glancing = ObstacleField {
            obstacles: vec![(Vec2::new(dist, 10_000.0), obs_radius)],
        };
        assert_eq!(
            obstacle_imminence(
                &glancing,
                Vec2::ZERO,
                Vec2::new(speed, 0.0),
                own_radius,
                &tuning
            ),
            0.0,
            "an obstacle the ship will cleanly miss is not imminent"
        );
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

    // --- R96 Part C: combat stances --------------------------------------

    /// The `perp` helper is the +90° (CCW) rotation in the `0 = +X`, CCW heading
    /// convention: `perp(+X) = +Y` (LEFT), `perp(+Y) = -X`. So `+perp` is the
    /// CCW orbit tangent (strafe LEFT) and `-perp` the CW one (strafe RIGHT).
    #[test]
    fn orbit_tangent_is_perpendicular_with_correct_sign() {
        // +perp rotates +X CCW to +Y (a quarter turn left).
        assert!((perp(Vec2::X) - Vec2::Y).length() < 1e-6);
        assert!((perp(Vec2::Y) - Vec2::NEG_X).length() < 1e-6);
        // Perpendicular to its input and unit-length-preserving.
        let v = Vec2::from_angle(0.9);
        assert!(perp(v).dot(v).abs() < 1e-6, "perp ⟂ input");
        assert!((perp(v).length() - v.length()).abs() < 1e-6);
        // The orbit tangent rotates the OUTWARD radial (-dir_to) by ±90°. The
        // CCW/CW tangents are exact opposites — opposite circulation.
        let dir_to = Vec2::from_angle(-0.4); // ship→target.
        let outward = -dir_to; // target→ship (the orbit radius).
        let ccw_tan = -perp(dir_to); // = perp(outward).
        let cw_tan = perp(dir_to);
        assert!(
            (ccw_tan + cw_tan).length() < 1e-6,
            "ccw tangent = −cw tangent"
        );
        // The CCW tangent is +90° (LEFT) of the OUTWARD radial → bearing increases.
        assert!(
            crate::ai::steering::wrap_angle(
                ccw_tan.to_angle() - outward.to_angle() - std::f32::consts::FRAC_PI_2
            )
            .abs()
                < 1e-5,
            "ccw tangent is +90° from the outward radial (bearing increases)"
        );
    }

    // R101 S5 — RETIRED `charge_stance_matches_legacy_engage_motion`. Its premise
    // (the Charge `engage_motion` body is BIT-identical to the pre-R96-C
    // context-map range-band controller) no longer holds: the Engage arm now drives
    // the unified `combat_intent` desired-velocity controller, and the legacy
    // `engage_motion`/`engage_channels` were removed as dead code this stage. The
    // tactical Charge property (the brawler closes, aims, kills) is covered by the
    // behavioral `engage_closes_aims_and_destroys_within_budget` integration test.

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

    // --- R97 Stage A: consideration library (strict f32) ---

    /// Every consideration is monotone in the documented direction, stays in
    /// `[0, 1]`, clamps degenerate inputs, and is bit-stable across calls.
    #[test]
    fn considerations_have_documented_shape_and_range() {
        // con_health: rises with hull (linear pass-through), clamped.
        assert_eq!(con_health(0.0), 0.0);
        assert_eq!(con_health(1.0), 1.0);
        assert!(con_health(0.7) > con_health(0.3));
        assert_eq!(con_health(2.0), 1.0, "clamps over-range");

        // con_health_low: near 0 healthy, ramps as hull bleeds (quadratic of inv).
        assert!(con_health_low(1.0).abs() < 1e-6, "healthy → ~0 pressure");
        assert_eq!(con_health_low(0.0), 1.0, "dead → full pressure");
        assert!(
            con_health_low(0.2) > con_health_low(0.5),
            "more hurt → more"
        );

        // con_energy: FLOORED at 0.3, full at 1, smooth between.
        assert!(
            (con_energy(0.0) - 0.3).abs() < 1e-6,
            "floored so it never vetoes"
        );
        assert!((con_energy(1.0) - 1.0).abs() < 1e-6);
        assert!(con_energy(0.8) > con_energy(0.2), "rises with energy");
        assert!((0.3..=1.0).contains(&con_energy(0.5)));

        // con_heat_ok: cool → ~1, hot → low (floored inverse, never < 0).
        assert_eq!(con_heat_ok(0.0), 1.0, "cool → full");
        assert_eq!(con_heat_ok(1.0), 0.0, "max heat → 0 headroom");
        assert!(con_heat_ok(0.2) > con_heat_ok(0.8));

        // con_in_band: tent peaked at range_norm == 1 (on the ring).
        assert!((con_in_band(1.0) - 1.0).abs() < 1e-6, "on the ring → peak");
        assert_eq!(con_in_band(0.0), 0.0, "too close → 0");
        assert_eq!(con_in_band(2.0), 0.0, "too far → 0");
        assert!((con_in_band(0.5) - 0.5).abs() < 1e-6, "half-band off → 0.5");
        assert_eq!(con_in_band(5.0), 0.0, "way off clamps to 0, not negative");

        // con_target_vulnerable: rises as the TARGET weakens (inverse).
        assert_eq!(con_target_vulnerable(0.0), 1.0, "hulk → max desire");
        assert_eq!(con_target_vulnerable(1.0), 0.0, "pristine → none");
        assert!(con_target_vulnerable(0.1) > con_target_vulnerable(0.9));

        // con_incoming_threat: S-curve, 0 at no threat, 1 at full.
        assert_eq!(con_incoming_threat(0.0), 0.0);
        assert_eq!(con_incoming_threat(1.0), 1.0);
        assert!(con_incoming_threat(0.7) > con_incoming_threat(0.3));

        // con_collision_imminence: high when ttc small (about to hit).
        assert_eq!(con_collision_imminence(0.0), 1.0, "about to hit → max");
        assert!(
            con_collision_imminence(1.0).abs() < 1e-6,
            "a horizon away → ~0"
        );
        assert!(
            con_collision_imminence(0.2) > con_collision_imminence(0.8),
            "nearer impact → higher"
        );

        // con_weapon_ready: product of three gates; any zero gate zeroes it.
        assert_eq!(con_weapon_ready(1.0, 1.0, 1.0), 1.0);
        assert_eq!(con_weapon_ready(0.0, 1.0, 1.0), 0.0, "mis-aimed → 0");
        assert_eq!(con_weapon_ready(1.0, 0.0, 1.0), 0.0, "no energy → 0");
        assert!(
            (con_weapon_ready(0.5, 0.5, 0.5) - 0.125).abs() < 1e-6,
            "product"
        );
        assert_eq!(
            con_weapon_ready(2.0, 2.0, 2.0),
            1.0,
            "clamps over-range gates"
        );

        // con_momentum_align: S-curve, low when fighting inertia, high in tune.
        assert_eq!(con_momentum_align(0.0), 0.0, "opposed → 0");
        assert_eq!(con_momentum_align(1.0), 1.0, "aligned → 1");
        assert!(con_momentum_align(0.7) > con_momentum_align(0.3));

        // Strict-f32: identical inputs → identical bits (determinism).
        assert_eq!(con_energy(0.5).to_bits(), con_energy(0.5).to_bits());
        assert_eq!(con_in_band(0.5).to_bits(), con_in_band(0.5).to_bits());
    }

    // --- R97 Stage A: physics-scalar prep helpers (outside the markers) ---

    /// TCA sign: a converging pair gives a positive FINITE time; a diverging
    /// (or non-converging) pair gives INFINITY (the "no pressure" sentinel).
    #[test]
    fn time_to_closest_approach_sign_and_sentinel() {
        // Other at +X 100 u, closing along -X at 10 u/s → meets in 10 s.
        let tca = time_to_closest_approach(Vec2::new(100.0, 0.0), Vec2::new(-10.0, 0.0));
        assert!(
            tca.is_finite() && (tca - 10.0).abs() < 1e-4,
            "converging → +finite"
        );
        // Same geometry but receding (+X) → never closes → INFINITY.
        assert_eq!(
            time_to_closest_approach(Vec2::new(100.0, 0.0), Vec2::new(10.0, 0.0)),
            f32::INFINITY,
            "diverging → INFINITY"
        );
        // Zero relative velocity → no future approach → INFINITY (no NaN divide).
        assert_eq!(
            time_to_closest_approach(Vec2::new(5.0, 0.0), Vec2::ZERO),
            f32::INFINITY
        );
    }

    /// Closest-approach distance on a crossing geometry: a body passing
    /// perpendicular at offset `d` has a miss distance of exactly `d`.
    #[test]
    fn closest_approach_dist_on_a_crossing() {
        // Other at (0, 3) moving +X across our path (we sit at origin, still):
        // r = (0, 3), v = (10, 0). Closest approach holds the y-offset → 3.
        let d = closest_approach_dist(Vec2::new(0.0, 3.0), Vec2::new(10.0, 0.0));
        assert!(
            (d - 3.0).abs() < 1e-4,
            "perpendicular crossing misses by 3 (got {d})"
        );
        // Head-on closing toward the origin reaches distance ~0 at TCA.
        let head_on = closest_approach_dist(Vec2::new(50.0, 0.0), Vec2::new(-10.0, 0.0));
        assert!(head_on.abs() < 1e-3, "head-on closes to ~0 (got {head_on})");
        // Diverging → reports the PRESENT separation (clamped at t = 0), never NaN.
        let div = closest_approach_dist(Vec2::new(4.0, 0.0), Vec2::new(10.0, 0.0));
        assert!(
            (div - 4.0).abs() < 1e-4,
            "diverging → present range (got {div})"
        );
    }

    /// Stopping distance is monotone in speed, scales inversely with decel, and
    /// floors the denominator (zero decel → finite, never NaN/∞-divide).
    #[test]
    fn stopping_distance_is_monotonic_and_safe() {
        assert_eq!(
            stopping_distance(0.0, 10.0),
            0.0,
            "stationary stops instantly"
        );
        // v²/(2a): 20²/(2·10) = 20.
        assert!((stopping_distance(20.0, 10.0) - 20.0).abs() < 1e-4);
        assert!(
            stopping_distance(40.0, 10.0) > stopping_distance(20.0, 10.0),
            "faster → longer stop"
        );
        assert!(
            stopping_distance(20.0, 5.0) > stopping_distance(20.0, 20.0),
            "weaker brake → longer stop"
        );
        // Finite for any positive decel; zero/negative decel floors the
        // denominator → the well-defined "+∞ can-never-stop" answer, NEVER NaN.
        assert!(
            stopping_distance(20.0, 5.0).is_finite(),
            "positive decel → finite"
        );
        assert!(
            !stopping_distance(20.0, 0.0).is_nan(),
            "zero decel → no NaN"
        );
        assert!(
            !stopping_distance(20.0, -5.0).is_nan(),
            "negative decel → no NaN"
        );
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
    ///
    /// **R101 S5 TUNE**: the mechanism changed with the unified controller. The
    /// Brawler (Charge) still burns FORWARD toward the target. The Kiter (Kite,
    /// inside its ring) now opens range with its nose PINNED on the target
    /// (`Facing::Aim(lead)`) — so it REVERSE-thrusts (`forward < 0`) instead of
    /// turning the body around to flee (old context-map `turn == ±1`, `forward
    /// 0`). The defining opposite-intent property — brawler accelerates IN, kiter
    /// accelerates OUT — holds; the new kiter additionally keeps its gun on
    /// target while opening (the intended kite feel).
    #[test]
    fn brawler_closes_where_kiter_opens_range() {
        let range = weapon_range(Some(&fighter_stats())).expect("armed fighter");
        let dist = range * 0.5; // Inside the brawler's 0.3·R band and the kite ring.
        assert!(
            range_band_radial(
                dist,
                standoff_distance(FitArchetype::Brawler, range),
                RANGE_BAND_FRAC
            ) > 0.0,
            "brawler radial: too far → close in"
        );
        // The kiter holds `kite_range_frac · range` (1.1·R); dist 0.5·R is inside
        // it → the Kite stance opens away.
        assert!(
            dist < AiTuning::default().kite_range_frac * range,
            "kiter is inside its kite ring → open range"
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
        assert!(
            kiter.forward < 0.0,
            "kiter opens range by REVERSE-thrusting, nose on target (got {})",
            kiter.forward
        );
        assert!(
            kiter.turn.abs() < 1e-5,
            "kiter keeps its gun on the target while opening (no flee-turn, got {})",
            kiter.turn
        );
    }

    /// R97 Phase 1 Stage C (re-pins the old T025 "never fire" contract — behavior
    /// INTENTIONALLY changed): the survival arms (Evade/Retreat) now AIM at the
    /// nearest hostile while fleeing, so a FIGHTING RETREAT emerges — they fire
    /// when armed + aligned + in range. The faithful new contract:
    /// - an armed Retreater with a firable pursuer DEAD AHEAD fires while
    ///   reverse-drifting toward home (open range, gun on the pursuer);
    /// - an Evader forced to TURN to face a threat astern does NOT fire on the
    ///   tick its nose is still off the threat (unaligned → no fire);
    /// - an UNARMED retreater never fires (the `fire_decision` armed gate).
    #[test]
    fn survival_arms_face_and_fire_on_the_threat_while_opening_range() {
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
        // Evade a threat ASTERN (heading +X): MOVE away (≈+X), AIM at the threat
        // behind (-X) → it must TURN to bring the gun to bear, so on this tick the
        // nose is still off the threat (unaligned → no fire — a faithful case).
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
        // Retreat with a home anchor (astern) while a firable pursuer sits dead
        // ahead (in range, aligned, charged, cold): the FIGHTING RETREAT — it
        // reverse-drifts toward home while the gun bears on the pursuer, and FIRES.
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
        // Retreat without a home: open range directly away from the pursuer ahead
        // (MOVE -X, AIM +X) — same fighting retreat, reverse-drift + fire.
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
        // An UNARMED retreater with the SAME firable geometry: the armed gate in
        // `fire_decision` means it never fires (it just runs).
        let mut unarmed_stats = fighter_stats();
        unarmed_stats.can_fire = false;
        unarmed_stats.weapon = None;
        let unarmed = world
            .spawn((
                ship_bundle(
                    AiBrain {
                        behavior: Behavior::Retreat,
                        target: Some(ahead),
                        ..AiBrain::default()
                    },
                    Vec2::ZERO,
                ),
                unarmed_stats,
                full_energy(),
                cold_heat(),
            ))
            .id();
        schedule.run(&mut world);

        let e = intent_of(&world, evader);
        assert!(
            e.turn.abs() > 0.5,
            "evader turns to bring its gun onto the threat astern"
        );
        assert!(
            !e.fire_primary,
            "unaligned (nose still off the threat astern) → no fire"
        );
        let r = intent_of(&world, retreater);
        assert!(
            r.fire_primary,
            "FIGHTING RETREAT: armed + aligned + in range → fire while withdrawing"
        );
        assert!(
            r.forward < -0.9,
            "reverse-drift toward home while facing the pursuer (got {})",
            r.forward
        );
        assert!(
            r.turn.abs() < 1e-5,
            "nose stays on the dead-ahead pursuer (no turn)"
        );
        let a = intent_of(&world, anchorless);
        assert!(
            a.fire_primary,
            "anchorless retreat fires on the pursuer too"
        );
        assert!(
            a.forward < -0.9,
            "opens range (reverse) while the gun bears"
        );
        assert!(a.turn.abs() < 1e-5, "faces the dead-ahead pursuer");
        let u = intent_of(&world, unarmed);
        assert!(
            !u.fire_primary,
            "an unarmed survival ship never fires (armed gate)"
        );
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
