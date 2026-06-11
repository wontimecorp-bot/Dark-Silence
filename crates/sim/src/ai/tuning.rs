//! Live-editable AI tuning resource (E011, data-model §`AiTuning`).
//!
//! Mirrors the [`SimTuning`](crate::tuning::SimTuning) pattern: one plain
//! `Resource` holding every AI magnitude (think cadences, AOI radii, squad
//! limits, utility/ram/archetype/sensor/steering knobs) so behavior can be
//! tuned live in the dev panel without touching logic, and saved/loaded as RON
//! dev settings with `#[serde(default)]` per-field fallback. The resource is
//! deterministic INPUT, not state: systems read it fresh each run, all `f32`
//! values feed strict-f32 scoring (no fast-math), and golden/bench runs use
//! these pinned defaults — a mid-run edit invalidates comparability with
//! previously recorded runs.

use bevy_ecs::prelude::Resource;

use crate::ai::brain::MovementProfile;
use crate::fitting::CELL_WORLD_SIZE;

/// Global AI tuning. Inserted by the scenario/server world (and edited live via
/// the dev panel, TR-020); a world that never inserts it reads
/// `AiTuning::default()` — the pinned values every golden/bench fixture is
/// built against. All tick counts are at the 30 Hz fixed step.
#[derive(Resource, Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)] // Older saved RONs missing newer fields fall back per-field (SimTuning pattern).
pub struct AiTuning {
    // --- Think cadence (TR-005, AD-003: events + phase-bucket fallback) ---
    /// Active-tier fallback think cadence, ticks (~0.5 s; events still react same-tick).
    pub think_ticks_active: u32,
    /// Mid-tier fallback think cadence, ticks (~0.5 s).
    pub think_ticks_mid: u32,
    /// Dormant-tier fallback think cadence, ticks (2–5 s band; 90 = 3 s).
    pub think_ticks_dormant: u32,
    /// Phase-bucket count for the stable-id-hash fallback cadence spread
    /// (`phase_bucket = stable_id hash % this`); each tick services ≈ N/buckets.
    pub fallback_bucket_count: u32,

    // --- AOI tiers (TR-007/TR-008) ---
    /// Player-proximity radius of the Active tier (full per-ship AI), world units.
    pub aoi_radius_active: f32,
    /// Player-proximity radius of the Mid tier (squad-driven AI), world units.
    pub aoi_radius_mid: f32,
    /// Minimum ticks between tier changes per entity — boundary hysteresis (no thrash).
    pub tier_hysteresis_ticks: u32,
    /// TR-008 validity-nudge bound: max de-penetration distance applied at
    /// promotion, world units (default = one fine grid cell, `CELL_WORLD_SIZE`).
    pub promote_nudge_max: f32,

    // --- Squad composition (TR-010) ---
    /// Maximum members per squad; larger groups split.
    pub max_squad_size: u32,
    /// Fleet size at/above which squads organize under a wing parent.
    pub wing_split_threshold: u32,

    // --- Utility scoring (TR-004, HINT-004) ---
    /// Incumbent-behavior score multiplier bonus (~25%): hysteresis against oscillation.
    pub momentum_bonus: f32,
    /// Mark's compensation factor for multiplied consideration curves
    /// (rescales scores so adding considerations doesn't starve them).
    pub compensation_k: f32,

    // --- Ramming cost/benefit (TR-012) ---
    /// Target hull fraction at/below which it counts as "near-dead/disabled" for a ram.
    pub ram_target_hull_frac: f32,
    /// Required ratio of projected dealt damage over projected self-damage ("much weaker").
    pub ram_self_margin: f32,
    /// Minimum closing speed to commit, as a fraction of the attacker's top speed.
    pub ram_min_closing_frac: f32,

    // --- Fit-archetype classification cuts (TR-006) ---
    /// Top speed at/above which a fit reads as "fast" (Kiter-leaning), world units/s.
    pub arch_speed_hi: f32,
    /// Sustained DPS at/above which a fit reads as "heavy-hitting" (Brawler-leaning).
    pub arch_dps_hi: f32,
    /// Armor/structure pool at/above which a fit reads as "tanky" (Rammer/Brawler-leaning).
    pub arch_armor_hi: f32,

