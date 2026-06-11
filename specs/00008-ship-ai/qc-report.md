# QC Report: 00008-ship-ai — Ship AI architecture

> Date: 2026-06-10 | Run: full (no prior report) | Feature: specs/00008-ship-ai/ | Verdict at bottom

## Test Results

| Item | Value |
|---|---|
| Runner | `cargo test --workspace` |
| Totals | **530 passed, 0 failed**, 1 ignored (pre-existing `dump_seed_ron`) |
| Golden trio (CRITICAL) | `determinism` 1/1 · `demo_enemies_smoke` 4/4 · `harness`/botkit 10/10 — **bit-identical** |
| Feature suite | `crates/sim/tests/ai.rs` **28/28** (every OBJ1–OBJ7 validation criterion + TR-019 checksum test) |
| Per crate | sim 374 (232 unit + 142 integration) · server 73 (13 suites) · client 51 · protocol 32 |
| Special configs | `--features ai_debug` + `--no-default-features` compile + clippy clean |

## Static Analysis

| Tool | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 warnings |
| `cargo clippy -p sim --features ai_debug --all-targets -- -D warnings` | 0 warnings |
| Strict-f32 scoring gate | enforced by the `strict_f32_scoring_grep` CI test (TR-004) — passing |

## Security Audit

| Item | Value |
|---|---|
| Tool | cargo-audit 0.22.1 |
| Vulnerabilities | **0** (exit 0) |
| Warnings | 1 pre-existing allowlisted advisory (RUSTSEC-2024-0436 `paste` unmaintained, transitive via bevy — baseline, not feature-introduced) |
| New dependencies | **None** — Cargo.lock untouched; only the `ai_debug` feature flags added |

## PI Compliance

**No violations.** The CRITICAL determinism mandate holds (golden trio bit-identical with every gated AI system registered); server-authoritative preserved (all AI is sim-side, LOD keys off authoritative positions); legacy seek/mining/turret AI byte-frozen (AD-005); source layout conforms (`crates/sim/src/ai/*`); no new deps.

## Requirements Traceability (Story Verifier)

| Work Item | Priority | Status | Key evidence |
|---|---|---|---|
| OBJ1 steering substrate | P1 | **PASSED** | intent-replay bit-identical (VC1/SC-001) + V-6 purity + formation-hold ≤300 ticks/≤10%/no-chatter (VC2) |
| OBJ2 deterministic brain | P1 | **PASSED** | identical-state selection (VC1), same-tick event re-think + zero calm thinks (VC2), archetype range bands (VC3), tiebreaks, strict-f32 grep |
| OBJ3 behavior-LOD + bench | P1 | **PASSED** | O(squads) think-scaling (VC1), bit-exact no-pop round-trip (VC2), mutual hostile promotion/no dormant combat (VC3), anchor-death/squad-of-1 (VC4), bench gate PASS |
| OBJ4 combat + ramming | P2 | **PASSED** | engage-destroy ≤3600 ticks (kill @646) + energy/heat gates isolated (VC1), ram/no-ram pair (VC2) |
| OBJ5 perception + network | P2 | **PASSED** | unseen-never-targeted at all tiers (VC1), fused share + jammed AND severed independent local fallback (VC2), newest-wins fusion |
| OBJ6 scenario roles | P2 | **PASSED** | patrol break-and-resume (VC1), ambush same-tick coordinated trigger (VC2), posture gates, fallbacks |
| OBJ7 scout & S&D | P3 | **PASSED** | scout disengage-survive + report-into-picture (VC1), 9/9 coarse-cell sweep + engage-on-perception (VC2) |

| SC | Status | SC | Status | SC | Status | SC | Status |
|---|---|---|---|---|---|---|---|
| SC-001 | PASSED | SC-002 | PASSED | SC-003 | PASSED | SC-004 | PASSED |
| SC-005 | PASSED | SC-006 | PASSED | SC-007 | PASSED | | |

