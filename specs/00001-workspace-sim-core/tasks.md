# Tasks: Workspace & Sim Core

**Input**: Design documents from `specs/00001-workspace-sim-core/`
**Prerequisites**: `plan.md` (required), `spec.md` (required), `checklists/testing.md`

**Tests**: Included — the spec/plan explicitly require the equivalence, trait-swap, serde-round-trip, and zero-`dt` no-op tests (TR-004/TR-005/TR-006/TR-008, SC-002/SC-004). The equivalence + negative-control suite ALREADY exists in `crates/sim/src/motion.rs`; new test tasks add only what is missing.

**Organization**: Technical spec → grouped by objective (`[OBJ1]`/`[OBJ2]`/`[OBJ3]`). Requirements tagged `{TR-###}`. Mapping is driven by the plan's Requirement Coverage Map.

## Project Mode

`Brownfield`

The Cargo workspace (`Cargo.toml`) and the `sim` crate (`crates/sim/`) already exist. `crates/sim/src/motion.rs` already contains the velocity-Verlet `integrate`, the closed-form `analytic`, `simulate`, and the full `#[cfg(test)]` equivalence suite (same-`dt` 1e-4, across-tick-rate {10,20,30,60,144} Hz 2e-4, zero-accel coasting, forward-Euler negative control). Tasks show new (`+`) and modified (`~`) work only; no bootstrap/rewrite of what exists.

## Epic / Capability Map *(OPTIONAL)*

- `[OBJ1]` → Shared, dependency-clean `sim` crate & workspace (P1) — deps wiring, serde-derivable domain types, bevy_ecs components, CI build/lint/format + dependency-graph + security gates.
- `[OBJ2]` → Integrator ↔ analytic equivalence with runtime `dt` (P1) — keystone already present; add zero-`dt` no-op coverage and confirm the existing equivalence suite satisfies TR-003/TR-004/TR-005.
- `[OBJ3]` → Swappable `Physics` trait (P2) — engine-agnostic trait, Rapier2D-backed impl, stub impl, swap integration test.

## Brownfield Notes *(OPTIONAL)*

- Existing flows touched: `Cargo.toml` (`[workspace.dependencies]`), `crates/sim/Cargo.toml`, `crates/sim/src/lib.rs`, `crates/sim/src/motion.rs` (`BodyState`), `crates/sim/src/motion.rs` `#[cfg(test)]` module.
- Patterns to reuse (HINT-001): the `motion.rs` keystone (`integrate`/`analytic`/`simulate` + equivalence tests) is correct and tested — extend, do NOT rewrite.
- Compatibility/seam concerns: serde derives must keep `PartialEq` round-trip equality (TR-008/AD-002); `bevy_ecs` MUST be `default-features = false` (HINT-004); NO Rapier2D type may cross the `Physics` trait boundary (HINT-003/TR-006); keep `sim` free of render/window/input/audio/net deps (TR-002).
- Regression focus: the existing equivalence + negative-control tests in `motion.rs` MUST keep passing after the serde derive and dependency additions.

---

## Phase 1: Setup (Repository / Workspace Delta)

**Repo-root + crate-manifest dependency wiring shared by every objective. No work-item label.**

- [X] T001 Add `rapier2d`, `serde` (derive), `bevy_ecs` to `[workspace.dependencies]` (pinned, `bevy_ecs` with `default-features = false`) in Cargo.toml {TR-001}
- [X] T002 Add `serde` (derive), `bevy_ecs` (`default-features = false`), `rapier2d` via `dep.workspace = true` in crates/sim/Cargo.toml after:T001 {TR-002}

---

## Phase 2: Objective 1 - Shared, dependency-clean sim crate & workspace (Priority: P1) 🎯 MVP

**Goal**: One dependency-clean source of gameplay truth — serde-derivable domain types, bevy_ecs gameplay components, and CI gates (build/test/lint/format + dependency-graph inspection + security audit).

- [X] T003 [OBJ1] {TR-008} Derive `Serialize, Deserialize` on `BodyState` (keep `PartialEq`/`Copy`; keystone unchanged) in crates/sim/src/motion.rs after:T002 → exports: BodyState(pos,vel)
- [X] T004 [P] [OBJ1] {TR-008} Create bevy_ecs components deriving Component + serde + PartialEq + Copy in crates/sim/src/components.rs after:T002 → exports: Position(Vec2), Velocity(Vec2)
- [X] T005 [OBJ1] Export `components` module (and re-export its types) from crates/sim/src/lib.rs after:T004 ← T004:Position,Velocity
- [X] T006 [OBJ1] {TR-008} [COMPLETES TR-008] Add serde round-trip test: deserialized == original under `PartialEq` for `BodyState` + components in crates/sim/src/motion.rs ← T003:BodyState ← T004:Position
- [X] T007 [OBJ1] {TR-007} Create CI workflow running `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` across the workspace in .github/workflows/ci.yml after:T002
- [X] T008 [OBJ1] {TR-009} Add CI step using `cargo tree`/`cargo metadata` to assert resolved `sim` deps exclude render/window/input/audio/net libs in .github/workflows/ci.yml ← T007:ci.yml
- [X] T009 [P] [OBJ1] {TR-010} Create cargo-audit triage config (ignore-list for justified low/medium advisories) in .cargo/audit.toml
- [X] T010 [OBJ1] {TR-010} [COMPLETES TR-010] Add `cargo-audit` CI step gating on high/critical, non-gating low/medium, reading triage config in .github/workflows/ci.yml ← T007:ci.yml ← T009:audit.toml
- [X] T011 [OBJ1] {TR-001,TR-002} [COMPLETES TR-002] Verify `cargo build` + dependency-graph step pass and `sim` stays render/window/net-free (run gates per HINT-005 env) after:T008

