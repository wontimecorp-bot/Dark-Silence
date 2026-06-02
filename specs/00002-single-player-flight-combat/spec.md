---
feature_branch: "00002-single-player-flight-combat"
created: "2026-06-01"
input: "E002 — single-player flight & combat vertical slice (first playable)"
spec_type: "product"
spec_maturity: "draft"
epic_id: "E002"
epic_sources: "{PRD:CAP-001}"
---

# Feature Specification: Single-player Flight & Combat

**Feature Branch**: `00002-single-player-flight-combat`  
**Created**: 2026-06-01  
**Status**: Draft  
**Spec Type**: product  
**Spec Maturity**: draft  
**Epic ID**: E002  
**Epic Sources**: {PRD:CAP-001}  
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

The entire game rests on one unproven bet: that weighty, skill-based Newtonian ship combat is *fun moment-to-moment* (PRD principle-zero gate, CAP-001). Until a person can actually fly a ship and shoot something, every downstream investment — networking, persistence, economy, war — is a gamble on unvalidated feel. This slice exists to answer "does momentum flight + shooting feel good?" with a hands-on, single-player playable before any multiplayer scaffolding is built. If the feel is wrong, it is far cheaper to discover and fix it here than after the world is built on top of it.

## Scope *(mandatory)*

### Included

- A standalone single-player window: render the 2D gameplay plane in 3D with a tinted composed-primitive ship and a top-down, zoomable camera.
- Newtonian piloting (thrust, rotate, strafe) driven by the E001 `sim` crate, with momentum/coasting and a flight-assist toggle (assist ON = drift-damped; OFF = decoupled full-momentum).
- Frame-rate-independent feel: a fixed-step simulation decoupled from interpolated rendering, smooth at 60+ FPS.
- A fixed forward-mounted weapon firing swept (continuous-collision) projectiles aimed by the ship's heading.
- Targets with health that take projectile damage and are destroyed: static dummies, drifting (constant-velocity) asteroids, and a single seeking-AI chaser.
- Physical ship↔asteroid collisions: rigid-body bounce (momentum transfer) plus ram damage above a velocity threshold.
- A minimal diegetic HUD (speed/throttle, flight-assist mode, aiming reticle, hit/destroy feedback).
- Keyboard-only controls.

### Excluded

- Networking, multiplayer, and server authority — that is E003; this slice is single-player to isolate feel.
- Persistence / save-load — not needed to validate feel (deferred to the persistence epic).
- Ship fitting, modules, and multiple weapons — one fixed weapon only; fitting is a later epic (CAP-003).
- The typed-damage pipeline (damage channels × defense layers) and shields/armor/hull layering — targets use simple health here; the full pipeline is the damage epic (CAP-004).
- Multiple ship classes and capital-ship command/turret controls — one fighter only.
- Destructible-hull cell-grid and severing — targets are destroyed wholesale; coarse hull destruction is a later epic.
- Gamepad and mouse input — keyboard only this slice.
- External 3D models / authored art — composed Bevy primitives only.
- Sensors, EW, and information-warfare UI — minimal flight HUD only.
- Broader combat AI — exactly one dumb seeking target, not a fightable opponent roster.

### Edge Cases & Boundaries

- Projectile fired at extreme relative closing speed against a thin/small target MUST still register a hit (no tunneling). The swept segment-vs-circle test guarantees this across the full projectile-velocity range by construction; the no-tunneling test is parameterised off the `Tuning` resource — it must hold for any projectile speed up to `Tuning.muzzle_speed` (the projectile-speed cap) and any target `CollisionRadius` at or above the inflated minimum hit-radius (the velocity-cap + inflated-hitbox robustness margins from Plan §Risk Mitigation). Concrete cap/min-radius values are in-engine tunable (FR-015), not fixed in the spec.
- Grazing/tangent hits resolve to a defined hit-or-miss outcome, not an undefined flicker. Tie-break rule: the swept segment-vs-circle test counts a hit when the segment's closest-approach distance to the target centre is `<= CollisionRadius` (tangent, i.e. distance exactly `== CollisionRadius`, counts as a hit). The test is a pure deterministic geometric computation with no per-frame randomness, so the same geometry always yields the same outcome (no flicker).
- Two projectiles striking the same target on one frame apply damage correctly without double-destroy artifacts.
- A target destroyed while a projectile is mid-flight toward it leaves the projectile to resolve harmlessly.
- Flight-assist toggled mid-maneuver transitions smoothly — no instantaneous velocity snap.
- Ramming below the lethal threshold bounces without destroying the ship; at/above it, the ship is destroyed.
- Frame-rate spikes/drops keep physics stable (fixed-step, no spiral of death); rendering interpolates between steps.
- At very high ship speed the camera/zoom keeps the ship on-screen; sector-relative coordinates avoid `f32` precision loss (no floating-origin needed at slice scale).

