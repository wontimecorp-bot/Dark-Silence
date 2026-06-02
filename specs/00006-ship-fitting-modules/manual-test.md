# Manual Playtest Checklist — E006 Ship Fitting & Modules

**Non-blocking.** QC PASSED on all automated checks. These are the runtime/visual surfaces that can't run headlessly (the Bevy fitting screen + the windowed ship flying under its fit). The fitting/stats/layout/preset *logic* is proven by 33 headless tests; this confirms the on-screen experience.

## Setup
- Toolchain: `rustup show` → `stable-x86_64-pc-windows-msvc`.
- Run with `CARGO_HTTP_CHECK_REVOKE=false` + sandbox disabled (build-env, see `crates/protocol/README.md`).
- `CARGO_HTTP_CHECK_REVOKE=false cargo run -p client`.

## 1. The fitting screen (FR-012/009/013/024)
- **Press Tab** to toggle Flying ⇄ Fitting.
- **Select a module**: R reactor, T thruster, G weapon (autocannon), H shield, J armor, K utility.
- **Install**: press a slot key **1–7** to place the selected module into that slot.
- **Verify**:
  - Live **power / CPU / mass budget bars** fill as modules are added and turn **red** when an axis goes over capacity.
  - A **before-commit preview** line shows the resulting top-speed / turn / mass with green (+) / amber (−) deltas and ARMED / NO WEAPON.
  - An invalid placement (wrong slot type, too-large module, or over-budget) is **rejected with the reason** shown on the status line (it does not apply).
- **Remove**: press **X then a slot key** to remove a module; the budget should free.
- **Presets**: press **P** to save the current fit, **L** to reload the most recent — status line confirms; reloading onto an incompatible hull is rejected.

## 2. The ship flies under its fit (FR-014, SC-003)
- Back in Flying view, the ship is the **seed fighter** with a starter fit (reactor + 2 thrusters + autocannon).
- **Verify**: it flies sanely — reaches roughly the E002 top speed (~80) and turn rate (~3 rad/s), but with **heavier-feeling acceleration** (the fighter's total fit mass ≈21.5 vs the old unit-mass ship). This heavier feel is the intended "fit drives the ship" payoff (HINT-002 / the flight-feel-regression risk). Space still fires (the autocannon makes the ship armed).
- If the feel is off (too sluggish / too floaty), it's a balance tuning call on the seed fighter's loadout — tell me and I'll adjust the starter fit or the module/hull numbers.

## 3. (Optional) Live re-fitting
- E006 ships a "build a fit, fly it" loop. Editing the fit and seeing flight change **mid-session** (live re-apply to the running ship) is a deeper integration than this epic scoped — note whether you want that wired now or later.

## Cleanup
- Close the window; no background processes persist.
