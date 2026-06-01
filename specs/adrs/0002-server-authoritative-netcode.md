---
adr_id: ADR-0002
status: accepted
date: 2026-06-01
tags: [networking, netcode, anti-cheat]
supersedes: []
superseded_by: ""
related_artifacts: [PRD CAP-001, PRD CAP-002, ADR-0003, ADR-0006, ADR-0007, docs/game-design.md §12]
---

# ADR-0002: Server-authoritative netcode with client prediction and reconciliation

## Status

Accepted.

## Context

A real-time, physics-based action MMO with untrusted clients needs anti-cheat, a consistent shared world, and responsive controls over the network, while scaling to many co-located players with variable area-of-interest and late joiners.

## Decision Drivers

- Anti-cheat: never trust clients.
- Consistency: one authoritative truth.
- Responsiveness despite latency.
- Must scale with variable AOI and late joiners.

## Considered Options

### Option A: Deterministic lockstep

- **Pros**: Low bandwidth; exact synchronization across machines.
- **Cons**: Cannot scale to thousands with variable AOI/late joiners; full cross-machine determinism is a tar pit. Rejected.

### Option B: Client-authoritative

- **Pros**: Trivial responsiveness.
- **Cons**: Cheating-prone; unacceptable for competitive PvP. Rejected.

### Option C: Server-authoritative with client-side prediction and reconciliation

- **Pros**: Server-authoritative model with client-side prediction (own ship), server reconciliation (input-replay), entity interpolation (~100 ms) for remotes, sparing extrapolation, and lag compensation (rewind) for hit validation. Newtonian motion is highly extrapolatable, so corrections are small.
- **Cons**: Cannot predict inter-player physics, so collisions produce visible corrections; reconciliation and input-ring machinery must be built.

## Decision Outcome

Chosen option: **Option C: Server-authoritative with client-side prediction and reconciliation** — the server is the single source of truth; clients predict their own input-driven motion and reconcile to server snapshots; remote entities are interpolated; hits are lag-compensated. Inter-player collisions are server-resolved, NOT predicted.

## Consequences

### Positive

- Fair, consistent, and responsive play.
- Lag compensation is lighter than an FPS because projectiles have travel time.

### Negative

- Cannot predict inter-player physics, leading to visible corrections when players collide (mitigate: keep hard physical coupling bounded, smooth corrections).
- Reconciliation and input-ring machinery must be built.

### Neutral

- Requires the shared sim crate (ADR-0003) and a UDP transport with delta snapshots.

## Links

- PRD CAP-001
- PRD CAP-002
- Related ADR: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md)
- Related ADR: [ADR-0006](0006-interest-management-and-bandwidth-scaling.md)
- Related ADR: [ADR-0007](0007-technology-stack-and-workspace.md)
- docs/game-design.md §12
