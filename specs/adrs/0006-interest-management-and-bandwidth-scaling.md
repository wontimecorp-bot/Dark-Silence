---
adr_id: ADR-0006
status: accepted
date: 2026-06-01
tags: [networking, scalability, interest-management, sensors]
supersedes: []
superseded_by: ""
related_artifacts: [PRD CAP-007, ADR-0001, ADR-0002, ADR-0005, docs/game-design.md §3, docs/game-design.md §6, docs/game-design.md §12]
---

# ADR-0006: Interest management and bandwidth-first scaling

## Status

Accepted.

## Context

For this game, the scaling wall is replication bandwidth and area-of-interest, not physics CPU. Naive replication is O(n²) and saturates client downlinks long before server CPU becomes the constraint. The world must support large co-located battles, multi-scale strategic awareness, and graceful degradation under overload. We need a replication strategy that treats bandwidth and area-of-interest as the primary budget to optimize against, and that unifies cleanly with the in-fiction sensors and electronic-warfare (EW) gameplay layer.

## Decision Drivers

- Bandwidth and area-of-interest (AOI) are the true bottleneck, not physics CPU — optimize against the bandwidth budget first.
- Deliver a "feels alive" experience at modest scale rather than chasing theoretical maximum player counts.
- Degrade megabattles gracefully instead of dropping players when load spikes.
- Unify the netcode interest system with the sensors/EW gameplay layer so they are one system rather than two.

## Considered Options

### Option A: Brute-force replicate all entities to all clients

- **Pros**: Trivial to implement; no interest-management complexity; every client has perfect, complete world state.
- **Cons**: O(n²) replication cost; saturates client downlinks well before server CPU is exhausted; economically infeasible past roughly 100 players. Rejected.

### Option B: Interest-managed, quantized, delta-compressed, budget-bounded replication (CHOSEN)

- **Pros**: Bandwidth scales because each client only receives what it is entitled to and only what changed; supports large co-located battles and multi-scale awareness; the netcode interest system and the gameplay sensor system collapse into a single system built once; overload is absorbed gracefully rather than catastrophically.
- **Cons**: Priority and budget tuning is non-trivial; the interaction between per-bubble time dilation and client-side prediction needs careful handling.

This option combines: Area-of-Interest filtering; position/field quantization; per-client delta compression against the last-acked baseline; a per-client bandwidth budget governed by a priority function (nearer, recently-changed, and threatening entities rank higher); tier-aware partitioning (Tier 0 spatial with adaptive authority regions plus a fine AOI/broadphase grid; Tier 1 by time-bucket/entity-hash; Tier 2 by entity/ID hash); and per-bubble time dilation for overload (slow the tick rate while keeping logical dt). Sensors and EW are the in-fiction surface of this same interest-management system.

#### Overload handling sub-decision

- **Drop players (rejected)**: Sheds load deterministically but breaks the player experience and contradicts the driver to degrade gracefully.
- **Per-bubble time dilation (chosen)**: Slows the tick rate of an overloaded bubble while preserving logical dt, keeping all players present and the world coherent under load.

## Decision Outcome

Chosen option: **Option B — Interest-managed, quantized, delta-compressed replication bounded by a per-client budget plus priority function** — because bandwidth/AOI is the genuine bottleneck and this design optimizes directly against it. Replication is tier-aware partitioned, applies per-bubble time dilation under load, and is driven by sensors/EW: what each client is entitled to receive is determined by the sensor/EW model, which also gates Tier-1 promotion. The interest system and the gameplay sensor system are the same system, built once.

## Consequences

### Positive

- Bandwidth scales with what clients can actually perceive rather than with total entity count.
- Megabattles degrade gracefully via per-bubble time dilation instead of dropping players.
- The netcode interest system and the gameplay sensor/EW system are one system — build once, maintain once.

### Negative

- Priority and budget tuning is non-trivial and will require iteration.
- The interaction between per-bubble time dilation and client-side prediction needs care; ship the current dilation factor in each snapshot header so clients can reconcile.

### Neutral

- Entity-hashing the real-time physics world is explicitly avoided because it destroys spatial locality; hashing is reserved for the coarser Tier 1 (time-bucket/entity-hash) and Tier 2 (entity/ID hash) partitions.

## Links

- PRD CAP-007 (information warfare)
- Related: ADR-0001, ADR-0002, ADR-0005
- docs/game-design.md §3, §6, §12
