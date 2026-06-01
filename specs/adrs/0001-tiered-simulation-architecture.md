---
adr_id: ADR-0001
status: accepted
date: 2026-06-01
tags: [simulation, scalability, core-architecture]
supersedes: []
superseded_by: ""
related_artifacts: [CAP-002, CAP-009, ADR-0003]
---

# ADR-0001: Tiered simulation architecture (real-time / transit / persistent)

## Status

Accepted.

## Context

Dark Silence is one seamless persistent universe that must support knife-fight skirmishes, hundreds-strong battles, and galaxy-spanning long-range weapons and "messages in a bottle" that travel for a long time — affordably, for a solo developer on a single node. Simulating the whole world in real time is impossible, and hard zones break the seamlessness goal. A decision is needed now because the simulation model is foundational: it constrains gameplay features (notably long-range weapons that reach players who never saw the attacker), persistence design, and the per-node cost budget that makes a solo-developer deployment viable.

## Decision Drivers

- Compute and bandwidth cost must scale with player attention, not world size.
- Affordability on a single node for a solo developer.
- Must enable long-range weapons that reach players who never saw the attacker.
- Persistence of a living world.

## Considered Options

### Option A: Flat real-time simulation of the whole world

- **Pros**: Simple and uniform; no tier boundaries or promote/demote logic; every entity behaves identically regardless of player presence.
- **Cons**: Prohibitively expensive; impossible at scale; cost grows with world size rather than player attention, defeating the affordability and single-node drivers.

### Option B: Discrete zoned instances with load screens

- **Pros**: Bounds cost by capping each instance; predictable per-instance resource use.
- **Cons**: Breaks the seamless-world goal; cross-zone interactions are awkward; incompatible with galaxy-spanning long-range weapons that must traverse arbitrary regions.

### Option C: Three tiers (real-time / transit / persistent)

- **Pros**: Cost tracks player attention; empty space and in-transit objects cost roughly a database row until something interacts; directly enables long-range weapons and offline/idle assets are cheap; preserves a single seamless universe.
- **Cons**: Introduces promote/demote "seam" complexity; correctness depends on the Tier-0 integrator agreeing with the Tier-1 closed form; requires a scheduler/timer-wheel for Tier-1 wake events.

## Decision Outcome

Chosen option: **Option C: Three tiers (real-time / transit / persistent)** — Tier 0 provides real-time authoritative physics "combat bubbles" where players are; Tier 1 provides coarse analytic transit, storing long-range and in-transit entities as closed-form trajectories plus scheduled events evaluated on demand; Tier 2 provides the persistent universe as durable, event-driven state. Entities promote and demote between tiers by re-seeding from the analytic form. Cost tracks player attention rather than world size, so empty space and in-transit objects cost approximately a database row until something interacts. The "message-in-a-bottle" long-range weapon is the litmus feature exercising the promote/demote path. Options A and B were rejected because A cannot scale on a single node and B breaks the seamless-world goal.

## Consequences

### Positive

- Affordable seamless world that scales with player attention rather than world size.
- Enables the signature long-range strategic gameplay, including weapons and messages that reach players who never saw the attacker.
- Offline and idle assets are cheap, costing roughly a database row until interacted with.

### Negative

- Promote/demote "seam" complexity between tiers.
- Correctness depends on the Tier-0 integrator agreeing with the Tier-1 closed form (see ADR-0003).

### Neutral

- Introduces a scheduler/timer-wheel for Tier-1 wake events.

## Links

- PRD CAP-002 — shared persistent universe.
- PRD CAP-009 — long-range strategic weapons and messaging.
- Related: ADR-0003.
- docs/game-design.md §3.
