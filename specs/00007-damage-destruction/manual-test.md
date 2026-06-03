# Manual Playtest Checklist — E007 Damage & Destruction

**Non-blocking.** QC PASSED on all automated checks (275/275 workspace tests, 0 clippy warnings, 0 vulnerabilities). The damage **system** is fully proven headlessly — 20 `damage.rs` tests including the end-to-end T040 chain (fired projectile → swept hit → `apply_damage` → module damage → emergent `ShipStats` drop → section destroyed → sever → wreck → salvage) and the T041 unfitted-target path. This checklist covers the runtime/visual surfaces that can't run headlessly.

## Setup
- Toolchain: `rustup show` → `stable-x86_64-pc-windows-msvc`.
- Run with `CARGO_HTTP_CHECK_REVOKE=false` + sandbox disabled (build-env).
- `CARGO_HTTP_CHECK_REVOKE=false cargo run -p client`.

## 1. The damage legibility cue (FR-024, SC-005)
- The HUD readout (top-left) shows a terse hit tag refining the existing HIT/KILL flash: **SHIELD** (shield-absorbed), **RICOCHET**, **PEN** (clean penetration), **OVERPEN** (pass-through), **MISS** (hit nothing). No damage numbers — diegetic, presentation-only.
- **Verify**: when a hit resolves on a fitted target, the tag appears alongside the flash and clears when the flash decays.

## 2. ⚠️ Live-visibility prerequisite (read this first)
The damage pipeline is wired into the **shared `sim` fixed step** (`add_fixed_step_systems`), but its systems are `run_if(resource_exists)`-gated so resource-bare worlds (the E002/E003 server/determinism worlds) skip them safely. For damage to be **visible in the windowed client**, the client app must:
1. **Insert the E007 config resources** into its world: `ResistanceMatrix` (`default_resistance_matrix()`), `PenetrationConfig`, `ShieldConfig`, `StatScalingConfig`, `SalvageConfig` (all `::default()`).
2. **Make ships mutually damageable** — give the player + AI ships a `FitLayout` (already attached via the starter fit) plus `Target` + `CollisionRadius` so the new `fitted_damage_system` resolves projectile hits against them.

Until that app-wiring lands, the windowed demo flies and fires exactly as in E006 but ships do **not** take projectile damage on-screen (only asteroids/dummies do, via the unchanged legacy path). **This is the natural "playtest E007" follow-on** — analogous to E006's "live re-fitting" note — and is offered as the next step. Tell me to wire it and you'll be able to shoot enemy ships apart: modules degrade their flight/weapons, sections blow off as drifting chunks, and destroyed ships leave lootable wrecks.

## 3. What to look for ONCE the live wiring is in (the payoff)
- **Hit location matters**: shots into a ship resolve to the module along the flight line; a centrally-buried module is reached only after its cover (angle your shots).
- **Armor angling**: steep glancing hits **ricochet** (RICOCHET tag, little damage); square-on or overmatching hits **penetrate** (PEN). Presenting armor vs. exposing it changes outcomes — the naval-angling feel.
- **The ship gets worse as it's hit**: a damaged thruster visibly lowers top speed / acceleration; a destroyed weapon stops firing; a destroyed reactor collapses power and drops shields.
- **Coming apart**: destroying a connecting section severs the disconnected region into a **drifting chunk** that inherits the ship's momentum + spin (it drifts, never zero-velocity-pops); a core hit destroys the whole ship.
- **Salvage**: a destroyed ship / severed chunk leaves a **persistent lootable wreck** — a clean sever yields an intact module, a through-killed one yields scrap (never empty loot).
- **Feel check (Principle VII)**: does typed-damage angling + emergent degradation feel good? Balance (resistance matrix, penetration tiers, module health, `WeaponSource` channel/pen) is all tunable content — tell me what feels off.

## Known tuning notes (content, not defects)
- **`WeaponSource` is MVP-typed**: the fixed-forward gun is **Kinetic**, `penetration = 3×damage`, `pen_size = 1` (a documented `NEW-CONFIG` seam). Per-weapon channel/pen/size (so different weapons feel different) is a clean later enrichment.
- **Radiation channel** currently has no `HIGH`-resisting layer (max mitigation `MID` 0.40) — it shares the Systems target with EM but is the "softest to resist" channel. Non-degenerate and valid, but a tunable balance lever if Radiation feels too strong/weak.

## Cleanup
- Close the window; no background processes persist.
