---
adr_id: ADR-0013
status: accepted
date: 2026-06-01
tags: [client, bevy, rendering, fixed-step, simulation]
supersedes: []
superseded_by: ""
related_artifacts: [Epic E002, Epic E003, ADR-0002, ADR-0003, ADR-0004, Principle II, Principle VII]
---

# ADR-0013: Thin Bevy client over the shared `sim`: fixed-step simulation with interpolated rendering

## Status

Accepted.

## Context

Epic E002 introduces the project's first Bevy client (a single-player flight & combat vertical slice). We must decide (a) how the Bevy client relates to the shared `sim` crate and to the physics engine, and (b) how to keep motion smooth across variable frame rates without compromising the deterministic fixed-step simulation (Principle II; ADR-0003) or the engine-agnostic physics seam (ADR-0004).

This is foundational and inherited by every future client epic (E003 networked client onward), so it is a project-level decision, not a feature-local one.

## Decision Drivers

- Preserve the deterministic fixed-step simulation invariant (Principle II; ADR-0003).
- Preserve the engine-agnostic physics seam — the `sim::Physics` trait (ADR-0004).
- One gameplay code path, reusable by the future headless server (Principle II).
- Frame-rate-independent motion / smooth feel across variable render rates.
- Clean path to the E003 networked client (server snapshots + prediction/reconciliation, ADR-0002) without restructuring rendering or input.
- Playable every phase (Principle VII).

## Considered Options

### Option A: Step the sim in Bevy's variable `Update` using frame-delta dt

- **Pros**: Simplest wiring; no fixed-step scheduling or interpolation machinery.
- **Cons**: Non-deterministic; breaks the fixed-step invariant (Principle II; ADR-0003). Rejected.

### Option B: Adopt `bevy_rapier2d` as the combined ECS + physics authority

- **Pros**: Mature, integrated physics + ECS out of the box; less glue code.
- **Cons**: Couples gameplay to engine/Bevy types, violates ADR-0004 and Principle II, and blocks headless server reuse. Rejected.

### Option C: Render raw sim state without interpolation

- **Pros**: No interpolation buffer or overstep bookkeeping.
- **Cons**: Visible stutter whenever render rate ≠ sim rate. Rejected.

### Option D: Thin Bevy render + input shell over the shared `sim`, fixed-step sim with interpolated rendering

- **Pros**: Gameplay state and logic (motion, collision, damage, weapon firing, simple AI) live in the shared `sim` crate as headless `bevy_ecs` systems plus pure functions; the client only schedules them, holding NO gameplay logic. The simulation advances at a fixed timestep (Bevy `FixedUpdate` / `Time<Fixed>`), decoupled from the variable render loop; rendered `Transform`s are interpolated between the two most recent simulation states using the fixed-step overstep fraction for frame-rate-independent motion. Physics flows through the existing `sim::Physics` trait (Rapier2D-backed, per ADR-0004); the client does NOT use the `bevy_rapier2d` plugin, and no Rapier or Bevy-app types appear in gameplay logic.
- **Cons**: More integration glue than adopting `bevy_rapier2d` wholesale.

## Decision Outcome

Chosen option: **Option D: Thin Bevy render + input shell over the shared `sim`, fixed-step sim with interpolated rendering** — the Bevy client is a thin render + input shell containing NO gameplay logic; all gameplay state and logic live in the shared `sim` crate as headless `bevy_ecs` systems plus pure functions that the client merely schedules. The sim advances at a fixed timestep (`FixedUpdate` / `Time<Fixed>`) decoupled from the variable render loop, and rendered `Transform`s are interpolated between the two most recent simulation states using the fixed-step overstep fraction, yielding frame-rate-independent motion. Physics flows through the existing `sim::Physics` trait (Rapier2D-backed, ADR-0004); the client does NOT use the `bevy_rapier2d` plugin, and no Rapier or Bevy-app types appear in gameplay logic. The extra integration glue is accepted to preserve the `Physics` seam (ADR-0004) and headless server reuse.

## Consequences

### Positive

- Frame-rate-independent feel; smooth motion regardless of render rate.
- Protects the integrator↔analytic invariant (ADR-0003).
- The client networkizes cleanly in E003 — the local fixed-step driver is replaced by server snapshots + client-side prediction/reconciliation (ADR-0002) without restructuring rendering or input.
- Preserves Principle II (one gameplay code path, reused by the future server).

### Negative

- More integration glue than adopting `bevy_rapier2d` wholesale — accepted to preserve the `Physics` seam (ADR-0004) and headless server reuse.

## Links

- Epic E002 (first Bevy client — single-player flight & combat vertical slice)
- Epic E003 (networked client)
- Related ADR: [ADR-0002](0002-server-authoritative-netcode.md) — client prediction/interpolation
- Related ADR: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md) — shared sim + fixed-step velocity-Verlet
- Related ADR: [ADR-0004](0004-2d-physics-behind-trait.md) — Rapier2D behind the `Physics` trait
- Principle II (Shared Deterministic Sim Core)
- Principle VII (Playable Every Phase)
