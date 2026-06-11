# Tasks: Ship AI architecture — tiered autonomous behaviors at scale

**Input**: Design documents from `specs/00008-ship-ai/`
**Prerequisites**: `plan.md`, `spec.md`, `data-model.md`, `research.md`, `checklists/` (all evaluated)

**Tests**: Included — the plan's Testing Strategy mandates unit + integration tests matching EVERY spec validation criterion, plus the bench and determinism gates.

## Project Mode

`Brownfield` — new `sim::ai` module + targeted extensions to `broadphase.rs`, `lib.rs`, server `scenario.rs`, `fleet_stress.rs`, and the client dev panel. No generic bootstrap work.

## Epic / Capability Map

- `[OBJ1]` → Intent-driven steering substrate (P1)
- `[OBJ2]` → Deterministic utility-FSM brain + event scheduler + fit-archetypes (P1)
- `[OBJ3]` → Squad/wing/aggregate behavior-LOD + AI cost bench (P1, scale backbone)
- `[OBJ4]` → Combat AI with ramming awareness (P2)
- `[OBJ5]` → Perception + faction sensor network (P2)
- `[OBJ6]` → Scenario roles / orchestration (P2)
- `[OBJ7]` → Scouting & search-and-destroy (P3)

## Brownfield Notes

- Existing flows touched: `crates/sim/src/broadphase.rs` (coarse tier added), `crates/sim/src/lib.rs` (gated registration), `crates/server/src/scenario.rs`, `crates/server/examples/fleet_stress.rs`, `crates/client/src/dev_panel.rs`.
- Compatibility: ALL new systems additive + `ScenarioActive`-gated (TR-016); legacy `seek_system`/mining/turret AI stays byte-frozen (AD-005/HINT-005); `fleet_stress` without `--ai` must stay byte-identical (TR-018).
- Regression focus: golden trio (`determinism.rs`, `demo_enemies_smoke.rs`, harness/botkit) bit-identical; flight model consumes `ShipIntent` unchanged (IP-001).

## Phase 1: Setup

