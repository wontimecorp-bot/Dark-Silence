# Analysis Report: 00008-ship-ai

> Date: 2026-06-10 | Mode: analyze + apply-all remediation | Artifacts: spec.md (clarified), plan.md, tasks.md, data-model.md, research.md, checklists/ (3, all evaluated)

## Findings Table

| ID | Category | Severity | Location(s) | Summary | Recommendation |
|----|----------|----------|-------------|---------|----------------|
| A-001 | Coverage | MEDIUM | spec Requirements; tasks T035/T036 | OBJ7 (scout/S&D) has NO requirement id — T035/T036 carry no `{TR}` tags (bidirectional traceability gap; only OBJ-VC/SC-007 trace) | Add **TR-021** (scout + S&D behaviors); tag T035/T036; add plan coverage row |
| A-002 | Duplication | MEDIUM | spec: Constraints, TR-017, TR-018, SC-003 (+OBJ3-VC1) | The ≤30% gate restated in ~5 places with differing parameter subsets (single-source drift risk); p99 budget only in TR-018 | Make TR-017 the canonical gate (incl. p99); other sites cite "per TR-017/TR-018" |
| A-003 | Contradiction | MEDIUM | SC-003/TR-017/Constraints vs TR-018 | Numerator wording mismatch: "think + steering + perception" vs TR-018's six in-numerator buckets (same run passes one wording, fails the other) | Harmonize to "all AI-attributable buckets per TR-018" |
| A-004 | Contradiction | MEDIUM | Scope/Excluded bullet 1 vs TR-020 | Excluded defers the brain-model choice to plan, but TR-020 (back-propagated) mandates utility-model internals — reads as self-contradiction | Annotate the bullet: decided in plan (ADR-0015); TR-020 reflects it |
| A-005 | Underspecification | MEDIUM | TR-020 | Orphan requirement: no parent OBJ/VC/SC references it (verification exists only in plan coverage + T014/T038/T039) | Add an in-requirement verification annotation |
| A-006 | Consistency | LOW | plan TR-019 row vs Project Structure | Test location hedge ("or crates/server/tests/") vs the pinned `crates/sim/tests/ai.rs` | Pin to `crates/sim/tests/ai.rs` |
| A-007 | Ambiguity | LOW | spec TR-008 | "bounded ε" de-penetration nudge — bound never quantified/named | Name an `AiTuning` field (`promote_nudge_max`) + add to data-model |
| A-008 | Ambiguity | LOW | spec TR-013/OBJ5 | Tilde cadences ("~0.5 s", "2–5 s") not anchored to a pinning artifact | Cite the `AiTuning` defaults (data-model) |
| A-009 | Ambiguity | LOW | spec TR-020(a) | "bounded recent transition history" — bound unspecified | Pin: `AiTuning.debug_history_len`, default 16 |
| A-010 | Consistency | LOW | spec SC-004 | Archetype-differentiation clause is OBJ2-VC3's outcome filed under OBJ4's SC | Annotate "(per OBJ2-VC3)" |
| A-011 | Consistency | LOW | spec SC-003 vs TR-008 | "no positional pop" stated without TR-008's ε-nudge allowance | Cite "per TR-008's definition" |
| A-012 | Clarity | LOW | spec TR-017/TR-018 (R57), TR-020 (AD-006) | External IDs unglossed at first use (resolve in-repo; spec not self-contained) | One-line glosses at first use |
| A-013 | Clarity | LOW | spec Clarifications Q5 | Session log says "≤ ~30%"; Constraints pin 30.0% — literal-reader mismatch | Append "(later pinned to exactly 30.0%)" annotation |
| A-014 | Coverage | INFO | spec TR-003 | Inertia-awareness has no direct micro-criterion (covered indirectly by T008 trajectory tests) | Accept — no change |
| A-015 | Layout | INFO | plan Project Structure | `crates/server/examples/fleet_stress.rs` outside `src/` — pre-existing idiomatic Cargo | Accept — no change |
| A-016 | Duplication | INFO | spec TR-013 vs TR-014 | Clean separation confirmed (gating vs fusion); only narrative overlap in OBJ5 | Accept — optional tightening skipped |

No CRITICAL or HIGH findings. No `[NEEDS CLARIFICATION]` markers. No artifact-convention violations (IDs intact, checkbox states valid, required sections present, task grammar conforms).

## Quality Summaries