## User Scenarios & Testing *(mandatory for product specs only)*

### User Story 1 - Fly a ship with Newtonian momentum (Priority: P1)

The player controls a single fighter on the 2D plane. Thrust accelerates the ship; releasing thrust leaves it coasting on momentum; rotating changes heading; strafe nudges sideways. A flight-assist toggle switches between an accessible drift-damped mode (the ship tends to fly where it points) and a decoupled mode that preserves full momentum (the ship can face one way while drifting another). Motion is smooth and weighty regardless of frame rate, and a top-down camera follows the ship and can zoom.

**Why this priority**: Core value proposition (CAP-001) — without believable, good-feeling inertial flight there is no game to validate; everything else builds on it.

**Independent Test**: Launch the window, fly around with assist ON then OFF, and confirm the ship carries momentum, coasts when input stops, and feels smooth at varying frame rates.

**Acceptance Scenarios**:

1. **Given** the ship is at rest, **When** the player applies forward thrust and releases it, **Then** the ship accelerates and then coasts at constant velocity without further input.
2. **Given** flight-assist is OFF, **When** the player rotates the ship while moving, **Then** the heading changes but the velocity vector is unchanged (the ship drifts in its original direction).
3. **Given** flight-assist is ON, **When** the player rotates and thrusts, **Then** drift is damped and the velocity vector trends toward the new heading.
4. **Given** the render frame rate varies (e.g., 30/60/144 FPS), **When** the player flies, **Then** motion remains smooth and the flight feel is consistent.

### User Story 2 - Aim and destroy targets (Priority: P1)

The player aims by pointing the ship's nose and fires a fixed forward weapon. Projectiles travel in the heading direction and strike targets using swept collision so even fast shots connect. Targets — static dummies and slowly drifting asteroids — have health; hits apply damage and a destroyed target is removed with clear feedback. Drifting targets force the player to lead their shots.

**Why this priority**: The other half of the core loop (CAP-001) — flight is only half the fun; the slice must prove the pilot→aim→fire→hit→destroy loop is satisfying and that fast projectiles reliably connect.

**Independent Test**: Fire at static and drifting targets and confirm hits register (no tunneling) and targets are destroyed with feedback.

**Acceptance Scenarios**:

1. **Given** a target ahead, **When** the player points the nose at it and fires, **Then** a projectile travels along the heading and strikes the target.
2. **Given** a fast projectile and a thin/small target, **When** the projectile's path crosses the target between frames, **Then** the hit still registers (no tunneling).
3. **Given** a target at low health, **When** a hit depletes its health, **Then** the target is destroyed and removed with visual/audio feedback.
4. **Given** a drifting asteroid, **When** the player leads the moving target and fires, **Then** a correctly-led shot connects.

### User Story 3 - Physical ram collisions (Priority: P2)

When the ship strikes an asteroid, the collision resolves physically: a rigid-body bounce transfers momentum and visibly shoves both bodies. Ramming an asteroid above a speed threshold damages or destroys the ship, making reckless flying consequential.

**Why this priority**: Enhances the momentum feel and adds stakes to flight, but the core fly-and-shoot loop is testable without it; it sharpens, not enables, the feel.

**Independent Test**: Fly into an asteroid slowly (bounce, survive) and at high speed (ship destroyed), confirming believable momentum transfer.

**Acceptance Scenarios**:

1. **Given** the ship contacts an asteroid below the lethal speed, **When** they collide, **Then** both bounce apart with momentum transfer and the ship survives.
2. **Given** the ship rams an asteroid above the lethal speed, **When** they collide, **Then** the ship is destroyed.

### User Story 4 - Minimal diegetic HUD (Priority: P2)

A restrained HUD shows the player's current speed/throttle, the active flight-assist mode, and an aiming reticle, and reflects hits and target destruction — enough to make flight state and combat outcomes readable without number spam.

**Why this priority**: Improves clarity and makes assist-mode and feel testable, but flight and combat function without any overlay (feel can be read from motion alone); a readability aid, not core.

