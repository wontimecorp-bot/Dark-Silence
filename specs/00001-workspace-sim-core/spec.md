---
feature_branch: "00001-workspace-sim-core"
created: "2026-06-01"
input: "E001"
spec_type: "technical"
spec_maturity: "draft"
epic_id: "E001"
epic_sources: "{SAD:ADR-0003}{SAD:ADR-0004}{SAD:ADR-0007}"
---

# Feature Specification: Workspace & Sim Core

**Feature Branch**: `00001-workspace-sim-core`
**Created**: 2026-06-01
**Status**: Draft
**Spec Type**: technical
**Spec Maturity**: draft
**Epic ID**: E001
**Epic Sources**: {SAD:ADR-0003}{SAD:ADR-0004}{SAD:ADR-0007}
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

Every other epic in the project depends on a single, authoritative simulation core: the client must predict using the exact same gameplay logic the server runs, and the tiered world must move entities between real-time and analytic representations without divergence. Without a shared, dependency-clean simulation crate and a stable physics seam established first, gameplay logic would fragment across client and server (the root cause of desync) and the project would couple itself to one physics engine. This foundation must exist and be trustworthy before flight, combat, networking, or persistence are built on it.

## Scope *(mandatory)*

### Included

- A Cargo workspace whose gameplay logic lives in one shared `sim` crate consumed by both client and server.
- Fixed-timestep motion integration with a runtime-variable timestep, plus a closed-form analytic evaluator that provably agrees with the integrator.
- A `Physics` trait that abstracts the 2D physics engine (initially Rapier2D-backed) behind a swappable boundary.
- Workspace build/quality gates (build, tests, lint, format) covering the above.

### Excluded

- Rendering, windowing, audio, input, and any client presentation — belongs to the client epic (E002).
- Networking, replication, and prediction/reconciliation machinery — belongs to the networking epic (E003).
- Persistence/storage — belongs to the persistence epic (E004).
- Collision response, weapons, damage, and projectile behavior — consume this core but are specified by later epics (E002/E007).
- A custom (non-Rapier) physics implementation — out of scope now; the trait exists so it *can* be added later without rework.

### Edge Cases & Boundaries

- Floating-point accumulation over long-running integration causes the integrator to drift from the closed-form result; this drift MUST be bounded and tested (per TR-004/SC-002) by exercising accumulation over a meaningful horizon — ≈100 s (3,000 steps) at 30 Hz, asserting drift stays within `rel_tol = 1e-4` — and tier transitions re-seed from the analytic form rather than accumulate.
- A runtime timestep that varies (time dilation) must not change the logical result of stepping (same logical `dt` per tick, different wall-clock rate). This is a stated test obligation under TR-005: the equivalence assertion MUST pass at every rate in {10, 20, 30, 60, 144} Hz, demonstrating the logical result is invariant to tick rate.
- Degenerate inputs must behave predictably: zero acceleration → straight-line coasting with velocity unchanged (asserted by a coasting test); zero `dt` → a step that returns the input state unchanged (a no-op tick); large absolute coordinate magnitudes risk `f32` precision loss and are explicitly OUT OF SCOPE for this crate — they are handled by sector-relative coordinates downstream.
- The `Physics` trait boundary must not leak engine-specific types into gameplay code, or the swap guarantee is void.

## Technical Objectives *(mandatory for technical specs only)*

### Objective 1 - Shared, dependency-clean sim crate & workspace (Priority: P1)

Establish a Cargo workspace in which all gameplay simulation logic lives in a single `sim` crate that both client and server depend on, with no rendering, windowing, or networking dependencies, so there is exactly one source of gameplay truth.

**Why this priority**: Every other epic consumes `sim`; nothing can be built correctly until this single source of truth exists and is dependency-clean.

**Rationale**: One shared code path is what makes client prediction and server reconciliation agree; duplicated logic is the root cause of desync (ADR-0003). A clean dependency boundary keeps the crate usable on a headless server.

**Deliverables**:
- A Cargo workspace with the `sim` crate as a member and pinned shared dependencies.
- The `sim` crate exposing gameplay components and motion APIs, free of render/window/network dependencies.

**Validation Criteria**:
1. **Given** the workspace, **When** it is built, **Then** it compiles successfully and the `sim` crate's dependency graph contains no rendering/windowing/networking libraries.
2. **Given** the `sim` crate, **When** inspected, **Then** gameplay components and motion functions are public and usable from a headless (non-graphical) context.

