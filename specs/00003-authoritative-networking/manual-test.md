# Manual Playtest Checklist — E003 Authoritative Networking

**Non-blocking.** QC PASSED on all automated checks; these are runtime/visual/feel confirmations that cannot run headlessly (the windowed Bevy client). They map to the OD-001 "tune in networked playtest" items. Record observations; file follow-ups only if behavior diverges from the headless proofs.

## Setup
- Toolchain: `rustup show` → `stable-x86_64-pc-windows-msvc`.
- Run cargo with `CARGO_HTTP_CHECK_REVOKE=false` and sandbox disabled (build-env, see `crates/protocol/README.md`).

## 1. Windowed client solo-loopback play
- **Run**: `CARGO_HTTP_CHECK_REVOKE=false cargo run -p client` (debug window opens; embedded `ServerApp::loopback()` drives the world).
- **Verify**: the ship flies with E002 feel (thrust/rotate/strafe, flight-assist toggle, fixed-forward fire, gunsight pip); no visible input lag (local ship is predicted); targets/asteroids present and shootable; camera follow + zoom intact.
- **Expected**: identical to E002 single-player feel — the netcode is transparent in solo loopback.

## 2. Networked remote-ship visuals (2-client renet session)
- The headless harness proves remote interpolation numerically; this confirms the **on-screen** path.
- **Note (known gap)**: `net_update` currently spawns a `Transform` marker for newly-appeared remotes **without a mesh**. In 1-ship solo loopback this never manifests. A real 2-client renet session would show remotes as position-only markers until a remote-mesh render prefab is attached.
- **Action**: when a 2-client launcher exists (or attach a temporary second client over `127.0.0.1` secure transport), verify remote ships render with meshes and interpolate smoothly (~100 ms behind). If meshes are absent, that render-prefab wiring is a small follow-up (render-only; logic is done/tested).

## 3. Reconciliation correction feel
- **Verify**: under induced misprediction (e.g. ram/collision resolved server-side), the local ship corrects **smoothly** (no teleport/snap), blended over a few frames.
- **Note**: `RenderSmoother::step` runs per render frame (Update) rather than per fixed tick — the no-teleport/non-oscillation contract is asserted per-tick in `client/tests/reconciliation.rs`, but confirm the on-screen smoothing reads well; adjust `MAX_SNAP_FRACTION` / `MAX_SMOOTH_TICKS` (OD-001) in playtest if needed.

## Cleanup
- Close the client window; no background processes persist.
