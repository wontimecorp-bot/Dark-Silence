//! AI disposition / personality (R102 Part B1): a tiny per-ship trait component
//! that PARAMETERIZES the existing utility-AI decision-making so different ships
//! act differently — a strict sentry vs a roaming patrol vs an aggressive hunter
//! vs a skittish runner — WITHOUT introducing a new decision system.
//!
//! **The principle (utility AI with personality)**: a [`Disposition`] is NOT a
//! new behavior; it is the WEIGHTS / THRESHOLDS on the decisions the brain
//! already makes. Four orthogonal `f32` traits in `[0, 1]` fold into the EXISTING
//! resolution seams in `ai_think_system` / `perception_scan_system`:
//!
//! - **aggression** → posture ([`Disposition::effective_posture`]), the engage
//!   score scale ([`Disposition::engage_score_scale`]), the move/combat style
//!   ([`Disposition::move_profile`] / [`Disposition::combat_stance`]), and the
//!   acquisition gate (a ship only auto-acquires a passing hostile if its
//!   effective posture permits engagement).
//! - **caution** → the flee/retreat score scale
//!   ([`Disposition::flee_score_scale`]) and the move/combat style (a cautious
//!   ship paces leisurely / kites).
//! - **leash** → the return-to-post radius ([`Disposition::leash_radius`]): how
//!   far from `home`/anchor the ship will chase a target before breaking off.
//! - **tenacity** → the lost-target grace ([`Disposition::target_grace_ticks`]):
//!   how long an out-of-contact target is held before it is cleared.
//!
//! **Precedence (the keystone)**: a `Disposition` sits BELOW a [`PlayerOrder`]
//! and ABOVE the role/squad/archetype in the posture and style chains
//! (`player ← disposition ← squad ← role ← archetype`). It does NOT change squad
//! membership — it is a per-ship personality trait, not a command.
//!
//! **Ephemeral (V-9)**: `Clone + Copy + Debug`, NO `Serialize` — like
//! [`PlayerOrder`](crate::ai::command::PlayerOrder) and [`AiBrain`] every trait
//! is reconstructable from scenario authoring, never persisted. The ABSENCE of
//! the component is "no personality" — every gated plug-in checks for presence
//! and PRESERVES today's behavior when it is missing, so the golden/headless
//! worlds (which spawn no `Disposition`) are byte-for-byte unchanged.
//!
//! **Determinism (V-3 / TR-004)**: pure component reads + strict-f32 trait math
//! — no RNG, no HashMap. The two SCORE scales
//! ([`Disposition::flee_score_scale`] / [`Disposition::engage_score_scale`]) are
//! strict-f32 (`+ - * clamp`) so they can be multiplied INSIDE the brain's
//! strict-f32 scoring fence; the GEOMETRY scales (leash distance) stay outside.

use crate::ai::brain::{CombatStance, MovementProfile};
use crate::ai::role::Posture;
use crate::ai::tuning::AiTuning;

/// A per-ship personality trait set (R102 Part B1) — four orthogonal `f32`
/// weights in `[0, 1]` that parameterize the existing utility-AI decisions. See
/// the module docs for the principle, the per-trait plug-in points, and the
/// precedence rule.
///
/// `Clone + Copy + Debug`, NO `Serialize` (ephemeral, V-9 — like
/// [`PlayerOrder`](crate::ai::command::PlayerOrder)). Authored via the PRESET
/// constructors ([`Disposition::sentry`] / [`patroller`](Disposition::patroller)
/// / [`hunter`](Disposition::hunter) / [`skittish`](Disposition::skittish) /
/// [`berserker`](Disposition::berserker) / [`neutral`](Disposition::neutral));
/// the sim reads the raw traits through the helper methods.
#[derive(bevy_ecs::prelude::Component, Clone, Copy, Debug, PartialEq)]
pub struct Disposition {
    /// Willingness to ENGAGE `[0, 1]`: drives the effective posture (high →
    /// FreeEngage, acquires/hunts; low → DefensiveOnly, ignores passers), the
    /// engage score scale, and the aggressive move/combat style.
    pub aggression: f32,
    /// Survival pressure `[0, 1]`: scales the flee/retreat desire (high → breaks
    /// off / Evades at higher health; low → brave, presses the attack) and the
    /// defensive move/combat style (high → Leisurely / Kite).
    pub caution: f32,
    /// Leash length `[0, 1]`: how far from `home`/anchor the ship will chase a
    /// target before breaking off and returning (low → holds its post; high →
    /// chases far).
    pub leash: f32,
    /// Pursuit doggedness `[0, 1]`: how long a lost (out-of-contact) target is
    /// held before clearing (low → drops a lost target fast, resumes its task;
    /// high → keeps hunting a vanished target).
    pub tenacity: f32,
}

