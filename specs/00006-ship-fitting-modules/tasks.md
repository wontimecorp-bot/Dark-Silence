# Tasks: Ship Fitting & Modules (E006)

**Feature Branch**: `00006-ship-fitting-modules`
**Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md) | **Data Model**: [data-model.md](data-model.md) | **Contracts**: [contracts/fitting-api.md](contracts/fitting-api.md)

## Project Mode

**Brownfield** — extends `crates/sim` (new `fitting` module set) and `crates/client` (new `fitting_ui` screen). Reuses E001 (`sim` core) + E002 (`flight`/`weapon`/`Tuning`). No new crate, no new workspace dependencies (`bevy_ecs`/`bevy`/`glam`/`serde` already present).

## Epic / Capability Map

- **Epic**: E006 — Ship fitting & modules `{PRD:CAP-003}{SAD:ADR-0008}`
- **US1 (P1)**: Fit a ship within its budgets → FR-006/007/008/009/010/011/013
- **US2 (P1)**: The fit drives flight & weapons (retires `Tuning`) → FR-014/015/016/017
- **US3 (P1)**: Placement is a survivability choice (hit-map + arcs) → FR-018/019/020/021
- **US4 (P2)**: Real tradeoffs and a hull ladder → FR-023
- **US5 (P3)**: Save, preview, reuse fits (fitting UI) → FR-012/024
- **Foundational**: domain keystone → FR-001/002/003/004/005/022/025

## Brownfield Notes

- **ADDS**: `crates/sim/src/fitting/{mod,module,hull,fit,validate,stats,layout,content}.rs`, `crates/sim/tests/fitting.rs`, `crates/client/src/fitting_ui/mod.rs`.
- **MODIFIES (`~`)**: `crates/sim/src/lib.rs` (register `fitting`, export `ShipStats`), `crates/sim/src/flight.rs` + `crates/sim/src/weapon.rs` (read `ShipStats` not `Tuning` — **BREAKING-CHANGE**), `crates/client/src/scene.rs` + `crates/client/src/main.rs` (spawn fitted ship, wire fitting-screen state).
- **Ordering (HINT-001/002, AD-003)**: pure `sim` fitting domain + tests FIRST → `Tuning`→`ShipStats` rewire SECOND → Bevy fitting UI LAST (cannot be headless-tested). The baseline seed fit MUST reproduce the E002 `Tuning` defaults so E001/E002 tests stay green.
- **Reused (do NOT redefine)**: `Position`/`Velocity`/`Heading`/`AngularVelocity`, `Health` (per-module in hit-map), `Weapon` (fit-populated), `Ship`, `integrate`/`analytic`, `Physics`/`SweptHit`; `Tuning` **demoted** to base-constant source + seed baseline.

---

## Phase 1: Setup

- [X] T001 Create the fitting module entry `crates/sim/src/fitting/mod.rs` (submodule re-exports only) and register `pub mod fitting;` in `crates/sim/src/lib.rs` → exports: fitting (module root)

---

## Phase 2: Foundational (domain model + content keystone)

- [X] T002 [P] {FR-001} Define shared enums `ModuleKind`, `HardpointType`, ordered `SlotSize` (S<M<L<XL), `Axis`, `Violation` in `crates/sim/src/fitting/module.rs` → exports: ModuleKind, HardpointType, SlotSize
- [X] T003 {FR-001,FR-025} [COMPLETES FR-001] Define data-driven `Module` stat block + `ModuleId` + `ModuleSpecifics` (power, cpu, mass, heat, health, hardpoint type/size) in `crates/sim/src/fitting/module.rs` ← T002:ModuleKind → exports: Module, ModuleId
- [X] T004 {FR-003,FR-004} Define `Hull` 2D cell-grid (`grid_dims`, sparse `cells`) + budgets (power/cpu/mass cap, base_mass) + `HullId`/`GridCell`/`SectionId` in `crates/sim/src/fitting/hull.rs` ← T002:SlotSize → exports: Hull, HullId, GridCell
- [X] T005 {FR-004,FR-020} Define `Slot` (`SlotId`, slot_type, size, coord, facing, is_weapon_mount) + `FiringArc{center,half_angle}` + hull `slots` in `crates/sim/src/fitting/hull.rs` after:T004 ← T002:HardpointType → exports: Slot, SlotId, FiringArc
- [X] T006 {FR-002,FR-005} Define `Fit{hull, assignments:Map<SlotId,ModuleId>}` + `ModuleRef` + bare install/remove map mutation (one per slot, INV-F04) in `crates/sim/src/fitting/fit.rs` ← T003:ModuleId ← T005:SlotId → exports: Fit, ModuleRef
- [X] T007 {FR-022,FR-025} [COMPLETES FR-025] Seed `ModuleCatalog`+`HullCatalog`: 2 hulls (fighter, corvette — scaling) + 6 archetypes (reactor/thruster/weapon/shield/armor/utility) in `crates/sim/src/fitting/content.rs` after:T006 → exports: seed_catalogs()

