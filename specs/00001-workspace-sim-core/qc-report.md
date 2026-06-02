# QC Report: Workspace & Sim Core (E001)

**Date**: 2026-06-01 | **Feature**: [spec.md](spec.md) | **Overall Verdict**: **PASS**

## Test Results

- **Runner**: `cargo test -p sim`
- **Result**: **11 passed, 0 failed, 0 ignored** (9 unit + 2 integration; 0 doc-tests)
- Unit: `integrator_matches_analytic_under_constant_accel`, `invariant_holds_across_tick_sizes`, `forward_euler_would_fail_the_invariant`, `zero_accel_is_straight_line_coasting`, `zero_dt_step_is_a_no_op`, `serde_round_trip_preserves_value`, `rapier_step_matches_the_motion_keystone`, `rapier_step_zero_dt_is_a_no_op`, `components_attach_to_an_entity_and_read_back`
- Integration (`tests/physics_swap.rs`): `rapier_and_stub_produce_identical_consumer_behavior`, `swap_requires_no_consumer_change_step_many`
- **Failures**: none

## Static Analysis

- **Tool**: `cargo clippy --all-targets -- -D warnings` → **clean** (no findings; warnings-as-errors).
- **Format**: `cargo fmt --check` → **clean** (exit 0).

## Security Audit

- **Tool**: `cargo audit` (RustSec advisory-db, 124 deps scanned) → exit 0.
- **Findings**: 1 informational advisory `RUSTSEC-2024-0436` — `paste 1.0.15` *unmaintained* (transitive: `rapier2d → simba → paste`). **Allowed** by `.cargo/audit.toml` triage policy (TR-010: gate high/critical, track informational). **No high/critical vulnerabilities.** Non-blocking.

## PI Compliance

**No violations.** This epic directly realizes Principle II (Shared Deterministic Sim Core) and III (Tiered Simulation via the integrator↔analytic equivalence). Technology Stack (Rust/Cargo workspace, `sim`, `bevy_ecs` no-default, Rapier2D behind a `Physics` trait, glam), Testing & Quality Policy (Clippy `-D warnings`, rustfmt, mandatory motion invariant tested), and Source Code Layout (`crates/<name>/src`) all aligned.

## Requirements Traceability

| Work item | Status | Evidence |
|---|---|---|
| OBJ1 — shared dependency-clean `sim` crate | **PASS** | Builds; `cargo tree` shows only `bevy_ecs`/`glam`/`rapier2d`/`serde` — no render/window/net libs |
| OBJ2 — integrator↔analytic equivalence, runtime `dt` | **PASS** | Equivalence + tick-rate + zero-`dt` tests green |
| OBJ3 — swappable `Physics` trait | **PASS** | `Physics` trait (glam/sim-only surface), `RapierPhysics`, `StubPhysics`, swap test green |

| Req | Status | Evidence |
|---|---|---|
| TR-001 workspace + `sim` crate | PASS | builds |
| TR-002 no render/window/net deps | PASS | dependency-graph check clean |
| TR-003 runtime `dt` | PASS | `integrate(dt)` + zero-`dt` no-op test |
| TR-004 analytic + bounded tolerance | PASS | equivalence tests (1e-4 / 2e-4) |
| TR-005 equivalence across rates + negative control | PASS | `{10,20,30,60,144}` Hz + forward-Euler control tests |
| TR-006 `Physics` trait + Rapier impl, no leak | PASS | trait surface glam/sim-only; swap test |
| TR-007 CI gates | PASS | `ci.yml`; build/test/clippy/fmt run green |
| TR-008 serde on domain types | PASS | derives + round-trip test |
| TR-009 dependency-graph inspection | PASS | `cargo tree` CI step + verified clean |
| TR-010 cargo-audit triage | PASS | `audit.toml` + `cargo audit` (allowed informational only) |

| SC | Status |
|---|---|
| SC-001 build + zero render/net deps | PASS |
| SC-002 equivalence tests within tolerance | PASS |
| SC-003 clippy `-D warnings` + fmt clean | PASS |
| SC-004 physics trait swap, no engine types in consumers | PASS |

## Traceability Gaps

None — every TR (001–010) and SC (001–004) maps to passing tests/checks.

## Code Coverage

Non-gated by project policy (no global coverage threshold; `Derived QC Policy` Coverage Target empty). The two mandatory motion invariants (integrator↔analytic equivalence + forward-Euler negative control) are enforced as failing unit tests, so the "invariants MUST be covered" rule is met by the gated Unit tier rather than a coverage percentage. `cargo-llvm-cov` available for ad-hoc reporting if desired.

## Checklist Fulfillment

`checklists/testing.md` (CHL001) spot-check — **PASSED**: quantified tolerance (1e-4 / 2e-4), pinned tick-rate set, forward-Euler negative control, zero-`dt` no-op, and serde round-trip are all present and green.

## Performance

N/A — no performance NFRs/budgets in this epic's spec (benchmarks deferred to E003/E008).

## Accessibility

N/A — headless library crate, no UI.

## Browser Runtime Validation

SKIPPED — not required (no UI/browser surface).

## Manual Testing

None required — all verification automated.

## Tool Recommendations

- `cargo-llvm-cov` (installed) for optional coverage reporting (non-gated).

## Bug Tasks Generated

None.