### Objective 2 - Integrator ↔ analytic equivalence with runtime timestep (Priority: P1)

Provide fixed-timestep motion integration whose per-tick result matches a closed-form analytic evaluator under constant acceleration to within a bounded tolerance, with the timestep supplied at runtime.

**Why this priority**: This invariant is what lets entities promote/demote between real-time and analytic tiers without teleporting, and lets the timestep vary (time dilation) safely. It is the keystone the tiered architecture relies on.

**Rationale**: The tiered simulation (ADR-0001) stores in-transit entities as closed-form trajectories and re-instantiates them as stepped bodies; the two representations must agree. A runtime timestep makes time dilation a parameter rather than a rewrite (ADR-0003).

**Deliverables**:
- A fixed-step integrator accepting `dt` as a parameter.
- A closed-form analytic evaluator for position/velocity over elapsed time.
- An automated test suite asserting equivalence — for **both** the position and velocity outputs — to a bounded relative tolerance (relative error `|a-b| ≤ rel_tol · max(|b|, 1)`; `rel_tol = 1e-4` for the primary same-`dt` test, `2e-4` for the across-tick-sizes test) across the representative tick-rate set {10, 20, 30, 60, 144} Hz, plus a negative control (forward/explicit Euler) demonstrating a wrong integrator would fail (absolute position error `> 0.1` under constant acceleration over the same horizon).

**Validation Criteria**:
1. **Given** the same initial state and constant acceleration, **When** the integrator is stepped N times at timestep `dt` over a meaningful flight horizon (≈100 s / 3,000 steps at 30 Hz) and the analytic evaluator is evaluated at `t = N·dt`, **Then** both the position and velocity results agree to within the bounded relative tolerance `rel_tol = 1e-4`.
2. **Given** the tick-rate set {10, 20, 30, 60, 144} Hz each run for a fixed 60 s horizon, **When** the equivalence test runs, **Then** both position and velocity agree to within `rel_tol = 2e-4` for every rate; a deliberately incorrect integrator (forward/explicit Euler) fails the same assertion (absolute position error `> 0.1`).

### Objective 3 - Swappable Physics trait (Priority: P2)

Expose physics (broadphase/collision/integration support) through a `Physics` trait with an initial Rapier2D-backed implementation, so the engine can be replaced later without changing gameplay or consumer code.

**Why this priority**: The abstraction is required for the epic to be complete and to keep ADR-0004's "replace at the thousand-body tier" option open, but a thin boundary suffices now; depth grows when collision/combat (E002/E007) consume it.

**Rationale**: Gameplay is planar, so 2D physics is far cheaper than 3D, and a full rigid-body engine may later be replaced by a custom broadphase; isolating it behind a trait protects every consumer from that swap (ADR-0004).

**Deliverables**:
- A `Physics` trait defining the engine-agnostic surface gameplay needs.
- A Rapier2D-backed implementation of the trait.

**Validation Criteria**:
1. **Given** gameplay code that uses physics, **When** it is written against the `Physics` trait, **Then** no Rapier2D-specific types appear in that gameplay code.
2. **Given** the trait, **When** a second (stub/alternate) implementation is provided, **Then** swapping it requires no changes to consumer code.

### Technical Constraints

- The `sim` crate MUST contain no rendering, windowing, input, audio, or networking dependencies (pure logic; usable headless).
- Motion modeling uses real physics *relationships* with gameplay-scaled magnitudes; this crate owns the relationships, not the tuned magnitudes (ADR-0012).
- Cross-machine bit-determinism is NOT a requirement; consistent `f32`, fixed `dt`, and one shared code path are sufficient (server-authoritative + reconciliation — ADR-0002/0003).
- Engine-specific physics types MUST NOT cross the `Physics` trait boundary.
- The workspace MUST pass build, tests, `clippy -- -D warnings`, and `fmt --check`.

## Integration Points *(mandatory for technical and operational specs)*

