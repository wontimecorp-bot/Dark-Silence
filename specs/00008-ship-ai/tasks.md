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

- [ ] T001 {TR-016} Create AI module skeleton crates/sim/src/ai/mod.rs (submodule decls, re-exports) + pub mod ai with ScenarioActive-gated AI system set in crates/sim/src/lib.rs add_fixed_step_systems
- [ ] T002 [P] Implement AiTuning resource (all data-model field groups, pinned defaults, RON #[serde(default)] per SimTuning pattern) in crates/sim/src/ai/tuning.rs → exports: AiTuning

---

## Phase 2: Foundational (Cross-Objective Blockers)

Coarse interest tier + `AoiTier` classifier come FIRST (HINT-001): scheduler, squads, perception, and glide all key off tiers. Stable ids underpin every deterministic tiebreak.

- [ ] T003 {TR-004,TR-005} Sim-stable spawn-order id for phase buckets/tiebreaks (V-4) + despawn-sweep system ordered first in the AI set (V-1) in crates/sim/src/ai/mod.rs
- [ ] T004 [P] {TR-007} Coarse interest tier beside the fine grid (AD-002: flat BTreeMap grid, build once/tick, ≤3x3-cell queries) in crates/sim/src/broadphase.rs → exports: CoarseGrid
- [ ] T005 {TR-007} AoiTier + Active/Mid/Dormant classifier (player proximity over coarse tier, since_tick hysteresis; HINT-001) + unit tests in crates/sim/src/ai/lod.rs → exports: AoiTier

---

## Phase 3: OBJ1 — Intent-driven movement & steering substrate (Priority: P1) 🎯 MVP

- [ ] T006 [OBJ1] {TR-002,TR-003} Steering primitives (seek/arrive/pursue-intercept/waypoint/formation-keep/avoid) with inertia-aware reachability bias in crates/sim/src/ai/steering.rs
- [ ] T007 [OBJ1] {TR-001,TR-002} 16-slot context maps (interest/danger + avoidance mask; AD-004 full maps Active-tier only) emitting ShipIntent only (V-6) in crates/sim/src/ai/steering.rs
- [ ] T008 [OBJ1] {TR-001,TR-002,TR-003} Tests: VC1 recorded-intent vs AI bit-identical trajectory + V-6 no-direct-mutation invariant; steering map/mask units in crates/sim/tests/ai.rs
- [ ] T009 [OBJ1] {TR-002} [COMPLETES TR-002] VC2 formation-hold test (settle ≤300 ticks, slot error ≤10%, no turn-sign chatter) in crates/sim/tests/ai.rs

---

## Phase 4: OBJ2 — Deterministic AI brain: scheduler + fit-archetypes (Priority: P1) 🎯 MVP

- [ ] T010 [OBJ2] {TR-004} AiBrain + Behavior enum-in-field (HINT-003); strict-f32 utility scoring, momentum/commit hysteresis (HINT-004), two-level tiebreaks (HINT-002) in crates/sim/src/ai/brain.rs
- [ ] T011 [OBJ2] {TR-005} Event-driven scheduler (AD-003): re-think events + phase_bucket fallback cadence, one-think-per-tick coalescing in crates/sim/src/ai/brain.rs after:T003
- [ ] T012 [OBJ2] {TR-006} Fit-archetype classification on Changed<ShipStats> (cached enum, AiTuning thresholds) in crates/sim/src/ai/brain.rs
- [ ] T013 [OBJ2] {TR-001} [COMPLETES TR-001] Behaviors Hold/Patrol/Waypoint/Follow/FormationKeep driving steering; derelict/no-power → pinned Hold, zero intent in crates/sim/src/ai/brain.rs
- [ ] T014 [OBJ2] {TR-020} Feature-gated score/transition capture seam (AD-006; compiled out of headless + bench builds) in crates/sim/src/ai/brain.rs
- [ ] T015 [OBJ2] {TR-004,TR-005} [COMPLETES TR-004] Tests: VC1 same-state selection; VC2 event re-think + zero idle thinks; tiebreak units; strict-f32 CI grep in crates/sim/tests/ai.rs

---

## Phase 5: OBJ3 — Behavior-LOD: squads, aggregates + AI bench (Priority: P1) 🎯 MVP

- [ ] T016 [OBJ3] {TR-009,TR-010} Squad/SquadOrder/FormationDef component on its own entity, scenario-authored members (Q6) in crates/sim/src/ai/squad.rs → exports: Squad, SquadOrder
- [ ] T017 [OBJ3] {TR-009} Squad-brain order selection + O(1) member execution (formation slot/order-vector steering, danger-mask-only on Mid; AD-004) in crates/sim/src/ai/squad.rs after:T007
- [ ] T018 [OBJ3] {TR-010} Pace-anchor (slowest essential member) + member-death re-derive + squad-of-1 degrade + wing parent in crates/sim/src/ai/squad.rs
- [ ] T019 [OBJ3] {TR-008,TR-013} Cheap-glide aggregates (AD-001) + expand/collapse w/ validity nudge + promotion triggers (player proximity, far hostile scan; Q1) in crates/sim/src/ai/lod.rs
- [ ] T020 [OBJ3] {TR-008} [COMPLETES TR-008] Tests: VC2 no-pop round-trip (bit-exact glide pos + bounded ε nudge), VC3 mutual hostile promotion / no dormant combat in crates/sim/tests/ai.rs
- [ ] T021 [OBJ3] {TR-009,TR-010} [COMPLETES TR-009,TR-010] Tests: VC4 anchor-death re-derive + squad-of-1; VC1 per-tier think counters (decisions O(squads)) in crates/sim/tests/ai.rs
- [ ] T022 [OBJ3] {TR-017,TR-018} fleet_stress --ai mode: spawn squads + brains, per-bucket timing (think/steer/scan/squad/LOD/off-screen); --ai off byte-identical in crates/server/examples/fleet_stress.rs
- [ ] T023 [OBJ3] {TR-018} [COMPLETES TR-018] Machine-readable report, non-zero exit on breach; calm/squad-sweep/Mid-Dormant cases; off-screen bucket (STF-001) in crates/server/examples/fleet_stress.rs
- [ ] T024 [OBJ3] {TR-017} [COMPLETES TR-017] VERIFY bench gate: paired runs, pinned N=2000 R57 @30Hz; mean ≤30% + p99 ≤33.3 ms; write absolute-N to plan.md; failure files [BUG:ERROR] + blocks P2+

---

## Phase 6: OBJ4 — Combat AI with ramming awareness (Priority: P2)

Blocked until T024 passes (plan Risk Mitigation: a >30% bench result blocks P2+ behavior work). Engage targets come from squad `Engage` orders until T029 wires perception contacts.

- [ ] T025 [OBJ4] {TR-011} Combat behaviors (Engage/Evade/Retreat + position/strafe-run maneuvers) in crates/sim/src/ai/brain.rs + crates/sim/src/ai/steering.rs after:T024
- [ ] T026 [OBJ4] {TR-011} Energy/heat fire gates + fire-group selection (read Energy/Heat/WeaponGroups; write fire + active_group intents) in crates/sim/src/ai/brain.rs
- [ ] T027 [OBJ4] {TR-012} Ram decision vs RAM_CARVE_K·closing² using AiTuning ram_target_hull_frac / ram_self_margin / ram_min_closing in crates/sim/src/ai/brain.rs
- [ ] T028 [OBJ4] {TR-006,TR-011,TR-012} [COMPLETES TR-011] Tests: VC1 engage-destroy ≤3600 ticks, never fires gated; VC2 ram/no-ram pair; OBJ2-VC3 range bands in crates/sim/tests/ai.rs

---

## Phase 7: OBJ5 — Perception + faction sensor network (Priority: P2)

- [ ] T029 [OBJ5] {TR-005,TR-013} [COMPLETES TR-005] ContactList + tier-scaled signature-gated scans (near/mid/far cadences, V-8) + new-contact re-think events in crates/sim/src/ai/perception.rs after:T011
- [ ] T030 [OBJ5] {TR-014} SensorNetworks TX flood-fill (sever pattern) + newest-wins fusion + LinkState{jammed,severed} exclusion → own-picture fallback in crates/sim/src/ai/perception.rs
- [ ] T031 [OBJ5] {TR-013,TR-014} [COMPLETES TR-013] Tests: VC1 unseen never targeted at any tier; VC2 fused share + jammed AND severed fallbacks; fusion dedupe unit in crates/sim/tests/ai.rs

---

## Phase 8: OBJ6 — Scenario scripting / orchestration (Priority: P2)

- [ ] T032 [OBJ6] {TR-015} ScenarioRole{goal,posture,route_index} + composition (script directs, brain fills tactics; posture gates; no-target role fallback) in crates/sim/src/ai/brain.rs
- [ ] T033 [OBJ6] {TR-015} Author AI squads/roles (patrol, ambush, defend; ambush trigger forces same-tick re-eval for all assigned ships) in crates/server/src/scenario.rs
- [ ] T034 [OBJ6] {TR-015} [COMPLETES TR-015] Tests: VC1 patrol-break-resume; VC2 ambush same-tick; HoldFire/DefensiveOnly posture gates; derelict + no-target fallbacks in crates/sim/tests/ai.rs

---

## Phase 9: OBJ7 — Scouting & search-and-destroy (Priority: P3)

Traces to TR-021 (added by `/sddp-analyze` remediation) + OBJ7 VC1/VC2 + SC-007.

- [ ] T035 [OBJ7] {TR-021} Scout + Sweep behaviors: area coverage, contact reporting, disengage vs superior threat; coarse-cell sweep + prosecute (SC-007) in crates/sim/src/ai/brain.rs after:T029
- [ ] T036 [OBJ7] {TR-021} [COMPLETES TR-021] Tests: VC1 scout disengages + survives; VC2 ≥90% coarse-cell sweep coverage in budget + engage on perception in crates/sim/tests/ai.rs

---

## Phase 10: Polish & Cross-Cutting Verification

- [ ] T037 [P] {TR-019} AI checksum determinism test: fresh-world re-run (squads/network/expand-collapse), per-tick Pos/Vel/Heading/Intent+behavior checksums in crates/sim/tests/ai.rs after:T031
- [ ] T038 [P] {TR-020} Dev-panel AI inspection view + live AiTuning editing (last-think score breakdown; archetype edit → force_rederive_all V-5) in crates/client/src/dev_panel.rs after:T014
- [ ] T039 {TR-020} [COMPLETES TR-020] Runtime AI metrics readout (tier counts/think time, event-vs-cadence thinks, promote/demote rates, off-screen battles) in crates/client/src/dev_panel.rs
- [ ] T040 {TR-016} [COMPLETES TR-016] VERIFY golden trio bit-identical (cargo test -p server: determinism, demo_enemies_smoke, harness/botkit); legacy AI byte-untouched (AD-005); clippy/fmt

---

## Dependencies

Setup (T001–T002) → Foundational (T003–T005) → OBJ1 → OBJ2 → OBJ3 → **T024 bench gate** → OBJ4 → OBJ5 → OBJ6 → OBJ7 → Polish.

- HINT-001: T004/T005 (coarse tier + AoiTier) precede the scheduler (T011), squads (T016+), perception (T029), and glide (T019), which all key off tiers.
- T024 is the hard P1→P2 gate: a >30% overhead result files `[BUG:ERROR] {TR-017}` re-scope tasks (stretch cadences, shrink Active AOI) and BLOCKS Phases 6–9.
- OBJ4 precedes OBJ5 per spec priority order; until T029 lands, OBJ4 tests source engage targets from squad `Engage(Entity)` orders (T016), not perception.
- T037 (TR-019) requires squads (OBJ3) + network (OBJ5) + an expand/collapse round-trip — hence `after:T031`.
- Tasks marked `[P]` may run in parallel within their phase (different files, no shared dependency in batch). All test tasks in `crates/sim/tests/ai.rs` are sequential with each other (same file).
- Tasks with `after:T###` require the referenced task to be `[X]` first.
