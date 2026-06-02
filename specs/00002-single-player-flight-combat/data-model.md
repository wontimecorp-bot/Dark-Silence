# Data Model: Single-player Flight & Combat (E002)

**Scope**: In-memory `bevy_ecs` gameplay data model for the single-player first-playable slice. There is **NO database, NO persistence, NO migrations, and NO ER/SQL schema this epic** — nothing here is stored. This document models the ECS **components** and **resources** (fields, relationships, state transitions, invariants) that gameplay systems read and write at runtime.

**Serde note**: New gameplay components/resources derive `Serialize`/`Deserialize` (matching the E001 pattern, AD-002) so they reuse cleanly when replication (E003) and persistence (E004) arrive — but they are **not** serialized or stored this epic. The derive is a seam, not a feature.

**Principle II placement**: New gameplay state lives in the shared `sim` crate (Ship, Projectile, Target, Weapon, Tuning) so a future server runs the same code path; the Bevy client crate adds **only** render/input components (`RenderInterp`, camera, HUD) that never carry authoritative gameplay truth.

## Reused E001 `sim` Types (do NOT redefine)

These already exist in `crates/sim/` and are reused as-is. All derive `Component + Clone + Copy + Debug + PartialEq + Serialize + Deserialize`.

| Type | Definition | Reuse |
|------|------------|-------|
| `Position(Vec2)` | `crates/sim/src/components.rs` — world-space sector-relative position, sim units | Ship, Projectile, all Targets |
| `Velocity(Vec2)` | `crates/sim/src/components.rs` — linear velocity, sim units/sec | Ship, Projectile, Asteroid/Seeker targets |
| `BodyState { pos: Vec2, vel: Vec2 }` | `crates/sim/src/motion.rs` — kinematic state passed to `integrate`/`analytic`/`Physics::step` | Assembled from `Position`+`Velocity` for each fixed-step motion update |
| `integrate(state, accel, dt)` / `analytic(...)` | `crates/sim/src/motion.rs` — velocity-Verlet step / closed form | All Newtonian motion (FR-002, FR-008) |
| `Physics` trait + `RapierPhysics` | `crates/sim/src/physics.rs` — engine-agnostic 2D physics seam (ADR-0004) | Ship↔asteroid rigid-body bounce (FR-009) |

## New Components & Resources (entity table — primary artifact)

