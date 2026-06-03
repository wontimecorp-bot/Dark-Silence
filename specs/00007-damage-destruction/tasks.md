# Tasks: Damage & Destruction

**Feature**: `00007-damage-destruction` | **Epic**: E007 | **Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md)
**Inputs**: [data-model.md](data-model.md), [contracts/damage-api.md](contracts/damage-api.md)

## Project Mode

**Brownfield** — extends `crates/sim` (the shared, server-authoritative domain). ADDS the `crates/sim/src/damage/` module set + `crates/sim/tests/damage.rs`; MODIFIES the E006 fitting stats path, the E002 weapon/combat hit path, `sim/lib.rs`, the E006 fitting tests, and a minimal client HUD cue. No new crate, no new dependency. Reuses E006 `FitLayout`/`resolve_hit`/`cell_map`/`ShipStats`, E001 `Physics`/`swept_cast`/`SweptHit`, E002 `Weapon`/`Projectile`/`combat`.

## Epic / Capability Map

| Work item | Priority | Spec FRs | Success criterion |
|-----------|----------|----------|-------------------|
| US1 — Damage pipeline (hit-location, layers, penetration) | P1 | FR-002, FR-003, FR-004, FR-005, FR-006, FR-007, FR-008, FR-009, FR-010, FR-011, FR-022 | SC-001 |
| US2 — Emergent damage (health scales stats) | P1 | FR-012, FR-013 | SC-002 |
| US3 — Destruction + connectivity severing | P1 | FR-014, FR-015, FR-016, FR-017 | SC-003 |
| US4 — Salvage (intact vs scrap, lootable wrecks) | P2 | FR-018, FR-019, FR-020 | SC-004 |
| US5 — Non-degenerate matrix + legible feedback | P2 | FR-023, FR-024 | SC-005 |
| Combat integration (live wire-up) | P1 (rewire) | FR-001, FR-021 | SC-001..SC-004 (e2e) |

## Brownfield Notes

- **BREAKING-CHANGE (load-bearing)**: `derive_ship_stats` gains a `&FitLayout` param (AD-004, HINT-002). The ripple (`recompute_ship_stats_system` at `fitting/mod.rs:90`, `preview_stats` at `fitting/fit.rs:289`, and ~10 call sites in `crates/sim/tests/fitting.rs`) is gated **before** any consumer in T021/T022 so E006 fitting tests stay green (the baseline-reproduces-`Tuning` guard must hold at FULL module health).
- **Fitted vs unfitted dispatch (HINT-004, AD-007, INV-D17)**: only ships with a `FitLayout` use the full pipeline; unfitted practice targets (dummies/asteroids) keep the simplified whole-ship `Health` path so E002/E003 targets + tests stay green.
- **Connectivity only on destruction (HINT-003, AD-005, FR-017, INV-D08)**: the flood-fill runs solely at a destruction event; chunks reuse the `sim::Physics` trait + inherit COM linear+angular velocity; never rebuild per-cell colliders.
- **Pure-logic before integration (HINT-001)**: build the substrate (event/channel/resist/penetration/layers/shields) + tests, then the emergent ripple, then destruction/severing, then salvage, then the live combat rewire, then the client cue.

---

## Phase 1: Setup

- [X] T001 Create the `damage` module scaffold `crates/sim/src/damage/mod.rs` with empty `event`/`resist`/`penetration`/`layers`/`shields`/`destruction`/`sever`/`salvage`/`content` submodule declarations → exports: damage (module root)
- [X] T002 Register `pub mod damage;` in `crates/sim/src/lib.rs` (alphabetical with the existing `pub mod` block) after:T001
- [X] T003 Create the empty integration test file `crates/sim/tests/damage.rs` with the `sim` world test harness imports (penetration/layers/emergent/sever/salvage/non-degenerate suites land here)

## Phase 2: Foundational (Pure-Logic Substrate)

*Cross-work-item blockers: the typed packet, the data-driven matrix, the penetration math, and the defense-layer data shapes — consumed by US1 and every downstream phase.*