- **Spec Quality** (Spec Validator, read-only): **PASS, 25/27** — two criteria degraded by the TR-020 amendment (orphan verification; impl-detail vocabulary), both remediated below. Structure confirmed: TR-001..020 sequential, SC-001..007 parented, STF-001 resolved-with-trigger, all mandatory technical sections present.
- **Compliance** (Policy Auditor on plan.md): **PASS — 0 violations.** Amendments verified consistent (player-local gate uniform across 5 sites; ShipIntent seam holds incl. AD-004 Mid-tier; AD-006/TR-020 capture gated out of the measured path; STF-001 Complexity Tracking intact). 1 LOW (TR-019 location hedge) + 1 INFO (examples/ path, pre-existing).

## Coverage Summary

| Requirement | Has Task? | Task IDs | Notes |
|---|---|---|---|
| TR-001 | ✓ | T007, T008, T013 | [COMPLETES] on T013 |
| TR-002 | ✓ | T006–T009 | [COMPLETES] on T009 |
| TR-003 | ✓ | T006, T008 | 2 tasks — no marker required |
| TR-004 | ✓ | T003, T010, T015 | [COMPLETES] on T015 |
| TR-005 | ✓ | T003, T011, T015, T029 | [COMPLETES] on T029 |
| TR-006 | ✓ | T012, T028 | 2 tasks |
| TR-007 | ✓ | T004, T005 | 2 tasks |
| TR-008 | ✓ | T019, T020 | [COMPLETES] on T020 |
| TR-009 | ✓ | T016, T017, T021 | [COMPLETES] on T021 |
| TR-010 | ✓ | T016, T018, T021 | [COMPLETES] on T021 |
| TR-011 | ✓ | T025, T026, T028 | [COMPLETES] on T028 |
| TR-012 | ✓ | T027, T028 | 2 tasks |
| TR-013 | ✓ | T019, T029, T031 | [COMPLETES] on T031 |
| TR-014 | ✓ | T030, T031 | 2 tasks |
| TR-015 | ✓ | T032–T034 | [COMPLETES] on T034 |
| TR-016 | ✓ | T001, T040 | [COMPLETES] on T040 |
| TR-017 | ✓ | T022, T024 | [COMPLETES] on T024 (the P1→P2 bench gate) |
| TR-018 | ✓ | T022, T023 | [COMPLETES] on T023 |
| TR-019 | ✓ | T037 | single task |
| TR-020 | ✓ | T014, T038, T039 | [COMPLETES] on T039 |
| TR-021 *(added by remediation)* | ✓ | T035, T036 | [COMPLETES] on T036 — closes A-001 |

## Instructions Alignment Issues

None. STF-001 (off-screen war vs Principle III) remains the single documented, justified exception (Complexity Tracking + ADR-0015 + measurable revisit trigger).

## Unmapped Tasks

T035/T036 (pre-remediation; OBJ7 untagged) — resolved by TR-021. T002 (Setup, exports `AiTuning`) is phase-exempt by rule.

## Metrics

- **Requirements**: 20 → **21** (TR-021 added) | **Tasks**: 40 | **Coverage**: 100% bidirectional (post-remediation)
- **Findings**: 0 CRITICAL · 0 HIGH · 5 MEDIUM · 8 LOW · 3 INFO — **13 remediated, 3 accepted (INFO)**

## Remediation Log (apply-all)

| # | Finding | File(s) | Change | Status |
|---|---------|---------|--------|--------|
| 1 | A-001 | spec.md, plan.md, tasks.md | TR-021 added; T035/T036 tagged (+[COMPLETES TR-021]); coverage row | Applied |
| 2 | A-002/A-003 | spec.md | TR-017 = canonical gate (p99 + buckets-per-TR-018); Constraints + SC-003 cite it | Applied |
| 3 | A-004 | spec.md | Excluded bullet annotated (ADR-0015 resolution) | Applied |
| 4 | A-005/A-009/A-012 | spec.md | TR-020 verification annotation + history bound + AD-006 gloss | Applied |
| 5 | A-006 | plan.md | TR-019 row pinned to `crates/sim/tests/ai.rs` | Applied |
| 6 | A-007 | spec.md, data-model.md | ε → `AiTuning.promote_nudge_max` (added to AOI group) | Applied |
| 7 | A-008 | spec.md | TR-013 cites AiTuning cadence defaults | Applied |
| 8 | A-010/A-011 | spec.md | SC-004 OBJ2-VC3 note; SC-003 TR-008 citation | Applied |
| 9 | A-012 | spec.md | R57 gloss at TR-018 first use | Applied |
| 10 | A-013 | spec.md | Q5 pinned-value annotation | Applied |
| 11 | A-014/A-015/A-016 | — | Accepted, informational | Skipped (by design) |
