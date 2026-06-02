---
adr_id: ADR-0007
status: accepted
date: 2026-06-01
tags: [tech-stack, tooling, persistence, networking-library]
supersedes: []
superseded_by: ""
related_artifacts: [ADR-0003, ADR-0004, ADR-0006, project-instructions.md Technology Stack, docs/game-design.md §12]
---

# ADR-0007: Technology stack and Cargo workspace

## Status

Accepted.

## Context

We need a stack for a Bevy client and a custom authoritative server that share simulation logic, with strong performance, durable persistence, and without prematurely building a general distributed-simulation framework. Developer preference is Rust.

## Decision Drivers

- Performance and memory safety.
- Share one simulation crate across client and server.
- Solo maintainability.
- Avoid premature generalization.
- Contain third-party maturity risk.

## Considered Options

### Option A: Engine/stack — Unity / Godot / Unreal

- **Pros**: Mature game engines with broad asset and tooling ecosystems; viable for the gameplay.
- **Cons**: Do not give the developer a single Rust codebase nor a clean shared client/server simulation crate; the developer chose Rust for performance plus a shared client/server sim.

### Option B: Engine/stack — Rust + Bevy client + custom Rust authoritative server + `bevy_ecs` in the shared sim (CHOSEN)

- **Pros**: One Rust codebase; `bevy_ecs` usable as a pure ECS library inside the shared sim so client and server run identical logic; strong performance and memory safety.
- **Cons**: Rust learning curve; thinner game-asset and tooling ecosystem than the established engines.

### Option C: Netcode — lightyear (CHOSEN)

- **Pros**: Prediction, reconciliation, interpolation, and interest management delivered as one Bevy-integrated package.
- **Cons**: Young library with churning APIs.

### Option D: Netcode — bevy_replicon (fallback)

- **Pros**: Bevy-integrated replication kept as a fallback path.
- **Cons**: Weaker prediction than lightyear.

### Option E: Netcode — raw renet / aeronet

- **Pros**: Maximum control over the transport.
- **Cons**: Too low-level; months of infrastructure work before any gameplay netcode exists.

### Option F: Storage — PostgreSQL + Redis (CHOSEN)

- **Pros**: Proven, durable persistence; decoupled from the authority model.
- **Cons**: Two systems to operate rather than a single integrated product.

### Option G: Storage — SpacetimeDB

- **Pros**: Integrated database plus simulation runtime.
- **Cons**: Young; couples the entire authority model to one product. Rejected.

### Option H: Project structure — Cargo workspace of focused crates (CHOSEN)

- **Pros**: Focused crates (`sim`, `protocol`, `server`, `client`, `transit`, `persistence`, `tools`) with clean boundaries; tailor-made for this game; reusability extracted on the rule-of-three rather than designed up front (cf. SpatialOS as a cautionary "platform-first" tale).
- **Cons**: Requires discipline to keep crate boundaries clean rather than letting concerns leak across them.

### Option I: Serialization and math — `bitcode` snapshots + `glam` (CHOSEN)

- **Pros**: `bitcode` gives bit-packed snapshots well suited to bandwidth-bounded replication; `glam` provides standard, fast math.
- **Cons**: Bit-packed encoding is less directly human-inspectable than text formats.

## Decision Outcome

Chosen option: **Rust 2021 Cargo workspace** — Bevy client; custom authoritative server; lightyear for networking (isolated behind `protocol` plus thin adapters so it can be upgraded or swapped for bevy_replicon); Rapier2D physics (see ADR-0004); PostgreSQL + Redis persistence; bitcode snapshots; tailor-made, not a framework. This stack maximizes performance and memory safety, shares one simulation crate across client and server, stays solo-maintainable, avoids premature generalization, and contains third-party maturity risk by isolating the youngest dependency behind adapters.

## Consequences

### Positive

- Performance.
- Memory safety.
- Code reuse across client and server.
- Contained dependencies.
- Fast iteration.

### Negative

- lightyear is young with churning APIs — mitigate by pinning a version, isolating it behind adapters, and keeping bevy_replicon as a fallback.
- Rust learning curve and a thinner game-asset/tooling ecosystem.

### Neutral

- Source lives in a Cargo workspace under `crates/<name>/src` (per project-instructions.md), not a single top-level `/src`.

## Links

- Networking-library choice refined by [ADR-0014](0014-netcode-transport-renet-own-netcode.md) (lightyear → renet).
- Related ADR: [ADR-0003](0003-shared-sim-crate-and-fixed-step-integration.md)
- Related ADR: [ADR-0004](0004-2d-physics-behind-trait.md)
- Related ADR: ADR-0006
- project-instructions.md Technology Stack
- docs/game-design.md §12
