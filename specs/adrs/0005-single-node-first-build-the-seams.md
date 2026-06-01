---
adr_id: ADR-0005
status: accepted
date: 2026-06-01
tags: [distribution, scalability, strategy]
supersedes: []
superseded_by: ""
related_artifacts: [ADR-0001, ADR-0006, docs/game-design.md §3, docs/game-design.md §12]
---

# ADR-0005: Single-node first; build the seams, defer multi-node meshing

## Status

Accepted.

## Context

The product goal is a seamless world, which at large scale implies multi-node "server meshing" — a studio-scale distributed-systems effort. The team is a solo developer with uncertain demand. A decision is needed now because the chosen server topology shapes the cost of every subsequent feature: a distributed design taxes all later work, while a single-node design risks painting the seamless-world goal into a corner if the meshing path is not deliberately kept open.

## Decision Drivers

- Distribution buys nothing until one machine is exceeded.
- The distributed-systems tax is paid on every subsequent feature.
- Ship-early / verify-fun.
- Reversibility — keep meshing reachable cheaply.

## Considered Options

### Option A: Full dynamic server meshing from day one

- **Pros**: Ultimate seamless scale.
- **Cons**: Studio-scale; forces solving authority handoff, ghosting, cross-node hit resolution, clock sync, resharding, partial failure, and distributed debugging BEFORE any gameplay exists; cf. SpatialOS abandonment. Rejected.

### Option B: Hard zones / sharded instances

- **Pros**: Simple.
- **Cons**: Not seamless. Rejected.

### Option C: Single-node with seamless-ready seams

- **Pros**: Runs single-node, but builds "seams" that don't assume single-node — per-entity authority flag, serializable entity state, explicit Area-of-Interest, and handoff hooks (which today move entities between in-process bubbles and the tiers). Switch to multi-node only on a measured trigger. Ships early, low-regret, keeps meshing reachable.
- **Cons**: ~5% up-front design tax for the seams; multi-node remains a large future effort if ever undertaken.

## Decision Outcome

Chosen option: **Option C: Single-node with seamless-ready seams** — single-node-first with seamless-ready seams. The trigger to distribute is a vertically-maxed node consistently blowing its tick budget on total load, or a single bubble exceeding an acceptable time-dilation floor. The promote/demote machinery (ADR-0001) is a dry-run of entity handoff. This avoids paying the studio-scale distributed-systems tax (Option A) and the loss of seamlessness (Option B) while keeping the meshing path reachable cheaply.

## Consequences

### Positive

- Ships early and is low-regret; meshing stays reachable.
- Hosting stays ~one machine for a long time.

### Negative

- ~5% up-front design tax for the seams.
- Multi-node remains a large future effort if ever undertaken.

### Neutral

- Prior art validates the seam pattern: HLA/DIS "Data Distribution Management" = AOI; HPC domain decomposition + MPI halo exchange = boundary ghosting.

## Links

- Related: [ADR-0001](0001-tiered-simulation-architecture.md)
- Related: ADR-0006
- docs/game-design.md §3
- docs/game-design.md §12
