# Implementation Plan: Workspace & Sim Core

**Branch**: `00001-workspace-sim-core` | **Date**: 2026-06-01 | **Spec**: [spec.md](spec.md)

## Summary

**Goal**: Establish the shared, dependency-clean `sim` crate (the single source of gameplay truth) plus a swappable `Physics` trait, on top of the existing Cargo workspace and motion keystone.
**Approach**: Extend the existing `crates/sim` (which already holds the velocity-Verlet integrator + closed-form analytic + equivalence tests) with an ECS component set, serde-derivable domain types, and a Rapier2D-backed `Physics` trait behind an engine-agnostic boundary; add CI quality gates.
**Key Constraint**: `sim` must remain free of rendering/windowing/networking dependencies and must not leak engine-specific physics types across the `Physics` trait.

## Technical Context

**Language/Version**: Rust (edition 2021; dev toolchain 1.92; MSRV TBD) — *baseline from `specs/sad.md`*
**Primary Dependencies**: `glam` (math, serde feature), `bevy_ecs` (no-default-features, pure ECS), `rapier2d` (physics), `serde` (derive) — heavier deps (bevy, lightyear, sqlx, redis, tokio, bitcode, tracing) intentionally deferred to later epics
**Storage**: N/A (no persistence in this epic)
**Testing**: `cargo test` (unit/property); equivalence test suite already present
**Target Platform**: Headless library crate (consumed by desktop client + Linux server later)
**Project Type**: single (library crate within a workspace)
**Project Mode**: brownfield (workspace + `sim` crate already exist)
**Performance Goals**: N/A as hard budgets this epic. The "allocation-free per step" expectation is non-gated design guidance, not a tested assertion in this epic — it is satisfied by construction (`integrate`/`analytic` operate on `Copy` `Vec2`/`BodyState` value types with no heap allocation) and there is no benchmark/allocation tier here. A benchmark gate on the integrator hot path is deferred to the epic that first stresses it (E003/E008).
**Constraints**: no render/window/net deps in `sim`; engine types must not cross the `Physics` trait; `dt` runtime-variable; consistent `f32` + fixed `dt` (no cross-platform bit-determinism required)
**Scale/Scope**: foundational crate consumed by all downstream epics

## Instructions Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **Principle II (Shared Deterministic Sim Core)**: PASS — this epic *is* the single shared `sim` crate consumed by client and server.
- **Principle III (Tiered Simulation)**: PASS — the integrator↔analytic equivalence is the promote/demote keystone.
- **Technology Stack**: PASS — Rust/Cargo workspace, `sim` crate, Rapier2D behind a `Physics` trait, glam — all per `specs/sad.md` / `project-instructions.md`.
- **Testing & Quality Policy**: PASS — the mandatory `sim` equations-of-motion invariant is covered by tests; CI enforces Clippy `-D warnings` + rustfmt.
- **Source Code Layout**: PASS — Cargo workspace, code under `crates/<name>/src`.
- **Principle VI (Bandwidth)**: N/A — replication/AOI out of scope (E003).

Gate status: **PASS** (re-checked post-design — still PASS).

## Architecture

```mermaid
C4Component
    title Workspace & sim core
    Container_Boundary(sim, "sim crate — shared gameplay truth") {
        Component(motion, "motion", "module", "Verlet + analytic")
        Component(physics, "physics", "module", "Physics trait")
        Component(components, "components", "module", "ECS gameplay data")
    }
    System_Ext(rapier, "Rapier2D", "physics engine")
    System_Ext(client, "client (E002)", "consumer")
    System_Ext(server, "server (E003)", "consumer")
    Rel(physics, rapier, "default impl wraps")
    Rel(motion, components, "operates on")
    Rel(physics, components, "operates on")
    Rel(client, sim, "predicts with")
    Rel(server, sim, "simulates with")
```

## Architecture Decisions

Feature-local tradeoffs only. Project-wide decisions live in standalone ADRs: **ADR-0003** (shared `sim` + velocity-Verlet), **ADR-0004** (Rapier2D behind a `Physics` trait), **ADR-0007** (tech stack & workspace) — referenced, not duplicated.