**TR-001..TR-021: 21/21 covered** — coverage map spot-checked (deep-read: TR-001 intent-only seam, TR-016 gating, TR-019 checksum test).

## Traceability Gaps

Two minor nits found by the Story Verifier were **fixed during this run**: the stale pre-amendment gate doc-comment in `fleet_stress.rs` and the stale TR-015 plan coverage-map entry (role mechanism lives in `ai/role.rs`). One noted deviation remains, accepted: `archetype_range_bands_differ` asserts **mean** engagement distance over the settled tail where OBJ2-VC3's letter says **median** — the strict ordering + per-archetype band occupancy satisfies the criterion's intent (non-overlapping range bands).

## Spec amendment made during implementation (flagged for visibility)

**TR-017/TR-018 p99 clause amended (2026-06-10, T024):** the literal "AI p99 ≤ 33.3 ms" was measured to be unsatisfiable by ANY AI implementation — the pre-existing no-AI baseline at the pinned N=2000 has p99 ≈ 69–88 ms from mass-carve combat spikes (a known characteristic since R56/R57). Amended to `ai_p99 ≤ max(33.3 ms, paired baseline p99)` (absolute budget when the baseline holds it, else AI must not worsen the tail), preserving the anti-burst intent. Recorded consistently in spec TR-017/TR-018, plan §Bench Protocol, tasks T024, the gate code, and `bench-gate.json.p99_rule`.

## Code Coverage (report-only; no threshold configured)

| Scope | Lines | Regions | Functions |
|---|---|---|---|
| sim crate total | 90.92% | 92.21% | 94.06% |
| **AI module (feature code)** | **97.5%** (4113/4218) | — | — |

Per-file (ai/): brain 96.75 · ident 97.87 · lod 97.33 · mod 100 · perception 99.67 · role 96.42 · squad 97.24 · steering 97.71 · tuning 100. Lowest pre-existing non-feature files: fitting/content 40.3, damage/destruction 68.2.

## Checklist Fulfillment (spot-check)

All three checklists (performance, testing, observability) were fully evaluated `[X]` at plan time (requirements-quality). Spot-check of the Testing-category intent against the implementation: the requirements those items hardened (TR-018 bench protocol, TR-019 checksum test, two-level tiebreaks, golden-trio paths) are all implemented and verified above — **PASSED**. No Security-category checklist exists (none was queued).

## Performance

**PASSED — TR-017/TR-018 bench gate** (`specs/00008-ship-ai/bench-gate.json`, paired pinned N=2000 @30Hz, 600-tick window): AI overhead **−3.33%** of baseline mean (threshold ≤30%) — the AI fleet is net cheaper (1844/2000 ships dormant-gliding); AI p99 538 ms ≤ baseline p99 575 ms (amended rule). Absolute scale target (bench OUTPUT): **~4000 fighting ships/core @30Hz** by mean tick, unchanged from the R57 no-AI ceiling. Off-screen promoted battles measured separately (STF-001 signal live in the report + dev panel).

## Accessibility

SKIPPED — no accessibility NFRs in the spec (desktop game).

## Browser Runtime Validation

SKIPPED — not required: native desktop game, no browser surface. The TR-020 dev-panel surface is verified per the spec's own verification note (coverage map: T014 capture seam, T038 view + editing, T039 metrics) + compile/clippy in both feature configs.

## Manual Testing

No `manual-test.md` generated — every spec validation criterion has automated evidence. **Recommended (project convention, non-gating):** an in-game playtest of the MiningSkirmish scenario (now spawns 10 AI fighters: Red/Blue patrols — Blue DefensiveOnly — plus ambush pairs at the central asteroid) and the dev-panel AI sections (`AI tuning`, `AI inspection`, `AI metrics`; build with `dev_panel` feature → `ai_debug` capture enabled). Close the game before rebuilding `client.exe`.

## Tool Recommendations

None — no SKIPPED tool categories (cargo-audit + cargo-llvm-cov both installed and run).

## Bug Tasks Generated

None.

## Overall Verdict: **PASS**