| Entity | Kind | Crate | Fields (name: type, constraints) | Relationships | State Transitions |
|--------|------|-------|----------------------------------|---------------|-------------------|
| **Ship** | Component (marker) | `sim` | *(zero-size marker; tags the player entity)* | 1:1 with `Weapon` (same entity); followed by `Camera`; referenced by `Projectile.owner` | alive → destroyed (ram asteroid at/above lethal threshold → despawn) |
| **Heading** | Component | `sim` | `0: f32` — facing angle in radians; aims weapon and (assist ON) the velocity trend | on Ship, Seeker; drives `Weapon` muzzle direction | wraps in `[-π, π]` or `[0, 2π)`; no lifecycle |
| **Health** | Component | `sim` | `0: f32`, CHECK `>= 0.0` | on Ship and every Target | `> 0` (alive) → `<= 0` (destroyed → entity despawns) |
| **FlightAssist** | Component | `sim` | `mode: AssistMode` (`On` \| `Off`) | on Ship | `On ↔ Off` (toggle; velocity is continuous across the switch — no snap) |
| **Projectile** | Component (marker) | `sim` | *(zero-size marker; tags a live shot)* | `owner: ProjectileOwner(Entity)` → Ship; carries `Position`/`Velocity`/`PrevPosition`/`Damage`/`Lifetime` | active → expired (hit OR `Lifetime <= 0` → despawn) |
| **Damage** | Component | `sim` | `0: f32`, CHECK `> 0.0` | on Projectile; subtracted from `Target.Health` on hit | none (constant for the shot's life) |
| **Lifetime** | Component | `sim` | `0: f32` — seconds remaining, CHECK `>= 0.0` | on Projectile | decremented each fixed step; `<= 0` → despawn |
| **PrevPosition** | Component | `sim` | `0: Vec2` — position at the previous fixed step (segment start for swept/CCD) | on Projectile; paired with `Position` (segment end) | updated to current `Position` at the start of each step |
| **ProjectileOwner** | Component | `sim` | `0: Entity` — owning Ship entity id | on Projectile → Ship | none (set at spawn; ignored if owner despawns) |
| **Target** | Component (marker) | `sim` | *(zero-size marker; tags a destructible)* | independent of Ship; struck by Projectile; Asteroid also collides with Ship | alive → destroyed (`Health <= 0` → despawn with feedback) |
| **TargetKind** | Component (enum) | `sim` | `Dummy` \| `Asteroid` \| `Seeker` | on Target; selects per-kind motion/collision behavior | none (kind is fixed at spawn) |
| **CollisionRadius** | Component | `sim` | `0: f32`, CHECK `> 0.0` — circle radius for swept hit + ram contact | on every Target (and conceptually the Ship hull) | none |
| **Weapon** | Component | `sim` | `cooldown: f32` (seconds until ready, `>= 0.0`); `fire_rate: f32` (shots/sec, `> 0.0`); `muzzle_speed: f32` (`> 0.0`) | 1:1 on the Ship entity; spawns Projectile entities | ready (`cooldown <= 0`) → cooling (just fired, `cooldown = 1/fire_rate`) → ready |
| **RenderInterp** | Component | `client` (Bevy) | `prev_pos: Vec2`; `curr_pos: Vec2`; `prev_heading: f32`; `curr_heading: f32` | client-only mirror of a sim entity's `Position`/`Heading`; drives the rendered `Transform` | each fixed step: `prev = curr`, `curr = <new sim state>`; render lerps by `alpha = accumulator/dt` |
| **Tuning** | Resource (singleton) | `sim` | `thrust_accel: f32`; `rotation_rate: f32` (rad/s); `strafe_accel: f32`; `max_speed: f32` (`> 0`); `muzzle_speed: f32` (`> 0`); `fire_rate: f32` (`> 0`); `lethal_ram_speed: f32` (`>= 0`); `assist_damping: f32` (`0.0..=1.0`) | global; read by every motion/weapon/collision system; grounded-but-scaled (ADR-0012), in-engine tunable | none (mutated only by the tuning UI) |
| **FixedStepClock** | Resource (singleton) | `client` (Bevy) | `dt: f32` (fixed logical step); `accumulator: f32` (`>= 0`); `alpha: f32` (`0.0..=1.0`, render blend factor) | global; gates the fixed-step sim loop and feeds `RenderInterp` | accumulate frame time → run N integer sim steps → carry remainder; clamp accumulator to avoid spiral-of-death |

### Enum value sets

| Enum | Variants | Notes |
|------|----------|-------|
| `AssistMode` | `On`, `Off` | `On` = drift-damped (velocity trends toward `Heading` via `Tuning.assist_damping`); `Off` = decoupled full momentum (FR-003). Default `On` (research recommendation). |
| `TargetKind` | `Dummy`, `Asteroid`, `Seeker` | `Dummy` = static (no `Velocity` motion); `Asteroid` = constant-velocity drift + Ship rigid-body collision (FR-008/FR-009); `Seeker` = thrusts toward Ship each step (FR-012). |

### Entity → component composition (which components co-occur)

| Logical entity | Required components | Notes |
|----------------|---------------------|-------|
| Player ship | `Ship`, `Position`, `Velocity`, `Heading`, `Health`, `FlightAssist`, `Weapon`, `CollisionRadius` (+ client `RenderInterp`) | Exactly **one** per session (FR-014). Weapon is on the same entity (1:1). |
| Projectile | `Projectile`, `Position`, `Velocity`, `PrevPosition`, `Damage`, `Lifetime`, `ProjectileOwner` (+ client `RenderInterp`) | Spawned by Weapon on fire; 0..N alive. |
| Dummy target | `Target`, `TargetKind::Dummy`, `Position`, `Health`, `CollisionRadius` | No `Velocity` (static). |
| Asteroid target | `Target`, `TargetKind::Asteroid`, `Position`, `Velocity`, `Health`, `CollisionRadius` (+ client `RenderInterp`) | Drifts at constant velocity; collides with Ship. |
| Seeker target | `Target`, `TargetKind::Seeker`, `Position`, `Velocity`, `Heading`, `Health`, `CollisionRadius` (+ client `RenderInterp`) | Thrusts toward Ship each step. |
| Camera (client) | Bevy `Camera3d` + follow/zoom marker | Follows the Ship; renders the 2D plane in 3D (FR-001). Not a `sim` entity. |

## Relationships

| From | To | Cardinality | Mechanism | Requirement |
|------|----|-------------|-----------|-------------|
| Ship | Weapon | 1:1 | Weapon component lives on the Ship entity (forward-mounted) | FR-005 |
| Projectile | Ship (owner) | N:1 | `ProjectileOwner(Entity)` reference; tolerates a despawned owner | FR-005 |
| Projectile | Target | N:1 per hit | Swept/CCD overlap test, resolved at time-of-impact | FR-006, FR-007 |
| Ship | Asteroid | M:N contact | `Physics` trait rigid-body bounce + ram-damage check | FR-009, FR-010 |
| Seeker | Ship | N:1 | Reads Ship `Position` to compute thrust direction each step | FR-012 |
| Camera (client) | Ship | 1:1 follow | Client transform follows Ship `Position`; supports zoom | FR-001 |
| RenderInterp (client) | sim entity | 1:1 mirror | Snapshots `Position`/`Heading`; render interpolates `Transform` | FR-004 |
| Tuning | all systems | 1:N read | Global resource read by motion/weapon/collision systems | FR-015 |
| Targets | each other / Ship | independent | No inter-target relationships (no formations this slice) | — |

## State Machines

Lifecycles with conditional branches are expanded here; trivial ones stay inline in the entity table.

### FlightAssist mode (Ship)

```
On  ──toggle key──▶  Off
Off ──toggle key──▶  On
```
- `On`: each step, velocity is nudged toward `Heading` by `Tuning.assist_damping` (drift-damped; "fly where you point").
- `Off`: heading and velocity are independent (decoupled full momentum).
- **Continuity invariant**: toggling does NOT modify `Velocity` on the transition frame — no instantaneous velocity snap (spec Edge Case; SC-002). The change only alters how future steps adjust velocity.

### Projectile lifecycle

```
spawn ──▶ Active ──hit registered (swept overlap)──▶ Despawned (apply Damage to Target)
              │
              └──Lifetime <= 0 (each step: Lifetime -= dt)──▶ Despawned (no effect)
```
- Each fixed step: `PrevPosition = Position`; advance `Position`/`Velocity` via `integrate`; `Lifetime -= dt`.
- Swept test uses the `PrevPosition → Position` segment vs each `Target` circle (`CollisionRadius`) so a fast shot cannot tunnel (FR-006).
- A Projectile whose target despawns mid-flight simply continues and later expires harmlessly (spec Edge Case).

### Target lifecycle

```
spawn ──▶ Alive ──Damage applied, Health -= Damage──▶ Alive (Health > 0)
                                                  └──▶ Destroyed (Health <= 0) ──▶ Despawn + hit/destroy feedback
```
- Multiple Projectile hits on one frame accumulate damage; the despawn happens **once** even if overkilled — no double-destroy artifact (spec Edge Case; FR-007).

### Ship lifecycle (ram)

```
Alive ──asteroid contact, closing speed < lethal_ram_speed──▶ Alive (rigid-body bounce, momentum transfer)
      └──asteroid contact, closing speed >= lethal_ram_speed──▶ Destroyed (despawn / session-end feedback)
```
- Below threshold: `Physics`-driven bounce only (FR-009). At/above `Tuning.lethal_ram_speed`: Ship destroyed (FR-010, SC-005).

### Weapon firing (cooldown gate)

```
Ready (cooldown <= 0) ──fire input──▶ spawn Projectile, cooldown = 1.0 / fire_rate ──▶ Cooling
Cooling ──each step: cooldown -= dt; when cooldown <= 0──▶ Ready
```
- Fire input while `Cooling` is ignored (cooldown enforced — invariant below).

## Invariants & Validation Rules

These are runtime invariants checked/maintained by sim systems (not DB constraints — there is no DB).

| ID | Invariant | Where enforced | Requirement |
|----|-----------|----------------|-------------|
| INV-01 | `Health >= 0.0` (clamp at 0; 0 triggers destruction, never negative) | damage-application system | FR-007 |
| INV-02 | Linear speed clamped: `Velocity.length() <= Tuning.max_speed` after each step | motion system, post-`integrate` | FR-002, SC-001 |
| INV-03 | Weapon fires only when `cooldown <= 0`; firing resets `cooldown = 1/fire_rate` | weapon system | FR-005 |
| INV-04 | `Damage > 0.0` | projectile spawn (construction guard) | FR-007 |
| INV-05 | `CollisionRadius > 0.0` | target/ship spawn | FR-006, FR-009 |
| INV-06 | `Lifetime >= 0.0`; Projectile despawns at `<= 0` | projectile-lifetime system | FR-006 |
| INV-07 | FlightAssist toggle never mutates `Velocity` on the transition frame (no snap) | input/assist system | SC-002 |
| INV-08 | Swept hit uses `PrevPosition → Position` segment so fast shots never tunnel; a hit applies damage exactly once per projectile | collision system | FR-006, FR-007 |
| INV-09 | A destroyed Target despawns exactly once even under multi-hit/overkill on one frame | destruction system | FR-007 (edge case) |
| INV-10 | `Tuning.assist_damping ∈ [0.0, 1.0]`; `fire_rate`, `muzzle_speed`, `max_speed > 0` | Tuning init / tuning-UI validation | FR-015 |
| INV-11 | Fixed-step accumulator is clamped (bounded catch-up) — no spiral-of-death; rendering interpolates by `alpha ∈ [0,1]` | fixed-step driver (client) | FR-004 |
| INV-12 | `sim` motion equivalence (`integrate` == `analytic`, E001 keystone) is reused unchanged — this slice does not modify `sim::motion` | covered-by-E001 | Principle II |

## Out of Scope (explicit non-data this epic)

| Excluded | Reason |
|----------|--------|
| SQL DDL / ER diagram / migrations | No database or persistence this epic (FR-014); state is in-memory ECS only. |
| Typed-damage pipeline (channels × defense layers), shields/armor/hull | Deferred to the damage epic (CAP-004); targets use a single `Health` scalar here. |
| Ship fitting / modules / multiple weapons | One fixed `Weapon`; fitting is CAP-003. |
| Destructible hull cell-grid / severing | Targets destroyed wholesale via `Health <= 0`. |
| Multiple ship classes, capital command, turrets | One fighter only. |
| Replication/persistence serialization | Serde derives present as a seam (E003/E004); not exercised this epic. |

## Data Model Summary (for plan.md)

Compact entity overview suitable for the plan's `## Data Model Summary` table:

| Entity | Key fields | Relationships | Notes |
|--------|-----------|---------------|-------|
| Ship (player) | `Position`, `Velocity`, `Heading(f32)`, `Health(f32 ≥0)`, `FlightAssist{On\|Off}`, `CollisionRadius` | has 1 `Weapon`; followed by Camera; owner of Projectiles | `sim` crate; one per session; alive→destroyed on lethal ram |
| Weapon | `cooldown:f32`, `fire_rate:f32 >0`, `muzzle_speed:f32 >0` | 1:1 on Ship entity; spawns Projectiles | `sim`; cooldown gate (INV-03); forward-mounted |
| Projectile | `Position`, `Velocity`, `PrevPosition(Vec2)`, `Damage(f32 >0)`, `Lifetime(f32 ≥0)`, `ProjectileOwner(Entity)` | N:1 → owning Ship; hits Targets (swept) | `sim`; despawns on hit or `Lifetime ≤0`; CCD via Prev→cur segment |
| Target | `TargetKind{Dummy\|Asteroid\|Seeker}`, `Health(f32 ≥0)`, `CollisionRadius(f32 >0)`, (+`Velocity`/`Heading` per kind) | independent; struck by Projectiles; Asteroid collides w/ Ship | `sim`; alive→destroyed on `Health ≤0` with feedback |
| RenderInterp (client) | `prev_pos`, `curr_pos:Vec2`, `prev_heading`, `curr_heading:f32` | 1:1 mirror of a sim entity | Bevy client only; lerps render `Transform` by `alpha` |
| Tuning (resource) | `thrust_accel`, `rotation_rate`, `strafe_accel`, `max_speed`, `muzzle_speed`, `fire_rate`, `lethal_ram_speed`, `assist_damping` | global; read by all sim systems | `sim` singleton; grounded-but-scaled (ADR-0012), in-engine tunable |

**Reused (E001, do not redefine)**: `Position(Vec2)`, `Velocity(Vec2)`, `BodyState{pos,vel}`, `integrate`/`analytic`, `Physics`+`RapierPhysics`.