impl Disposition {
    // --- Preset constructors (scenario authors with these) ------------------

    /// A STRICT SENTRY: brave (low caution) but holds its post — a short leash,
    /// low tenacity (drops chases), and a defensive-by-default posture so it
    /// ignores a passing hostile until fired upon.
    pub fn sentry() -> Self {
        Self {
            aggression: 0.25,
            caution: 0.2,
            leash: 0.15,
            tenacity: 0.2,
        }
    }

    /// A ROAMING PATROLLER: the balanced middle — roams its route, investigates
    /// a threat, and returns (all four traits at the neutral 0.5).
    pub fn patroller() -> Self {
        Self {
            aggression: 0.5,
            caution: 0.5,
            leash: 0.5,
            tenacity: 0.5,
        }
    }

    /// An AGGRESSIVE HUNTER: acquires immediately (high aggression → FreeEngage),
    /// chases far (long leash), and pursues a lost target relentlessly (high
    /// tenacity) — brave (low caution).
    pub fn hunter() -> Self {
        Self {
            aggression: 0.9,
            caution: 0.2,
            leash: 0.95,
            tenacity: 0.9,
        }
    }

    /// A SKITTISH runner: flees early (high caution → high flee scale), barely
    /// engages (low aggression), holds no post worth chasing past (short leash),
    /// and gives up a lost target quickly (low tenacity).
    pub fn skittish() -> Self {
        Self {
            aggression: 0.2,
            caution: 0.9,
            leash: 0.2,
            tenacity: 0.3,
        }
    }

    /// A BERSERKER: maxed aggression, ZERO caution (never flees), a maximal
    /// leash (chases anywhere), and maximal tenacity (never drops a target) —
    /// the all-in extreme.
    pub fn berserker() -> Self {
        Self {
            aggression: 1.0,
            caution: 0.0,
            leash: 1.0,
            tenacity: 1.0,
        }
    }

    /// The NEUTRAL default: every trait at 0.5 — a ship that DEFERS everywhere
    /// (mid posture, no style override, neutral score scales, a mid leash/grace),
    /// a safe baseline when a scenario wants a personality slot without a flavor.
    pub fn neutral() -> Self {
        Self {
            aggression: 0.5,
            caution: 0.5,
            leash: 0.5,
            tenacity: 0.5,
        }
    }

    // --- Resolution helpers (the sim reads these) ---------------------------

    /// The effective fire-control [`Posture`] this disposition implies, from
    /// `aggression` (a precedence source ABOVE the role posture, BELOW a
    /// `PlayerOrder` posture in `ai_think_system`).
    ///
    /// **Documented cutoffs (tunable later)**:
    /// - `aggression > 0.66` → [`FreeEngage`](Posture::FreeEngage) (a hunter
    ///   acquires/engages on sight);
    /// - otherwise → [`DefensiveOnly`](Posture::DefensiveOnly) (a sentry ignores
    ///   a passing hostile until fired upon).
    ///
    /// It NEVER returns [`HoldFire`](Posture::HoldFire): a ship still defends
    /// itself regardless of personality — pacifism is a SCRIPTED posture
    /// (`ScenarioRole`/`PlayerOrder`), not a disposition.
    pub fn effective_posture(&self) -> Posture {
        if self.aggression > 0.66 {
            Posture::FreeEngage
        } else {
            Posture::DefensiveOnly
        }
    }

    /// The optional [`MovementProfile`] override this disposition implies (the
    /// disposition LINK of the style chain — `Some(...)` pins a pace, `None`
    /// defers to squad/role/archetype). Documented cutoffs (tunable later):
    /// - `aggression > 0.7` → [`Rush`](MovementProfile::Rush) (hard charger);
    /// - `aggression < 0.3` OR `caution > 0.7` → [`Leisurely`](MovementProfile::Leisurely)
    ///   (timid / energy-saving);
    /// - else `None` (defer — a balanced ship keeps its role/archetype pace).
    pub fn move_profile(&self) -> Option<MovementProfile> {
        if self.aggression > 0.7 {
            Some(MovementProfile::Rush)
        } else if self.aggression < 0.3 || self.caution > 0.7 {
            Some(MovementProfile::Leisurely)
        } else {
            None
        }
    }

    /// The optional [`CombatStance`] override this disposition implies (the
    /// disposition LINK of the stance chain). Documented cutoffs (tunable later):
    /// - `aggression > 0.7` → [`Charge`](CombatStance::Charge) (close and slug);
    /// - `caution > 0.7` → [`Kite`](CombatStance::Kite) (keep range);
    /// - else `None` (defer to squad/role/archetype).
    pub fn combat_stance(&self) -> Option<CombatStance> {
        if self.aggression > 0.7 {
            Some(CombatStance::Charge)
        } else if self.caution > 0.7 {
            Some(CombatStance::Kite)
        } else {
            None
        }
    }