---

## Phase 3: US1 — Fit a ship within its budgets (Priority: P1) 🎯 MVP

**Goal**: `validate_fit` enforces slot type/size gating + per-axis budgets, names violations, treats empty hull as valid, and `budget_usage` gives live readouts. Pure-logic, headless-tested.

**Independent test**: place/remove modules, observe live budget readouts, get a named rejection on over-budget or slot mismatch, and a valid empty/baseline fit.

- [X] T008 [US1] {FR-009,FR-013} Define `BudgetUsage`/`AxisUsage{used,capacity,over}` + `budget_usage(&Hull,&Fit)` (power cap = hull cap + Σ reactor gen; mass = base + Σ mass) in `crates/sim/src/fitting/validate.rs` ← T007:seed_catalogs ← T006:Fit → exports: BudgetUsage, budget_usage()
- [X] T009 [P] [US1] {FR-006,FR-007} Implement slot type-match (INV-F01) + size-fit (INV-F02) gate helper producing `SlotTypeMismatch`/`SlotSizeMismatch` in `crates/sim/src/fitting/validate.rs` after:T008 ← T005:Slot ← T003:Module → exports: check_slot_fit()
- [X] T010 [US1] {FR-008,FR-010,FR-011} Implement `validate_fit(&Hull,&Fit)->FitValidation` (per-axis OverBudget, empty=valid INV-F05, valid==violations.is_empty() INV-F09, dangling-id reject INV-F13) in `crates/sim/src/fitting/validate.rs` after:T009 → exports: validate_fit(), FitValidation
- [X] T011 [US1] {FR-005,FR-006,FR-007} [COMPLETES FR-007] Promote install to validate-then-apply `Result<(),FitRejection>` (reject type/size/over-budget; remove frees budget) in `crates/sim/src/fitting/fit.rs` after:T010 ← T010:validate_fit → exports: FitRejection
- [X] T012 [P] [US1] {FR-006,FR-008,FR-010,FR-011} [COMPLETES FR-006] [COMPLETES FR-008] [COMPLETES FR-010] [COMPLETES FR-011] validate_fit unit tests in `crates/sim/tests/fitting.rs`: each axis named (SC-001), type/size mismatch, empty valid + remove frees (SC-002) after:T011

---

## Phase 4: US2 — The fit drives flight & weapons (Priority: P1) 🎯 MVP

**Goal**: `derive_ship_stats(&Hull,&Fit)->ShipStats` produces fit-derived flight + weapon profile with graceful floors, and the `sim` flight/weapon systems read `ShipStats` instead of the global `Tuning` (**BREAKING-CHANGE**). Baseline seed fit reproduces E002 `Tuning` defaults.

**Independent test**: two fits of one hull fly measurably differently; no-weapon fit cannot fire; better thrust raises top speed; crippled fit floors (no NaN/inf).

