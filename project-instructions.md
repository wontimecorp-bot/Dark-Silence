<!-- template-version: 2 -->
# Dark Silence Project Instructions

## Core Principles

### I. Server-Authoritative Simulation

The server MUST be the single source of truth for all gameplay state; clients predict and interpolate but never dictate authoritative outcomes, and every client input MUST be validated server-side. — A real-time action MMO cannot trust clients: authority is the foundation of anti-cheat and of a consistent shared world.

### II. Shared Deterministic Sim Core

Gameplay logic MUST live in the shared `sim` crate that both client and server depend on; it MUST NOT be duplicated or forked between them. — One code path is what makes client-side prediction and server reconciliation agree; divergent logic is the root cause of desync.

### III. Tiered Simulation by Attention

Compute and bandwidth cost MUST scale with player attention, not world size: Tier 0 (real-time physics bubbles), Tier 1 (closed-form analytic transit), Tier 2 (persistent universe). Entities move between tiers via re-seeding from the analytic form, never by accumulating per-tick state across the boundary. — This is what makes a seamless, zoneless world affordable for a solo developer.

### IV. Agent Output Style

All agent output MUST be concise and outcome-oriented. This principle supersedes any verbose defaults.

- **Progress reports**: Facts and outcomes only — no narration, no restating the task.
- **Artifacts**: Emit required sections only — no preamble paragraphs, no summary epilogues.
- **Reasoning**: Omit unless the user asks "why" or the decision is non-obvious.
- **Errors / blockers**: State the problem, the attempted fix, and the result — nothing else.
- **Phase-boundary reports**: ≤ 5 bullet points.
- **Preserve without compressing**: Artifact template structure and required sections; explicit decision / registration / validation guidance in shared skills; delegation constraints and sub-agent role definitions; existing size limits (spec ≤ 1000 KB, research ≤ 400 KB, stories ≤ 200 words).

### V. Build the Seams, Defer Distribution

Entities MUST be serializable and accessed behind an authority / area-of-interest / handoff abstraction, but the system MUST run single-node until a measured limit forces otherwise. — Keeps seamless multi-node meshing reachable without paying its studio-scale distributed-systems cost before any gameplay exists.

### VI. Bandwidth Is the Budget

Replication MUST be area-of-interest–filtered, quantized, and delta-compressed, and MUST be bounded by an explicit per-client bandwidth budget with a priority function. — Bandwidth and interest management, not physics CPU, are the scaling wall for large co-located battles.

### VII. Playable Every Phase

Every delivery increment MUST yield something runnable and demoable, and core "fun" MUST be verified before scaling work begins. — Solo-scope risk management: avoid sinking years into infrastructure before there is a game to play.

## Technology Stack

<!-- Downstream phases (Plan, QC, Autopilot) read this section as the authoritative tech-stack reference. -->

- **Language/Runtime**: Rust (edition 2021; dev toolchain 1.92.0; MSRV to be pinned later).
- **Frameworks**: Bevy (client) + `bevy_ecs` as a pure ECS library inside the shared `sim` crate; renet (transport-level UDP) for networking, isolated behind the `protocol` crate + a swappable `NetTransport` adapter, with prediction/reconciliation/interpolation/snapshots game-owned against the shared `sim` (ADR-0014, refining the earlier lightyear choice; `bevy_replicon`/`aeronet` documented fallbacks) (planned); Rapier2D for planar physics behind a swappable `Physics` trait (planned). Currently wired: `glam`. Serialization: `bitcode` for snapshots (planned).
- **Storage**: PostgreSQL + Redis for Tier 1 transit and Tier 2 persistent state (planned); none wired currently.
- **Infrastructure**: Local dev now; single VPS + managed PostgreSQL at launch; containerization later; multi-node orchestration deferred (Phase 5).

## Testing & Quality Policy

<!-- QC extracts enforcement rules from this section. Use the keywords below so automated checks activate correctly. -->
<!-- Keywords recognised by QC: lint, static analysis, code quality, coverage, security, vulnerability, OWASP, WCAG, accessibility, benchmark, performance -->

- **Coverage Target**: No global coverage gate. Two invariants are mandatory-to-cover: the `sim` equations-of-motion equivalence (integrator ↔ closed-form analytic) and the `transit` promote/demote round-trip. (Revisit a global percentage once gameplay stabilizes.)
- **Required QC Categories**: linting (static analysis / code quality), security (untrusted client input validation, authentication, dependency vulnerability scanning), performance (benchmarks on simulation and replication hot paths). Accessibility / WCAG omitted — native game client, not a web app.
- **Test Strategy**: Unit and property tests for `sim` and `transit` invariants; an equivalence test guarding the integrator-vs-analytic invariant; integration tests driven by a headless-bot harness for networked behavior.
- **Linting / Formatting**: Clippy with `-D warnings`; rustfmt enforced.

## Source Code Layout

- **Policy**: ENFORCE_SRC_ROOT — adapted to a Cargo workspace. The designated source roots are each crate's `src/` directory under `crates/<name>/` (idiomatic Rust; a justified deviation from a single top-level `/src`).
- **Convention**: Shared gameplay logic lives in `crates/sim`; per-tier and per-process concerns split into sibling crates (`server`, `client`, `protocol`, `transit`, `persistence`, `tools`). Unit tests co-located via `#[cfg(test)]`; integration tests under each crate's `tests/`. Manifests and DB migrations at crate/repo root.

## Development Workflow

- **Branching**: Feature branches named `#####-feature-name` (SDD Feature Workspace convention) cut from `main`; squash merge. (Note: the repository is not yet under version control — `git init` is a recommended follow-up.)
- **Commit Convention**: Conventional Commits.
- **CI Requirements**: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` MUST all pass before merge.

## Governance

- Project instructions supersede all other documentation and practices.
- Amendments require a version bump with ISO-dated changelog entry.
- All implementations MUST pass the Instructions Check gate during planning.
- Complexity beyond these principles MUST be justified and documented.
- The SDD-Pilot lifecycle and phase gates are enforced; any violation of these project instructions is CRITICAL severity.

**Version**: 1.1.0 | **Last Amended**: 2026-06-02

> **Changelog** — 1.1.0 (2026-06-02): networking library changed from lightyear to **renet** (transport-level; prediction/reconciliation/interpolation/snapshots game-owned), isolated behind the `protocol` adapter, per ADR-0014 (refines ADR-0007). · 1.0.0 (2026-05-31): initial.