    /// STRICT-F32 multiplier (`>= 1`) applied to the Evade/Retreat candidate
    /// SCORE: a cautious ship breaks off at higher health, a brave one shrinks
    /// its flee desire. `(1 + caution · knob).clamp(min, max)` with
    /// `knob = AiTuning::disposition_caution_flee_scale`. Strict-f32 (`+ * clamp`
    /// only) so the multiply is legal INSIDE the brain's strict-f32 fence.
    ///
    /// Note `caution ∈ [0, 1]` and the knob is non-negative, so the raw value is
    /// already `>= 1`; the clamp is a defensive bound for a live-edited knob.
    pub fn flee_score_scale(&self, tuning: &AiTuning) -> f32 {
        (1.0 + self.caution * tuning.disposition_caution_flee_scale).clamp(0.0, 8.0)
    }

    /// STRICT-F32 multiplier (`>= 1`) applied to the Engage candidate SCORE: an
    /// aggressive ship prefers engaging more strongly. `(1 + aggression · knob)`
    /// with `knob = AiTuning::disposition_aggression_engage_scale`, clamped for a
    /// live-edited knob. Strict-f32 — legal inside the scoring fence.
    pub fn engage_score_scale(&self, tuning: &AiTuning) -> f32 {
        (1.0 + self.aggression * tuning.disposition_aggression_engage_scale).clamp(0.0, 8.0)
    }

    /// The return-to-post LEASH radius (world units): how far from `home`/anchor
    /// the ship chases a target before breaking off.
    /// `base · (low + leash · span)` (with `low + span = 1` here) so a short-leash
    /// sentry holds close to its post and a long-leash hunter chases far. This is
    /// GEOMETRY (a distance the caller compares against a `length()`), so it lives
    /// OUTSIDE the strict-f32 scoring fence.
    pub fn leash_radius(&self, tuning: &AiTuning) -> f32 {
        // `low` is the floor fraction of `base` even a zero-leash ship gets (so a
        // sentry still prosecutes a target a little way off its post); the `span`
        // up to `1.0` is added by `leash`. A berserker (leash 1) → full `base`,
        // scaled by its own knob below for "very long".
        const LOW: f32 = 0.15;
        const SPAN: f32 = 1.0 - LOW;
        tuning.disposition_leash_base * (LOW + self.leash.clamp(0.0, 1.0) * SPAN)
    }

