//! Shared simulation crate — the single source of gameplay truth.
//!
//! Both the authoritative server (Tier 0 per-tick integration) and the client
//! (prediction) run this exact code, and the transit layer (Tier 1) uses its
//! closed-form evaluator. The load-bearing invariant of the whole tiered design
//! lives in [`motion`]: the per-tick integrator and the analytic evaluator must
//! agree, so an entity demoted to a closed-form trajectory and later promoted
//! back into the live sim reappears exactly where the math said it would.
//!
//! E002 grows this crate with the single-player flight & combat gameplay —
//! flight dynamics, swept collision, weapon, combat, and seek AI — all as
//! headless `bevy_ecs` systems so the Bevy client stays a thin shell (ADR-0013).

// ECS systems take tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

pub mod ai;
pub mod broadphase;
pub mod clock;
pub mod collision;
pub mod combat;
pub mod components;
pub mod damage;
pub mod energy;
pub mod fitting;
pub mod flight;
pub mod intent;
pub mod mining;
pub mod motion;
pub mod physics;
pub mod scenario;
pub mod tuning;
pub mod turret;
pub mod voxelize;
pub mod weapon;

pub use clock::{CurrentTick, FixedDt};
pub use collision::{
    damage_flash_decay_system, fitted_damage_system, shield_hit_flash_decay_system,
};
pub use combat::HitFeedback;
pub use components::{
    hostile, AngularVelocity, CollisionRadius, CombatRules, Damage, DamageFlash, Destructible,
    Faction, FlightAssist, Heading, Health, Lifetime, Position, PrevPosition, Projectile,
    ProjectileFaction, ProjectileOwner, ShieldHitFlash, Ship, Target, TargetKind, Velocity, Weapon,
};
pub use fitting::{
    build_layout, cell_map, derive_ship_stats, hardpoint_arc, load_preset, module_at,
    preview_stats, recompute_ship_stats_system, resolve_hit, save_preset, CellOccupant, FitLayout,
    FitPreset, HitResolution, PresetId, ShipStats, WeaponProfile,
};
pub use intent::ShipIntent;
pub use mining::{
    mining_transport_system, Cargo, MiningState, MiningTransport, MiningTuning, RefinedResources,
};
pub use motion::{analytic, integrate, simulate, BodyState};
pub use physics::{Physics, RapierPhysics, SweptHit};
pub use scenario::{FactionSpawns, ScenarioActive};
pub use tuning::{SimTuning, Tuning};
pub use turret::{aim_angle, turret_system, Turret, TurretSpec};
pub use voxelize::{voxelize_pending_system, PendingVoxelize, VoxelizeOnHit};
pub use weapon::{damage_event_from_hit, WeaponSource};

use bevy_ecs::schedule::common_conditions::resource_exists;
use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule};