- **IP-001**: All downstream epics (E002 client, E003 networking, E004 persistence, E007 damage, E008 transit, and others) depend on the `sim` crate for gameplay components and motion APIs — `sim` is the shared substrate.
- **IP-002**: The future networking (`protocol`) and persistence layers depend on `sim` domain types being serializable; the crate should keep its public types serialization-friendly to avoid downstream rework.
- **IP-003**: The tiered-simulation framework (E008) depends on the integrator ↔ analytic equivalence (Objective 2) to promote/demote entities without divergence.
- **IP-004**: Any future custom physics backend depends on the `Physics` trait (Objective 3) as the replacement seam, per ADR-0004.

## Requirements *(mandatory)*

### Technical Requirements *(technical specs only)*

- **TR-001**: The repository MUST be organized as a Cargo workspace, with all gameplay simulation logic in a single shared `sim` crate depended on by both client- and server-side code.
- **TR-002**: The `sim` crate MUST NOT depend on rendering, windowing, input, audio, or networking libraries; it MUST be usable in a headless context.
- **TR-003**: `sim` MUST provide fixed-timestep motion integration that accepts the timestep `dt` as a runtime parameter (enabling per-bubble time dilation downstream).
- **TR-004**: `sim` MUST provide a closed-form analytic evaluator for motion under constant acceleration, and the per-tick integrator MUST agree with it — in **both** position and velocity — to within a bounded relative tolerance of `rel_tol = 1e-4` for the primary same-`dt` test and `2e-4` for the across-tick-sizes test (relative bound: `|a-b| ≤ rel_tol · max(|b|, 1)`), accumulated over a meaningful horizon (≈100 s / 3,000 steps at 30 Hz).
- **TR-005**: The equivalence between integrator and analytic evaluator MUST be enforced by automated tests covering the representative tick-rate set {10, 20, 30, 60, 144} Hz, including a negative control that uses a forward (explicit) Euler integrator and asserts it diverges from the closed form (absolute position error `> 0.1`) under constant acceleration.
- **TR-006**: Physics capabilities MUST be exposed through a `Physics` trait with an initial Rapier2D-backed implementation; engine-specific (Rapier2D) types MUST NOT appear in the trait's public signatures or in consumer code — only `glam`/`sim` types may cross the boundary. This MUST be guarded by an integration test in which a second (stub) `Physics` implementation is substituted with **no changes to consumer code**, where "identical consumer behavior" means the consumer produces the **same outputs for the same inputs** under both implementations; the consumer signatures referencing only `glam`/`sim` types constitute the type-leak audit.
- **TR-007**: The workspace MUST build and pass `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check`. Each of these is an unambiguous pass/fail gate: clippy with `-D warnings` fails the build on any warning ("no findings"), `fmt --check` fails on any unformatted file, and a green `cargo test` requires every test (including the equivalence invariant) to pass.
- **TR-008**: `sim` public domain types (at minimum `BodyState` and the gameplay components) MUST be serialization-friendly (serde-derivable) and MUST survive a serialize→deserialize round-trip such that the deserialized value equals the original under `PartialEq`, to support downstream replication and persistence without rework.
- **TR-009**: SC-001's "zero rendering/windowing/input/audio/networking libraries" constraint MUST be verifiable by an automatable dependency-graph inspection (e.g., `cargo tree`/`cargo metadata` over the `sim` crate's resolved dependency set) runnable in CI, not by manual inspection alone.
- **TR-010**: The Security tier (`cargo-audit`) is run as a CI check; a reported advisory of `high` or `critical` severity is treated as a gating failure (must be fixed, updated, or — with a recorded justification — explicitly ignored via the audit config), while `low`/`medium`/informational advisories are non-gating and tracked for follow-up. The `sim::physics` owner triages reported advisories.

### Key Entities *(include for product or technical specs if feature involves data)*

- **BodyState**: The kinematic state (position and velocity) of a point mass on the 2D gameplay plane; the value the integrator and analytic evaluator operate on.
- **Sim components**: The gameplay-state data types (e.g., position/velocity and related fields) that make up the shared simulation model.
- **Physics trait**: An engine-agnostic abstraction over 2D physics (broadphase/collision/integration support), with a Rapier2D-backed implementation.
- **Integrator / analytic evaluator**: The two equivalent expressions of motion — a per-tick stepped integrator and a closed-form evaluator — required to agree.

## Assumptions & Risks *(mandatory)*

### Assumptions

