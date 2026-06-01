---
adr_id: ADR-0008
status: accepted
date: 2026-06-01
tags: [domain-model, combat, ships, data-driven]
supersedes: []
superseded_by: ""
related_artifacts: [PRD:CAP-001, PRD:CAP-003, PRD:CAP-004, PRD:CAP-006, ADR-0003, ADR-0004]
---

# ADR-0008: Unified domain data model — damage pipeline, modules/fitting, destructible hulls

## Status

Accepted.

## Context

Combat, ship fitting, destruction, and salvage must interoperate and stay extensible. The game wants physical and positional fidelity (where a hit lands, what it passes through, what survives) without building a bespoke system per feature. A decision is needed now so that combat resolution, fitting, destruction, and salvage share one foundation rather than diverging into independent, hard-to-reconcile codepaths.

## Decision Drivers

- One model serving many features — less code, fewer bugs.
- Data-driven content authoring.
- Physical and positional fidelity.
- Networkable and affordable at scale.

## Considered Options

### Option A: Bespoke per-feature systems

Separate, independent codepaths for damage, fitting, and destruction.

- **Pros**: Each feature can be tuned in isolation without coordinating a shared schema.
- **Cons**: Duplication across features; behavior diverges over time; hard to extend because a new feature must touch multiple unrelated systems. Rejected.

### Option B: A unified, data-driven domain model

A single data-driven domain model where one foundation powers combat resolution, fitting, destruction, and salvage:

1. **Typed-damage pipeline** — every damage event is a typed packet with channels {kinetic/penetration, thermal/energy, blast, EM, radiation} that flows through ordered defense layers (avoidance → shields → armor → hull → systems/crew), each absorbing or modifying per channel; projectiles are swept rays.
2. **Data-driven Module abstraction** — every installed device (reactor, turret, shield, sensor, engine, etc.) shares a stat block (power gen/draw, CPU/control, mass, heat, hitbox/health, hardpoint type/size); a ship = hull + hardpoints + modules; fitting uses positional slots, so the fit layout IS the damage hitbox/armor map, bounded by power + CPU + mass budgets.
3. **Destructible hulls** — hulls authored as a 2D cell-grid (cells grouped into sections/modules); coarse module/section destruction now, fine cell-by-cell destruction deferred without a data-model refactor; severing via grid connectivity yields physical chunks; a clean sever (module health intact, surrounding structure gone) yields intact, scavengeable equipment.

- **Pros**: One model powers many features (less code, fewer bugs); enables emergent depth (angling, penetration, harvesting); naturally ties combat to salvage to economy; content is data-driven and extensible. Chosen.
- **Cons**: Content and balance authoring burden; cell-grid state and networking cost.

## Decision Outcome

Chosen option: **Option B: A unified, data-driven domain model** — adopt the unified data-driven model so that one pipeline powers combat resolution, fitting, destruction, and salvage, delivering physical/positional fidelity without per-feature bespoke systems.

## Consequences

### Positive

- One model powers many features.
- Emergent depth (angling, penetration, harvesting).
- Ties combat → salvage → economy.

### Negative

- Content and balance authoring burden.
- Cell-grid state and networking cost. Mitigations: simulate at cell granularity / render fine; compute connectivity only on destruction; raycast against the grid; use AOI + delta + LOD for networking; use coarser cells for large stations.

### Neutral

- Fitting is data-driven, but constructibility stays designer-authored — players fit modules, they do not build hull geometry.

## Links

- PRD: CAP-001, CAP-003, CAP-004, CAP-006
- Related ADRs: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md), [ADR-0004](0004-2d-physics-behind-trait.md)
- docs/game-design.md §4, §5, §7
