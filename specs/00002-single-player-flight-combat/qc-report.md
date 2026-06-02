# QC Report: Single-player Flight & Combat (E002)

**Date**: 2026-06-02 | **Feature**: [spec.md](spec.md) | **Overall Verdict**: **PASS** — all gates green and the SC-008 hands-on feel gate met in playtest (2026-06-02). No deferred items remain.

## Test Results

- **Runner**: `cargo test --workspace` (MSVC toolchain)
- **Result**: **44 passed, 0 failed, 0 ignored**
  - `sim` lib unit: 30 · `sim` integration `tests/gameplay.rs`: 11 · `sim` integration `tests/physics_swap.rs`: 3 · `client`: 0 (rendering shell — validated via build + the hands-on playtest)
  - Flight-model coverage added in the post-playtest refinement: terminal-velocity cap, angular-rate convergence, and shared-power-budget (hard-turn bleeds speed).
- Covers: integrator↔analytic keystone (E001, reused unchanged), flight-assist on/off, coasting, weapon cooldown, swept-CCD (no-tunnel / grazing / thin-target / start-inside), damage/destroy, elastic ram bounce + lethal threshold, seek steering, and **bit-identical fixed-step determinism** (`fixed_step_is_bit_identical_under_identical_inputs`).
- **Failures**: none.

## Static Analysis

- `cargo clippy --workspace --all-targets -- -D warnings` → **clean** (warnings-as-errors). `clippy::type_complexity` allowed crate-wide for the Bevy/ECS tuple-query idiom.
- `cargo fmt --check` → **clean**.

## Security Audit

- `cargo audit` (RustSec, 587 deps) → exit 0. **1 allowed informational** advisory `RUSTSEC-2024-0436` (`paste` unmaintained, transitive via the rapier2d/bevy trees) — allowed by `.cargo/audit.toml` (TR-010: gate high/critical, track informational). **No high/critical vulnerabilities.**

## PI Compliance

**No violations.** Principle II realized — all gameplay logic (motion/flight/collision/weapon/combat/ai/tuning) lives in `crates/sim` as headless `bevy_ecs` systems; `crates/client` is input/render/HUD/camera/scene only (ADR-0013). Principle I correctly deferred (single-player slice; E003 owns server authority); Principle V (serde-derivable components, single-node in-memory); Principle VII (runnable window + the SC-008 feel gate). Technology Stack aligned: Rapier2D confined behind the `sim::Physics` trait (no engine type leaks into `sim` public signatures — verified); grounded-but-scaled magnitudes via the `Tuning` resource (ADR-0012). Source layout `crates/sim` + new `crates/client` per ENFORCE_SRC_ROOT. Testing policy: the E001 motion invariant is intact and reused; new pure-fn + headless-ECS tests added.

## Requirements Traceability

| Work item | Status | Evidence |
|---|---|---|
| US1 Newtonian flight (P1) | **PASS** | `flight.rs` (motion/assist) + `gameplay.rs` coast test; camera/render-sync in client |
| US2 Aim & destroy (P1) | **PASS** | `weapon.rs`/`collision.rs`/`combat.rs` + `firing_destroys_a_target_ahead`, swept-CCD tests, drifting-shot test |
| US3 Physical ram (P2) | **PASS** | `collision.rs` elastic + lethal; sub-lethal & lethal ram integration tests |
| US4 Minimal HUD (P2) | **PASS** | `hud.rs` (speed/assist/reticle/HIT-KILL via `HitFeedback`) |
| US5 Reactive seeker (P3) | **PASS** | `ai.rs` + seeker-closes-distance test |

| SC | Status | Notes |
|---|---|---|
| SC-001 | **PASS** (automated) / manual half | bit-identical determinism test green; live 30/60/144 "consistent feel" folds into SC-008 |
| SC-002 | PASS | terminal-velocity, angular-convergence, shared-power-budget + decoupled-coast tests |
| SC-003 | PASS | swept hits across velocity range incl. grazing, thin-target, **simultaneous multi-hit** (all tested) |
| SC-004 | PASS | damage + destroy-once + feedback |
| SC-005 | PASS | elastic momentum-conserving bounce + lethal threshold |
| SC-006 | PASS (code) | single restrained HUD line + reticle; subjective "no number spam" in the manual gate |
| SC-007 | PASS | seeker maneuvers + destroyable |
| **SC-008** | **PASS** | Hands-on feel gate performed and passed (2026-06-02); flight model iterated to the grounded-arcade model and rated good. T043 complete. |

All FR-001…FR-017 implemented and covered (see plan Requirement Coverage Map).

## Traceability Gaps

None. The two edge-case coverage gaps surfaced by the Story Verifier (simultaneous multi-hit; led-shot-on-drifter + target-despawns-mid-flight; thin-target min-radius) were closed this run by adding `two_projectiles_one_target_destroy_once`, `shot_connects_with_a_drifting_asteroid`, `projectiles_resolve_harmlessly_after_target_destroyed`, and `thin_target_still_hit_at_small_radius`.

## Code Coverage

Non-gated (no global threshold; `Derived QC Policy` Coverage Target empty). The mandatory motion invariant stays covered by E001; this slice's gameplay invariants are covered by the 42-test suite. `cargo-llvm-cov` available for ad-hoc reporting.

## Checklist Fulfillment

`checklists/testing.md` (CHL001, 31 items, all `[X]`) — Testing-category spot-check **PASSED**: determinism (bit-identical harness), swept-CCD edge cases (now all tested), assist on/off observability, and the feel-gate measurability all satisfied.

## Performance

N/A automated — no performance NFR in spec; the 60+ FPS / fixed-step-independence target is part of the SC-008 manual playtest.

## Accessibility

N/A — native desktop game client, no UI accessibility surface in scope.

## Browser Runtime Validation

SKIPPED — native Bevy desktop application; no browser/web surface and no browser tooling applicable.

## Manual Testing

SC-008 hands-on playtest **performed and passed** (2026-06-02). During the playtest the flight model was iterated to the grounded-arcade model (drag-based terminal velocity, angular inertia, shared power budget, asymmetric reverse); the GDD §4 Flight and this spec (FR-002/FR-003, SC-002) were updated to match. `manual-test.md` retains the playtest script.

## Tool Recommendations

- `cargo-llvm-cov` (installed) — optional coverage reporting (non-gated).
- Bevy `dynamic_linking` dev feature (`cargo run -p client --features dynamic_linking`) — faster iterative client builds; keep it dev-only.

## Bug Tasks Generated

None.