/// Register the shared fixed-step gameplay systems, in their **canonical order**,
/// onto a caller-owned [`Schedule`] (Principle II, HINT-003).
///
/// This is the single entry point both the authoritative server (E003) and the
/// client must use to advance the sim, so the two run **bit-identical** logic in
/// the same order — the determinism guarantee the reconciliation/prediction layer
/// (and the Phase 5 determinism test) depends on. It is purely additive: it
/// registers the existing `pub fn` gameplay systems unchanged (it does not modify
/// any system's behavior), `.chain()`ed so the order is deterministic.
///
/// The canonical order mirrors the client's `FixedUpdate` pipeline, minus the
/// client-only render-capture system (which is not gameplay):
///
/// 1. [`ai::seek_system`]
/// 2. [`flight::ship_motion_system`] (+ Phase M4 [`flight::wreck_motion_system`] integrating
///    `Wreck` bodies' inherited drift/spin, then [`damage::destruction::wreck_lifetime_system`]
///    despawning old wreckage)
/// 3. [`weapon::weapon_fire_system`]
/// 4. [`weapon::projectile_step_system`]
/// 5. [`collision::collision_detect_system`] — unfitted flat-`Health` hits (INV-D17)
/// 6. [`collision::fitted_damage_system`] — fitted per-module E007 pipeline (E007)
/// 7. [`collision::ram_collision_system`]
/// 8. [`damage::shield_regen_system`] — powered shield regen/decay (E007, gated)
/// 9. [`fitting::recompute_ship_stats_system`] — emergent re-derive (E007, gated)
/// 10. [`combat::destruction_system`]
/// 11. [`combat::feedback_decay_system`]
/// 12. [`collision::damage_flash_decay_system`] — per-entity hit-pop decay (E007)
/// 13. [`collision::shield_hit_flash_decay_system`] — per-entity shield-flash decay (E007)
///
/// **Ordering rationale (E007, FR-021/SC-002, INV-D16)**:
/// - `fitted_damage_system` runs right after the legacy `collision_detect_system`:
///   the two are mutually exclusive (unfitted vs fitted targets, INV-D17), so they
///   form the complete weapon-hit resolution for this tick.
/// - It mutates each struck ship's [`FitLayout`] (per-module health) and, on a
///   module kill, runs the destruction chain (`on_section_destroyed`).
/// - `recompute_ship_stats_system` MUST run **after** it so the `Changed<FitLayout>`
///   re-derive applies the emergent [`ShipStats`] drop **this** tick (SC-002 live —
///   a battered ship immediately flies/fires worse).
/// - `shield_regen_system` sits before the re-derive so a depleted/unpowered shield
///   regens/decays on the freshly-applied damage state.
///
/// **Graceful degradation (INV-D16)**: the two E007 query systems read content
/// resources ([`ShieldConfig`](damage::ShieldConfig) /
/// [`HullCatalog`](fitting::HullCatalog) + [`ModuleCatalog`](fitting::ModuleCatalog))
/// that the E002/E003/determinism worlds do **not** insert. They are therefore each
/// gated on `resource_exists` so they are simply **skipped** (never panic) in a
/// world without those resources — the unfitted-only worlds (the determinism bots,
/// E002/E003 tests) keep the exact prior behavior. `fitted_damage_system` is
/// exclusive (`&mut World`) and self-degrades (it finds no fitted targets / bails on
/// a missing resource), so it needs no gate.
///
/// The caller is responsible for inserting the resources the **base** systems read
/// ([`FixedDt`], [`Tuning`], [`HitFeedback`]) into the `World` before running the
/// schedule, and for attaching a [`ShipIntent`] **component** to every piloted ship
/// (intent is per-entity, not a global resource — the server drives N
/// independently-controlled ships in one shared step). For the **fitted** E007 path
/// to resolve, the caller additionally inserts
/// [`ResistanceMatrix`](damage::ResistanceMatrix),
/// [`PenetrationConfig`](damage::PenetrationConfig),
/// [`ShieldConfig`](damage::ShieldConfig),
/// [`SalvageConfig`](damage::SalvageConfig),
/// [`ModuleCatalog`](fitting::ModuleCatalog), and
/// [`HullCatalog`](fitting::HullCatalog); absent these the fitted path is a
/// well-defined no-op.
pub fn add_fixed_step_systems(schedule: &mut Schedule) {
    schedule.add_systems(
        (
            ai::seek_system,
            // Mining skirmish: the AI transports' navigate→load→return→unload economy loop. Runs
            // after `seek_system` (which integrates a transport's Velocity→Position, since the
            // transport is a `Target`) so it reads the fresh position + sets next-tick velocity.
            // Gated on `ScenarioActive` → a no-op in every non-scenario / determinism / test world.
            mining::mining_transport_system.run_if(resource_exists::<scenario::ScenarioActive>),
            // AI systems (00008-ship-ai, TR-016): registered here per-task, all
            // `.run_if(resource_exists::<scenario::ScenarioActive>)` so every
            // non-scenario world (determinism harness, botkit, demo goldens) is
            // bit-identical to today. Canonical intra-set order: despawn-sweep
            // FIRST (V-1), then coarse-index build → LOD/tier classify →
            // perception → squad → brain → steering — all BEFORE
            // `ship_motion_system` so brains' `ShipIntent`
            // is consumed this tick (TR-001). The AI set + its intent consumer
            // (`ship_motion_system`) are grouped into one chained sub-tuple so
            // the outer chained tuple stays within Bevy's 20-arity limit as AI
            // systems accumulate (same trick as the ram group below).
            (
                // T003 (V-1): prune dangling Entity refs in AI state before any
                // AI system reads them. A true no-op until brains/squads/contacts
                // exist, so the scenario goldens (which DO carry `ScenarioActive`)
                // stay bit-identical.
                ai::ai_despawn_sweep_system.run_if(resource_exists::<scenario::ScenarioActive>),
                // T004 {TR-007}: rebuild the coarse interest tier each tick
                // (build-once-read-many for the AOI/LOD classifier, far scans, and
                // promotion triggers; HINT-001). Double-gated (the
                // `recompute_ship_stats_system` pattern): a `ScenarioActive` world
                // that never inserted the index skips it instead of panicking. It
                // writes ONLY `CoarseIndex` — no gameplay state — so the scenario
                // goldens (demo_enemies_smoke) stay bit-identical.
                broadphase::build_coarse_index_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<broadphase::CoarseIndex>),
                // R96 Part D: rebuild the per-tick `ObstacleField` (the large
                // neutral bodies the move/combat arms steer around) right after
                // the coarse-index build. Double-gated (the
                // `build_coarse_index_system` pattern): a `ScenarioActive` world
                // that never inserted the field skips it instead of panicking.
                // It writes ONLY `ObstacleField` — no gameplay state — and no
                // golden world spawns an `AiBrain` to CONSUME it, so even
                // `demo_enemies_smoke` (which DOES populate the field from its
                // `Target` bodies) stays bit-identical.
                broadphase::build_obstacle_field_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<broadphase::ObstacleField>)
                    .run_if(resource_exists::<ai::AiTuning>),
                // T005 {TR-007}: classify each `AoiTier` carrier Active/Mid/
                // Dormant from authoritative player proximity, with promotion-
                // asymmetric hysteresis — after the coarse-index rebuild,
                // before every tier consumer (scheduler/perception/squad/brain,
                // later tasks). Triple-gated (graceful degradation): a world
                // without the AI resources skips it. It writes ONLY `AoiTier`
                // — additive state nothing else reads yet — so the scenario
                // goldens (demo_enemies_smoke) stay bit-identical.
                ai::classify_aoi_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>),
                // T029 {TR-005, TR-013}: tier-cadence signature-gated
                // perception scans into per-ship ContactLists + NewContact
                // re-think events. AFTER the coarse-index rebuild + AOI
                // classify (it reads both) and BEFORE squad_think/ai_think —
                // the documented ordering choice: this tick's squad decisions
                // and thinks see this tick's contacts, and squad Engage orders
                // still override perception-acquired targets (squad_think
                // re-asserts member targets after the scan). Five-fold gated
                // (graceful degradation); no golden world spawns a
                // `ContactList`, so the scenario goldens stay bit-identical.
                ai::perception_scan_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<broadphase::CoarseIndex>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                // T030 {TR-014}: per-faction sensor-network flood-fill +
                // newest-wins fusion at the mid scan cadence, with fused
                // write-back into member ContactLists (jammed/severed ships
                // excluded → local-only fallback). AFTER the local scans
                // (fusion sees this tick's detections) and BEFORE
                // squad_think/ai_think (thinks see the fused picture). The
                // resource write is `!=`-guarded, so the golden scenario
                // worlds (no ContactLists → empty map) stay bit-identical.
                ai::sensor_network_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<ai::SensorNetworks>),
                // T017 {TR-009, TR-010}: the squad brain — centroid Position
                // upkeep every tick, plus (at the squad's tier cadence, or
                // same-tick on a membership change) the assignment pass that
                // translates the squad order into member brain state (leader
                // waypoint + formation-keep wingmen + pace throttle cap):
                // O(squads) decisions, O(1) member execution. AFTER the AOI
                // classify (it reads the squad's fresh tier) and BEFORE the
                // member think/execute so squad orders constrain member brains
                // the same tick. Quadruple-gated (graceful degradation); no
                // golden world spawns a `Squad`, so the scenario goldens stay
                // bit-identical.
                ai::squad_think_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                // T019 {TR-008}: collapse hysteresis-settled Dormant squads
                // into cheap-glide aggregates (AD-001: members stay live,
                // marked `Gliding`, skipped by flight). AFTER squad_think (the
                // centroid Position is fresh at collapse) and BEFORE the far
                // scan / glide step. Triple-gated; no golden world spawns a
                // `Squad`, so the scenario goldens stay bit-identical.
                ai::glide_collapse_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>),
                // T019 {TR-013, Q1}: the far hostile scan — at the far cadence,
                // a dormant/gliding/contact-holding squad that detects a
                // hostile-factioned body over the coarse grid promotes to Mid
                // and gains/refreshes the `HostileContact` demotion hold
                // (mutual promotion emerges from each squad's own scan).
                // BEFORE glide_motion so a scan promotion expands the SAME
                // tick. Quadruple-gated; goldens spawn no `Squad`.
                ai::far_hostile_scan_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<broadphase::CoarseIndex>),
                // T019 {TR-008}: per gliding squad, EXPAND when its tier left
                // Dormant (members resume full physics at their bit-exact
                // glide positions, reverse-glide validity nudge ≤
                // promote_nudge_max, re-think events pushed) or advance the
                // glide one tick (squad pos += vel·dt, members = pos +
                // offset). AFTER the promotion triggers (classify + far scan)
                // and BEFORE think/execute/ship_motion so an expanded member
                // steers + flies full physics this very tick. Quadruple-gated;
                // goldens spawn no `Squad`/`GlideState`.
                ai::glide_motion_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<broadphase::CoarseIndex>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                // T032 {TR-015}: the scenario-role trigger pass — ONE shared
                // per-tick evaluation over role-carrying ships in stable-id
                // order: DefensiveOnly fired-upon bookkeeping, ambush hold
                // (target release), and ambush trigger groups firing TOGETHER
                // (target + commit-clear + OrderChanged for every assigned
                // ship the SAME tick, OBJ6-VC2). AFTER the perception scan +
                // network fusion (triggers see this tick's contacts) and the
                // squad/glide passes, and BEFORE `ai_think_system` (fired
                // ships think + transition this very tick). Triple-gated; no
                // golden world spawns a `ScenarioRole`, so the goldens stay
                // bit-identical.
                ai::role_trigger_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                // T012 {TR-006}: recompute the cached fit-archetype ONLY on
                // `Changed<ShipStats>` (V-5) — before the think so a brain's
                // selection sees this tick's archetype. Double-gated; no golden
                // world spawns an `AiBrain`, so its query is empty there and
                // the scenario goldens (demo_enemies_smoke) stay bit-identical.
                ai::archetype_refresh_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>),
                // T011 {TR-005, AD-003}: the event-driven think scheduler —
                // queued re-think events react this tick, plus the phase-bucket
                // fallback cadence; one think per brain per tick. After the AOI
                // classify (it mirrors the tier) and BEFORE `ship_motion_system`
                // so a think's behavior switch steers the SAME tick once T013
                // wires behaviors → steering. Quadruple-gated (graceful
                // degradation); it writes only `AiBrain` fields + drains the
                // `RethinkQueue`, and no golden world spawns an `AiBrain`, so
                // the scenario goldens stay bit-identical.
                ai::ai_think_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::AiTuning>)
                    .run_if(resource_exists::<clock::CurrentTick>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                // T013 {TR-001}: the EXECUTION half — every tick, each
                // Active/Mid brain's selected behavior becomes steering math
                // emitting `ShipIntent` ONLY (V-6); derelict/unpowered fitted
                // ships are pinned to zero intent (graceful degrade). AFTER
                // the think (this tick's selection steers this tick) and
                // BEFORE `ship_motion_system` (which consumes the intent).
                // Double-gated (graceful degradation: it pushes `Arrived`
                // into the `RethinkQueue`); no golden world spawns an
                // `AiBrain`, so the scenario goldens stay bit-identical.
                ai::ai_execute_system
                    .run_if(resource_exists::<scenario::ScenarioActive>)
                    .run_if(resource_exists::<ai::RethinkQueue>),
                flight::ship_motion_system,
            )
                .chain(),
            // Phase M4: wreckage drifts/tumbles on its inherited velocity+spin (no thrust/drag),
            // moved before collision so a drifting wreck is hit at its current-tick position; and
            // a per-wreck lifetime despawns old debris (frictionless space never slows it). Both
            // are no-ops in a world with no `Wreck` entities.
            flight::wreck_motion_system,
            damage::destruction::wreck_lifetime_system,
            // Phase E: recharge the Energy capacitor + cool Heat each tick (no-op without the pools,
            // so determinism/botkit worlds are untouched). Before weapon_fire so the firing gate
            // sees this tick's recharged values.
            energy::energy_system,
            // Phase F: drain/recharge the afterburner pool (no-op without the pool).
            energy::afterburner_system,
            weapon::weapon_fire_system,
            // Mining skirmish: automated turrets aim (intercept lead + deterministic jitter) + fire
            // along their own heading; gated on `ScenarioActive` → a no-op in every non-scenario
            // world. After weapon_fire so turret shots spawn in the same phase as ship shots.
            turret::turret_system.run_if(resource_exists::<scenario::ScenarioActive>),
            weapon::projectile_step_system,
            collision::collision_detect_system,
            // Mining skirmish: lazy-voxelize a structure the first time it's hit (the flat path tagged
            // it `PendingVoxelize`) — build its cell hull + swap, BEFORE `fitted_damage_system` so it
            // is a carve target from here on. Gated on `ScenarioActive` → a no-op everywhere else.
            voxelize::voxelize_pending_system.run_if(resource_exists::<scenario::ScenarioActive>),
            collision::fitted_damage_system,
            // Ram (asteroid, elastic) then the solid-structure wall, grouped into one chained
            // sub-tuple so the outer system tuple stays within Bevy's chained-tuple arity limit
            // (20). The inner `.chain()` keeps ram before the structure push-out; the outer chain
            // keeps the whole group between `fitted_damage_system` and `shield_regen_system`.
            (
                collision::ram_collision_system,
                // Mining skirmish (Refinement 10): ship↔structure RAM — the craft bounces off the
                // outpost/transport (by mass) and takes carve damage proportional to the impact (a
                // fast slam can wreck it); then integrate any shoved movable structure's drift. Both
                // gated on `ScenarioActive` (only touch windowed-only entities) → a no-op headless.
                collision::structure_ram_system.run_if(resource_exists::<scenario::ScenarioActive>),
                collision::structure_motion_system
                    .run_if(resource_exists::<scenario::ScenarioActive>),
            )
                .chain(),
            // E007 powered shield regen/decay — gated so an unfitted world (no
            // ShieldConfig) skips it without panicking (graceful degradation).
            damage::shield_regen_system.run_if(resource_exists::<damage::ShieldConfig>),
            // E007 emergent re-derive — runs AFTER fitted_damage_system so the
            // Changed<FitLayout> stat drop applies this tick (SC-002). Gated so a
            // world without the fit catalogs skips it (graceful degradation).
            fitting::recompute_ship_stats_system
                .run_if(resource_exists::<fitting::HullCatalog>)
                .run_if(resource_exists::<fitting::ModuleCatalog>),
            combat::destruction_system,
            combat::feedback_decay_system,
            // E007 per-entity hit-pop decay — bleeds each struck entity's `DamageFlash`
            // toward 0 alongside the global `HitFeedback` decay. Ungated: a world with
            // no `DamageFlash` entities is a no-op (graceful degradation, INV-D16).
            collision::damage_flash_decay_system,
            // E007 shield-hit deflector-shimmer decay — bleeds each struck entity's
            // `ShieldHitFlash` toward 0 so the client's cyan shield flash fades. Ungated:
            // a world with no `ShieldHitFlash` entities is a no-op (INV-D16).
            collision::shield_hit_flash_decay_system,
        )
            .chain(),
    );
}