**Independent Test**: Confirm the HUD shows speed, assist mode, and a reticle, and that it updates on hits and destruction.

**Acceptance Scenarios**:

1. **Given** the ship is flying, **When** speed and assist mode change, **Then** the HUD reflects the current speed/throttle and assist mode.
2. **Given** the player fires and hits a target, **When** the hit lands, **Then** the HUD/feedback indicates the hit (and destruction when health is depleted).

### User Story 5 - Reactive seeking target (Priority: P3)

A single dumb seeking target thrusts toward the player, giving the player a maneuvering thing to chase, lead, and destroy — a first taste of reactive dogfight feel.

**Why this priority**: Nice-to-have that tests leading against an actively-maneuvering target; valuable for feel validation but not required for the P1 loop, and the heaviest item (introduces a sliver of AI).

**Independent Test**: Spawn the seeker, confirm it thrusts toward the player, and destroy it by leading shots.

**Acceptance Scenarios**:

1. **Given** the seeking target exists, **When** the player moves, **Then** the target thrusts to close distance toward the player's position.
2. **Given** the seeker is maneuvering, **When** the player leads and fires, **Then** the seeker can be hit and destroyed like other targets.

## Requirements *(mandatory)*

### Functional Requirements *(product specs only)*

- **FR-001**: System MUST render the 2D gameplay plane in 3D with a tinted composed-primitive ship and a top-down camera that follows the ship and supports zoom.
- **FR-002**: System MUST drive ship translation by applying thrust along the nose (forward via the main drive, reverse via weaker retro thrusters) plus lateral strafe, computed via the reused E001 fixed-step integrator. In the default flight-model, linear drag opposes velocity so top speed is the emergent terminal velocity `thrust_force / linear_drag` (no hard clamp) and cutting thrust bleeds speed; in the decoupled mode, motion is pure Newtonian (no drag, coasts indefinitely).
- **FR-003**: System MUST default to a flight-model with (a) **angular inertia** — the turn rate spins up/down toward an emergent maximum `turn_torque / angular_drag` rather than snapping — and (b) a **shared power budget** — hard turning diverts translational thrust (available thrust ×= `1 − turn_power_share·|turn|`), so the ship cannot boost and hard-turn at once. A toggle MUST switch to a **decoupled/Newtonian** mode (instant rotation, no drag).
- **FR-004**: System MUST decouple the fixed-step simulation from rendering using interpolation so flight feel is frame-rate independent and smooth at 60+ FPS. "Frame-rate independent" means the observable equivalence criterion of FR-017: for the same inputs and start state, the per-tick sim trajectory is identical regardless of render frame rate (rendering only interpolates between sim ticks); this replaces reliance on the subjective term "smooth" as the testable property. (FR-004, FR-016, and FR-017 are layered facets of the fixed-step model — render-decoupling, tick-rate assertability, and per-tick determinism respectively — not redundant requirements.)
- **FR-005**: System MUST fire projectiles from a fixed forward-mounted weapon in the ship's heading direction on player input.
- **FR-006**: System MUST resolve projectile–target collisions with swept/continuous tests so fast projectiles never tunnel through targets.
- **FR-007**: System MUST give targets health, apply damage on projectile hits, and destroy and remove a target when its health is depleted, with visual/audio feedback.
- **FR-008**: System MUST support static dummy targets and drifting asteroids that move at constant velocity (Newtonian, via `sim`) so the player must lead moving targets.
- **FR-009**: System MUST resolve ship↔asteroid contact as a rigid-body collision (momentum-transferring bounce) via the `sim` `Physics` trait abstraction.
- **FR-010**: System MUST destroy the player ship when it rams an asteroid at or above a configurable velocity threshold, and bounce non-lethally below it.
- **FR-011**: System MUST display a minimal HUD showing current speed/throttle, active flight-assist mode, an aiming reticle, and hit/destroy feedback.
- **FR-012**: System MUST provide one seeking-AI target that thrusts toward the player's position and is destroyable like other targets.
- **FR-013**: System MUST map all controls (thrust, reverse, rotate, strafe, flight-assist toggle, fire) to the keyboard as the single input device for this slice.
- **FR-014**: System MUST run as a self-contained single-player session with no network connection and no persistence — a window in which the player flies, shoots, and destroys targets.
- **FR-015**: System MUST expose flight/weapon/collision magnitudes (`thrust_force`, `reverse_force`, `strafe_force`, `mass`, `linear_drag`, `turn_torque`, `angular_drag`, `angular_inertia`, `turn_power_share`, `muzzle_speed`, `fire_rate`, `lethal_ram_speed`) as in-engine tunable values, grounded-but-scaled per ADR-0012. They are a global stand-in that will later be sourced from installed equipment/modules (main/retro/maneuvering thrusters, module mass) rather than a global resource.
- **FR-016**: System MUST advance the gameplay simulation at a fixed logical tick rate (default 60 Hz, i.e. `dt = 1/60 s`) that is decoupled from render frame rate; the tick rate MUST be a defined, assertable value (not only a plan note) so tests can drive the sim a known number of ticks.
- **FR-017**: System MUST keep the fixed-step gameplay simulation deterministic: for the same start state and the same per-tick inputs, advancing the same number of ticks MUST produce the same resulting sim state (motion, projectiles, collisions, AI). This reuses the E001 integrator↔analytic motion invariant unchanged (covered-by-E001) and extends the same reproducibility guarantee to this slice's gameplay systems (weapon, swept collision, ram, seek). The guarantee level is **bit-identical (zero tolerance)**: under a deterministic test harness supplying identical per-tick inputs, advancing N ticks MUST yield byte-for-byte equal sim state. Live cross-frame-rate equivalence — where inputs are sampled per render frame and may therefore land on slightly different ticks at different FPS — is explicitly NOT required to be bit-identical and is not asserted with a numeric tolerance; it is validated through the SC-008 manual feel gate instead.

