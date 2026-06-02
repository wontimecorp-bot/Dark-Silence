---
adr_id: ADR-0014
status: accepted
date: 2026-06-02
tags: [networking, netcode, transport, renet, prediction, replication]
supersedes: []
superseded_by: ""
related_artifacts: [Epic E003, ADR-0002, ADR-0007, ADR-0013, ADR-0005, docs/game-design.md §12]
---

# ADR-0014: Netcode backend — transport-level (renet) with game-owned prediction and replication

## Status

Accepted. Refines the networking-library sub-decision of [ADR-0007](0007-technology-stack-and-workspace.md) (lightyear → renet); ADR-0007 otherwise stands.

## Context

Epic E003 implements the server-authoritative netcode. ADR-0007 (Technology Stack) named **lightyear** as the networking library — with **bevy_replicon** as a fallback — isolated behind the `protocol` crate plus thin adapters so the youngest dependency could be upgraded or swapped without redesign.

However, ADR-0002, ADR-0013, and the E003 spec make the determinism-critical machinery the game's *own* logic, run against the shared deterministic `sim` (the single source of truth on both client and server):

- client **prediction**,
- server **reconciliation** (input-replay),
- remote-entity **interpolation**, and
- **custom delta + quantized snapshots**.

We must now choose the backend for the first transport adapter behind the `protocol` crate. Plan-phase research compared transport-level libraries (renet, aeronet, raw UDP) against full netcode frameworks (lightyear, bevy_replicon). The forces: a full framework bundles prediction/rollback/replication/interpolation that would overlap or sit inert next to our own machinery and impose its own replication model, whereas a transport-level library supplies only connection, fragmentation, channels, and acking — exactly the layer we still need — while leaving the gameplay netcode to us.

## Decision Drivers

- Fit with the existing design: we already own prediction, reconciliation, interpolation, and delta-snapshots against the shared deterministic `sim` (ADR-0002, ADR-0013, ADR-0003).
- Contain third-party maturity/churn risk (the concern ADR-0007 flagged for the youngest dependency).
- Keep the backend swappable behind the `protocol` crate + `NetTransport` adapter seam (ADR-0005, ADR-0007).
- Avoid rebuilding low-level transport plumbing (connection, fragmentation, ack) for no gameplay gain.
- Clean integration with the deterministic fixed-step `sim` and with Bevy.

## Considered Options

### Option A: renet 2.0 transport-level + game-owned netcode (CHOSEN)

- **Pros**: Supplies exactly the missing layer — `renet_netcode` UDP transport, channels, fragmentation, acking, and an in-memory loopback channel — via `bevy_renet` integration. Leaves prediction/reconciliation/interpolation/delta-snapshots to us, matching ADR-0002 and the E003 spec. renet 2.0 is more mature and less churn-prone than lightyear 0.26, reducing the maturity risk ADR-0007 flagged. Sits cleanly behind the `protocol` crate + `NetTransport` adapter, so a wrong bet costs an adapter rewrite, not a redesign.
- **Cons**: We implement prediction/reconciliation/interpolation ourselves — more code than a batteries-included framework. renet/bevy_renet track Bevy 0.18, so versions must be pinned and upgrades gated on the bot-harness tests.

### Option B: lightyear full framework

- **Pros**: Batteries-included prediction, rollback, replication, interpolation, and interest management as one Bevy-integrated package.
- **Cons**: Its bundled prediction/rollback/replication/interpolation overlaps or sits inert next to our own determinism-critical machinery; it imposes its own replication model on top of the shared `sim`; at 0.26 it is younger and churnier than renet 2.0. The framework's value (the gameplay netcode) is the part we deliberately own.

### Option C: bevy_replicon

- **Pros**: Bevy-integrated, replication-first; the documented ADR-0007 fallback.
- **Cons**: Replication-first design still imposes a replication model, and it needs an external prediction layer bolted on — so it neither removes our work nor avoids model lock-in.

### Option D: raw UDP

- **Pros**: Maximum control over the wire.
- **Cons**: Rebuilds connection management, fragmentation, and acking from scratch for no gameplay gain. Already rejected in ADR-0007 (Option E).

## Decision Outcome

Chosen option: **Option A — renet 2.0 transport-level + game-owned netcode**. Use renet 2.0 (`renet_netcode` UDP transport plus `bevy_renet` integration) behind the `protocol` crate and a swappable `NetTransport` adapter, and build prediction, reconciliation, interpolation, and delta-snapshots ourselves as ADR-0002 and the E003 spec describe. This **refines ADR-0007's networking-library sub-decision** (lightyear → renet); ADR-0007 otherwise stands. lightyear is evaluated and NOT adopted because its bundled machinery overlaps or sits inert beside our own, it imposes its own replication model, and it is younger/churnier than renet 2.0 — so renet both fits the design and reduces the maturity risk ADR-0007 flagged. **bevy_replicon** and **aeronet** remain documented fallback transports behind the same adapter.

Channel mapping:

- handshake = reliable-ordered;
- `ClientInput` = unreliable + redundant recent inputs;
- `Snapshot` = unreliable + delta + ack;
- loopback via renet's in-memory channel.

## Consequences

### Positive

- Matches the spec and ADR-0002: we own the determinism-critical machinery (prediction, reconciliation, interpolation, delta-snapshots) rather than fighting a framework's model.
- renet 2.0 is more mature than lightyear 0.26 — lower maturity/churn risk.
- The `protocol` + `NetTransport` adapter seam keeps the backend swappable: a wrong bet costs an adapter rewrite, not a redesign.
- Clean fit with the deterministic fixed-step `sim` and with Bevy.

### Negative

- We implement prediction/reconciliation/interpolation ourselves — more code than a batteries-included framework.
- renet/bevy_renet track Bevy 0.18 — pin exact versions and gate upgrades on the bot-harness tests.

### Neutral

- The youngest dependency stays isolated behind the adapter, exactly as ADR-0007's rationale prescribed.

## Links

- Refines: [ADR-0007](0007-technology-stack-and-workspace.md) — networking-library choice (lightyear → renet).
- Builds on: [ADR-0002](0002-server-authoritative-netcode.md) — server-authoritative netcode design.
- Builds on: [ADR-0013](0013-thin-client-fixed-step-interpolated-rendering.md) — thin client / interpolated rendering.
- Builds on: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md) — shared deterministic `sim`.
- Builds on: [ADR-0005](0005-single-node-first-build-the-seams.md) — build-the-seams / isolate the youngest dependency.
- Epic E003 — server-authoritative netcode.
- docs/game-design.md §12
