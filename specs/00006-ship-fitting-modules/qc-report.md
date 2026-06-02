# QC Report — E006 Ship Fitting & Modules

**Feature**: `specs/00006-ship-fitting-modules/`
**Date**: 2026-06-02
**Run**: full (no prior report)
**Toolchain**: `stable-x86_64-pc-windows-msvc`, MSVC (`CARGO_HTTP_CHECK_REVOKE=false`, sandbox-disabled cargo)

## Overall Verdict: **PASS**

All required QC categories (linting, security, performance) pass; the full test suite is green in both feature modes; all 5 user stories and all 6 success criteria trace to implementing code and an asserting test. Three runtime-only surfaces (the fitting screen, the windowed ship flying under its fit) are recommended for manual playtest — none is a success-criterion failure (each is proven headlessly at the logic level).

## Test Results

Runner: `cargo test`. **0 failures across all suites.**

| Crate | Default features | With `--features udp` |
|-------|------------------|------------------------|
| sim | **100** (53 lib + 33 `tests/fitting.rs` + 11 gameplay + 3 physics_swap) | — |
| protocol | 31 (14 lib + 6 quantize + 11 roundtrip) | 36 (+ 2 isolation, 1 renet_udp, 2 secure_connect) |
| server | 69 (39 lib + 30 integration) | 71 (+ 1 bandwidth, 1 equivalence) |
| client | 23 (15 lib + 8 integration) | — |

E006's `crates/sim/tests/fitting.rs` (33 tests) spans validate (SC-001/002), stats_phase4 (SC-003 + baseline-reproduces-`Tuning`), layout_phase5 (SC-004), tradeoffs_phase6 (SC-005), presets_phase7 (SC-006). E001/E002/E003 suites stayed green (the `Tuning`→`ShipStats` rewire used override-or-fallback; unfitted ships unchanged).

## Static Analysis

- `cargo clippy --workspace --all-targets -- -D warnings` — **clean**.
- `cargo clippy --workspace --all-targets --features udp -- -D warnings` — **clean**.
- `cargo fmt --check` — **clean**.

## Security Audit

- `cargo audit` — DB fetched (1102 advisories, 611 deps), exit 0. **0 vulnerabilities.**
- One **informational, non-gating** advisory: RUSTSEC-2024-0436 `paste 1.0.15` (unmaintained), transitive via `rapier2d`/`simba` + `wgpu-hal`/`metal`; allowed by `.cargo/audit.toml`. Not a failure.

## PI Compliance

**No violations (0 CRITICAL).**
- **II Shared Sim Core** — the Module/Hull/Fit domain + validation/derivation/layout live in `crates/sim`; fitted ships drive the SAME shared `flight`/`weapon` systems via `ShipStats` (override-or-fallback, unfitted ships keep `Tuning`). No forked client model.
- **I Server-Authoritative** — `validate_fit`/`derive_ship_stats` are pure functions (client preview today, server authority later); T033 attaches the Fit/ShipStats to the *embedded server's* ship (server computes flight).
- **V Build the Seams** — hull is a coarse 2D cell-grid, cell-upgrade-ready (ADR-0008); the fit layout (`resolve_hit`/`cell_map`/`hardpoint_arc`) is the hit/armor-map seam E007 reads.
- **No P2W** — acquisition/economy excluded to E013/E014; no real-money surface.

## Requirements Traceability

**User stories** — US1–US5 all **PASSED** (fit-within-budgets, fit-drives-flight, layout-as-hit-map, tradeoffs/ladder, presets+UI).

**Success Criteria** — all **PASSED**:

| SC | Proof |
|----|-------|
| SC-001 | `tests/fitting.rs` — each over-budget axis named; type/size mismatch rejected with the `Violation` |
| SC-002 | live `budget_usage`; empty hull valid; remove frees budget |
| SC-003 | stats tests (mass→agility, thrust→top-speed, no-weapon→can't-fire, floors) + `fitted_ship_flies_to_its_fit_derived_top_speed` (running ship) + `baseline_seed_fit_reproduces_tuning_defaults` |
| SC-004 | `resolve_hit` outer-before-inner; reactor central-vs-edge depth; `cell_map` complete; `hardpoint_arc` bounded (0,π] |
| SC-005 | no-fit-maxes-all guard over the seed catalog; tank binds mass / damage binds cpu; corvette scales at the cost of agility |
| SC-006 | preset save→reload round-trip; incompatible-hull rejected; `preview_stats` == committed derive |

All FR-001…FR-025 trace requirement → task → code → test.

## Traceability Gaps

**None.** Every FR has implementing code and ≥1 asserting test. Three UI-bound FRs (FR-012 fitting screen; FR-009/013 on-screen bars/preview; FR-014 windowed application) have a headless-tested logic core + a compile-/structure-verified UI shell (the on-screen part is manual). Minor doc note (not a gap): T033's fitted-ship spawn landed in `crates/client/src/net.rs` (`attach_starter_fit`) rather than `scene.rs` — correct, since the windowed client computes flight in the embedded server's world.

## Code Coverage

No global gate. The fitting domain invariants (INV-F01..F14), validation edge cases, stats floors, hit-map ordering, and the tradeoff guard are covered by the 33 `fitting.rs` tests + 25 inline unit cases.

## Checklist Fulfillment (Data Integrity / Testing spot-check)

- **Data Integrity**: budget non-exceedance per axis, slot type/size gating, dangling-id reject, graceful stat floors (no NaN/inf), occlusion ordering, arc bounds — all invariant-tested — **PASSED**.
- **Testing**: per-axis validation, stats derivation, hit-map resolution, preset round-trip, the no-degenerate-fit guard, + full E001/E002/E003 regression — **PASSED**.

## Performance

**N/A — no performance NFR in this feature.** Fitting validation/stats derivation are per-fit-change (not a hot path); hit-map raycast perf is E007's concern. No budget/bench in E006.

## Accessibility

N/A — native game client (project omits WCAG).

## Browser Runtime Validation

N/A — native Bevy/headless project; no browser surface.

## Manual Testing

`manual-test.md` provides a playtest checklist for the runtime-only surfaces (the fitting screen interactions; the windowed ship flying under its fit; on-screen budget bars + green/red preview + presets). **Non-blocking** — the underlying fitting/stats/layout/preset logic is proven by the 33 headless tests; these are visual/feel confirmations. They do **not** gate this PASS.

## Tool Recommendations

None outstanding (cargo-audit installed; no skipped tools).

## Bug Tasks Generated

**None.**