    // --- Sensors / perception (TR-013/TR-014, Q3/Q4) ---
    /// Baseline own-ship sensor range, world units (v1 faction baseline).
    pub base_sensor_range: f32,
    /// Faction datalink connectivity radius for the sensor-network flood-fill, world units.
    pub datalink_radius: f32,
    /// Minimum contact signature to detect (V-8 gate; 0.0 = everything visible in v1).
    pub sig_threshold: f32,
    /// Mid-tier scan cadence, ticks (~0.5 s fused query per squad).
    pub scan_ticks_mid: u32,
    /// Far-tier scan cadence, ticks (2–5 s coarse-grid neighborhood; 90 = 3 s).
    pub scan_ticks_far: u32,
    /// Cap per fused network picture (keep newest/highest-signature, deterministic cut).
    pub max_fused_contacts: u32,

    // --- Context steering (TR-002, AD-004) ---
    /// Direction slots per context map (interest/danger), 8–16.
    pub slot_count: u32,
    /// Floor subtracted-to value for danger-mask suppression (0.0 = full block).
    pub danger_mask_floor: f32,

    // --- Movement profiles (R96 Part A) ---
    // Each profile is a (forward cap, brake aggression, arrive slow-factor) triple
    // read by `ai_execute_system`'s `fly_to` via `profile_params`. CRUISE values
    // are PINNED to today's constants so `MovementProfile::Cruise` is byte-identical
    // to the pre-R96 path (cap 1.0 = `*1.0` no-op; slow-factor 4.0 = the
    // `steering::WAYPOINT_SLOW_FACTOR`); only Rush/Leisurely diverge.
    /// Forward throttle cap for `Rush` (hot pace — full authority).
    pub profile_rush_cap: f32,
    /// Forward throttle cap for `Cruise` (PINNED 1.0 — the parity no-op).
    pub profile_cruise_cap: f32,
    /// Forward throttle cap for `Leisurely` (lazy pace — capped to half).
    pub profile_leisurely_cap: f32,
    /// Brake aggression for `Rush` (earlier-braking multiplier on stopping distance).
    pub brake_aggression_rush: f32,
    /// Brake aggression for `Cruise` (PINNED 1.0 — unused on the no-brake parity path).
    pub brake_aggression_cruise: f32,
    /// Brake aggression for `Leisurely` (brakes earliest — a long, gentle settle).
    pub brake_aggression_leisurely: f32,
    /// Arrive slow-radius factor for `Rush` (snug ramp — `× ARRIVE_RADIUS`).
    pub arrive_slow_factor_rush: f32,
    /// Arrive slow-radius factor for `Cruise` (PINNED 4.0 — the `WAYPOINT_SLOW_FACTOR`).
    pub arrive_slow_factor_cruise: f32,
    /// Arrive slow-radius factor for `Leisurely` (wide ramp — eases in early).
    pub arrive_slow_factor_leisurely: f32,

    // --- Combat stances (R96 Part C) ---
    // The per-stance combat-steering knobs read by `engage_motion`. None of
    // these touch the `CombatStance::Charge` path (the PARITY default reuses the
    // legacy range-band controller verbatim), so existing combat fixtures are
    // unaffected regardless of these values.
    /// `Orbit` ring radius as a multiple of `standoff_distance` (1.0 = the
    /// archetype standoff ring itself; > 1 orbits wider, < 1 tighter).
    pub orbit_radius_frac: f32,
    /// `Orbit` tangential interest weight: scales the around-the-target term
    /// (`× (1 − radial.abs())`, so it DOMINATES on-ring and yields to the radial
    /// correction off-ring).
    pub orbit_tangential_weight: f32,
    /// `Kite` standoff target as a multiple of `weapon_range` (1.1 = hold just
    /// beyond the envelope edge, so the gun still bears while the target chases).
    pub kite_range_frac: f32,
    /// Lateral strafe magnitude (`0..=1`) a stance commands on the strafe
    /// channel — applied ONLY when `ShipStats::can_strafe` is set (R93); a basic
    /// fighter keeps `strafe = 0` and orbits by turning alone.
    pub strafe_stance_lateral: f32,

