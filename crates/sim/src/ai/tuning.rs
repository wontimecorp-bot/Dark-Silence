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
            // Debug history ring (TR-020a).
            debug_history_len: 16,
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
