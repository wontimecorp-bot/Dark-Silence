# QC Report: E007 — Damage & Destruction

**Feature**: `00007-damage-destruction` | **Date**: 2026-06-02 | **Auditor**: QCAuditor
**Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md) | **Tasks**: [tasks.md](tasks.md)

## Overall Verdict: PASS

All required QC categories (linting, security, performance) plus compilation and the full test suite pass. Zero CRITICAL/ERROR/WARNING findings. One informational, non-gating, non-actionable advisory noted.

Toolchain: `stable-x86_64-pc-windows-msvc` (active), cargo 1.95.0. All cargo commands run with `CARGO_HTTP_CHECK_REVOKE=false`, sandbox disabled.

---

## Compilation: PASSED

- Command: `cargo build --workspace`
- Result: `Finished dev profile`. All four crates (`sim`, `protocol`, `server`, `client`) build clean.

## Lint / Static Analysis: PASSED

- Tool: Clippy — `cargo clippy --workspace --all-targets -- -D warnings`
- Result: exit 0, zero warnings (forced recompile of `sim`/`damage` modules across the full workspace; not a stale cache hit).
- Tool: rustfmt — `cargo fmt --check`
- Result: exit 0, zero diffs.
- Policy match: `project-instructions.md` Linting/Formatting (Clippy `-D warnings` + rustfmt) — satisfied.

## Security: PASSED

- Tool: `cargo audit` — exit 0.
- Advisory DB: 1102 advisories loaded; 611 crate dependencies scanned.
- **Vulnerabilities: 0.**
- **Informational warnings: 1 (non-gating, non-actionable)** — RUSTSEC-2024-0436, `paste 1.0.15`, status **unmaintained** (NOT a vulnerability). Transitive dependency via `wgpu-hal`/`wgpu-core`/`bevy_render` → client only. No first-party code path touches it; nothing actionable in this codebase.
- Audit triage config (`.cargo/audit.toml`) reviewed: `ignore` list is **empty** (no advisory IDs suppressed); only `unmaintained`/`unsound`/`yanked` are classed informational/non-gating; never blanket-ignores high/critical. The `paste` warning surfaced correctly — no real vulnerability is hidden.
- Server-authoritative crash-resistance (security-relevant) spot check on the new `crates/sim/src/damage/` surface:
  - No `unsafe` blocks.
  - No production `panic!`/`unreachable!`. The two `.unwrap()` calls (`event.rs:122-123`) are inside a `#[cfg(test)]` serde round-trip. The single production `.expect()` (`destruction.rs:90`) is provably guarded by the `core_after.is_none()` early-return at line 85 (documented SAFETY note) — not reachable on untrusted input.
  - All missing-resource/missing-component paths (`apply_damage`, `on_section_destroyed`, `shield_regen_system`) degrade gracefully via `let-else`/early-return — never a crash.

## Performance: PASSED (observation noted, not a blocker)

- Policy names performance (sim/replication hot paths). No dedicated benchmarks exist this epic — consistent with the plan (the damage pipeline is pure/deterministic per-hit math; connectivity runs only on destruction events).
- Code-level confirmation of the hot-path posture:
  - Per-frame damage systems are limited to `shield_regen_system` (gated `run_if(resource_exists::<ShieldConfig>)`) and the `Changed<FitLayout>` emergent re-derive (change-driven, not O(world)/tick).
  - The connectivity flood-fill (`connected_region`) and `sever_chunk` run **only** inside `on_section_destroyed`, invoked solely from the collision/destruction trigger when a module is destroyed (INV-D08) — never per-frame.
  - `apply_damage` / `resolve_penetration` / `layer_resist` are pure per-hit functions; cost scales with hits, not world size.
- No hot-path concern found. Observation: add explicit `criterion` benchmarks for `apply_damage` and the sever flood-fill if a future epic puts dozens of simultaneous destruction events on the tick. Not required by this epic's scope.

## Tests: PASSED

- Runner: `cargo test --workspace`. **Total: 275; Passed: 275; Failed: 0; Ignored: 0.**
- Per-crate (independently counted from the per-binary `test result:` lines):
  - **sim — 148**: lib 81, `damage.rs` 20, `fitting.rs` 33, `gameplay.rs` 11, `physics_swap.rs` 3.
  - **server — 69**: lib 39, `capacity_mtu` 2, `determinism` 1, `dos_guard` 6, `harness` 10, `rates` 2, `session` 3, `validation` 4, `validation_parity` 2 (+ `bandwidth`/`equivalence` 0).
  - **protocol — 31**: lib 14, `quantize` 6, `roundtrip` 11 (+ `isolation`/`renet_udp`/`secure_connect` 0).
  - **client — 27**: lib 17, `interpolation` 4, `prediction` 2, `reconciliation` 2, `snapshot_order` 2.
- Counts match the orchestrator's reported numbers (sim 148 / server 69 / protocol 31 / client 27).
- Mandatory-to-cover invariants (`project-instructions.md`): the `sim` integrator↔analytic equivalence is covered (`server/tests/equivalence.rs` present; `gameplay.rs` `fixed_step_is_bit_identical...`, motion/flight tests green). The `transit` promote/demote round-trip invariant is N/A — no `transit` crate exists in the current workspace.
- E006/E002 regression (the BREAKING `derive_ship_stats(+&FitLayout)` ripple): `fitting.rs` 33/33 green, including `baseline_seed_fit_reproduces_tuning_defaults` (baseline holds at full module health) and the unfitted-target gameplay path.

## Code Coverage: SKIPPED (by policy)

- `project-instructions.md`: no global coverage gate. Damage invariants are covered by the `damage.rs` unit/integration suite (20 tests spanning penetration tiers, matrix traversal, shields, emergent stats, sever/COM-momentum, salvage, and the non-degenerate-matrix guard). No threshold to enforce.

---

## Findings

| # | Severity | Category | Location | Finding | Action |
|---|----------|----------|----------|---------|--------|
| 1 | INFO | security | transitive (`paste 1.0.15` via wgpu/bevy) | RUSTSEC-2024-0436 — `paste` unmaintained (status notice, not a vuln) | None — not first-party, non-gating, already classed informational in `.cargo/audit.toml`. Track for follow-up. |
| 2 | INFO | performance | `crates/sim/src/damage/` | No dedicated benchmarks this epic | None required — hot-path posture verified in code (per-hit pure math; severing only on destruction). Consider `criterion` benches in a future high-destruction-load epic. |

No CRITICAL, ERROR, or WARNING findings. No `project-instructions.md` violation.

## Provisioning Actions

- No tools installed. Clippy, rustfmt, `cargo audit` all present.