- The Rust toolchain (edition 2021) and the `glam` math library are the basis for the crate.
- Gameplay is planar, so 2D physics is sufficient (ADR-0004).
- Server-authoritative simulation with reconciliation means cross-machine bit-determinism is not required (ADR-0003).
- A pre-existing motion keystone (integrator + analytic evaluator + equivalence tests) is the baseline that already satisfies Objective 2; Objective 3 (the `Physics` trait and engine integration) is the principal new work.

### Risks

- **Physics-engine coupling/fit** *(likelihood: medium, impact: medium)*: relying on Rapier2D's API surface; mitigated by the `Physics` trait isolating it so a future swap is contained (ADR-0004).
- **Floating-point drift** *(likelihood: low, impact: low)*: `f32` accumulation makes the integrator diverge from the closed form over long runs; mitigated by a bounded, documented tolerance and re-seeding from the analytic form at tier transitions.
- **Premature/over-broad trait abstraction** *(likelihood: low, impact: low)*: an over-designed `Physics` trait adds friction; mitigated by keeping the initial surface minimal and consumer-driven.

## Implementation Signals *(mandatory)*

- `NEW-CONFIG` — Cargo workspace and `sim` crate manifests, pinned shared workspace dependencies, and the build/test/lint/format quality gates the plan phase should establish.
- `NEW-CONFIG` — a new internal abstraction seam (the `Physics` trait) the plan phase should design as the engine-replacement boundary; keep its surface minimal and free of engine-specific types.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [OBJ1]: The workspace builds successfully and the `sim` crate's dependency graph — verified by automatable dependency-graph inspection in CI (TR-009) — contains zero rendering, windowing, input, audio, or networking libraries.
- **SC-002** [OBJ2]: Automated tests confirm the integrator matches the closed-form analytic — in both position and velocity — to within the bounded relative tolerance (`rel_tol = 1e-4` primary, `2e-4` across tick sizes) across the representative tick-rate set {10, 20, 30, 60, 144} Hz, and `cargo test` is green.
- **SC-003** [OBJ1]: `cargo clippy -- -D warnings` and `cargo fmt --check` pass on the entire workspace with no findings (any clippy warning or unformatted file fails the gate — TR-007).
- **SC-004** [OBJ3]: Gameplay code uses physics only via the `Physics` trait — no Rapier2D-specific types appear in consumer code (consumer signatures reference only `glam`/`sim` types — TR-006), and substituting an alternate trait implementation produces the same outputs for the same inputs with no consumer changes.

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| `sim` crate | The shared, dependency-clean Rust crate holding all gameplay simulation logic; the single source of gameplay truth used by client and server. |
| BodyState | Position + velocity of a point mass on the 2D gameplay plane. |
| Velocity-Verlet | The fixed-step integration method whose result is exact (in real arithmetic) versus the closed form under constant acceleration. |
| Analytic evaluator | The closed-form function giving position/velocity at an elapsed time under constant acceleration. |
| Runtime timestep (`dt`) | The simulation step duration supplied at runtime so it can vary (time dilation) without changing logical results. |
| Physics trait | The engine-agnostic abstraction over 2D physics, allowing the backing engine (initially Rapier2D) to be swapped without changing gameplay code. |

## Compliance Check

**Auditor verdict**: PASS — no CRITICAL violations (validated against `project-instructions.md`).

- All seven Core Principles aligned or not contradicted; this epic directly realizes **II (Shared Deterministic Sim Core)** and **III (Tiered Simulation by Attention)**. **VI (Bandwidth)** is N/A here (replication/AOI is out of scope, deferred to E003).
- **Technology Stack** (Rust, Cargo workspace, `sim` crate, Rapier2D behind a `Physics` trait, `glam`), **Testing & Quality Policy** (the mandatory `sim` equations-of-motion invariant is covered by automated tests; Clippy `-D warnings` + rustfmt enforced), and **Source Code Layout** (Cargo workspace, `crates/<name>/src`) all aligned.
- **Advisories (LOW, non-blocking — address during Planning/QC):**
  - **Principle V (Build the Seams)**: TR-008 marks serialization-friendliness a SHOULD; confirm serde-derivability of `sim` domain types is actually realized before E003/E004 consume them, so Principle V's MUST is met in time.
  - **Principle VII (Playable Every Phase)**: this foundation epic yields a library + test harness (buildable, green gates) rather than a runtime demo — inherent to a substrate crate; acceptable.
- **Required corrective actions**: none. Spec is release-ready for this gate.