- [X] T013 [US2] {FR-014,FR-015,FR-017} Define `ShipStats` component (thrust/torque, total_mass, drags/inertia, power_supply/draw, cpu_draw, can_fire) + `WeaponProfile` in `crates/sim/src/fitting/stats.rs` ← T006:Fit → exports: ShipStats, WeaponProfile
- [X] T014 [US2] {FR-014,FR-015,FR-016,FR-017} Implement `derive_ship_stats(&Hull,&Fit)->ShipStats` (sum thruster thrust/torque, total_mass, can_fire iff weapon, floors INV-F07/F14) in `crates/sim/src/fitting/stats.rs` after:T013 ← T007:seed_catalogs → exports: derive_ship_stats()
- [X] T015 [US2] {FR-014} Export `ShipStats`/`derive_ship_stats` from `crates/sim/src/lib.rs` + add the fit-change recompute system (re-derive when `Fit` mutates, INV-F08) in `crates/sim/src/fitting/mod.rs` after:T014 → exports: recompute_ship_stats_system
- [X] T016 [US2] {FR-014,FR-015,FR-017} [COMPLETES FR-015] [COMPLETES FR-017] Rewire `flight::ship_motion_system` in `crates/sim/src/flight.rs` to read per-entity `ShipStats` instead of `Res<Tuning>`; formulae unchanged (HINT-002) after:T015 ← T013:ShipStats
- [X] T017 [US2] {FR-014,FR-016} [COMPLETES FR-016] Rewire `weapon::weapon_fire_system` in `crates/sim/src/weapon.rs` to gate `Weapon` on `ShipStats.can_fire`/`WeaponProfile` (no weapon = no fire) after:T016 ← T013:WeaponProfile
- [X] T018 [P] [US2] Unit tests in `crates/sim/tests/fitting.rs`: derive_ship_stats mass→agility, thrust→top_speed, no-weapon→can_fire=false, crippled-fit floors (no NaN/inf) after:T014 ← T014:derive_ship_stats
- [X] T019 [US2] Integration test in `crates/sim/tests/fitting.rs`: baseline seed fit's `ShipStats` reproduces the E002 `Tuning` defaults (flight-feel-preservation guard, HINT-002) + fit→running-ship stats applied to a `sim` world (SC-003) after:T017 ← T014:derive_ship_stats ← T015:recompute_ship_stats_system

---

## Phase 5: US3 — Where you put a module is a survivability choice (Priority: P1) 🎯 MVP

**Goal**: the fit layout IS the hit/armor map — `module_at`/`cell_map` expose occupancy+health, `resolve_hit` returns the first module struck outer-before-inner, and `hardpoint_arc` derives a position/facing firing arc. The E007 dependency surface.

**Independent test**: a traced hit resolves to the correct module outer-first; two fits differing only in reactor placement expose it at different depths; each weapon hardpoint reports a position-derived arc.

- [X] T020 [US3] {FR-019} Define `FitLayout` (Map<cell,`CellOccupant{slot,module,health,depth}`>) + `CellMap` + build-from-`Fit` (depth, every authored cell INV-F11) in `crates/sim/src/fitting/layout.rs` ← T006:Fit ← T004:GridCell → exports: FitLayout, build_layout()
- [X] T021 [P] [US3] {FR-019} Implement `module_at(&Fit,Cell)->Option<ModuleRef>` + `cell_map(&Fit)->CellMap` (per-cell occupant + live health for E007) in `crates/sim/src/fitting/layout.rs` after:T020 ← T020:FitLayout → exports: module_at(), cell_map()
- [X] T022 [US3] {FR-018,FR-021} [COMPLETES FR-018] [COMPLETES FR-021] Implement `resolve_hit(&Fit,p0,p1)->Option<HitResolution>` (segment vs grid, outer-before-inner by depth INV-F10, toi∈[0,1]) in `crates/sim/src/fitting/layout.rs` after:T021 ← physics::SweptHit → exports: resolve_hit(), HitResolution
- [X] T023 [US3] {FR-020} [COMPLETES FR-020] Implement `hardpoint_arc(&Hull,SlotId)->Option<Arc>` (center=heading+facing, half_angle from position ∈ (0,π] INV-F12; None for non-weapon) in `crates/sim/src/fitting/layout.rs` after:T020 ← T005:FiringArc → exports: hardpoint_arc()
- [X] T024 [US3] {FR-019} [COMPLETES FR-019] Wire `FitLayout` recompute into the fit-change system (rebuild on `Fit` mutation, INV-F08) and add the `FitLayout` component to the fitted ship in `crates/sim/src/fitting/mod.rs` after:T022,T015 ← T020:build_layout
- [X] T025 [P] [US3] Unit tests in `crates/sim/tests/fitting.rs`: resolve_hit outer-before-inner ordering, reactor central-vs-edge reached at different depths, cell_map completeness+health, hardpoint_arc bounded (0,π] (SC-004) after:T023 ← T022:resolve_hit ← T023:hardpoint_arc ← T021:cell_map