| ID | Decision | Options Considered | Chosen | Rationale |
|----|----------|--------------------|--------|-----------|
| AD-001 | `Physics` trait surface scope | Full Rapier wrapper / Minimal consumer-driven surface | Minimal, consumer-driven surface | Keep the engine swap cheap (ADR-0004); avoid leaking Rapier types so the boundary holds |
| AD-002 | Serialize `sim` domain types now? | Defer to E003/E004 / Derive serde now | Derive serde now | glam serde already enabled; satisfies Principle V seam + downstream replication/persistence; near-zero cost (resolves spec Compliance advisory) |
| AD-003 | ECS layer in `sim` | Plain structs now / `bevy_ecs` (no-default) now | `bevy_ecs` with `default-features = false` | Per ADR-0003; pure ECS data without pulling Bevy render/window into the headless crate |

## Data Model Summary

N/A — no persistent data (this epic introduces in-memory gameplay components only; persistence is E004).

## API Surface Summary

N/A — no API surface (no networking/endpoints in this epic; networking is E003).

## Testing Strategy

| Tier | Tool | Scope | Validates | Mock Boundary | Gated? | Install |
|------|------|-------|-----------|---------------|--------|---------|
| Unit | `cargo test` | `sim` equations-of-motion equivalence (integrator↔analytic, position AND velocity, `rel_tol` 1e-4 primary / 2e-4 across the tick-rate set {10,20,30,60,144} Hz), zero-accel coasting, zero-`dt` no-op, forward-Euler negative control; component/serde round-trip | TR-003, TR-004, TR-005, TR-008 / SC-002 | none (pure logic) | Yes (part of `cargo test`) | configured |
| Integration | `cargo test` (`crates/sim/tests/`) | `Physics` trait swap (Rapier-backed vs. stub impl) yields identical consumer behavior = same outputs for the same inputs | TR-006 / SC-004 | physics backend stubbed: the stub MUST implement the full `Physics` trait surface (the same method set, taking/returning only `glam`/`sim` types) so it is a drop-in substitute requiring no consumer change | Yes (part of `cargo test`) | configured |
| Build / Lint / Format | `cargo build` + `cargo clippy -- -D warnings` + `cargo fmt --check` + dependency-graph inspection (`cargo tree`/`cargo metadata`, TR-009) | workspace compiles; no lint findings; formatted; `sim` dep graph free of render/window/input/audio/net libs | TR-001, TR-002, TR-007, TR-009 / SC-001, SC-003 | — | Yes (CI gate) | configured |
| Security | `cargo-audit` | dependency vulnerability scan (RustSec advisory DB) | TR-010 | — | Yes for `high`/`critical` advisories; non-gating for `low`/`medium`/informational (TR-010); triaged by `sim::physics` owner | `cargo install cargo-audit` |
| Coverage | `cargo-llvm-cov` | line/branch coverage of `sim` | Testing & Quality Policy | — | No global coverage gate per `project-instructions.md`; coverage is reported, not thresholded, but the two mandatory motion invariants (equivalence + negative control) are themselves enforced as failing unit tests in the Unit tier, so the "motion invariants MUST be covered" rule is met by the gated Unit tier rather than a coverage percentage | `cargo install cargo-llvm-cov` |

## Error Handling Strategy

N/A — pure-logic library crate with no API, external service calls, or user-facing error states. Failures surface as Rust `Result`/`Option` at call sites and as assertion failures in tests; there is no runtime error-handling surface in this epic.

## Integration Points

| Spec Reference | System/Service | Technical Approach | Contract |
|----------------|----------------|--------------------|----------|
| IP-001 | Downstream epics (E002 client, E003 networking, E004 persistence, E007 damage, E008 transit) | Depend on the `sim` crate for components + motion APIs | Rust crate dependency on `sim` |
| IP-002 | Future `protocol` / persistence layers | `sim` public domain types are serde-derivable | `#[derive(Serialize, Deserialize)]` on domain types (AD-002) |
| IP-003 | Tiered-sim framework (E008) | Promote/demote relies on integrator↔analytic equivalence | Shared `sim::motion` + equivalence tests |
| IP-004 | Future custom physics backend | Engine replacement seam | `Physics` trait (AD-001; ADR-0004) |

## Risk Mitigation

| Risk (from spec) | Likelihood | Impact | Mitigation | Owner |
|-------------------|------------|--------|------------|-------|
| Physics-engine coupling/fit | M | M | Isolate Rapier2D behind the `Physics` trait; no Rapier types in consumer signatures (AD-001) | `sim::physics` |
| Floating-point drift (integrator vs. analytic) | L | L | Bounded, documented tolerance in tests; re-seed from the analytic form at tier transitions (downstream) | `sim::motion` |
| Premature/over-broad trait abstraction | L | L | Keep the `Physics` trait surface minimal and consumer-driven (AD-001) | `sim::physics` |