    // --- Obstacle avoidance (R96 Part D) ---
    // The shared obstacle-avoidance knobs read by `ai_execute_system`'s move +
    // combat arms via `add_obstacle_danger` over the per-tick `ObstacleField`.
    // None touch the empty-field path (zero in-range obstacles is a no-op
    // `add_danger_threat`), so an obstacle-free world is byte-identical to
    // pre-R96-D regardless of these values.
    /// Danger weight written for each in-range obstacle (`add_danger_threat`
    /// `weight`): scales how strongly an obstacle masks the headings into it.
    pub obstacle_danger_weight: f32,
    /// Predictive lookahead (s) for the obstacle closeness test: a body is live
    /// if the ship's CURRENT or `pos + vel·this` position is inside its avoid
    /// radius (mirrors `steering::avoid`'s lookahead model).
    pub obstacle_lookahead_s: f32,
    /// Extra clearance pad (world units) added to `obstacle_radius + own_radius`
    /// so the ship steers around with margin, not skimming the surface.
    pub obstacle_clearance_pad: f32,
    /// Scan radius (world units) around the ship: obstacles farther than this
    /// are ignored (the field is tiny, so this is a linear-scan gate, not an
    /// index query).
    pub obstacle_query_radius: f32,
    /// Minimum body radius (world units) to enter the `ObstacleField` — only
    /// LARGE neutral bodies (asteroids/outposts/transports) are avoided; small
    /// debris/ships are not.
    pub obstacle_min_radius: f32,

    // --- Debug (TR-020a) ---
    /// Per-brain transition-history ring length (capture feature-gated off in
    /// headless/bench builds).
    pub debug_history_len: u32,
}

impl Default for AiTuning {
    fn default() -> Self {
        Self {
            // Think cadence: active/mid ≈ 0.5 s fallback, dormant 3 s (Q4 band 2–5 s).
            think_ticks_active: 15,
            think_ticks_mid: 15,
            think_ticks_dormant: 90,
            fallback_bucket_count: 16,
            // AOI: Active within ~60 u of a player, Mid to 240 u, beyond = Dormant;
            // 30-tick (1 s) hysteresis; nudge bounded to one fine cell (TR-008).
            aoi_radius_active: 60.0,
            aoi_radius_mid: 240.0,
            tier_hysteresis_ticks: 30,
            promote_nudge_max: CELL_WORLD_SIZE,
            // Squads.
            max_squad_size: 8,
            wing_split_threshold: 12,
            // Utility: research-pinned ~25% incumbent momentum; neutral compensation.
            momentum_bonus: 0.25,
            compensation_k: 1.0,
            // Ramming: the pinned knobs the OBJ4-VC2 ram/no-ram fixtures are built against.
            ram_target_hull_frac: 0.25,
            ram_self_margin: 2.0,
            ram_min_closing_frac: 0.5,
            // Archetype cuts vs the seed fighter's derived stats (top speed 80 u/s,
            // autocannon ~60 DPS): 60 u/s = "fast", 20 DPS = "armed", 100 = "tanky".
            // Live-tunable; an edit triggers mass re-classification (V-5).
            arch_speed_hi: 60.0,
            arch_dps_hi: 20.0,
            arch_armor_hi: 100.0,
            // Sensors: v1 faction baseline (Q3); scan cadences match the think band (Q4).
            base_sensor_range: 200.0,
            datalink_radius: 300.0,
            sig_threshold: 0.0,
            scan_ticks_mid: 15,
            scan_ticks_far: 90,
            max_fused_contacts: 64,
            // Steering: 16-slot context maps (AD-004); danger fully suppresses a slot.
            slot_count: 16,
            danger_mask_floor: 0.0,
            // Movement profiles (R96): Cruise PINNED to the pre-R96 constants
            // (cap 1.0, slow-factor 4.0 = WAYPOINT_SLOW_FACTOR) so it stays
            // byte-identical; Rush brakes onto a snug ring, Leisurely paces slow
            // and coasts wide.
            profile_rush_cap: 1.0,
            profile_cruise_cap: 1.0,
            profile_leisurely_cap: 0.5,
            brake_aggression_rush: 1.0,
            brake_aggression_cruise: 1.0,
            brake_aggression_leisurely: 1.6,
            arrive_slow_factor_rush: 1.5,
            arrive_slow_factor_cruise: 4.0,
            arrive_slow_factor_leisurely: 8.0,
            // Combat stances (R96 Part C): orbit at the standoff ring with a
            // moderate tangential bank, kite just past the envelope edge, full
            // lateral strafe for hulls that can. None affect the Charge parity path.
            orbit_radius_frac: 1.0,
            orbit_tangential_weight: 0.6,
            kite_range_frac: 1.1,
            strafe_stance_lateral: 1.0,
            // Obstacle avoidance (R96 Part D): a full-weight danger per in-range
            // obstacle, a 1.5 s lookahead, a 15-unit clearance pad, a 200-unit
            // scan, and a 20-unit min radius (only large neutral bodies). The
            // lookahead + pad were bumped from the original (0.5 s / 8 u) so a
            // FAST fighter (top ~80 u/s) reacts well before it reaches a body and
            // visibly clears the MiningSkirmish central asteroid (radius 30) with
            // a clean margin (avoid radius ≈ 30 + own ~2 + 15 ≈ 47 u — a ~17 u
            // surface gap, wide enough to SEE the detour, narrow enough that a
            // combat ship still approaches a target orbiting near a structure).
            // Determinism-safe: golden worlds have no `AiBrain` consuming the
            // field, so these only affect scenario AI (no observable golden shift).
            obstacle_danger_weight: 1.0,
            obstacle_lookahead_s: 1.5,
            obstacle_clearance_pad: 15.0,
            obstacle_query_radius: 200.0,
            obstacle_min_radius: 20.0,
            // Debug history ring (TR-020a).
            debug_history_len: 16,
        }
    }
}

