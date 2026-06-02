# Manual Test: SC-008 "Feels Good" Gate (E002)

The one verification the automated loop cannot perform — the Principle-VII hands-on playtest of flight + combat feel. Everything else (determinism, assist behavior, swept hits, ram, seek, build/lint/security) is covered by the 42-test automated suite and passes.

## Start

```
cd "s:\claudecode\Dark Silence"
cargo run -p client            # MSVC toolchain (already set as the dir override)
# faster iterative builds (dev only): cargo run -p client --features dynamic_linking
```
Readiness: a window opens showing a blue dart (your ship) on a dark plane with a top-left readout (`SPD / ASSIST|MANUAL / HP`) and a centre reticle. Red cubes = dummies, grey spheres = drifting asteroids, a green dart = the seeker.

## Controls

- **W / S** — thrust forward / reverse
- **A / D** — rotate left / right
- **Q / E** — strafe left / right
- **Space** — fire
- **F** — toggle flight-assist (ASSIST ↔ MANUAL)
- **= / -** — zoom in / out

## Scenarios to judge (pass = feels good / behaves as described)

1. **Momentum flight (assist ON, default).** Thrust, then release — the ship coasts and eases toward where it points. Turns feel responsive, drift is readable, no uncommanded skidding. *Pass: weighty but controllable.*
2. **Decoupled flight (press F → MANUAL).** Thrust up to speed, rotate — the ship keeps drifting its original way while the nose points elsewhere; only opposing thrust changes the velocity. *Pass: full Newtonian momentum, no auto-damping.* Toggling F mid-maneuver does **not** snap or jolt velocity.
3. **Frame-rate feel (SC-001 live half).** If you can cap/vary FPS (e.g., 30/60/144 via your GPU control panel or a frame limiter), flight should feel the same — smooth, no stutter, same handling. *Pass: consistent across frame rates.*
4. **Aim & destroy (SC-003/SC-004).** Point the nose at a dummy and fire — hits land and the dummy is destroyed with feedback (HIT/KILL on the HUD). Try a fast pass at a small/edge target — shots still connect (no tunneling).
5. **Lead a drifter (US2 #4).** Fire at a moving asteroid; you may need to lead it. *Pass: a well-led shot connects.*
6. **Ram (SC-005).** Nudge an asteroid slowly → bounce, survive. Charge one at high speed → ship destroyed (`-- SHIP DESTROYED --`). *Pass: believable momentum transfer; lethal ram kills.*
7. **Seeker (SC-007).** The green dart thrusts toward you; lead and destroy it. *Pass: it chases, and you can kill it.*
8. **HUD readability (SC-006).** Speed/assist-mode/HP read at a glance; hit/kill feedback is clear; no number spam.

## Verdict

- Rate flight feel and combat feel **positive / negative**.
- Log any negative findings (tune the magnitudes in `crates/sim/src/tuning.rs` — `Tuning::default()` — and re-run; values are grounded-but-scaled, ADR-0012).
- When satisfied, mark **T043** complete in `tasks.md` (or accept as-is). The slice's purpose is this gate: confirm momentum flight + shooting is fun before building outward.
