---
adr_id: ADR-0004
status: accepted
date: 2026-06-01
tags: [physics, performance, abstraction]
supersedes: []
superseded_by: ""
related_artifacts: [PRD CAP-001, PRD CAP-004, ADR-0003, docs/game-design.md §12]
---

# ADR-0004: 2D authoritative physics (Rapier2D) behind a swappable `Physics` trait

## Status

Accepted.

## Context

Gameplay is planar/top-down (rendered with 3D models), and the authoritative server must simulate many bodies (ships, projectiles, debris) cheaply, with the option to optimize at the thousand-body tier.

## Decision Drivers

- Planar gameplay makes 2D physics far cheaper than 3D.
- Many entities at scale.
- Ability to replace the engine if profiling demands.
- Avoid fast-projectile tunneling.

## Considered Options

### Option A: 3D physics constrained to a plane

- **Pros**: Matches 3D visuals literally.
- **Cons**: Wasteful CPU/memory for planar gameplay. Rejected.

### Option B: Rapier2D used directly

- **Pros**: Mature, fast, integrated.
- **Cons**: Full rigid-body features (joints, resting-contact solving) are overhead for mostly-non-colliding open-space bodies; hard to replace later.

### Option C: Rapier2D behind a swappable `Physics` trait

- **Pros**: Start with Rapier2D; if profiling at the thousand-body tier shows it is the bottleneck, swap in a custom spatial-hash broadphase + Newtonian integrator without touching gameplay code. Projectiles resolved as swept rays (CCD) to avoid tunneling.
- **Cons**: Trait-abstraction overhead; must avoid depending on Rapier-specific features that would not survive a swap.

## Decision Outcome

Chosen option: **Option C: Rapier2D behind a swappable `Physics` trait** — start with Rapier2D for mature, fast, integrated planar simulation, isolated behind a `Physics` trait so a custom spatial-hash broadphase + Newtonian integrator can replace it at the thousand-body tier without touching gameplay code. Projectiles are resolved as swept/raycast (CCD) tests to avoid tunneling.

## Consequences

### Positive

- Cheap planar simulation.
- Engine is replaceable.
- No tunneling.

### Negative

- Trait-abstraction overhead.
- Must avoid depending on Rapier-specific features that would not survive a swap.

### Neutral

- Physics lives inside the shared sim boundary (ADR-0003).

## Links

- PRD CAP-001
- PRD CAP-004
- Related ADR: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md)
- docs/game-design.md §12