### Key Entities *(include for product or technical specs if feature involves data)*

- **Player Ship**: the controllable fighter; carries kinematic state (position/velocity/heading) reusing `sim` `BodyState`/`Position`/`Velocity`, a health value, and a flight-assist mode; rendered as a tinted composed primitive.
- **Projectile**: a fast-moving entity fired from the ship along its heading; has a velocity and damage value; collides via swept tests and despawns on hit or after a lifetime.
- **Target**: a destructible entity with health; subtypes are static dummy, drifting asteroid (constant velocity), and seeking chaser (thrusts toward the player); asteroids also participate in physical collision.
- **Camera**: the top-down view that follows the ship and supports zoom; renders the 2D plane in 3D.
- **Weapon**: the fixed forward mount that spawns projectiles on input at a defined fire rate/muzzle speed.

## Assumptions & Risks *(mandatory)*

### Assumptions

- The E001 `sim` crate (velocity-Verlet `integrate`/`analytic`, the `Physics` trait + `RapierPhysics`, `Position`/`Velocity`) is the authoritative motion/physics source and is stable (E001 is complete and QC-passed).
- Adding the full Bevy client stack and a Rapier2D-backed physics integration to the Cargo workspace is acceptable for this epic.
- Tinted composed Bevy primitive meshes are sufficient art for the slice; no external models are required.
- A single-player, single-window, in-memory session (no server, no save) is sufficient to validate flight and combat feel.
- Magnitudes (thrust, speed, projectile velocity, lethal ram threshold) are tuned in-engine and need not match real physical values (ADR-0012).

### Risks

- **Subtle-realistic feel is hard to land** *(likelihood: medium, impact: high)*: the slice's entire value rests on subjective feel. Mitigation: in-engine tunable parameters, iterate against the playtest gate, and prioritize input responsiveness and audio feedback.
- **Swept-collision tuning for thin/fast projectiles** *(likelihood: medium, impact: medium)*: residual tunneling or double-counting on edge cases. Mitigation: velocity caps, slightly thicker proxy hitboxes on fragile targets, and explicit edge-case tests.
- **Bevy + Rapier build friction on this environment** *(likelihood: medium, impact: medium)*: heavy-dependency builds have wedged here before. Mitigation: pin versions in the workspace and apply the documented build workarounds (TLS-revoke flag, sandbox off, AV exclusion for `target/`).

## Implementation Signals *(mandatory)*