- [X] T004 [P] {FR-001} Define `DamageEvent` + `Channel` value types (channel, magnitude, penetration, pen_size, impact point/dir, source) with serde derives in `crates/sim/src/damage/event.rs` → exports: DamageEvent(channel,magnitude,penetration,pen_size,point,dir), Channel (5 variants)
- [X] T005 {FR-004,FR-022} Define `DefenseLayer` + `ResistanceMatrix` (`table:[[f32;5];4]`) Resource with `layer_resist(&ResistanceMatrix, DefenseLayer, Channel) -> f32 ∈ [0,1)` in `crates/sim/src/damage/resist.rs` after:T004 ← T004:Channel → exports: ResistanceMatrix(table), DefenseLayer (4 variants), layer_resist()
- [X] T006 [P] {FR-004,FR-022} Author the data-driven content tables (matrix const-seed + penetration/armor/shield/stat-scaling tuning Resources + `ArmorMaterial`) in `crates/sim/src/damage/content.rs` → exports: default_resistance_matrix(), PenetrationConfig, ShieldConfig, StatScalingConfig (see data-model.md#Content-Tables)
- [X] T007 {FR-022} [COMPLETES FR-022] Unit test: every matrix/penetration/shield value loads from content (no hardcoded balance in code paths) and each cell respects bounds `∈ [0,1)` (INV-D02) in `crates/sim/tests/damage.rs` after:T006 ← T005:layer_resist ← T006:default_resistance_matrix
- [X] T008 {FR-005,FR-006,FR-007,FR-008} Implement `resolve_penetration(thickness,angle,pen,size) -> PenetrationResult` in `crates/sim/src/damage/penetration.rs` — finite eff-armor `clamp(thickness·material/cosθ,0,cap)` (INV-D03), ricochet, overmatch bypass (INV-D04), pen/overpen/non tiers (INV-D05) ← T006:PenetrationConfig → exports: resolve_penetration(), PenetrationResult (see data-model.md#PenetrationResult)
- [X] T009 [P] {FR-005} Unit test: effective armor = `thickness/cos(angle)` increases with angle and stays finite as `cos→0` (clamped to `effective_armor_cap`, INV-D03) in `crates/sim/tests/damage.rs` after:T008 ← T008:resolve_penetration
- [X] T010 [P] {FR-006} Unit test: a steep glancing hit past `ricochet_angle` returns `Ricochet` with little/no damage; below the threshold it does not in `crates/sim/tests/damage.rs` after:T008 ← T008:PenetrationResult
- [X] T011 [P] {FR-007} Unit test: a hit with `pen_size >= overmatch_ratio·thickness` bypasses the angle/ricochet test and forces at least `Penetration` (INV-D04) in `crates/sim/tests/damage.rs` after:T008 ← T008:resolve_penetration
- [X] T012 {FR-008} [COMPLETES FR-008] Unit test: tier ordering `pen_tier_non < pen_tier_over < pen_tier_full <= 1.0` — clean Penetration applies the full tier, OverPenetration a strictly lower tier, NonPenetration little/none (INV-D05) in `crates/sim/tests/damage.rs` after:T008 ← T008:PenetrationResult
- [X] T013 {FR-003} Define the per-ship defense-layer state components `Shields`, `SectionArmor`, `HullStructure`, `SectionHealth`, `DamageContext` in `crates/sim/src/damage/layers.rs` (shapes only; traversal in US1) ← T005:DefenseLayer → exports: Shields, SectionArmor, HullStructure, SectionHealth, ArmorFacet (see data-model.md#Runtime-Components)

## Phase 3: US1 — Hits land where you aim and armor matters (Priority: P1) 🎯 MVP

**Goal**: a typed hit resolves to the entry-point module, is mitigated through Shields→Armor→Hull→Systems per the matrix, ricochets/overmatches at the armor gate, and routes surviving post-penetration damage to the module behind. **Independent test**: SC-001.

- [X] T014 [US1] {FR-010} Implement `shield_absorb(&mut Shields, DamageEvent, &ResistanceMatrix) -> (surviving, depleted)` in `crates/sim/src/damage/shields.rs` — absorbs first, mitigated by `layer_resist(Shields, channel)`; a depleted/`!powered` shield passes through untouched (armor exposed) ← T005:layer_resist ← T013:Shields → exports: shield_absorb()
- [X] T015 [US1] {FR-010} Implement `shield_regen_system(dt, Query<(&mut Shields,&ShipStats)>)` in `crates/sim/src/damage/shields.rs` — regen toward `max` only while powered; set `powered = power_supply >= power_draw`; decay at `unpowered_decay` while `power_linked && !powered` (INV-D14) ← T006:ShieldConfig → exports: shield_regen_system()
- [X] T016 [US1] {FR-002,FR-009} Implement entry-point resolution + post-pen routing helper in `crates/sim/src/damage/layers.rs` — reuse E006 `resolve_hit(&Fit,p0,p1,&Hull,&ModuleCatalog)` to find the entry-point `HitResolution.module` and route surviving damage to the cell behind (outer-before-inner, INV-D06); empty/structural cell → `NoModule` (no panic) ← T004:DamageEvent → exports: resolve_entry_point(), route_behind()
- [X] T017 [US1] {FR-003,FR-004,FR-011} Implement `apply_damage(&mut World, target: Entity, DamageEvent) -> DamageOutcome` in `crates/sim/src/damage/layers.rs` — traverse Shields→Armor→Hull→Systems (`shield_absorb` → `resolve_penetration` → matrix at each layer) and reduce the struck `CellOccupant.health` clamped `>=0`, flag `destroyed` at 0 (INV-D01) after:T008,T014,T016 ← T008:resolve_penetration ← T014:shield_absorb ← T016:resolve_entry_point → exports: apply_damage(), DamageOutcome(struck,applied,layer_reached,result,destroyed), HitKind
- [X] T018 [US1] [P] {FR-004} [COMPLETES FR-004] Unit test: matrix traversal mitigates per `(layer×channel)` — each channel loses its strong-vs layer's mitigation as it passes; surviving magnitude monotonically non-increasing across layers in `crates/sim/tests/damage.rs` after:T017 ← T017:apply_damage
- [X] T019 [US1] [P] {FR-010} [COMPLETES FR-010] Unit test: shields absorb first (strong vs ThermalEnergy), regenerate while powered, decay/expose-armor while `power_linked && !powered` (INV-D14) in `crates/sim/tests/damage.rs` after:T015 ← T014:shield_absorb ← T015:shield_regen_system
- [X] T020 [US1] {FR-002,FR-009,FR-011} [COMPLETES FR-002] [COMPLETES FR-009] [COMPLETES FR-011] Integration test: a `DamageEvent` resolves to the entry point, passes the layers, and a clean penetration drives the cell **behind** it to `health=0`=destroyed; a buried module is reached only after its cover (SC-001) in `crates/sim/tests/damage.rs` after:T017 ← T017:apply_damage

## Phase 4: US2 — The ship gets worse as it's hit (Priority: P1) 🎯 MVP

**Goal**: the load-bearing BREAKING-CHANGE — `derive_ship_stats` gains `&FitLayout` so per-module health scales each module's `ShipStats` contribution (destroyed = off); the E006 fitting tests stay green at full health. **Independent test**: SC-002. **This block is gated before all consumers (combat, weapon-fire).**

- [X] T021 [US2] {FR-012,FR-013} **[BREAKING]** Extend `derive_ship_stats(&Hull,&Fit,&ModuleCatalog,&FitLayout)` in `crates/sim/src/fitting/stats.rs` — `health_factor` scales each module's contribution; destroyed=`0`; floored at `stat_health_floor`; INV-F07 floors preserved (INV-D13) ← T006:StatScalingConfig → exports: derive_ship_stats(+&FitLayout), health_factor()
- [X] T022 [US2] {FR-012,FR-013} **[BREAKING]** Thread the new `&FitLayout` arg through `recompute_ship_stats_system` (`crates/sim/src/fitting/mod.rs:90`, trigger now `Changed<Fit>` OR `Changed<FitLayout>`) and `preview_stats` (`crates/sim/src/fitting/fit.rs:289`) after:T021 ← T021:derive_ship_stats
- [X] T023 [US2] {FR-012,FR-013} **[BREAKING]** Update the ~10 `derive_ship_stats` call sites + the baseline-reproduces-`Tuning` guard in `crates/sim/tests/fitting.rs` to pass a full-health `FitLayout`; confirm the E006 fitting suite stays green (baseline holds at FULL module health) after:T022 ← T021:derive_ship_stats
- [X] T024 [US2] [P] {FR-012} Unit test: a damaged thruster's `health_frac` lowers `ShipStats` top speed + acceleration proportionally (floored at `stat_health_floor`, never NaN); a healthy vs battered same-fit ship derive measurably different stats (SC-002) in `crates/sim/tests/damage.rs` after:T021 ← T021:derive_ship_stats
- [X] T025 [US2] {FR-013} [COMPLETES FR-012] [COMPLETES FR-013] Unit test: a destroyed weapon → `can_fire=false`/profile dropped; a destroyed reactor → `power_gen=0` collapsing `power_supply` and dropping `power_linked` shields (FR-013) in `crates/sim/tests/damage.rs` after:T021 ← T021:health_factor

## Phase 5: US3 — Blow it apart: sections destroyed and severed (Priority: P1) 🎯 MVP

**Goal**: a destroyed section is removed from the layout and a connectivity flood-fill (only on destruction) splits disconnected regions into drifting chunks that inherit COM momentum. **Independent test**: SC-003.

- [X] T026 [US3] {FR-015} Implement `connected_region(&FitLayout, core: Cell) -> HashSet<Cell>` flood-fill over the **remaining** hull grid in `crates/sim/src/damage/sever.rs` — cells outside the set are disconnected regions; core gone → whole-ship destroyed (INV-D15) ← T013:SectionHealth → exports: connected_region()
- [X] T027 [US3] {FR-016} Implement `sever_chunk(&mut World, ship, &HashSet<Cell>) -> WreckChunk` in `crates/sim/src/damage/sever.rs` — split into a `Physics` body inheriting COM linear+angular velocity (INV-D07); orphan cell severs/absorbs (INV-D09) ← T026:connected_region → exports: sever_chunk(), WreckChunk(body,cells,salvage), Wreck
- [X] T028 [US3] {FR-014,FR-017} Implement `on_section_destroyed(&mut World, ship, section)` in `crates/sim/src/damage/destruction.rs` — remove the section's cells (coarse, cell-ready), then `connected_region` + `sever_chunk` per region; ONLY at destruction events (INV-D08) after:T017,T027 ← T027:sever_chunk → exports: on_section_destroyed()
- [X] T029 [US3] [P] {FR-015,FR-017} Unit test: destroying a connecting section splits the hull (flood-fill finds the disconnected region); a hull that stays connected produces no split; connectivity runs only on a destruction event, not per frame (INV-D08) in `crates/sim/tests/damage.rs` after:T028 ← T026:connected_region ← T028:on_section_destroyed
- [X] T030 [US3] {FR-016} [COMPLETES FR-014] [COMPLETES FR-015] [COMPLETES FR-016] [COMPLETES FR-017] Unit test: a severed chunk inherits parent linear+angular velocity at its COM (drifts, momentum conserved, INV-D07); core-sever destroys the ship (SC-003) in `crates/sim/tests/damage.rs` after:T028 ← T027:sever_chunk

## Phase 6: US4 — Salvage: cut it clean for an intact part (Priority: P2)

**Goal**: a clean sever (module health intact, structure gone) yields an intact module; a through-killed module yields scrap; destroyed ships + chunks persist as lootable wrecks with over-kill ≥ scrap. **Independent test**: SC-004.

- [X] T031 [US4] {FR-018,FR-019} Implement `salvage(&Wreck) -> Vec<SalvageOutcome>` + `intact_threshold(occupant,module) -> bool` in `crates/sim/src/damage/salvage.rs` — `health >= INTACT_FRACTION·health_max` → `IntactModule`, else `Scrap` (INV-D12) ← T027:Wreck → exports: salvage(), SalvageOutcome{IntactModule,Scrap}, intact_threshold()
- [X] T032 [US4] {FR-020} Spawn a persistent lootable `Wreck` (physical body + residual cell-grid + `claimed:bool` single-resolution, INV-D10) on ship-destroy/sever in `crates/sim/src/damage/sever.rs`; over-kill clamps to ≥ a `Scrap` floor (never zero loot, INV-D09) after:T027,T031 ← T031:salvage,SalvageOutcome → exports: Wreck(origin,contents,claimed)
- [X] T033 [US4] [P] {FR-018,FR-019} Unit test: a clean-severed module (`health >= INTACT_FRACTION·health_max`) yields `IntactModule`; a destroyed/penetrated-through module (`health` below threshold) yields `Scrap` and never an intact module — through-kill does not beat clean-sever (INV-D12) in `crates/sim/tests/damage.rs` after:T031 ← T031:salvage,intact_threshold
- [X] T034 [US4] {FR-020} [COMPLETES FR-018] [COMPLETES FR-019] [COMPLETES FR-020] Unit test: a destroyed ship leaves a persistent lootable `Wreck`; an over-killed ship still yields ≥ a `Scrap` floor (never empty); a wreck is claimed exactly once (INV-D09/D10) (SC-004) in `crates/sim/tests/damage.rs` after:T032 ← T032:Wreck

## Phase 7: US5 — The defense matrix is a real, readable choice (Priority: P2)

**Goal**: the non-degenerate-matrix guard (every channel beats a layer, every layer resists a channel) and a minimal legible client cue (ricochet/penetrate/shield-absorb). **Independent test**: SC-005.

- [ ] T035 [US5] {FR-023} [COMPLETES FR-023] Non-degenerate matrix guard test in `crates/sim/tests/damage.rs` — over `(Channel × DefenseLayer)` every channel beats a layer and every layer resists a channel; no dominant channel, no bypassed layer (INV-D11) (SC-005) after:T006 ← T005:layer_resist ← T006:default_resistance_matrix
- [ ] T036 [US5] {FR-024} [COMPLETES FR-024] Surface the `HitKind`/`DamageOutcome` legibility tag (ricochet vs penetration vs shield-absorb + affected module/layer) as a minimal diegetic cue in `crates/client/src/hud.rs` — presentation-only, no authority, no numeric spam (SC-005) after:T017 ← T017:DamageOutcome,HitKind

## Phase 8: Combat Integration (live wire-up)

**Goal**: build a `DamageEvent` from a weapon hit and route a fitted-ship hit through `apply_damage`, replacing the E002 whole-ship `Health` path for fitted ships while keeping unfitted targets on simplified `Health`; register the pipeline + destruction worker in the `sim` fixed step.

- [ ] T037 {FR-001} Implement `damage_event_from_hit(projectile, &SweptHit, &WeaponSource) -> DamageEvent` in `crates/sim/src/weapon.rs` — channel/pen/size from weapon data + geometry from the reused `SweptHit` after:T004,T021 ← T004:DamageEvent → exports: damage_event_from_hit(), WeaponSource
- [ ] T038 {FR-001,FR-021} [COMPLETES FR-001] Rewire the combat hit path in `crates/sim/src/combat.rs` — **fitted** target → `apply_damage` (replacing the E002 whole-ship `Health` path); **unfitted** target → legacy `Health` clamp verbatim (degenerate, INV-D17); reuse the swept CCD (no tunnel) after:T017,T021,T037 ← T017:apply_damage ← T037:damage_event_from_hit
- [ ] T039 {FR-021} [COMPLETES FR-021] Register the damage systems in the `sim` fixed step in `crates/sim/src/lib.rs` — `shield_regen_system` + the destruction worker (`on_section_destroyed`, only on destruction events) + the `Changed<FitLayout>` re-derive ordering; all resolution server-authoritative (INV-D16) after:T015,T028,T038 ← T015:shield_regen_system ← T028:on_section_destroyed
- [ ] T040 Integration test (end-to-end, fitted ship): a fired E002 projectile → swept hit → `damage_event_from_hit` → `apply_damage` → module damage → emergent `ShipStats` drop → section destroyed → sever → wreck → salvage, in a `sim` world (SC-001..SC-004 e2e) after:T020,T028,T032,T038,T039
- [ ] T041 [P] Integration test (unfitted degenerate path): a projectile hit on a `FitLayout`-less dummy/asteroid resolves via the flat `Health` clamp and despawns at `<=0` — E002/E003 targets + the simplified path stay green (INV-D17) in `crates/sim/tests/damage.rs` after:T038

## Phase 9: Polish & Cross-Cutting

- [ ] T042 [P] Run the E001/E002/E006 regression suites — `cargo test -p sim` across `gameplay.rs`, `physics_swap.rs`, and especially `fitting.rs` (the `derive_ship_stats` signature ripple) + the unfitted-target path — and confirm all green after:T040,T041
- [ ] T043 [P] Full workspace gate: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo audit` after:T040,T041

---

## Dependencies

### Phase order
`Setup (P1) → Foundational (P2) → US1 (P3) → US2 (P4) → US3 (P5) → US4 (P6) → US5 (P7) → Combat Integration (P8) → Polish (P9)`

Pure-logic substrate (P2–P7 logic + tests) precedes the live combat rewire (P8). The `derive_ship_stats(+&FitLayout)` BREAKING-CHANGE (T021–T023) is gated **before** its consumers (T037 weapon-event, T038 combat rewire, T039 system registration).

### Cross-phase edges
- **Setup**: T002 after T001; T003 standalone.
- **Foundational**: T007 after T006 (and ← T005); T008 ← T006; penetration tests T009–T012 after T008; T013 ← T005.
- **US1**: T014 ← T005,T013; T015 ← T006; T016 ← T004; T017 after T008,T014,T016; tests T018–T020 after T017 (T019 also after T015).
- **US2 (gated ripple)**: T021 ← T006; T022 after T021; T023 after T022; T024 after T021; T025 after T021.
- **US3**: T026 ← T013; T027 after T026; T028 after T017,T027; tests T029–T030 after T028.
- **US4**: T031 ← T027; T032 after T027,T031; tests T033–T034 (T033 after T031, T034 after T032).
- **US5**: T035 after T006 (← T005); T036 after T017.
- **Combat integration**: T037 after T004,T021; T038 after T017,T021,T037; T039 after T015,T028,T038; T040 after T020,T028,T032,T038,T039; T041 after T038.
- **Polish**: T042, T043 after T040,T041.

### Parallel-safe batches (distinct files / independent tests, no intra-batch dependency)
- Foundational kickoff: **T004, T006** (distinct files, no inter-dependency). T005 follows T004 (`← T004:Channel`), then runs alongside T006.
- Penetration tests: **T009, T010, T011** (T012 closes FR-008 sequentially after).
- US1 tests: **T018, T019** (after their producers land).
- US5: **T035, T036** are independent (matrix-guard test vs client cue).
- Polish: **T042, T043**.

### MVP scope
P1 work items **US1 + US2 + US3** plus the Combat Integration rewire constitute the viable MVP (typed hit-location damage, emergent degradation, and ships coming apart — each independently testable). US4 (salvage) and US5 (matrix guard + cue) are P2.

---

## Requirement Coverage

| Req | Tasks | Completion task |
|-----|-------|-----------------|
| FR-001 | T004, T037, T038 | T038 |
| FR-002 | T016, T017, T020 | T020 |
| FR-003 | T013, T017 | T017 |
| FR-004 | T005, T006, T017, T018 | T018 |
| FR-005 | T008, T009 | T008 |
| FR-006 | T008, T010 | T008 |
| FR-007 | T008, T011 | T008 |
| FR-008 | T008, T012 | T012 |
| FR-009 | T016, T017, T020 | T020 |
| FR-010 | T014, T015, T019 | T019 |
| FR-011 | T017, T020 | T020 |
| FR-012 | T021, T022, T023, T024, T025 | T025 |
| FR-013 | T021, T022, T023, T025 | T025 |
| FR-014 | T028, T030 | T030 |
| FR-015 | T026, T028, T029, T030 | T030 |
| FR-016 | T027, T028, T030 | T030 |
| FR-017 | T028, T029, T030 | T030 |
| FR-018 | T031, T033, T034 | T034 |
| FR-019 | T031, T033, T034 | T034 |
| FR-020 | T032, T034 | T034 |
| FR-021 | T038, T039, T040 | T039 |
| FR-022 | T005, T006, T007 | T007 |
| FR-023 | T035 | T035 |
| FR-024 | T036 | T036 |

All FR-001…FR-024 covered (no gaps). Success criteria trace: SC-001 → T009–T012/T020; SC-002 → T024/T025; SC-003 → T029/T030; SC-004 → T033/T034; SC-005 → T035/T036; e2e → T040 (fitted) + T041 (unfitted degenerate path).
