---
adr_id: ADR-0012
status: accepted
date: 2026-06-01
tags: [physics, modeling, simulation, scope]
supersedes: []
superseded_by: ""
related_artifacts:
  - PRD constraint: physically grounded, gameplay-scaled
  - ADR-0001
  - ADR-0003
  - ADR-0008
  - docs/game-design.md §4
---

# ADR-0012: Physically grounded, gameplay-scaled modeling

## Status

Accepted.

## Context

Dark Silence must decide how realistically to model physics and specs — missile speeds, warhead yields (and the blast radius / damage they imply), sensor ranges, and similar quantities. The choice spans a spectrum: real-world accurate at one end, cartoonish/arbitrary at the other, or something deliberately in between. The decision shapes how every interaction "feels," how readable combat is, and how much scope a solo developer must carry. It must be made now because the simulation model, damage pipeline, and entity budgets all depend on the chosen approach.

## Decision Drivers

- A believable, consistent, emergent feel — interactions should "make sense" and reward player understanding.
- Playability and readability — combat must be legible and fun at human timescales and screen scales.
- Tier, scale, and bandwidth limits — magnitudes must fit the tiered simulation and replication budgets.
- Solo-developer scope — the approach must be sustainable for one person.
- The locked "subtle-realistic" feel established for the game.

## Considered Options

### Option A: Maximal real-world accuracy

- **Pros**: Ultimate realism; physically authentic numbers throughout.
- **Cons**: Real space distances, speeds, and lethality are unplayable — invisible instant death from off-screen, or hours of waiting; real warheads one-shot everything. Huge time-sink to build and tune. Cf. *Children of a Dead Earth*, a deliberately niche and punishing example. Rejected.

### Option B: Cartoonish / arbitrary numbers

- **Pros**: Easy to author and tune.
- **Cons**: Loses consistency and emergent depth; arbitrary interactions don't "feel right." Rejected.

### Option C: Physically grounded, gameplay-scaled

- **Pros**: Keeps real physics relationships and models (Newtonian motion, momentum, kinetic energy KE = ½mv², penetration ∝ energy/area + angle, inverse-square falloff, plausible explosive-yield curves) for consistency, believable feel, and learnable emergence — while choosing the magnitudes (distance, speed, lethality, timescale, entity counts) for playability/readability and tier limits, kept internally self-consistent as a single self-consistent unit system.
- **Cons**: Requires choosing and maintaining consistent scale factors; deliberately not a realism simulator.

## Decision Outcome

Chosen option: **Option C: Physically grounded, gameplay-scaled** — adopt grounded relationships with scaled magnitudes. Keep the real physics relationships and models so interactions remain consistent, believable, and emergent, but choose the magnitudes for playability, readability, and tier limits. Real specs are inspiration and texture, not binding numbers; internal consistency — not real-world accuracy — is what sells "realism." Distances, speeds, and timescales are compressed (often by orders of magnitude), and damage, armor, and time-to-kill are gameplay-tuned within a single self-consistent unit system.

## Consequences

### Positive

- Believable and playable and emergent at once, with sane scope; matches the subtle-realistic feel.
- Aids learnable, emergent research and manufacturing — players can reason about systems because the relationships are real.

### Negative

- Must choose and maintain consistent scale factors across the model.
- Deliberately not a realism simulator; players seeking literal real-world fidelity are not the target.

### Neutral

- The specific scale factors remain an open tuning question.

## Links

- PRD constraint: physically grounded, gameplay-scaled.
- Related ADRs: [ADR-0001](0001-tiered-simulation-architecture.md), [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md), [ADR-0008](0008-unified-domain-data-model.md).
- docs/game-design.md §4.