impl AiTuning {
    /// R96 Part A — the movement-profile `(forward_cap, brake_aggression,
    /// arrive_slow_factor)` triple for `profile`. `ai_execute_system`'s `fly_to`
    /// reads it: the cap limits forward intent (composed with the squad
    /// `throttle_cap`), the brake aggression scales [`arrive_braked`]'s stopping
    /// distance, and the slow factor sizes the arrive ramp
    /// (`× ARRIVE_RADIUS`). Cruise returns the PINNED parity triple
    /// `(1.0, 1.0, 4.0)`.
    ///
    /// [`arrive_braked`]: crate::ai::steering::arrive_braked
    pub fn profile_params(&self, profile: MovementProfile) -> (f32, f32, f32) {
        match profile {
            MovementProfile::Rush => (
                self.profile_rush_cap,
                self.brake_aggression_rush,
                self.arrive_slow_factor_rush,
            ),
            MovementProfile::Cruise => (
                self.profile_cruise_cap,
                self.brake_aggression_cruise,
                self.arrive_slow_factor_cruise,
            ),
            MovementProfile::Leisurely => (
                self.profile_leisurely_cap,
                self.brake_aggression_leisurely,
                self.arrive_slow_factor_leisurely,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned defaults are well-formed: positive cadences/radii/counts, fractions
    /// in range, and the AOI tiers strictly ordered.
    #[test]
    fn default_aituning_is_sane() {
        let t = AiTuning::default();
        assert!(t.think_ticks_active > 0 && t.think_ticks_mid > 0 && t.think_ticks_dormant > 0);
        assert!(t.fallback_bucket_count > 0 && t.tier_hysteresis_ticks > 0);
        assert!(t.aoi_radius_active > 0.0 && t.aoi_radius_active < t.aoi_radius_mid);
        assert!(t.promote_nudge_max > 0.0 && (t.promote_nudge_max - CELL_WORLD_SIZE).abs() < 1e-6);
        assert!(t.max_squad_size > 0 && t.wing_split_threshold > t.max_squad_size);
        assert!((0.0..=1.0).contains(&t.ram_target_hull_frac));
        assert!((0.0..=1.0).contains(&t.ram_min_closing_frac));
        assert!(t.ram_self_margin >= 1.0 && t.momentum_bonus >= 0.0 && t.compensation_k > 0.0);
        assert!(t.arch_speed_hi > 0.0 && t.arch_dps_hi > 0.0 && t.arch_armor_hi > 0.0);
        assert!(t.base_sensor_range > 0.0 && t.datalink_radius > 0.0 && t.sig_threshold >= 0.0);
        assert!(t.scan_ticks_mid > 0 && t.scan_ticks_far > 0 && t.max_fused_contacts > 0);
        assert!((8..=16).contains(&t.slot_count) && t.danger_mask_floor >= 0.0);
        assert!(t.debug_history_len > 0);
        // R96 movement profiles: caps in [0,1], positive aggressions/factors.
        assert!((0.0..=1.0).contains(&t.profile_rush_cap));
        assert!((0.0..=1.0).contains(&t.profile_cruise_cap));
        assert!((0.0..=1.0).contains(&t.profile_leisurely_cap));
        assert!(t.brake_aggression_rush > 0.0 && t.brake_aggression_leisurely > 0.0);
        assert!(t.arrive_slow_factor_rush > 0.0 && t.arrive_slow_factor_leisurely > 0.0);
        // R96 Part C combat stances: positive ring/range fracs, a strafe magnitude in [0,1].
        assert!(t.orbit_radius_frac > 0.0 && t.orbit_tangential_weight > 0.0);
        assert!(t.kite_range_frac > 0.0 && (0.0..=1.0).contains(&t.strafe_stance_lateral));
        // R96 Part D obstacle avoidance: positive weight/lookahead/pad/radii.
        assert!(t.obstacle_danger_weight > 0.0 && t.obstacle_lookahead_s > 0.0);
        assert!(t.obstacle_clearance_pad >= 0.0 && t.obstacle_query_radius > 0.0);
        assert!(t.obstacle_min_radius > 0.0);
    }

    /// R96 Part A — Cruise's triple is PINNED to the pre-R96 constants (cap 1.0,
    /// aggression 1.0, slow-factor 4.0 = `steering::WAYPOINT_SLOW_FACTOR`); the
    /// other profiles map to their own knobs.
    #[test]
    fn profile_params_pins_cruise_to_baseline_constants() {
        let t = AiTuning::default();
        assert_eq!(
            t.profile_params(MovementProfile::Cruise),
            (1.0, 1.0, 4.0),
            "Cruise is the byte-identical parity triple"
        );
        assert_eq!(
            t.profile_params(MovementProfile::Rush),
            (
                t.profile_rush_cap,
                t.brake_aggression_rush,
                t.arrive_slow_factor_rush
            )
        );
        assert_eq!(
            t.profile_params(MovementProfile::Leisurely),
            (
                t.profile_leisurely_cap,
                t.brake_aggression_leisurely,
                t.arrive_slow_factor_leisurely
            )
        );
    }

    /// RON round-trip (the SimTuning dev-settings persistence pattern): defaults
    /// serialize → deserialize back to the exact same values.
    #[test]
    fn default_aituning_round_trips_through_ron() {
        let t = AiTuning::default();
        let text = ron::ser::to_string(&t).expect("AiTuning serializes to RON");
        let back: AiTuning = ron::from_str(&text).expect("AiTuning deserializes from RON");
        assert_eq!(back, t);
    }

    /// `#[serde(default)]` per-field fallback: an older/partial RON (only some
    /// fields present) fills every missing field from the pinned default.
    #[test]
    fn partial_ron_falls_back_per_field() {
        let t: AiTuning = ron::from_str("(aoi_radius_active: 99.0)").expect("partial RON parses");
        assert_eq!(t.aoi_radius_active, 99.0);
        let d = AiTuning::default();
        assert_eq!(t.aoi_radius_mid, d.aoi_radius_mid);
        assert_eq!(t.think_ticks_dormant, d.think_ticks_dormant);
    }
}
