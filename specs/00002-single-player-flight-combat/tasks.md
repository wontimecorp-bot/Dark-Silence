# Tasks: Single-player Flight & Combat (E002)

**Input**: Design documents from `specs/00002-single-player-flight-combat/`
**Prerequisites**: `plan.md` (required), `spec.md` (required), `research.md`, `data-model.md`, `checklists/testing.md`

**Tests**: Included — the spec and user constraints explicitly request pure-fn unit tests, the extended-`Physics` swap-equivalence test, headless `bevy_ecs`/`Time<Fixed>` integration tests, and a bit-identical determinism test (FR-017). Test tasks are authored to FAIL before the implementation task they cover, per TDD.

**Organization**: Grouped by user story (`US#`) by priority. US1 (flight) + US2 (combat) are both P1 = the MVP first-playable loop; US3 (ram) + US4 (HUD) are P2; US5 (seek) is P3.

## Project Mode

`Brownfield`

- Adds a NEW binary crate `crates/client` (render/input/HUD/camera/scene only).
- EXTENDS the existing, QC-passed `crates/sim` (E001 motion/components/physics) with gameplay modules and an extended `Physics` trait.
- Does NOT re-bootstrap E001: `crates/sim` `motion`/`components`/`physics` + their tests already exist and are QC-passed — extend, never rewrite. The E001 integrator↔analytic motion keystone (`motion.rs`) is reused unchanged (INV-12 / covered-by-E001) and MUST NOT be modified.

## Epic / Capability Map

- `[US1]` → CAP-001 momentum flight (Newtonian piloting + flight-assist + fixed-step/interp feel)
- `[US2]` → CAP-001 combat (fixed forward weapon + swept-CCD projectiles + health/damage/destroy)
- `[US3]` → CAP-001 stakes (ship↔asteroid rigid-body bounce + lethal ram threshold)
- `[US4]` → CAP-001 readability (minimal diegetic HUD)
- `[US5]` → CAP-001 reactive feel (single seeking-AI target)

## Brownfield Notes

- **Existing flows touched**: `Cargo.toml` (workspace members + `[workspace.dependencies]`); `crates/sim/src/lib.rs` (export new modules); `crates/sim/src/components.rs` (+ gameplay components); `crates/sim/src/physics.rs` (extend `Physics` trait + `RapierPhysics` impl); `crates/sim/tests/physics_swap.rs` (mirror new trait methods in `StubPhysics`).
- **Compatibility / ordering**: HINT-001 — the `Physics` trait MUST be extended (swept-cast + contact query) AND mirrored in `StubPhysics` BEFORE any weapon/collision system that depends on it, or `tests/physics_swap.rs` fails to compile. HINT-004 — pin `bevy = "0.18"` to the `bevy_ecs = "0.18"` already in the workspace (a second `bevy_ecs` version fails to build).
- **Constraint (Principle II / ADR-0013 / AD-004, HINT-002)**: ALL gameplay logic (flight, collision, weapon, combat, ai, tuning) lives under `crates/sim/src/`; `crates/client/src/` holds ONLY input/render_sync/camera/hud/scene/app wiring — no motion or damage math.
- **Gotcha (HINT-003 / AD-001)**: step the sim ONLY in Bevy `FixedUpdate` with `Time<Fixed>` (default 60 Hz); never frame-delta dt in `Update`. Interpolate render `Transform`s via `overstep_fraction()`.
- **Regression focus**: the E001 `physics_swap.rs` swap-equivalence and `motion` keystone tests MUST keep passing after the trait extension.
- **Build env**: the heavy Bevy tree needs the documented workarounds — `CARGO_HTTP_CHECK_REVOKE=false`, sandbox off, AV exclusion for `target/` (HINT-004, Plan §Risk Mitigation).

---

## Phase 1: Setup (Repository / Workspace Delta)

