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

/// R103 (was R102 Part A) — the default [`AiTuning::glide_min_radius`] (world
/// units): the perceivable-space radius the dormant-glide cutoff is floored to,
/// so no ship ever runs the no-physics kinematic glide within ANY surface the
/// player can perceive (the main view OR the radar). 750.0 = client
/// `radar::RADAR_RANGE` (700u) + ~50u margin: the radar (not the ~375u main
/// view) is the WIDEST perceivable surface, so the floor must clear it. The 50u
/// margin puts the glide→physics handoff beyond the radar, so a squad expands at
/// 750u — ~50u before the 700u radar — and combined with the `skirmish_physics`
/// |Δv| ≤ 5 bound the handoff is settled physics before it is ever perceptible.
/// Keep in sync if `RADAR_RANGE` changes. A free `fn` so the `#[serde(default =
/// …)]` per-FIELD fallback restores this value when an older RON omits the field
/// (otherwise serde's container default would supply `f32::default()` = 0.0,
/// disabling the floor).
fn default_glide_min_radius() -> f32 {
    750.0
}

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
    /// R98 HOTFIX B3: must comfortably exceed the max-zoom view corner (~202 u)
    /// so dormant cheap-glide kinematics are NEVER visible near the player.
    pub aoi_radius_mid: f32,
    /// Minimum ticks between tier changes per entity — boundary hysteresis (no thrash).
    pub tier_hysteresis_ticks: u32,
    /// R103 (was R102 Part A) — the GLIDE-visibility FLOOR (world units): a hard
    /// lower bound on the Dormant/cheap-glide cutoff, DECOUPLED from the tunable
    /// [`Self::aoi_radius_mid`]. [`classify_aoi_system`](crate::ai::classify_aoi_system)
    /// floors the effective Dormant boundary at `aoi_radius_mid.max(glide_min_radius)`,
    /// so a ship within `glide_min_radius` of a player is NEVER classified
    /// `Dormant` (and so never collapses to the no-physics kinematic glide),
    /// **no matter how small the dev panel sets `aoi_radius_mid`**. This is the
    /// fix for the dormant-GLIDE LOD leak (allied/enemy ships "sliding across
    /// the screen without physics"): it must be ≥ client `radar::RADAR_RANGE`
    /// (700u) + ~50u margin so no ship glides within ANY surface the player can
    /// PERCEIVE — the main view OR the 700u radar (R102 floored only the ~375u
    /// main view at 600u and MISSED the wider radar; R103 raises it to 750.0).
    /// The 50u margin puts the glide→physics handoff beyond the radar so a ship
    /// is settled physics before it's perceptible. Keep in sync if `RADAR_RANGE`
    /// changes. Default 750.0. `aoi_radius_mid` still freely tunes the Active/Mid
    /// *think*-cadence split below this floor.
    #[serde(default = "default_glide_min_radius")]
    pub glide_min_radius: f32,
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
    /// R101 S5 — `Orbit` tangential speed as a fraction of the profile pace
    /// `v_max` (the `combat_intent` controller path): the desired tangential
    /// orbit velocity on-ring is `orbit_speed_frac · v_max · (1 − |radial|)`, so
    /// the ship circles at a fraction of cruise on its ring and blends to the
    /// radial correction off-ring. `(0, 1]` — 1.0 orbits at full pace. The
    /// struct-level `#[serde(default)]` falls an absent field back to the pinned
    /// default, so older RONs parse.
    pub orbit_speed_frac: f32,
    /// `Kite` standoff target as a multiple of `weapon_range` (1.1 = hold just
    /// beyond the envelope edge, so the gun still bears while the target chases).
    pub kite_range_frac: f32,
    /// R101 S5 — ON-BAND COMBAT WEAVE: the tangential weave speed (as a fraction
    /// of the profile pace `v_max`) a `Charge`/`Standoff` ship circles its ring at
    /// while ON-BAND, instead of dead-stopping (`v_des = 0`). A small, constant
    /// TANGENTIAL drift keeps the ship from being a sitting duck (and lets a
    /// `can_strafe` hull sidle so its bore rakes the hull), while keeping RANGE
    /// ~constant (the weave is purely tangential — it holds the ring). Default
    /// ~0.2 (a gentle circle, well inside the range band). `Orbit` already circles
    /// at `orbit_speed_frac` (a bigger amplitude) — the weave does NOT apply there
    /// (no double-apply). `Kite` opens/holds (never parks on-band), so it is
    /// unweaved too. `0.0` restores the legacy dead-stop. The struct-level
    /// `#[serde(default)]` falls an absent field back to the pinned default.
    pub combat_weave_frac: f32,
    /// R101 S5 — COMBAT RAKE: the gun's aim-jink half-amplitude as a fraction of
    /// the target's hull half-width — a slow deterministic sweep of the bore ACROSS
    /// the hull (perpendicular to the line of sight), so a parked combat ship's gun
    /// carves FRESH cells across the full width instead of boring one fixed tunnel
    /// that stalls. This is the half of the stationary-target fix that actually
    /// kills (works for forward-only hulls too — it is a few-degrees nose sweep at
    /// standoff range, far too small to perturb the ring-hold). Default ~0.6 (sweep
    /// most of the half-width); `0.0` aims dead at the live-cell centroid (no rake).
    /// `[0, ~1]`. The struct-level `#[serde(default)]` falls an absent field back.
    pub combat_rake_frac: f32,
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

    // --- R97 Phase 1 Stage A — drive/threat/collision primitives ---
    // Pinned placeholders the later R97 stages (B/C/D) consume; NOTHING reads
    // them yet on the execute path, so they are inert in every existing world
    // (golden trio unaffected). Pinned defaults keep golden/bench comparability.
    /// Recency window (ticks) over which a `last_damaged_tick` stamp counts as
    /// "recently fired upon" — the survival-pressure horizon Stage B/C read
    /// (`now − last_damaged_tick < this`). 90 ticks ≈ 3 s at 30 Hz.
    pub threat_recency_window_ticks: u64,
    /// Proximity range (world units) inside which a hostile contributes to the
    /// incoming-threat consideration — the spatial half of the threat scalar.
    pub threat_proximity_range: f32,
    /// Gain applied to the collision-imminence consideration when sizing its
    /// preemptive avoidance response (Stage D) — higher brakes/turns earlier.
    pub collision_preempt_gain: f32,
    /// Collision look-ahead horizon (s): the time window over which the
    /// time-to-collision is normalized for `con_collision_imminence`
    /// (`ttc / this`, clamped) — a collision beyond it scores ~0 imminence.
    pub collision_horizon_s: f32,
    /// Per-channel BASE weight (R97) for the MOVE-drive utility channel — a
    /// placeholder Stage B scales its move-intent considerations by. `1.0` = the
    /// neutral pass-through default.
    pub move_drive_weight: f32,
    /// Per-channel BASE weight (R97) for the AIM-drive utility channel — the
    /// twin of [`Self::move_drive_weight`] for the aim/fire considerations
    /// (Stage C). `1.0` = neutral.
    pub aim_drive_weight: f32,
    /// Floor applied to the flee/evade desire while weapons-free (R97): a
    /// non-zero floor keeps a minimum break-off willingness even mid-attack.
    /// `0.0` = no floor (the inert default — flee is purely score-driven).
    pub weapons_free_flee_floor: f32,

    // --- R102 Part B1 — disposition / personality knobs ---
    // The magnitudes the [`Disposition`](crate::ai::disposition::Disposition)
    // helper methods scale their traits by. Inert in every world WITHOUT a
    // `Disposition` (the gated plug-ins only run when the component is present),
    // so the golden trio is unaffected regardless of these values. Each is
    // `#[serde(default)]` via the struct attribute, so older RONs parse.
    /// R102 B1 — how strongly `caution` lifts the Evade/Retreat candidate score:
    /// `flee_scale = 1 + caution · this` (a skittish ship at caution 0.9 with the
    /// default ~1.0 flees ~1.9× as eagerly; a brave ship at caution 0 is ×1).
    pub disposition_caution_flee_scale: f32,
    /// R102 B1 — how strongly `aggression` lifts the Engage candidate score:
    /// `engage_scale = 1 + aggression · this` (a hunter at aggression 0.9 with the
    /// default ~0.5 engages ~1.45× as eagerly).
    pub disposition_aggression_engage_scale: f32,
    /// R102 B1 — the BASE leash radius (world units): the return-to-post distance
    /// a max-leash (berserker) ship reaches; a short-leash sentry holds a small
    /// fraction of it (see [`Disposition::leash_radius`](crate::ai::disposition::Disposition::leash_radius)).
    pub disposition_leash_base: f32,
    /// R102 B1 — the BASE lost-target grace (ticks): how long a `tenacity == 0`
    /// ship holds an out-of-contact target before clearing it; a tenacious ship
    /// holds `· (1 + tenacity · disposition_tenacity_grace_scale)` longer.
    pub disposition_target_grace_base: f32,
    /// R102 B1 — how strongly `tenacity` extends the lost-target grace:
    /// `grace = base · (1 + tenacity · this)` (a hunter at tenacity 0.9 with the
    /// default ~4.0 holds a lost target ~4.6× the base before dropping it).
    pub disposition_tenacity_grace_scale: f32,

    // --- R97 Phase 2 Stage E — strategic objective/planner tier (HTN) ---
    // The SLOW squad-objective planner knobs (`strategic_plan_system`). Inert in
    // every world without a `SquadObjective` (the planner's query is empty), so
    // the golden trio is unaffected regardless of these values.
    /// Strategic re-plan cadence, ticks (~3 s; the SLOW HTN decomposition runs
    /// every `strategic_plan_ticks`, offset by the squad's phase bucket).
    pub strategic_plan_ticks: u32,
    /// Outnumbered threshold: a `DestroyTarget` squad WITHDRAWS when the
    /// perceived enemy strength is `>= outnumbered_ratio ×` its own strength.
    pub outnumbered_ratio: f32,
    /// Regroup cohesion radius (world units): a squad is "cohered" when every
    /// member is within this distance of the centroid — the Regroup-complete test.
    pub regroup_cohesion_radius: f32,
    /// Arrive radius (world units) for the strategic planner's arrival tests:
    /// DefendZone hold, Withdraw-complete, PatrolRoute waypoint advance, and the
    /// escort-screening ring around a DestroyTarget.
    pub defend_arrive_radius: f32,
    /// R98 HOTFIX D — DefendZone engage-release HYSTERESIS: a squad already
    /// engaging an intruder keeps engaging while the intruder's last-seen
    /// position stays within `radius × this` of the anchor; it releases (falls
    /// back to acquisition / MoveTo) only when the intruder despawns, leaves
    /// the fused picture, or exits that wider release ring. `> 1` kills the
    /// Engage↔MoveTo flap for an intruder hovering on the acquisition edge.
    pub defend_release_factor: f32,
    /// R98 HOTFIX E — minimum ticks between `DamageTaken` re-think events per
    /// brain: sustained fire pushes at most one survival re-think per interval
    /// (the first-ever hit always pushes), while `last_damaged_tick` itself
    /// stays per-hit accurate for the threat-recency consideration.
    pub damage_rethink_interval: u64,

    // --- R101 unified motion controller (Stage S1) ---
    // The per-profile desired-velocity projection read by
    // [`AiTuning::control_params`]: a `(v_max, tau_track)` pair the R101
    // controller (`ai::control::allocate_intent`) consumes — `v_max` caps a
    // behavior's desired CLOSING/cruise speed (world units/s, on the seed
    // fighter's ~80 u/s top-speed scale) and `tau_track` is the velocity-tracking
    // time constant (s; smaller = snappier/harder accel, larger = gentler). These
    // are pinned placeholders in S1 — NOTHING reads them on the execute path yet
    // (the controller is wired in R101 S3/S5), so the golden trio is unaffected
    // regardless of these values.
    /// R101 — desired top speed (world units/s) for the `Rush` profile (hot pace
    /// — most of the seed fighter's ~80 u/s envelope).
    pub control_vmax_rush: f32,
    /// R101 — desired top speed (world units/s) for the `Cruise` profile (a
    /// moderate pace).
    pub control_vmax_cruise: f32,
    /// R101 — desired top speed (world units/s) for the `Leisurely` profile (a
    /// lazy pace).
    pub control_vmax_leisurely: f32,
    /// R101 — velocity-tracking time constant (s) for the `Rush` profile (small —
    /// snappy, hard acceleration onto the desired velocity).
    pub control_tau_rush: f32,
    /// R101 — velocity-tracking time constant (s) for the `Cruise` profile.
    pub control_tau_cruise: f32,
    /// R101 — velocity-tracking time constant (s) for the `Leisurely` profile
    /// (large — gentle acceleration, a soft settle).
    pub control_tau_leisurely: f32,

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
            // AOI (R98 HOTFIX B3): Active within ~120 u of a player, Mid to
            // 520 u, beyond = Dormant. The Mid radius must clear the max-zoom
            // view corner (~202 u) with margin for the collapse/expand dance so
            // dormant cheap-glide kinematics never happen on screen; 30-tick
            // (1 s) hysteresis; nudge bounded to one fine cell (TR-008).
            aoi_radius_active: 120.0,
            aoi_radius_mid: 520.0,
            tier_hysteresis_ticks: 30,
            // R103 (was R102 Part A) — the Dormant/glide cutoff is floored to this
            // PERCEIVABLE-space radius (≥ client `radar::RADAR_RANGE` 700u + 50u
            // margin), so a ship the player can SEE OR SEE ON RADAR never collapses
            // to the no-physics kinematic glide regardless of how small
            // `aoi_radius_mid` is tuned. 750 clears the 700u radar (the widest
            // perceivable surface — wider than the ~375u main view R102's 600 only
            // covered) with a 50u glide→physics handoff margin.
            glide_min_radius: default_glide_min_radius(),
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
            // Combat stances (R96 Part C): orbit at the standoff ring with a
            // moderate tangential bank, kite just past the envelope edge, full
            // lateral strafe for hulls that can. None affect the Charge parity path.
            orbit_radius_frac: 1.0,
            orbit_tangential_weight: 0.6,
            // R101 S5: orbit tangentially at ~70% of the profile pace on-ring —
            // brisk enough to circulate clearly, slow enough to hold the ring
            // (the radial correction blends in off-ring). A playtest-feel knob.
            orbit_speed_frac: 0.7,
            kite_range_frac: 1.1,
            // R101 S5: on-band combat weave at ~20% of the profile pace — a gentle
            // tangential circle so a brawler/standoff ship is never a sitting duck
            // (and a strafe hull sidles, raking its bore); purely tangential, so the
            // range stays in-band. A playtest-feel knob (0.0 = the legacy dead-stop).
            combat_weave_frac: 0.2,
            // R101 S5: rake the gun across ~60% of the target's hull half-width — a
            // slow deterministic bore sweep so the carve disconnects an off-centre
            // core instead of stalling in one tunnel. A playtest-feel knob.
            combat_rake_frac: 0.6,
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
            // R97 Phase 1 Stage A — drive/threat/collision primitives (pinned
            // placeholders for Stage B/C/D). Inert on the execute path today, so
            // the golden trio is unaffected regardless of these values.
            threat_recency_window_ticks: 90,
            threat_proximity_range: 250.0,
            collision_preempt_gain: 4.0,
            collision_horizon_s: 1.5,
            move_drive_weight: 1.0,
            aim_drive_weight: 1.0,
            weapons_free_flee_floor: 0.0,
            // R102 Part B1 — disposition / personality knobs. Sane v1 defaults:
            // caution roughly doubles the flee desire at full caution (1.0),
            // aggression adds ~half again to the engage desire at full aggression
            // (0.5), a 300 u base leash (so a sentry holds ~50–60 u of its post
            // and a hunter chases the full ~300 u), a 45-tick (1.5 s) base
            // lost-target grace, and a 4× tenacity extension (a hunter holds a lost
            // target ~7.5 s). Inert without a `Disposition`, so the golden trio is
            // unaffected regardless.
            disposition_caution_flee_scale: 1.0,
            disposition_aggression_engage_scale: 0.5,
            disposition_leash_base: 300.0,
            disposition_target_grace_base: 45.0,
            disposition_tenacity_grace_scale: 4.0,
            // R97 Phase 2 Stage E — strategic objective/planner tier (HTN).
            // SLOW 90-tick (3 s) re-plan cadence; withdraw when outnumbered
            // 1.5×; 40 u cohesion ring for Regroup; 50 u arrive radius for the
            // DefendZone/Withdraw/Patrol arrival + escort-screening tests. Inert
            // without a `SquadObjective`, so the golden trio is unaffected.
            strategic_plan_ticks: 90,
            outnumbered_ratio: 1.5,
            regroup_cohesion_radius: 40.0,
            defend_arrive_radius: 50.0,
            // R98 HOTFIX D — keep engaging an already-engaged DefendZone
            // intruder out to 1.25× the acquisition ring (release hysteresis).
            defend_release_factor: 1.25,
            // R98 HOTFIX E — at most one DamageTaken re-think per 15 ticks
            // (0.5 s at 30 Hz) of sustained fire.
            damage_rethink_interval: 15,
            // R101 unified motion controller (Stage S1): per-profile
            // (v_max, tau_track) for `control_params`. Tuned to the seed fighter's
            // ~80 u/s top speed — Rush flies hot (60 u/s) and tracks hard (0.25 s),
            // Cruise is moderate (45 / 0.35), Leisurely lazy (28 / 0.5). Pinned
            // placeholders; nothing reads them on the execute path yet.
            control_vmax_rush: 60.0,
            control_vmax_cruise: 45.0,
            control_vmax_leisurely: 28.0,
            control_tau_rush: 0.25,
            control_tau_cruise: 0.35,
            control_tau_leisurely: 0.5,
            // Debug history ring (TR-020a).
            debug_history_len: 16,
        }
    }
}