## Requirement Coverage Map

| Req ID | Component(s) | File Path(s) | Notes |
|--------|--------------|--------------|-------|
| TR-001 | Cargo workspace + `sim` crate | `Cargo.toml`, `crates/sim/Cargo.toml` | Workspace exists; `sim` is the shared crate (extend) |
| TR-002 | `sim` dependency set | `crates/sim/Cargo.toml` | Only glam/bevy_ecs(no-default)/rapier2d/serde — no render/window/net |
| TR-003 | Motion integrator | `crates/sim/src/motion.rs` | `dt` runtime parameter (already present) |
| TR-004 | Analytic + integrator | `crates/sim/src/motion.rs` | Closed-form + equivalence (already present) |
| TR-005 | Equivalence tests | `crates/sim/src/motion.rs` (`#[cfg(test)]`) + `crates/sim/tests/` | Multi-tick-rate + negative control (present); add trait-swap integration test |
| TR-006 | `Physics` trait + Rapier impl | `crates/sim/src/physics.rs` (new) | Engine-agnostic surface; Rapier2D default impl |
| TR-007 | CI quality gates | `.github/workflows/ci.yml` (new) | build + test + `clippy -- -D warnings` + `fmt --check` |
| TR-008 | Serde on domain types | `crates/sim/src/motion.rs`, `crates/sim/src/components.rs` (new) | `#[derive(Serialize, Deserialize)]` (AD-002); serde round-trip equality test (`PartialEq`) |
| TR-009 | Dependency-graph inspection | `.github/workflows/ci.yml` (new) | `cargo tree`/`cargo metadata` check that `sim` deps exclude render/window/input/audio/net libs |
| TR-010 | Security advisory triage | `.github/workflows/ci.yml` (new) + `.cargo/audit.toml` (new) | `cargo-audit`; gate on `high`/`critical`, track `low`/`medium`; `sim::physics` owner triages |

## Project Structure

### Source Code

```text
~ Cargo.toml                     # add rapier2d, serde, bevy_ecs to [workspace.dependencies]
  crates/sim/
~   Cargo.toml                   # + rapier2d, + serde (derive), + bevy_ecs (default-features = false)
~   src/lib.rs                   # export physics + components modules
~   src/motion.rs                # #[derive(Serialize, Deserialize)] on BodyState; (keystone logic unchanged)
+   src/physics.rs               # Physics trait + Rapier2D-backed implementation (AD-001)
+   src/components.rs            # bevy_ecs gameplay components (Position, Velocity, ...) (AD-003)
+   tests/physics_swap.rs        # integration test: Rapier vs. stub Physics impl → identical consumer behavior
+ .github/workflows/ci.yml       # build / cargo test / clippy -D warnings / fmt --check / dep-graph inspection (TR-009) / cargo-audit (TR-010)
+ .cargo/audit.toml              # cargo-audit triage config: ignore-list for justified low/medium advisories (TR-010)
```

**Brownfield Notes**
- **Patterns to reuse**: the `motion.rs` keystone (velocity-Verlet `integrate`, closed-form `analytic`, `simulate`, and the `#[cfg(test)]` equivalence suite) is correct and tested — extend, do not rewrite.
- **Tests to extend**: add to the existing `motion::tests` module (serde round-trip) and add `tests/physics_swap.rs`.
- **Naming conventions**: follow the existing module style and the workspace dependency-pinning convention (`[workspace.dependencies]` + `dep.workspace = true`).

## Implementation Hints

- **[HINT-001]** Reuse: the integrator + analytic + equivalence tests already exist in `crates/sim/src/motion.rs` — build the `Physics` trait, components, and serde *around* them; do not rewrite the keystone.
- **[HINT-002]** Determinism: keep `f32` + fixed `dt` + one shared code path; do NOT pursue cross-platform bit-determinism (ADR-0003) — it is a tar pit and unnecessary under server-authority + reconciliation.
- **[HINT-003]** Trait boundary: no Rapier2D-specific types may appear in the `Physics` trait's public signatures (use `glam`/`sim` types) or the swap guarantee (SC-004) breaks.
- **[HINT-004]** Deps: add `bevy_ecs` with `default-features = false` (pure ECS, no render/window); keep `sim` free of any bevy render/window/net and of any networking crate.
- **[HINT-005]** Local env: building on this machine requires `CARGO_HTTP_CHECK_REVOKE=false` and running the build/test outside the sandbox (known toolchain/network quirk); CI must run build + `cargo test` + `clippy -- -D warnings` + `fmt --check`.