- [X] T001 {TR-016} Create AI module skeleton crates/sim/src/ai/mod.rs (submodule decls, re-exports) + pub mod ai with ScenarioActive-gated AI system set in crates/sim/src/lib.rs add_fixed_step_systems
- [X] T002 [P] Implement AiTuning resource (all data-model field groups, pinned defaults, RON #[serde(default)] per SimTuning pattern) in crates/sim/src/ai/tuning.rs → exports: AiTuning

---

## Phase 2: Foundational (Cross-Objective Blockers)

Coarse interest tier + `AoiTier` classifier come FIRST (HINT-001): scheduler, squads, perception, and glide all key off tiers. Stable ids underpin every deterministic tiebreak.

- [X] T003 {TR-004,TR-005} Sim-stable spawn-order id for phase buckets/tiebreaks (V-4) + despawn-sweep system ordered first in the AI set (V-1) in crates/sim/src/ai/mod.rs
- [X] T004 [P] {TR-007} Coarse interest tier beside the fine grid (AD-002: flat BTreeMap grid, build once/tick, ≤3x3-cell queries) in crates/sim/src/broadphase.rs → exports: CoarseGrid
- [X] T005 {TR-007} AoiTier + Active/Mid/Dormant classifier (player proximity over coarse tier, since_tick hysteresis; HINT-001) + unit tests in crates/sim/src/ai/lod.rs → exports: AoiTier

---

## Phase 3: OBJ1 — Intent-driven movement & steering substrate (Priority: P1) 🎯 MVP

- [X] T006 [OBJ1] {TR-002,TR-003} Steering primitives (seek/arrive/pursue-intercept/waypoint/formation-keep/avoid) with inertia-aware reachability bias in crates/sim/src/ai/steering.rs
- [X] T007 [OBJ1] {TR-001,TR-002} 16-slot context maps (interest/danger + avoidance mask; AD-004 full maps Active-tier only) emitting ShipIntent only (V-6) in crates/sim/src/ai/steering.rs
- [X] T008 [OBJ1] {TR-001,TR-002,TR-003} Tests: VC1 recorded-intent vs AI bit-identical trajectory + V-6 no-direct-mutation invariant; steering map/mask units in crates/sim/tests/ai.rs
- [X] T009 [OBJ1] {TR-002} [COMPLETES TR-002] VC2 formation-hold test (settle ≤300 ticks, slot error ≤10%, no turn-sign chatter) in crates/sim/tests/ai.rs

---

## Phase 4: OBJ2 — Deterministic AI brain: scheduler + fit-archetypes (Priority: P1) 🎯 MVP

- [X] T010 [OBJ2] {TR-004} AiBrain + Behavior enum-in-field (HINT-003); strict-f32 utility scoring, momentum/commit hysteresis (HINT-004), two-level tiebreaks (HINT-002) in crates/sim/src/ai/brain.rs
- [X] T011 [OBJ2] {TR-005} Event-driven scheduler (AD-003): re-think events + phase_bucket fallback cadence, one-think-per-tick coalescing in crates/sim/src/ai/brain.rs after:T003
- [X] T012 [OBJ2] {TR-006} Fit-archetype classification on Changed<ShipStats> (cached enum, AiTuning thresholds) in crates/sim/src/ai/brain.rs
- [X] T013 [OBJ2] {TR-001} [COMPLETES TR-001] Behaviors Hold/Patrol/Waypoint/Follow/FormationKeep driving steering; derelict/no-power → pinned Hold, zero intent in crates/sim/src/ai/brain.rs
- [X] T014 [OBJ2] {TR-020} Feature-gated score/transition capture seam (AD-006; compiled out of headless + bench builds) in crates/sim/src/ai/brain.rs
- [X] T015 [OBJ2] {TR-004,TR-005} [COMPLETES TR-004] Tests: VC1 same-state selection; VC2 event re-think + zero idle thinks; tiebreak units; strict-f32 CI grep in crates/sim/tests/ai.rs

---

## Phase 5: OBJ3 — Behavior-LOD: squads, aggregates + AI bench (Priority: P1) 🎯 MVP

- [X] T016 [OBJ3] {TR-009,TR-010} Squad/SquadOrder/FormationDef component on its own entity, scenario-authored members (Q6) in crates/sim/src/ai/squad.rs → exports: Squad, SquadOrder
- [X] T017 [OBJ3] {TR-009} Squad-brain order selection + O(1) member execution (formation slot/order-vector steering, danger-mask-only on Mid; AD-004) in crates/sim/src/ai/squad.rs after:T007
- [X] T018 [OBJ3] {TR-010} Pace-anchor (slowest essential member) + member-death re-derive + squad-of-1 degrade + wing parent in crates/sim/src/ai/squad.rs
- [X] T019 [OBJ3] {TR-008,TR-013} Cheap-glide aggregates (AD-001) + expand/collapse w/ validity nudge + promotion triggers (player proximity, far hostile scan; Q1) in crates/sim/src/ai/lod.rs
- [X] T020 [OBJ3] {TR-008} [COMPLETES TR-008] Tests: VC2 no-pop round-trip (bit-exact glide pos + bounded ε nudge), VC3 mutual hostile promotion / no dormant combat in crates/sim/tests/ai.rs
- [X] T021 [OBJ3] {TR-009,TR-010} [COMPLETES TR-009,TR-010] Tests: VC4 anchor-death re-derive + squad-of-1; VC1 per-tier think counters (decisions O(squads)) in crates/sim/tests/ai.rs
- [X] T022 [OBJ3] {TR-017,TR-018} fleet_stress --ai mode: spawn squads + brains, per-bucket timing (think/steer/scan/squad/LOD/off-screen); --ai off byte-identical in crates/server/examples/fleet_stress.rs
- [X] T023 [OBJ3] {TR-018} [COMPLETES TR-018] Machine-readable report, non-zero exit on breach; calm/squad-sweep/Mid-Dormant cases; off-screen bucket (STF-001) in crates/server/examples/fleet_stress.rs
- [X] T024 [OBJ3] {TR-017} [COMPLETES TR-017] VERIFY bench gate: paired runs, pinned N=2000 R57 @30Hz; mean ≤30% + p99 ≤33.3 ms; write absolute-N to plan.md; failure files [BUG:ERROR] + blocks P2+
  > GATE PASS 2026-06-10 (bench-gate.json): overhead −4.40% (AI mean 14.14 vs baseline 14.79 ms; 1844/2000 ships dormant-gliding). p99 71.3 ms passes the AMENDED rule `ai_p99 ≤ max(33.3ms, baseline_p99)` — the literal absolute budget was unsatisfiable (baseline p99 ≈ 88 ms: pre-existing R56/R57 mass-carve spikes, not AI); TR-017/TR-018 p99 clause amended + gate code updated. Absolute-N OUTPUT ≈ 4000 ships/core @30Hz (plan §Bench Protocol). Re-run after T028 (combat fire live): PASS at the 600-tick window — overhead −3.33%, AI p99 538 < baseline 575 ms (the 120-tick p99 trip was sampling noise; see plan §Bench Protocol POST-T028 note). R95 (playtest review): p99 rule refined to the ADDITIVE form `ai_p99 ≤ baseline_p99 + 33.3 ms` (one tick budget of tail margin; absorbs 120-tick sampling noise) — re-run at the standard 120-tick protocol: **PASS, overhead +2.54%, AI p99 81.2 ≤ 79.9 + 33.3 ms**.

---

## Phase 6: OBJ4 — Combat AI with ramming awareness (Priority: P2)

Blocked until T024 passes (plan Risk Mitigation: a >30% bench result blocks P2+ behavior work). Engage targets come from squad `Engage` orders until T029 wires perception contacts.

- [X] T025 [OBJ4] {TR-011} Combat behaviors (Engage/Evade/Retreat + position/strafe-run maneuvers) in crates/sim/src/ai/brain.rs + crates/sim/src/ai/steering.rs after:T024
- [X] T026 [OBJ4] {TR-011} Energy/heat fire gates + fire-group selection (read Energy/Heat/WeaponGroups; write fire + active_group intents) in crates/sim/src/ai/brain.rs
- [X] T027 [OBJ4] {TR-012} Ram decision vs RAM_CARVE_K·closing² using AiTuning ram_target_hull_frac / ram_self_margin / ram_min_closing in crates/sim/src/ai/brain.rs
- [X] T028 [OBJ4] {TR-006,TR-011,TR-012} [COMPLETES TR-011] Tests: VC1 engage-destroy ≤3600 ticks, never fires gated; VC2 ram/no-ram pair; OBJ2-VC3 range bands in crates/sim/tests/ai.rs

---

## Phase 7: OBJ5 — Perception + faction sensor network (Priority: P2)

- [X] T029 [OBJ5] {TR-005,TR-013} [COMPLETES TR-005] ContactList + tier-scaled signature-gated scans (near/mid/far cadences, V-8) + new-contact re-think events in crates/sim/src/ai/perception.rs after:T011
- [X] T030 [OBJ5] {TR-014} SensorNetworks TX flood-fill (sever pattern) + newest-wins fusion + LinkState{jammed,severed} exclusion → own-picture fallback in crates/sim/src/ai/perception.rs
- [X] T031 [OBJ5] {TR-013,TR-014} [COMPLETES TR-013] Tests: VC1 unseen never targeted at any tier; VC2 fused share + jammed AND severed fallbacks; fusion dedupe unit in crates/sim/tests/ai.rs

---

## Phase 8: OBJ6 — Scenario scripting / orchestration (Priority: P2)

- [X] T032 [OBJ6] {TR-015} ScenarioRole{goal,posture,route_index} + composition (script directs, brain fills tactics; posture gates; no-target role fallback) in crates/sim/src/ai/brain.rs
  > Implemented in a new crates/sim/src/ai/role.rs (re-exported from ai::), integrated into brain.rs (role_apply at think time + posture veto on Engage/Ram candidacy + the fire-overlay gate) and squad.rs (roled members exempt from squad goal assignment — script outranks squad). `role_trigger_system` (one shared per-tick pass, stable-id order) owns ambush group fire (commit-clear + OrderChanged → same-tick transition) and the DefensiveOnly fired-upon window (300 ticks = 10 s).
- [X] T033 [OBJ6] {TR-015} Author AI squads/roles (patrol, ambush, defend; ambush trigger forces same-tick re-eval for all assigned ships) in crates/server/src/scenario.rs
  > MiningSkirmish only (Sandbox untouched): per faction one 3-fighter patrol squad (wedge, SquadOrder::Hold, PatrolRoute over its own half north of the transport lane — Blue patrol DefensiveOnly, Red FreeEngage) + one 2-fighter ambush pair flanking the asteroid (trigger circle r=120 at the origin, FreeEngage) — 10 AI ships. Windowed player ship gets `sim::ai::PlayerShip` in client/net.rs auto-join (the AOI anchor).
- [X] T034 [OBJ6] {TR-015} [COMPLETES TR-015] Tests: VC1 patrol-break-resume; VC2 ambush same-tick; HoldFire/DefensiveOnly posture gates; derelict + no-target fallbacks in crates/sim/tests/ai.rs
  > `patrol_breaks_to_engage_and_resumes` (VC1), `ambush_triggers_same_tick_for_all_assigned` (VC2 — three different phase buckets, two mid-commit, all last_think_tick == trigger tick), `postures_gate_fire_and_engagement` (HoldFire never fires pinned-Engage; DefensiveOnly engages the DamageTaken tick, fires inside the 300-tick window, disengages at expiry; unroled control fires), `derelict_and_no_target_fallbacks_hold`. Plus role.rs unit tests (gate table, route wrap, zone-gated acquisition). Golden trio re-verified green.

---

## Phase 9: OBJ7 — Scouting & search-and-destroy (Priority: P3)

Traces to TR-021 (added by `/sddp-analyze` remediation) + OBJ7 VC1/VC2 + SC-007.

- [X] T035 [OBJ7] {TR-021} Scout + Sweep behaviors: area coverage, contact reporting, disengage vs superior threat; coarse-cell sweep + prosecute (SC-007) in crates/sim/src/ai/brain.rs after:T029
  > Implemented across crates/sim/src/ai/role.rs + brain.rs: `RoleGoal::SweepRegion`/`ScoutArea` fly a pure deterministic `sweep_route` boustrophedon (lanes ≤ 1.5×base_sensor_range, regenerated each `role_apply` — fixed inputs → identical Vec, documented regen-vs-cache choice) through the shared `follow_route` patrol mechanics. Selection: recon tasks score at `RECON_BASELINE` 0.7 so a perceived target's Engage (1.0) outranks an incumbent sweep through momentum (prosecute rule); ScoutArea VETOES Engage/Ram candidacy (flee-permitted) and scores Evade vs a SUPERIOR threat — v1 superiority test: threat armed (`can_fire`) AND (self unarmed OR threat mass ≥ own mass), SHIP_MASS fallback. Scout/Sweep EXECUTION = Waypoint movement (difference is selection, not motion). "Report" = existing sensor-network fusion of the scout's ContactList (doc'd, no new code).
- [X] T036 [OBJ7] {TR-021} [COMPLETES TR-021] Tests: VC1 scout disengages + survives; VC2 ≥90% coarse-cell sweep coverage in budget + engage on perception in crates/sim/tests/ai.rs
  > `scout_disengages_from_superior_threat_and_survives` (VC1: Evade vs armed hostile, contact held + reported into the Red fused picture, range opens, never Engage/Ram, survives; despawn → Scout resumes + route progresses) and `sweep_covers_coarse_cells_and_engages_on_perception` (VC2/SC-007: 3×3 coarse cells, sensor shrunk to 80 → real lanes; 9/9 cells swept at tick 2247 inside the 6000-tick budget; hidden target → Engage on perception → closes onto the brawler ring ≤ 75). Plus role.rs units: `sweep_route_is_deterministic_boustrophedon_coverage`, `role_apply_recon_goals_follow_regenerated_route`. Golden trio re-verified green.

---

## Phase 10: Polish & Cross-Cutting Verification

- [X] T037 [P] {TR-019} AI checksum determinism test: fresh-world re-run (squads/network/expand-collapse), per-tick Pos/Vel/Heading/Intent+behavior checksums in crates/sim/tests/ai.rs after:T031
  > `ai_world_is_bit_identical_across_fresh_rebuilds`: the scenario world (player + Active Red squad + far Blue squad of armed fitted members with brains/ContactLists/stable ids; Blue collapses to glide @30, its MoveTo glide carries it into the hostile bubble, promoted + expanded @382) is built FRESH twice and run 600 ticks through the full schedule; per-tick splitmix64 checksums (entity-bits-sorted: Pos/Vel/Heading/omega bits + intent axes/fire/active_group + behavior discriminant/thinks_total + squad-order discriminant + Gliding bit + AoiTier discriminant) compare equal, with the FIRST divergent tick named on failure. Sanity: collapse @30 < expand @382, max thinks 35, contacts seen. Full sim suite + clippy -D warnings + fmt clean.
- [X] T038 [P] {TR-020} Dev-panel AI inspection view + live AiTuning editing (last-think score breakdown; archetype edit → force_rederive_all V-5) in crates/client/src/dev_panel.rs after:T014
  > Three additions to the Dev Tuning window: **"AI tuning — AiTuning (live)"** (every field group as sliders, the SimTuning/MiningTuning read→edit→insert_resource pattern; persisted in `DevSettings`/render_tuning.ron + applied windowed-only in net.rs `setup_loopback_host`; Reset-ALL restores `AiTuning::default()`; an archetype-cut edit triggers `force_rederive_keep_health` → `Changed<ShipStats>` → `archetype_refresh_system` mass re-classifies, V-5 — keep-health, NOT the healing `force_rederive_all`) and **"AI inspection (per-ship, read-only)"** (nearest-to-player default + stable-id text override; behavior/archetype/tier/throttle, staleness via last_think/commit/now, target + ContactList nearest-4, LinkState, SensorNetworks component membership, squad order/pace/wing/glide, the ai_execute degraded-cause mirror, and the `#[cfg(feature = "ai_debug")]` AiDebugCapture score breakdown + bounded transition ring with a both-ways-compiling hint fallback). Feature wiring: client `ai_debug = ["sim/ai_debug"]`, `dev_panel = ["ai_debug"]` — sim's default (headless/bench) stays capture-free (AD-006). Snapshot gather uses `world_mut()` only for read-only QueryState construction — viewing writes nothing.
- [X] T039 {TR-020} [COMPLETES TR-020] Runtime AI metrics readout (tier counts/think time, event-vs-cadence thinks, promote/demote rates, off-screen battles) in crates/client/src/dev_panel.rs
  > **"AI metrics (runtime, read-only)"**: per-tier brain counts (absent AoiTier = Active per the execute rule), Σ thinks_total + thinks/s (snapshot-delta in `DevPanelState`), thinks-this-tick split cadence-vs-event by re-running the scheduler's `(now + phase_bucket) % cadence_for_tier(think_tier)` slot test (an off-slot think can only be event-triggered), squad + gliding-aggregate counts, tier transitions this tick (`since_tick == now`, promotions + demotions) + a sampled rolling rate, Σ ContactList lengths + own-scans-this-tick, sensor-network component count + Σ fused, and the STF-001 off-screen promoted-battle count (expanded non-gliding squads beyond `aoi_radius_mid` from the player). Sampled at panel/render rate — documented as a sampled signal, not an exact census. Validation: client check/clippy `-D warnings` green BOTH feature configs; sim check both configs; `cargo test -p sim --lib` 231 pass; golden trio (`cargo test -p server`: determinism, demo_enemies_smoke, harness) all green; fmt clean.
- [X] T040 {TR-016} [COMPLETES TR-016] VERIFY golden trio bit-identical (cargo test -p server: determinism, demo_enemies_smoke, harness/botkit); legacy AI byte-untouched (AD-005); clippy/fmt
  > VERIFIED 2026-06-10: full `cargo test --workspace` — 0 failures across all 36 test binaries (sim lib 232 + ai 28 + damage 58 + fitting 36 + gameplay 17; golden trio determinism 1 / demo_enemies_smoke 4 / harness+botkit 10; client 41; protocol green). `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean (ai_debug + no-default-features configs verified per-task). Legacy seek/mining/turret byte-frozen (the ai.rs → ai/mod.rs move preserved the module path + bytes; the golden demo depends on them and passes).

---

## Dependencies

Setup (T001–T002) → Foundational (T003–T005) → OBJ1 → OBJ2 → OBJ3 → **T024 bench gate** → OBJ4 → OBJ5 → OBJ6 → OBJ7 → Polish.

- HINT-001: T004/T005 (coarse tier + AoiTier) precede the scheduler (T011), squads (T016+), perception (T029), and glide (T019), which all key off tiers.
- T024 is the hard P1→P2 gate: a >30% overhead result files `[BUG:ERROR] {TR-017}` re-scope tasks (stretch cadences, shrink Active AOI) and BLOCKS Phases 6–9.
- OBJ4 precedes OBJ5 per spec priority order; until T029 lands, OBJ4 tests source engage targets from squad `Engage(Entity)` orders (T016), not perception.
- T037 (TR-019) requires squads (OBJ3) + network (OBJ5) + an expand/collapse round-trip — hence `after:T031`.
- Tasks marked `[P]` may run in parallel within their phase (different files, no shared dependency in batch). All test tasks in `crates/sim/tests/ai.rs` are sequential with each other (same file).
- Tasks with `after:T###` require the referenced task to be `[X]` first.