---

## Phase 3: Objective 2 - Integrator ↔ analytic equivalence with runtime dt (Priority: P1) 🎯 MVP

**Goal**: Confirm the keystone satisfies the runtime-`dt` integrator + analytic equivalence requirements and close the one degenerate-input gap (zero-`dt` no-op). Existing equivalence + negative-control tests are reused, NOT rewritten (HINT-001).

- [X] T012 [OBJ2] {TR-003} Confirm `integrate`/`simulate` accept `dt` as a runtime parameter (no constant `dt`); document the runtime-`dt` contract in crates/sim/src/motion.rs ← T003:BodyState
- [X] T013 [P] [OBJ2] {TR-004,TR-005} [COMPLETES TR-005] Verify equivalence suite passes (same-`dt` 1e-4, {10,20,30,60,144} Hz 2e-4, pos+vel, Euler control >0.1) in crates/sim/src/motion.rs after:T003
- [X] T014 [OBJ2] {TR-003} [COMPLETES TR-003] Add a zero-`dt` no-op unit test asserting a step with `dt = 0` returns the input `BodyState` unchanged in crates/sim/src/motion.rs after:T013

---

## Phase 4: Objective 3 - Swappable Physics trait (Priority: P2)

**Goal**: Engine-agnostic `Physics` trait with a Rapier2D-backed impl and a drop-in stub, proven swappable by an integration test — no Rapier types in public signatures (HINT-003).

- [X] T015 [OBJ3] {TR-006} Define minimal `Physics` trait, glam/sim types only in public signatures (AD-001, HINT-003) in crates/sim/src/physics.rs after:T002 ← T003:BodyState → exports: trait Physics
- [X] T016 [OBJ3] {TR-006} Implement Rapier2D-backed `Physics` impl (Rapier types confined to impl body, not trait surface) in crates/sim/src/physics.rs ← T015:Physics → exports: RapierPhysics
- [X] T017 [OBJ3] Export `physics` module (and `Physics` trait) from crates/sim/src/lib.rs after:T015 ← T015:Physics
- [X] T018 [P] [OBJ3] {TR-006} Implement drop-in stub `Physics` impl (full trait surface, glam/sim types only) in crates/sim/tests/physics_swap.rs after:T017 ← T015:Physics → exports: StubPhysics
- [X] T019 [OBJ3] {TR-006} [COMPLETES TR-006] Swap test: same outputs for same inputs, Rapier vs. stub, no consumer change in crates/sim/tests/physics_swap.rs ← T016:RapierPhysics ← T018:StubPhysics

---

## Phase 5: Polish & Cross-Cutting Concerns *(OPTIONAL)*

**Cross-objective release-gate verification after all delivery work is in place.**

- [X] T020 [P] {TR-007} [COMPLETES TR-007] Run full gate suite — `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` — green across the workspace (HINT-005 env) after:T019

---

## Dependencies

Setup → Objective 1 (P1) → Objective 2 (P1) → Objective 3 (P2) → Polish.

- **Phase 1 (Setup)** has no dependencies; T002 depends on T001 (crate manifest references workspace deps).
- **Phase 2 (OBJ1)** depends on Setup (deps must resolve before serde derives, components, and CI run). T005 after T004; T006 after T003+T004; T008 after T007; T010 after T007+T009; T011 after T008.
- **Phase 3 (OBJ2)** depends on T003 (serde'd `BodyState`) for the round-trip-adjacent tests; T013 after T003; T014 after T013.
- **Phase 4 (OBJ3)** depends on Setup (T002) and the domain types (T003/T004). T016/T017 after T015; T018 after T017; T019 after T016+T018.
- **Phase 5 (Polish)** depends on all delivery work (T019).
- Tasks marked `[P]` can run in parallel within their phase (distinct files / no intra-batch dependency).
- A task with `after:T###` or `← T###:Symbol` is never `[P]`-batched with the referenced task.

### Requirement Coverage

| Req | Tasks |
|-----|-------|
| TR-001 | T001, T011 |
| TR-002 | T002, T011 (COMPLETES) |
| TR-003 | T012, T014 (COMPLETES) |
| TR-004 | T013 |
| TR-005 | T013 (COMPLETES) |
| TR-006 | T015, T016, T018, T019 (COMPLETES) |
| TR-007 | T007, T020 (COMPLETES) |
| TR-008 | T003, T004, T006 |
| TR-009 | T008 |
| TR-010 | T009, T010 (COMPLETES) |