- `NEW-UI` — A Bevy client window: 3D rendering of the 2D plane, the player ship, projectiles, targets, a follow/zoom camera, and a minimal HUD.
- `NEW-CONFIG` — A new client crate under `crates/` plus full Bevy (and a Rapier2D integration) added to `[workspace.dependencies]`; an input mapping for keyboard controls.
- `NEW-WORKER` — A fixed-timestep game/simulation loop decoupled from the render loop, with interpolation.
- `NEW-ENTITY` — In-memory ECS entities/components for ship, projectile, target subtypes, weapon, and camera (gameplay state, not persisted).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [US1]: A player can fly the ship with visible momentum/inertia and the flight feels smooth at 60+ FPS, with consistent feel across 30/60/144 FPS. Cross-frame-rate consistency is verified two ways: (a) automated — a deterministic harness asserts bit-identical sim state at equal tick counts (FR-017, zero tolerance); (b) manual — live 30/60/144 FPS "consistent feel" is confirmed in the SC-008 playtest, since live per-frame input timing makes live cross-FPS states non-bit-identical by design.
- **SC-002** [US1]: In the flight-model the ship accelerates to an emergent drag-capped top speed and no further, turns with visible angular inertia, bleeds speed during a hard turn (shared power budget), and reverses below its forward top speed; toggling to decoupled gives instant rotation and free Newtonian drift.
- **SC-003** [US2]: A player can aim by pointing the nose and destroy static and drifting targets, with hits registering across the full projectile-velocity range — including grazing and simultaneous hits — and no tunneling.
- **SC-004** [US2]: Every projectile hit applies damage, and a destroyed target is removed with clear visual/audio feedback.
- **SC-005** [US3]: Ramming an asteroid below the threshold produces a believable momentum-transferring bounce; ramming at/above it destroys the ship. Observable acceptance signal for the bounce: after a sub-lethal contact both bodies' post-collision velocities change consistently with a closed-form elastic 2-body impulse (AD-003) — total linear momentum is conserved and the bodies separate (no overlap/sticking) — so the bounce is asserted from velocities, not judged subjectively.
- **SC-006** [US4]: The HUD legibly shows current speed, flight-assist mode, and an aiming reticle and reflects hits/destruction, without number spam.
- **SC-007** [US5]: The seeking target maneuvers toward the player and can be led and destroyed.
- **SC-008** [US1, US2]: The "feels good" gate is met — in a hands-on playtest an evaluator completes the pilot→aim→fire→hit→destroy loop in both assist modes and rates flight and combat feel positive; any negative findings are logged for tuning (PRD principle-VII gate).

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| Flight-model | The default grounded-arcade flight: drag-capped top speed, angular inertia, and a shared power budget. Toggled against the decoupled mode (the `FlightAssist` component: `On` = flight-model, `Off` = decoupled). |
| Decoupled mode | The toggle alternative: instant rotation and no drag — full Newtonian free-drift (heading independent of velocity). |
| Coasting | Moving with thrust released; in decoupled mode velocity stays constant, in the flight-model drag gently bleeds it. |
| Terminal velocity | The emergent top speed where thrust balances drag (`thrust_force / linear_drag`); replaces a hard speed clamp. |
| Angular inertia | Turning has momentum: the turn rate spins up/down toward `turn_torque / angular_drag` rather than snapping. |
| Shared power budget | Hard turning diverts drive power from translational thrust, so a ship cannot boost and hard-turn at once. |
| Retro thrust | Reverse thrust from weaker rear thrusters; reverse top speed sits below the forward top speed. |
| Swept collision (CCD) | Continuous collision testing along a projectile's path between frames, preventing fast objects from tunneling through targets. |
| Fixed timestep | Advancing the simulation in constant `dt` steps independent of frame rate, with rendering interpolated between steps. |
| Ram damage | Damage applied to the ship when it collides with an asteroid above a velocity threshold. |
| Composed primitive | A ship/target visual built from tinted Bevy primitive meshes, requiring no external art. |

## Compliance Check

**Status**: PASS — no `project-instructions.md` violations.

Single-player/no-network is a sanctioned **Principle I (Server-Authoritative Simulation)**-deferred feel-validation step; E003 owns authority/anti-cheat. **Principle II (Shared Deterministic Sim Core)** satisfied — all motion/physics route through the E001 `sim` crate (FR-002, FR-008, FR-009); no forked gameplay logic. **Principle V (Build the Seams)** — single-node in-memory session reusing serializable `sim` state. **Principle VII (Playable Every Phase)** — deliverable is a runnable window with the SC-008 hands-on "feels good" gate. Technology Stack aligned (Rust/Bevy client, Rapier2D behind the `Physics` trait per ADR-0004, swept CCD per FR-006); ADR-0012 grounded-but-scaled honored (FR-015).

**Advisory (LOW, non-blocking)**: downstream Plan/Tasks/Implement gates are not yet met (expected for a Draft spec); the E001 `sim` integrator↔analytic equivalence invariant remains covered-by-E001 (this slice reuses, does not modify, `sim`).