    /// How long (ticks) a lost (out-of-contact) target is HELD before clearing,
    /// from `tenacity`: `round(base · (1 + tenacity · knob))` with
    /// `base = AiTuning::disposition_target_grace_base` and
    /// `knob = AiTuning::disposition_tenacity_grace_scale`. A fickle (low
    /// tenacity) ship drops a lost target fast; a tenacious one holds it far
    /// longer. Rounded to a whole tick (a count, not a score — outside the fence).
    pub fn target_grace_ticks(&self, tuning: &AiTuning) -> u64 {
        let base = tuning.disposition_target_grace_base;
        let scaled =
            base * (1.0 + self.tenacity.clamp(0.0, 1.0) * tuning.disposition_tenacity_grace_scale);
        // `base`/knobs are non-negative defaults; guard a live-edited negative.
        scaled.max(0.0).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented preset values are exactly as authored (scenario authors
    /// rely on these magnitudes).
    #[test]
    fn preset_values_are_documented() {
        let s = Disposition::sentry();
        assert_eq!(
            (s.aggression, s.caution, s.leash, s.tenacity),
            (0.25, 0.2, 0.15, 0.2)
        );
        let p = Disposition::patroller();
        assert_eq!(
            (p.aggression, p.caution, p.leash, p.tenacity),
            (0.5, 0.5, 0.5, 0.5)
        );
        let h = Disposition::hunter();
        assert_eq!(
            (h.aggression, h.caution, h.leash, h.tenacity),
            (0.9, 0.2, 0.95, 0.9)
        );
        let k = Disposition::skittish();
        assert_eq!(
            (k.aggression, k.caution, k.leash, k.tenacity),
            (0.2, 0.9, 0.2, 0.3)
        );
        let b = Disposition::berserker();
        assert_eq!(
            (b.aggression, b.caution, b.leash, b.tenacity),
            (1.0, 0.0, 1.0, 1.0)
        );
        let n = Disposition::neutral();
        assert_eq!(
            (n.aggression, n.caution, n.leash, n.tenacity),
            (0.5, 0.5, 0.5, 0.5)
        );
    }

    /// `effective_posture`: a hunter/berserker frees engagement; everyone else
    /// is DefensiveOnly; NEVER HoldFire (a ship always defends itself).
    #[test]
    fn effective_posture_cutoffs() {
        assert_eq!(
            Disposition::hunter().effective_posture(),
            Posture::FreeEngage
        );
        assert_eq!(
            Disposition::berserker().effective_posture(),
            Posture::FreeEngage
        );
        assert_eq!(
            Disposition::sentry().effective_posture(),
            Posture::DefensiveOnly
        );
        assert_eq!(
            Disposition::skittish().effective_posture(),
            Posture::DefensiveOnly
        );
        assert_eq!(
            Disposition::patroller().effective_posture(),
            Posture::DefensiveOnly
        );
        // No preset ever yields HoldFire.
        for d in [
            Disposition::sentry(),
            Disposition::patroller(),
            Disposition::hunter(),
            Disposition::skittish(),
            Disposition::berserker(),
            Disposition::neutral(),
        ] {
            assert_ne!(d.effective_posture(), Posture::HoldFire);
        }
    }

    /// `move_profile` / `combat_stance`: a hunter charges/rushes, a skittish ship
    /// kites/leisurely, a balanced (patroller/neutral) defers (`None`).
    #[test]
    fn style_overrides_match_cutoffs() {
        assert_eq!(
            Disposition::hunter().move_profile(),
            Some(MovementProfile::Rush)
        );
        assert_eq!(
            Disposition::hunter().combat_stance(),
            Some(CombatStance::Charge)
        );
        assert_eq!(
            Disposition::skittish().move_profile(),
            Some(MovementProfile::Leisurely)
        );
        assert_eq!(
            Disposition::skittish().combat_stance(),
            Some(CombatStance::Kite)
        );
        // Sentry: low aggression (< 0.3) → Leisurely pace, but no stance override
        // (caution 0.2 isn't > 0.7, aggression 0.25 isn't > 0.7).
        assert_eq!(
            Disposition::sentry().move_profile(),
            Some(MovementProfile::Leisurely)
        );
        assert_eq!(Disposition::sentry().combat_stance(), None);
        // Balanced ships defer entirely.
        assert_eq!(Disposition::patroller().move_profile(), None);
        assert_eq!(Disposition::patroller().combat_stance(), None);
        assert_eq!(Disposition::neutral().move_profile(), None);
        assert_eq!(Disposition::neutral().combat_stance(), None);
    }

    /// The score scales: caution lifts the flee scale, aggression lifts the
    /// engage scale; both are `>= 1` and ordered as the personalities imply.
    #[test]
    fn score_scales_are_ordered_and_at_least_one() {
        let t = AiTuning::default();
        let brave = Disposition::berserker(); // caution 0.0
        let timid = Disposition::skittish(); // caution 0.9
        assert!(brave.flee_score_scale(&t) >= 1.0);
        assert!(timid.flee_score_scale(&t) > brave.flee_score_scale(&t));
        // Brave (caution 0) leaves the flee desire unscaled (× 1).
        assert_eq!(brave.flee_score_scale(&t), 1.0);

        let hunter = Disposition::hunter(); // aggression 0.9
        let meek = Disposition::skittish(); // aggression 0.2
        assert!(hunter.engage_score_scale(&t) > meek.engage_score_scale(&t));
        assert!(meek.engage_score_scale(&t) >= 1.0);
    }

    /// `leash_radius`: monotone in `leash`, a short-leash sentry holds far
    /// closer to its post than a long-leash hunter.
    #[test]
    fn leash_radius_orders_sentry_short_hunter_long() {
        let t = AiTuning::default();
        let sentry = Disposition::sentry().leash_radius(&t);
        let hunter = Disposition::hunter().leash_radius(&t);
        let berserker = Disposition::berserker().leash_radius(&t);
        assert!(
            sentry > 0.0,
            "even a sentry prosecutes a little off its post"
        );
        assert!(
            sentry < hunter,
            "sentry leash {sentry} << hunter leash {hunter}"
        );
        assert!(hunter <= berserker, "berserker chases at least as far");
        assert_eq!(berserker, t.disposition_leash_base, "max leash = full base");
    }

    /// `target_grace_ticks`: monotone in `tenacity`, a fickle ship drops a lost
    /// target within a fraction of a tenacious one's grace.
    #[test]
    fn target_grace_orders_fickle_short_tenacious_long() {
        let t = AiTuning::default();
        let fickle = Disposition::skittish().target_grace_ticks(&t); // tenacity 0.3
        let tenacious = Disposition::hunter().target_grace_ticks(&t); // tenacity 0.9
        let base = Disposition::berserker().target_grace_ticks(&t); // tenacity 1.0
        assert!(fickle >= t.disposition_target_grace_base.round() as u64);
        assert!(
            fickle < tenacious,
            "fickle {fickle} < tenacious {tenacious}"
        );
        assert!(tenacious <= base, "berserker holds at least as long");
    }
}
