---
adr_id: ADR-0003
status: accepted
date: 2026-06-01
tags: [simulation, determinism, code-sharing, physics-numerics]
supersedes: []
superseded_by: ""
related_artifacts: [ADR-0001, ADR-0002, ADR-0004, ADR-0007]
---

# ADR-0003: Shared `sim` crate with fixed-timestep velocity-Verlet integration

## Status

Accepted.

## Context

Client prediction requires the client and server to compute identical motion; if the two sides diverge, predicted positions drift and players experience rubber-banding. The tiered architecture (ADR-0001) compounds this: the Tier-0 per-tick integrator must agree with the Tier-1 closed-form trajectory so that promoting or demoting an entity between tiers does not teleport it. A decision is needed now because the motion code path is foundational — it is depended on by both the client and the server and by every tier transition, so the choice of integrator and of how that code is shared determines whether the system's keystone invariant (Tier-0 ↔ Tier-1 position continuity) can hold at all.

## Decision Drivers

- Prevent desync via a single code path shared by client and server.
- Guarantee Tier-0 ↔ Tier-1 position continuity across promote/demote.
- Support a runtime-variable timestep for per-bubble time dilation.

## Considered Options

### Option A: Integrator — Forward / semi-implicit Euler

- **Pros**: Simple to implement and reason about.
- **Cons**: Under- or overshoots the closed-form solution, so it drifts away from the analytic trajectory and breaks the promote/demote position-continuity invariant. Rejected.

### Option B: Integrator — Velocity Verlet (chosen)

- **Pros**: Exact versus the closed-form solution under constant acceleration; symplectic and numerically stable over long runs.
- **Cons**: Slightly more arithmetic per step than Euler; requires care to carry the half-step velocity state.

### Option C: Code sharing — Duplicate gameplay logic in client and server

- **Pros**: No shared dependency to coordinate; each side evolves independently.
- **Cons**: Divergent implementations are the root cause of desync; the two code paths inevitably drift. Rejected.

### Option D: Code sharing — Shared `sim` crate (chosen)

- **Pros**: A single pure-logic crate (no render, windowing, or networking dependencies) that both client and server depend on, eliminating the duplicate-logic desync source; it also exposes the closed-form analytic evaluator, unit-tested for equivalence with the integrator.
- **Cons**: The crate must be kept free of render/net dependencies, which is an ongoing discipline constraint.

## Decision Outcome

Chosen option: **Option B (Velocity Verlet) combined with Option D (shared `sim` crate)** — a shared `sim` crate uses fixed-timestep velocity-Verlet integration with `dt` as a runtime parameter (enabling per-bubble time dilation), and additionally exposes a closed-form analytic evaluator proven equivalent to the integrator by unit tests. Velocity Verlet was selected over Euler because it is exact against the closed form under constant acceleration, preserving the promote/demote invariant; the shared crate was selected over duplicated logic because duplication is the root cause of client/server desync. This is already implemented in `crates/sim` with passing equivalence tests.

## Consequences

### Positive

- Correctness: a single integrator matched to a tested analytic evaluator preserves the Tier-0 ↔ Tier-1 position-continuity invariant.
- Code reuse: client and server share one motion code path, eliminating duplicate-logic desync.
- Time-dilation-ready: `dt` as a runtime parameter supports per-bubble time dilation.
- The integrator/analytic equivalence is a tested keystone invariant.

### Negative

- f32 accumulation drifts over thousands of steps, so tier transitions must re-seed from the analytic form rather than accumulate stepped state (documented and accepted).
- The crate must stay free of render and networking dependencies — an ongoing discipline constraint.

### Neutral

- Server-authoritative play with client reconciliation means full cross-machine bit-determinism is NOT required.

## Links

- Related: ADR-0001, ADR-0002, ADR-0004, ADR-0007.
- Implemented in `crates/sim` (passing integrator/analytic equivalence tests).
- docs/game-design.md §12.