---

## Phase 6: US4 — Real tradeoffs and a hull ladder (Priority: P2)

**Goal**: the seed ladder scales (larger hull = more slots/power, more base mass → lower agility) and the budgets bind on different axes so no single fit maxes tank+damage+speed. Enforced by an automated guard test over the seed catalog.

**Independent test**: build a tank fit and a damage fit on one hull, confirm each binds a different budget ceiling and neither dominates; confirm corvette offers more slots/power at the cost of agility.

- [X] T026 [US4] {FR-022,FR-023} [COMPLETES FR-022] Tune the seed catalog so modules bind different axes (tank=mass/power, damage=cpu/power); corvette scales slots/power/mass over fighter in `crates/sim/src/fitting/content.rs` after:T007,T014
- [X] T027 [US4] {FR-023} [COMPLETES FR-023] Integration tests in `crates/sim/tests/fitting.rs`: no-fit-maxes-all guard over the seed catalog (tank/damage bind different axes), larger-hull-more-slots-less-agility (SC-005) after:T026 ← T014:derive_ship_stats

---

## Phase 7: US5 — Save, preview, and reuse fits (Priority: P3)

**Goal**: the interactive Bevy fitting screen (place/remove into positional slots, live power/CPU/mass budget bars, green/red before-commit preview) + save/name/reload/preview presets; spawn the running ship with a `Fit` + derived `ShipStats`. UI is structure/compile-verified + manual playtest (cannot be headless-tested).

**Independent test**: save a named fit, reload onto a compatible hull, preview its derived flight/weapon/budget stats before applying.

- [X] T028 [US5] {FR-024} Define `FitPreset{name,fit}`/`PresetId` + `save_preset`/`load_preset` (reload on compatible hull via `validate_fit`) + `preview_stats` in `crates/sim/src/fitting/fit.rs` after:T010,T014 ← T010:validate_fit → exports: save_preset(), load_preset(), preview_stats()
- [X] T029 [P] [US5] {FR-024} Integration test in `crates/sim/tests/fitting.rs`: preset save→reload round-trip on compatible hull, incompatible-hull rejected, preview matches committed derive (SC-006) after:T028 ← T028:save_preset
- [X] T030 [US5] {FR-012} Create the Bevy `fitting_ui` screen `crates/client/src/fitting_ui/mod.rs`: app-state, slot widgets, interactive place/remove (compile-verified + manual playtest) after:T011 ← T011:FitRejection → exports: FittingUiPlugin, FittingScreenState
- [X] T031 [US5] {FR-009,FR-013} [COMPLETES FR-009] [COMPLETES FR-013] Live power/CPU/mass bars + green/red before-commit stat-delta preview via `FittingPreview` in `crates/client/src/fitting_ui/mod.rs` (manual playtest) after:T030 ← T028:preview_stats → exports: FittingPreview
- [X] T032 [US5] {FR-024} [COMPLETES FR-024] Add save/name/reload preset controls to the fitting screen `crates/client/src/fitting_ui/mod.rs` (structure/compile-verified + manual playtest) after:T031 ← T028:save_preset,load_preset
- [X] T033 [US5] {FR-014} [COMPLETES FR-014] Spawn the ship with a `Fit` + derived `ShipStats` + `FitLayout` (replace the `Tuning`-driven spawn) in `crates/client/src/scene.rs` after:T024 ← T014:derive_ship_stats ← T020:FitLayout
- [X] T034 [US5] {FR-012} [COMPLETES FR-012] Register the `FittingUiPlugin` / fitting-screen app state in `crates/client/src/main.rs` after:T032,T033 ← T030:FittingUiPlugin

