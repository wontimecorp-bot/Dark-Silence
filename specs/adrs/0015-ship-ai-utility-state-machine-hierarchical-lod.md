---
adr_id: ADR-0015
status: accepted
date: 2026-06-10
tags: [ai, ships, determinism, performance, simulation, lod]
supersedes: []
superseded_by: ""
related_artifacts: [specs/00008-ship-ai, Epic E011, ADR-0003, ADR-0005, ADR-0006, ADR-0011, CAP-007, STF-001]
---

# ADR-0015: Ship AI architecture — utility-scored state-machine brains with hierarchical command and behavior-LOD

## Status

Accepted.

## Context

The game has rich deterministic physics — intent-driven flight, modules, ramming-as-kinetic-damage, energy/heat gates — but no AI framework. Only three hardcoded systems exist (seek, turret, mining), and they bypass `ShipIntent` by mutating velocity/heading directly. E011 needs autonomous ship behaviors (movement, formation, combat, scout, search-and-destroy) that obey the full physics, stay bit-identical deterministic (the project's golden-test safety story), and scale to big battles — AI cost must not be O(ships) per tick. This decision governs all future AI work (NPC director, escorts, drones), so the substrate must be chosen before behavior count grows.

## Decision Drivers

- AI must obey every game mechanic by construction — same control surface as a human player, no physics bypasses.
- Bit-identical determinism: golden tests must keep passing (additive, `ScenarioActive`-gated systems; strict-f32 scoring; seeded hashing; stable iteration order).
- Decision cost must scale with squads/groups, not individual ships, to support big battles (ADR-0006 attention-scaled cost).
- One substrate for all future NPC work, including the automation floor of ADR-0011.
- Tunable, debuggable behavior selection with explicit anti-oscillation behavior.
- Seeds future seams: sector sharding (ADR-0005/0006), EW/jamming (CAP-007).

## Considered Options

### Option A: Utility-scored state-machine brains with hierarchical command and behavior-LOD

- **Pros**: Explicit enum states in one `AiBrain` component avoid archetype thrash; deterministic multiplicative utility scoring with incumbent momentum and entity-id tiebreaks gives data-driven tuning without transition explosion; intent-only output reuses the human control path; hierarchical command + LOD makes decision cost O(squads); event-driven re-evaluation keeps per-tick cost low.
- **Cons**: Utility tuning needs a score-breakdown debug view; promote/demote state synthesis across LOD tiers is delicate.

### Option B: Behavior trees

- **Pros**: General-purpose, well-understood, composable; good editor tooling exists in the ecosystem.
- **Cons**: Heavier per-tick tree walk; weaker determinism story; over-general for roughly ten behaviors. Rejected.

### Option C: Pure hierarchical finite state machine

- **Pros**: Simple, fully explicit, trivially deterministic.
- **Cons**: Transition explosion as behaviors multiply; tuning lives in code instead of data. Rejected.

### Option D: GOAP (goal-oriented action planning)

- **Pros**: Emergent multi-step plans; expressive goals.
- **Cons**: Plan-search cost per agent; hardest determinism story of the candidates; overkill for the behavior set. Rejected.

### Option E: Direct velocity/heading mutation (today's seek pattern)

- **Pros**: Cheapest possible per-ship cost; already exists.
- **Cons**: Bypasses physics, modules, energy/heat gates — exactly the anti-pattern this decision replaces. Rejected.

### Option F: Per-ship-only AI with uniform throttling

- **Pros**: No hierarchy to build; uniform code path for every ship.
- **Cons**: Uniform throttling degrades ships in front of the player; cost still scales O(ships). The AC-Unity-style capped-full-brains + group-tree approach (Option A) is strictly better. Rejected.

## Decision Outcome

Chosen option: **Option A: Utility-scored state-machine brains with hierarchical command and behavior-LOD** — adopted as five pillars:

1. **Brain model = utility-scored state machine.** Behaviors are explicit enum states in ONE `AiBrain` component (no per-state marker components — archetype thrash). Selection is deterministic multiplicative utility scoring over consideration curves, with a ~25% incumbent momentum bonus (anti-oscillation) and entity-id tiebreaks. Re-evaluation is event-driven (hit, target-lost, new-contact, arrived) with a stable-id-hash cadence fallback. Tactics are parameterized by a fit-derived archetype cached on `Changed<ShipStats>`.
2. **Intent-driven control seam.** Full-physics AI writes `ShipIntent` exclusively — the same surface as a human. One code path; physics, modules, and determinism come for free. Only dormant-LOD groups use a cheap-glide path.
3. **Hierarchical command + behavior-LOD.** Ship → squad → wing → aggregate. Near players: individual brains. Mid: one squad brain orders members — O(squads) decisions, O(1) member execution. Far: cheap-glide aggregates. Promote/demote is deterministic with boundary hysteresis, keyed on AUTHORITATIVE player proximity (never a per-client camera). Hostile far groups mutually auto-promote on detection; off-screen combat is accepted unbounded in v1 (recorded MMO-scale risk STF-001).
4. **Tiered build-once-read-many spatial index.** The existing fine broadphase (collision + sensor queries) plus a new coarse interest tier (AOI/LOD; seeds future sector sharding per ADR-0005/0006). All consumers READ the same per-tick structure.
5. **Tier-scaled sensor perception + faction datalink fusion.** Perception is gated by sensor range/signature at every tier: near each think, mid fused per squad, far coarse scan (the promotion trigger). A faction sensor network fuses connected components over transmitting members (baseline TX+RX in v1); jamming is consumed as a seam flag — the EW mechanic and Sensor/Datalink module taxonomy are CAP-007. Steering is inertia-aware context-steering (danger MASKING) plus per-squad-objective flow-field tiles.

## Consequences

### Positive

- One deterministic AI substrate for all future NPC work (NPC director, escorts, drones, ADR-0011 automation floor).
- Decision cost scales with squads, not ships.
- AI obeys every game mechanic by construction — it drives the same `ShipIntent` surface as a human.

### Negative

- Utility tuning needs a score-breakdown debug view (planned dev-panel section).
- Promote/demote state synthesis is the delicate part (continuity + hysteresis across LOD tiers).
- Off-screen war cost is unbounded until an MMO-scale cap/abstract-resolution lands (deferred, STF-001).

### Neutral

- Constraints honored: bit-identical golden tests (additive `ScenarioActive`-gated systems; strict-f32 scoring, seeded hashing, stable iteration), ADR-0003 shared sim, ADR-0006 attention-scaled cost, ADR-0011 automation floor — this decision is its technical substrate.

## Links

- Feature spec: specs/00008-ship-ai/spec.md (and Clarifications)
- Research: specs/00008-ship-ai/research.md
- Project-plan epic: E011
- Related ADRs: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md), [ADR-0005](0005-single-node-first-build-the-seams.md), [ADR-0006](0006-interest-management-and-bandwidth-scaling.md), [ADR-0011](0011-automation-floor-human-ceiling.md)
- PRD capability: CAP-007 (EW mechanic + Sensor/Datalink module taxonomy)
- Recorded risk: STF-001 (off-screen combat cost unbounded in v1)