impl AiTuning {
    /// R101 Stage S1 — the unified-controller `(v_max, tau_track)` projection for
    /// `profile`: `v_max` is the desired CLOSING/cruise speed cap (world units/s)
    /// and `tau_track` is the velocity-tracking time constant (s) the R101
    /// controller (`ai::control::allocate_intent`) consumes. One `(v_max, tau)`
    /// preset per [`MovementProfile`]; read live by the nav/combat/survival arms
    /// (R101 S3/S5/S6).
    pub fn control_params(&self, profile: MovementProfile) -> (f32, f32) {
        match profile {
            MovementProfile::Rush => (self.control_vmax_rush, self.control_tau_rush),
            MovementProfile::Cruise => (self.control_vmax_cruise, self.control_tau_cruise),
            MovementProfile::Leisurely => (self.control_vmax_leisurely, self.control_tau_leisurely),
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
        // R96 Part C combat stances: positive ring/range fracs, a strafe magnitude in [0,1].
        assert!(t.orbit_radius_frac > 0.0 && t.orbit_tangential_weight > 0.0);
        // R101 S5: the orbit tangential-speed fraction is a positive `(0, 1]` knob.
        assert!(t.orbit_speed_frac > 0.0 && t.orbit_speed_frac <= 1.0);
        // R101 S5: the on-band combat weave is a gentle `[0, 1)` fraction of pace —
        // non-negative (0 = legacy dead-stop) and well below full pace (a small
        // tangential drift that holds the range band, never an orbit-scale circle).
        assert!((0.0..1.0).contains(&t.combat_weave_frac));
        // R101 S5: the combat rake jink is a `[0, 1]` fraction of the hull half-
        // width (0 = no rake; ~1 = sweep the full half-width).
        assert!((0.0..=1.0).contains(&t.combat_rake_frac));
        assert!(t.kite_range_frac > 0.0 && (0.0..=1.0).contains(&t.strafe_stance_lateral));
        // R96 Part D obstacle avoidance: positive weight/lookahead/pad/radii.
        assert!(t.obstacle_danger_weight > 0.0 && t.obstacle_lookahead_s > 0.0);
        assert!(t.obstacle_clearance_pad >= 0.0 && t.obstacle_query_radius > 0.0);
        assert!(t.obstacle_min_radius > 0.0);
        // R97 Stage A drive/threat/collision primitives: positive windows/ranges,
        // neutral (positive) channel weights, a non-negative flee floor.
        assert!(t.threat_recency_window_ticks > 0 && t.threat_proximity_range > 0.0);
        assert!(t.collision_preempt_gain > 0.0 && t.collision_horizon_s > 0.0);
        assert!(t.move_drive_weight > 0.0 && t.aim_drive_weight > 0.0);
        assert!(t.weapons_free_flee_floor >= 0.0);
        // R102 Part B1 disposition knobs: non-negative scales (a 0 scale is the
        // inert "trait does nothing" edge), a positive leash base + grace base.
        assert!(t.disposition_caution_flee_scale >= 0.0);
        assert!(t.disposition_aggression_engage_scale >= 0.0);
        assert!(t.disposition_leash_base > 0.0 && t.disposition_target_grace_base > 0.0);
        assert!(t.disposition_tenacity_grace_scale >= 0.0);
        // R97 Stage E strategic planner: positive slow cadence, an outnumbered
        // ratio >= 1, and positive cohesion/arrive radii.
        assert!(t.strategic_plan_ticks > 0 && t.outnumbered_ratio >= 1.0);
        assert!(t.regroup_cohesion_radius > 0.0 && t.defend_arrive_radius > 0.0);
        // R98 HOTFIX B3 — the Mid radius must clear the max-zoom view corner
        // (~202 u) so dormant kinematics never occur on screen.
        assert!(t.aoi_radius_mid > 202.0);
        // R103 (was R102 Part A) — the glide-visibility floor is positive and at
        // least the WIDEST perceivable surface (the 700u radar; the ~375u main
        // view is narrower), so a ship the player can see — on screen OR on radar
        // — never collapses to the no-physics glide even when `aoi_radius_mid` is
        // tuned smaller than the floor. `>= 700.0` is the radar bound (the 750
        // default carries +50u handoff margin over it).
        assert!(t.glide_min_radius >= 700.0);
        // R98 HOTFIX D — the release ring must be at least the acquisition ring
        // (>= 1 keeps the hysteresis a pure widening, never a shrink).
        assert!(t.defend_release_factor >= 1.0);
        // R98 HOTFIX E — a positive damage re-think interval.
        assert!(t.damage_rethink_interval > 0);
        // R101 unified controller: positive desired speeds + tracking time
        // constants, ordered by pace (Rush fastest/snappiest → Leisurely
        // slowest/gentlest).
        assert!(t.control_vmax_rush > 0.0 && t.control_vmax_cruise > 0.0);
        assert!(t.control_vmax_leisurely > 0.0);
        assert!(t.control_vmax_rush >= t.control_vmax_cruise);
        assert!(t.control_vmax_cruise >= t.control_vmax_leisurely);
        assert!(t.control_tau_rush > 0.0 && t.control_tau_cruise > 0.0);
        assert!(t.control_tau_leisurely > 0.0);
        assert!(t.control_tau_rush <= t.control_tau_cruise);
        assert!(t.control_tau_cruise <= t.control_tau_leisurely);
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
        // R103 (was R102 Part A) — `glide_min_radius` carries a per-FIELD serde
        // default (`#[serde(default = "default_glide_min_radius")]`): absent from
        // this partial RON it must restore 750.0, NOT `f32::default()` (0.0) — a 0
        // floor would re-open the dormant-glide leak.
        assert_eq!(t.glide_min_radius, d.glide_min_radius);
        assert_eq!(t.glide_min_radius, 750.0);
        // R97 Stage A fields are also `#[serde(default)]`: absent from this
        // older/partial RON, they fall back to the pinned defaults.
        assert_eq!(t.threat_recency_window_ticks, d.threat_recency_window_ticks);
        assert_eq!(t.move_drive_weight, d.move_drive_weight);
        assert_eq!(t.weapons_free_flee_floor, d.weapons_free_flee_floor);
        // R102 Part B1 disposition knobs are `#[serde(default)]` too: absent from
        // this older/partial RON, they fall back to the pinned defaults.
        assert_eq!(
            t.disposition_caution_flee_scale,
            d.disposition_caution_flee_scale
        );
        assert_eq!(
            t.disposition_aggression_engage_scale,
            d.disposition_aggression_engage_scale
        );
        assert_eq!(t.disposition_leash_base, d.disposition_leash_base);
        assert_eq!(
            t.disposition_target_grace_base,
            d.disposition_target_grace_base
        );
        assert_eq!(
            t.disposition_tenacity_grace_scale,
            d.disposition_tenacity_grace_scale
        );
        // R97 Stage E strategic planner fields are also `#[serde(default)]`:
        // absent from this partial RON, they fall back to the pinned defaults.
        assert_eq!(t.strategic_plan_ticks, d.strategic_plan_ticks);
        assert_eq!(t.outnumbered_ratio, d.outnumbered_ratio);
        assert_eq!(t.regroup_cohesion_radius, d.regroup_cohesion_radius);
        assert_eq!(t.defend_arrive_radius, d.defend_arrive_radius);
        // R98 HOTFIX fields are also `#[serde(default)]`: absent from an
        // older/partial RON, they fall back to the pinned defaults.
        assert_eq!(t.defend_release_factor, d.defend_release_factor);
        assert_eq!(t.damage_rethink_interval, d.damage_rethink_interval);
        // R101 unified-controller fields are also `#[serde(default)]`: absent from
        // this older/partial RON, they fall back to the pinned defaults.
        assert_eq!(t.control_vmax_rush, d.control_vmax_rush);
        assert_eq!(t.control_vmax_cruise, d.control_vmax_cruise);
        assert_eq!(t.control_vmax_leisurely, d.control_vmax_leisurely);
        assert_eq!(t.control_tau_rush, d.control_tau_rush);
        assert_eq!(t.control_tau_cruise, d.control_tau_cruise);
        assert_eq!(t.control_tau_leisurely, d.control_tau_leisurely);
        // R101 S5 — the orbit-speed-fraction knob is `#[serde(default)]` via the
        // struct attribute: absent from this older/partial RON, it falls back.
        assert_eq!(t.orbit_speed_frac, d.orbit_speed_frac);
        // R101 S5 — the on-band combat-weave + rake knobs are `#[serde(default)]`
        // too: absent from this older/partial RON, they fall back to the pinned
        // defaults.
        assert_eq!(t.combat_weave_frac, d.combat_weave_frac);
        assert_eq!(t.combat_rake_frac, d.combat_rake_frac);
    }
}
