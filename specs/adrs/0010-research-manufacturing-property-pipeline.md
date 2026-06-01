---
adr_id: ADR-0010
status: accepted
date: 2026-06-01
tags: [research, manufacturing, emergent-systems, content]
supersedes: []
superseded_by: ""
related_artifacts: [PRD CAP-006, PRD CAP-010, PRD CAP-012, ADR-0008, ADR-0001]
---

# ADR-0010: Shared property pipeline — empirical research → generative manufacturing → emergent tech

## Status

Accepted.

## Context

The game wants genuinely "sciencey," non-repetitive research and a manufacturing system that expresses ALL the variations research makes possible, producing emergent/weird tech. For a solo dev, content must scale far beyond hand-authoring. A fixed, authored content set cannot deliver both the non-repetition and the breadth the design calls for, so a decision is needed on how research and manufacturing relate and how content is generated rather than enumerated.

## Decision Drivers

- Non-repetition: research and its outputs must not feel like the same puzzle each time.
- Emergent (systemic) content over authored content.
- Tight research↔manufacturing integration.
- Believable + learnable for the player.
- Bounded/balanced so emergence stays controllable.

## Considered Options

### Option A: Fixed tech tree + recipe tables + puzzle-based research

- **Pros**: Simple to build; fully authorable; predictable balance.
- **Cons**: Finite content; repetitive; no emergence. Rejected as the core.

### Option B: One SHARED property pipeline

Research = empirical discovery: procedurally-generated phenomena/materials have hidden properties + interactions that you discover by experiment (hypothesize → experiment with measurement uncertainty → infer → apply). Manufacturing = generative composition: modules are parametric templates whose stats + side-effects are COMPUTED from the researched materials/components composed into them. "Weird"/emergent tech arises from cross-domain property coupling (materials/damage-channels/sensors/comms/physics share one model). Research and manufacturing are two ends of the same property model, so manufacturing's variation-space = research's discovery-space automatically.

- **Pros**: Emergent, player-discovered content; effectively infinite breadth from authoring systems and rules rather than items; research and manufacturing stay coupled by construction.
- **Cons**: Balance risk from cross-domain coupling; significant scope; incomplete-knowledge failure modes.

## Decision Outcome

Chosen option: **Option B — One SHARED property pipeline** — Authoring SYSTEMS & RULES (not items) yields effectively infinite, player-discovered content at solo-feasible authoring cost; a few iconic authored phenomena (lore tech) sit atop the emergent long tail. Pacing is diegetic: throughput = samples × labs × iteration (no daily cap). Phase it: simple "analyze → properties reveal with uncertainty" first, then deep experimental design later.

## Consequences

### Positive

- Emergent, player-discovered content at solo-feasible authoring cost.
- Unifies with exploration (samples), espionage/hacking (stealable research data), exotic gear (applied discoveries with liabilities), and the economy.

### Negative

- Balance risk (accidentally OP or merely-noisy combos). Guardrails: instability/risk throttle, counterability + wide-shallow power band + skill>gear, rarity gating, strong lab-notebook feedback.
- Significant scope (later pillar).

### Neutral

- Incomplete knowledge = unknown failure modes (ties to capability+liability).

## Links

- PRD CAP-006, PRD CAP-010, PRD CAP-012
- Related ADRs: [ADR-0008](0008-unified-domain-data-model.md), [ADR-0001](0001-tiered-simulation-architecture.md)
- docs/game-design.md §7