---

## Phase 8: Polish & Cross-Cutting Concerns

- [X] T035 [P] E001/E002 regression: run existing `crates/sim` tests (motion-equivalence keystone, flight, weapon, combat) green across the `Tuning`→`ShipStats` rewire after:T019
- [X] T036 Full workspace gate: `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo audit` after:T034,T035

---

## Dependencies

### Phase order (strict)
1. **Setup** (T001) → 2. **Foundational** (T002–T007) → 3. **US1** (T008–T012) → 4. **US2** (T013–T019) → 5. **US3** (T020–T025) → 6. **US4** (T026–T027) → 7. **US5** (T028–T034) → 8. **Polish** (T035–T036).

### Loopback-of-this-epic ordering (HINT-001/002, AD-003)
- Pure `sim` fitting domain + tests (Phases 2–6) MUST complete before the Bevy fitting UI (Phase 7 T030–T034). UI cannot be headless-tested.
- The `Tuning`→`ShipStats` rewire (T015–T017) depends on `derive_ship_stats` (T014) existing first.
- The flight-feel-preservation guard (T019) gates the regression check (T035).

### Key edges
- T002 → T003 → T006; T002 → T004 → T005 → T006; T006 → T007 (foundational chain).
- T007 → T008 → T009 → T010 → T011 → T012 (US1 validation chain).
- T013 → T014 → T015 → T016 → T017; T014 → T018; {T014,T015,T017} → T019 (US2 derive + rewire).
- T020 → T021 → T022; T020 → T023; {T022,T015} → T024; {T021,T022,T023} → T025 (US3 layout).
- {T007,T014} → T026 → T027 (US4 tradeoff guard).
- {T010,T014} → T028 → T029; T011 → T030 → T031 → T032; T024 → T033; {T032,T033} → T034 (US5 presets + UI).
- T019 → T035; {T034,T035} → T036 (polish gate).

### Parallel-safety note
No `[P]` task shares a batch with a task it lists in `after:` or `←`. `[P]` tasks (T002, T009, T012, T018, T021, T025, T029, T035) touch distinct files/independent test cases or carry only satisfied prior-phase dependencies.

---

## Requirement Coverage

| Req | Tasks | Last (COMPLETES) |
|-----|-------|------------------|
| FR-001 | T002, T003 | T003 |
| FR-002 | T006 | — |
| FR-003 | T004 | — |
| FR-004 | T004, T005 | — |
| FR-005 | T006, T011 | — |
| FR-006 | T009, T010, T011, T012 | T012 |
| FR-007 | T009, T010, T011 | T011 |
| FR-008 | T010, T012 | T012 |
| FR-009 | T008, T031 | T031 |
| FR-010 | T010, T012 | T012 |
| FR-011 | T010, T012 | T012 |
| FR-012 | T030, T034 | T034 |
| FR-013 | T008, T031 | T031 |
| FR-014 | T013, T014, T015, T016, T017, T033 | T033 |
| FR-015 | T013, T014, T016 | T016 |
| FR-016 | T014, T017 | T017 |
| FR-017 | T013, T014, T016, T018 | T016 |
| FR-018 | T022 | T022 |
| FR-019 | T020, T021, T024, T025 | T024 |
| FR-020 | T005, T023 | T023 |
| FR-021 | T022 | T022 |
| FR-022 | T007, T026 | T026 |
| FR-023 | T026, T027 | T027 |
| FR-024 | T028, T029, T032 | T032 |
| FR-025 | T003, T007 | T007 |

> Each `[COMPLETES]` is placed on the last task carrying its FR in execution order: FR-009/FR-013 on T031, FR-014 on T033 (client applies fit-derived stats to the spawned ship), FR-024 on T032 (round-trip verified by the T029 test), FR-012 on T034 (the screen becomes reachable once the plugin/app-state is registered). Success-criteria traces: SC-001→T012, SC-002→T012, SC-003→T018/T019, SC-004→T025, SC-005→T027, SC-006→T029.