- [X] T001 Register `crates/client` as a workspace member and add `bevy = { version = "0.18" }` to `[workspace.dependencies]` (pinned to the existing `bevy_ecs` 0.18 per HINT-004) in `~ Cargo.toml`
- [X] T002 {FR-014} Create new binary crate manifest `+ crates/client/Cargo.toml` with deps `bevy = { workspace = true }`, `sim = { path = "../sim" }`, `glam.workspace = true`; add the `dynamic_linking` dev convenience feature (AD-005) after:T001
- [X] T003 [P] {FR-014} Scaffold the Bevy app entry `+ crates/client/src/main.rs` — a `DefaultPlugins` window App with empty module stubs (`input`, `render_sync`, `camera`, `hud`, `scene`) so the crate compiles before systems land after:T002

---

## Phase 2: Foundational (Cross-Work-Item Blockers)

**Shared model + the order-critical `Physics`-trait extension (HINT-001) and the fixed-step driver — every delivery story depends on these.**

- [X] T004 [P] {FR-015} Create `Tuning` resource (`thrust_accel`,`rotation_rate`,`strafe_accel`,`max_speed`,`muzzle_speed`,`fire_rate`,`lethal_ram_speed`,`assist_damping`; INV-10 range guards; ADR-0012 grounded-scaled) in `+ crates/sim/src/tuning.rs` → exports: Tuning{..}, Tuning::default()
- [X] T005 [P] {FR-002,FR-005,FR-007,FR-008,FR-012} Add gameplay components (`Ship`,`Heading`,`Health`,`FlightAssist`,`Projectile`,`Damage`,`Lifetime`,`PrevPosition`,`ProjectileOwner`,`Target`,`TargetKind`,`CollisionRadius`,`Weapon`; full `Component+serde` derive set) in `~ crates/sim/src/components.rs` → exports: Ship, Heading, Health, FlightAssist, Projectile, Damage, Lifetime, PrevPosition, ProjectileOwner, Target, TargetKind, CollisionRadius, Weapon (see data-model.md#New-Components)
- [X] T006 {FR-006,FR-009} Write FAILING swap-equivalence cases for the extended `Physics` (swept segment-cast + contact query: Stub vs Rapier identical) in `~ crates/sim/tests/physics_swap.rs` — mirror new methods in `StubPhysics` (CHK031)
- [X] T007 {FR-006,FR-009} Extend the `Physics` trait with `swept_cast(seg,circle,radius)->Option<ToiHit>` + `contact_query(a,b)->Option<Contact>` and implement both in `RapierPhysics` (Rapier symbols confined to method bodies) in `~ crates/sim/src/physics.rs` after:T006 → exports: Physics::swept_cast(), Physics::contact_query()
- [X] T008 {FR-004,FR-016} Create the fixed-step driver scaffold in `~ crates/client/src/main.rs`: register sim systems in `FixedUpdate` driven by `Time<Fixed>` at default 60 Hz (`dt = 1/60`, assertable), insert `Tuning`; never step in `Update` (HINT-003, AD-001) after:T003,T004 ← T004:Tuning

---

## Phase 3: US1 - Fly a ship with Newtonian momentum (Priority: P1) 🎯 MVP

**Goal**: Newtonian thrust/rotate/strafe with coasting, a flight-assist toggle, and frame-rate-independent feel via fixed-step + interpolated render.

- [X] T009 [P] [US1] {FR-003} Write FAILING pure-fn unit test for the flight-assist transform (OFF: heading change leaves velocity vector unchanged; ON: velocity trends toward heading by `assist_damping`; toggle never snaps velocity, INV-07) in `+ crates/sim/src/flight.rs` `#[cfg(test)]`
- [X] T010 [P] [US1] {FR-002,FR-016,FR-017} Write FAILING headless `bevy_ecs`/`Time<Fixed>` integration test: thrust→accelerate then release→coast at constant velocity (FR-002 steady-state, CHK025), speed clamp INV-02, in `+ crates/sim/tests/gameplay.rs`
- [X] T011 [US1] {FR-002,FR-003,FR-015} Implement flight module — `apply_thrust`/`rotate`/`strafe` (reuse `sim::integrate`, NOT a new integrator) + `flight_assist` transform reading `Tuning` in `+ crates/sim/src/flight.rs` after:T005,T004 ← T004:Tuning → exports: flight_assist(vel,heading,mode,damping), flight_step()
- [X] T012 [US1] {FR-002} Implement the fixed-step ship-motion system (assemble `BodyState` from `Position`+`Velocity`, `integrate`, write back, clamp to `Tuning.max_speed` INV-02) in `+ crates/sim/src/flight.rs` after:T011 ← T011:flight_step → exports: ship_motion_system
- [X] T013 [P] [US1] {FR-013} Implement keyboard input mapping (thrust/reverse/rotate/strafe/assist-toggle/fire → sim intents; assist toggle is no-snap INV-07) in `+ crates/client/src/input.rs` after:T003 → exports: read_input_system, ShipIntent
- [X] T014 [US1] {FR-001,FR-004} Implement render-sync: maintain `RenderInterp` (prev/curr per fixed step) and lerp the rendered `Transform` by `overstep_fraction()` (interpolated, not raw state — CHK006/INV-11) in `+ crates/client/src/render_sync.rs` after:T008,T012 ← T012:ship_motion_system → exports: RenderInterp, interpolate_transforms_system
- [X] T015 [US1] {FR-001} Implement top-down follow camera with zoom (`Camera3d` follows Ship `Position`; renders the 2D plane in 3D) in `+ crates/client/src/camera.rs` after:T003 → exports: camera_follow_system, camera_zoom_system
- [X] T016 [US1] {FR-001} Implement scene spawn — tinted composed-primitive ship mesh/material + initial `Position`/`Velocity`/`Heading`/`Health`/`FlightAssist`/`Weapon`/`CollisionRadius` (+`RenderInterp`) in `+ crates/client/src/scene.rs` after:T005,T008 ← T005:Ship,Weapon → exports: spawn_ship
- [X] T017 [US1] {FR-002,FR-003,FR-004} [COMPLETES FR-002] [COMPLETES FR-003] [COMPLETES FR-004] Wire input→flight→motion→render-sync into the App (`FixedUpdate` order); confirm T009/T010 pass in `~ crates/client/src/main.rs` after:T011,T012,T013,T014,T015,T016

---

## Phase 4: US2 - Aim and destroy targets (Priority: P1) 🎯 MVP

**Goal**: fixed forward weapon firing swept (CCD) projectiles in the heading direction; targets with health take damage and are destroyed with feedback; static dummies + drifting asteroids force leading.

- [X] T018 [P] [US2] {FR-005} Write FAILING pure-fn unit test for weapon cooldown (fires only when `cooldown<=0`; firing sets `cooldown=1/fire_rate`; fire while cooling is ignored, INV-03) in `+ crates/sim/src/weapon.rs` `#[cfg(test)]`
- [X] T019 [P] [US2] {FR-006} Write FAILING pure-fn unit tests for swept segment-vs-circle CCD, all four edge cases — high-velocity no-tunnel (to `Tuning.muzzle_speed`); grazing/tangent closest-approach `<=CollisionRadius`=hit, no flicker (CHK027); thin-target min-radius (CHK028); simultaneous multi-hit — in `+ crates/sim/src/collision.rs`
- [X] T020 [P] [US2] {FR-007} Write FAILING pure-fn unit test for damage application (`Health -= Damage`, clamp `>=0` INV-01; destroy at `Health<=0`; despawn-exactly-once under overkill INV-09) in `+ crates/sim/src/combat.rs` `#[cfg(test)]`
- [X] T021 [US2] {FR-006,FR-008} Write FAILING headless `Time<Fixed>` integration test: fire→swept-hit→despawn projectile; correctly-led shot connects with a drifting asteroid (CHK012); target-despawns-mid-flight resolves harmlessly (CHK017) in `~ crates/sim/tests/gameplay.rs` after:T010
- [X] T022 [US2] {FR-006} Implement swept segment-vs-circle CCD (`PrevPosition→Position` segment) + order-independent multi-hit resolution via `Physics::swept_cast` in `+ crates/sim/src/collision.rs` after:T007,T019 ← T007:Physics::swept_cast → exports: swept_segment_circle(), collision_detect_system
- [X] T023 [US2] {FR-005,FR-015} [COMPLETES FR-015] Implement weapon firing + cooldown gate (spawn `Projectile` along `Heading` at `Tuning.muzzle_speed`; cooldown INV-03; `Damage>0` INV-04) in `+ crates/sim/src/weapon.rs` after:T005,T004,T018 ← T004:Tuning → exports: weapon_fire_system, spawn_projectile()
- [X] T024 [US2] {FR-006} Implement projectile lifetime/advance system (`PrevPosition=Position`; `integrate`; `Lifetime-=dt`; despawn at `<=0` INV-06) in `+ crates/sim/src/weapon.rs` after:T023 ← T023:spawn_projectile → exports: projectile_step_system
- [X] T025 [US2] {FR-007} Implement combat module — apply damage on hit, clamp Health INV-01, destroy + despawn-once INV-09 + emit a test-detectable destroy/hit event for feedback (CHK016) in `+ crates/sim/src/combat.rs` after:T022,T020 ← T022:collision_detect_system → exports: damage_system, destruction_system, HitEvent
- [X] T026 [P] [US2] {FR-008} Implement target spawning — static `Dummy` + constant-velocity `Asteroid` (drift via `sim` motion, FR-008) tinted primitives in `~ crates/client/src/scene.rs` after:T005,T016 ← T005:Target,TargetKind → exports: spawn_dummy, spawn_asteroid
- [X] T027 [US2] {FR-006,FR-007} [COMPLETES FR-006] [COMPLETES FR-007] Wire weapon→projectile-step→swept-collision→damage→destruction into `FixedUpdate` (after motion, before render-sync) + render projectiles + hit/destroy feedback hook in `~ crates/client/src/main.rs` after:T022,T023,T024,T025,T026

---

## Phase 5: US3 - Physical ram collisions (Priority: P2)

**Goal**: ship↔asteroid rigid-body bounce (momentum transfer, closed-form elastic impulse) plus lethal-ram destruction above a tunable speed threshold.

- [X] T028 [P] [US3] {FR-009} Write FAILING pure-fn unit test for the closed-form elastic 2-body impulse (post-collision velocities conserve total linear momentum; bodies separate, no overlap/sticking — AD-003 / SC-005 / CHK011) in `+ crates/sim/src/collision.rs` `#[cfg(test)]`
- [X] T029 [US3] {FR-010} Write FAILING pure-fn unit test for the lethal-ram threshold (closing speed `>=Tuning.lethal_ram_speed` → destroy; below → bounce/survive; boundary inclusive at threshold — CHK010) in `+ crates/sim/src/collision.rs` `#[cfg(test)]`
- [X] T030 [US3] {FR-009,FR-010} Write FAILING headless `Time<Fixed>` integration test: sub-lethal ram → both bodies bounce + momentum conserved + ship survives; at/above threshold → ship destroyed in `~ crates/sim/tests/gameplay.rs` after:T021
- [X] T031 [US3] {FR-009} Implement ship↔asteroid contact + elastic bounce (`Physics::contact_query` for detection; closed-form elastic 2-body impulse applied in `sim`; motion stays authoritative AD-003) in `~ crates/sim/src/collision.rs` after:T007,T028 ← T007:Physics::contact_query → exports: elastic_impulse(), ram_collision_system
- [X] T032 [US3] {FR-010} Implement lethal-ram check (closing speed vs `Tuning.lethal_ram_speed`: at/above → destroy Ship via combat destruction; below → bounce only) in `~ crates/sim/src/collision.rs` after:T031,T025 ← T025:destruction_system → exports: ram_damage_system
- [X] T033 [US3] {FR-009,FR-010} [COMPLETES FR-009] [COMPLETES FR-010] Wire ram-collision + lethal-ram systems into `FixedUpdate` (after motion, before combat destruction) in `~ crates/client/src/main.rs` after:T032

---

## Phase 6: US4 - Minimal diegetic HUD (Priority: P2)

**Goal**: a restrained HUD showing speed/throttle, active flight-assist mode, an aiming reticle, and hit/destroy feedback — no number spam.

- [X] T034 [US4] {FR-011} Implement the minimal HUD (Bevy UI): speed/throttle readout, active `FlightAssist` mode, aiming reticle aligned to `Heading`, and hit/destroy feedback subscribing to `HitEvent` (CHK023, no number spam SC-006) in `+ crates/client/src/hud.rs` after:T017,T025 ← T025:HitEvent → exports: hud_update_system, spawn_hud
- [X] T035 [US4] {FR-011} [COMPLETES FR-011] Wire the HUD systems into the App `Update` schedule (reads interpolated ship state + assist mode + hit events) in `~ crates/client/src/main.rs` after:T034

---

## Phase 7: US5 - Reactive seeking target (Priority: P3)

**Goal**: a single dumb seeking-AI target that thrusts toward the player and is destroyable like other targets.

- [X] T036 [P] [US5] {FR-012} Write FAILING pure-fn unit test for seek steering (thrust direction points from seeker `Position` toward player `Position`; magnitude from `Tuning.thrust_accel` — observable behavior, algorithm-independent CHK013) in `+ crates/sim/src/ai.rs` `#[cfg(test)]`
- [X] T037 [US5] {FR-012} Write FAILING headless `Time<Fixed>` integration test: seeker closes distance toward a moving player and is hit/destroyed like other targets (SC-007, shared destroy lifecycle CHK014) in `~ crates/sim/tests/gameplay.rs` after:T030
- [X] T038 [US5] {FR-012} Implement seek steering system (`Seeker` thrusts toward player each step via `sim` motion; destroyable through the shared combat path) in `+ crates/sim/src/ai.rs` after:T005,T004,T036 ← T005:Target,TargetKind ← T004:Tuning → exports: seek_system
- [X] T039 [US5] {FR-012} [COMPLETES FR-012] Spawn the single seeker target (tinted primitive) and wire `seek_system` into `FixedUpdate` (before motion) in `~ crates/client/src/{scene,main}.rs` after:T038,T026 ← T038:seek_system

---

## Phase 8: Polish & Cross-Cutting Concerns

- [X] T040 {FR-016,FR-017} [COMPLETES FR-016] [COMPLETES FR-017] Bit-identical determinism test: same start + same N per-tick inputs → byte-equal `sim` state; assert 60 Hz tick in `~ crates/sim/tests/gameplay.rs` after:T017,T027,T033,T039
- [X] T041 {FR-001,FR-005,FR-013} [COMPLETES FR-001] [COMPLETES FR-005] [COMPLETES FR-013] Export new `sim` gameplay modules from `~ crates/sim/src/lib.rs` (`flight`,`collision`,`weapon`,`combat`,`ai`,`tuning`); audit that no Rapier/Bevy types leak into `sim` public signatures (HINT-002) after:T011,T022,T023,T025,T031,T038
- [X] T042 Run the full workspace gate suite green — `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo audit` — applying the build-env workarounds (`CARGO_HTTP_CHECK_REVOKE=false`, sandbox off, AV exclusion for `target/`); fix any failures across `crates/sim` + `crates/client` after:T040,T041
- [ ] T043 [DEFERRED] {SC-008} [COMPLETES SC-008] Manual SC-008 "feels good" gate (Principle-VII): run the pilot→aim→fire→hit→destroy loop hands-on in BOTH assist modes (ON drift-damped, OFF decoupled), confirm live 30/60/144 FPS consistent feel (SC-001b), rate flight/combat feel positive, and log any negative findings for tuning after:T042

---

## Dependencies

Setup (Phase 1) → Foundational (Phase 2) → US1 (Phase 3, P1) → US2 (Phase 4, P1) → US3 (Phase 5, P2) → US4 (Phase 6, P2) → US5 (Phase 7, P3) → Polish (Phase 8).

- **MVP = US1 + US2** (both P1) — the first-playable pilot→aim→fire→hit→destroy loop. US3/US4 (P2) and US5 (P3) are additive and each independently testable on top of the MVP.
- **Phase 1 (Setup)** has no dependencies.
- **Phase 2 (Foundational)** depends on Setup. **Order-critical (HINT-001)**: T006 (failing swap test + `StubPhysics` mirror) → T007 (extend trait + `RapierPhysics`) MUST precede any swept/contact consumer (T022, T031, T032). The trait extension is the keystone every collision/weapon system imports.
- **Within US1**: tests (T009, T010) before implementation (T011, T012); `flight_assist`/motion before render-sync (T014) and App wiring (T017).
- **Within US2**: tests (T018, T019, T020, T021) before implementation; `Physics::swept_cast` (T007, foundational) before swept collision (T022); damage/destruction (T025) before HUD feedback (US4) and ram destruction (US3).
- **Within US3**: depends on T007 (`contact_query`) and T025 (`destruction_system`) from earlier phases via `after:`.
- **US4** depends on `HitEvent` (T025, US2) and the running App (T017).
- **US5** reuses the shared combat destroy path (T025) and target spawning (T026).
- **Polish (Phase 8)** depends on all delivery stories: T040 needs every gameplay system wired (T017/T027/T033/T039); T043 (manual gate) runs last, after the green gate suite (T042).
- Tasks marked `[P]` can run in parallel within their phase (distinct files, no intra-batch dependency).
- A task with `after:T###` or `← T###:Symbol` is never `[P]`-batched with its referenced task.

### Parallelizable tasks

`[P]`-eligible: T003, T004, T005, T009, T010, T013, T018, T019, T020, T026, T028, T036 (12 tasks). Note: T004 and T005 are both `[P]` in Phase 2 but T006/T007 are sequential (HINT-001 ordering) and not parallel. T029 is NOT `[P]` (it shares `collision.rs` with T028).

## Requirement Coverage

Every FR-001…FR-017 and the SC-008 manual gate maps to at least one task.

| Req ID | Task IDs |
|--------|----------|
| FR-001 | T014, T015, T016, T041 |
| FR-002 | T010, T011, T012, T017 |
| FR-003 | T009, T011, T017 |
| FR-004 | T008, T014, T017 |
| FR-005 | T018, T023, T041 |
| FR-006 | T006, T007, T019, T021, T022, T024, T027 |
| FR-007 | T020, T025, T027 |
| FR-008 | T021, T026 |
| FR-009 | T006, T007, T028, T030, T031, T032, T033 |
| FR-010 | T029, T030, T032, T033 |
| FR-011 | T034, T035 |
| FR-012 | T036, T037, T038, T039 |
| FR-013 | T013, T041 |
| FR-014 | T002, T003 |
| FR-015 | T004, T011, T023 |
| FR-016 | T008, T040 |
| FR-017 | T010, T040 |
| SC-008 | T043 |

## Deferred Issues

- **T043 [DEFERRED] {SC-008}** — The SC-008 "feels good" gate is a **hands-on human playtest** (fly the loop in both assist modes, judge feel, confirm live 30/60/144 FPS consistency). It cannot be executed inside the automated implement→QC loop, so it is deferred to the user. The slice is code-complete and runnable: launch with `cargo run -p client` (MSVC toolchain). All automated coverage for the underlying behaviour (determinism, assist on/off, swept hits, ram, seek) passes; only the subjective feel verdict remains.
