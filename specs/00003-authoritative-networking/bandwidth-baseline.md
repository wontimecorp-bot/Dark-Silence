# Bandwidth + Encode-Cost Baseline (E003, SC-005)

Recorded-only figure. There is **no budget gate** here — the per-client byte
budget is owned by E009, which optimizes against this baseline. The PRESENCE of
this artifact is what asserts-as-satisfied SC-005.

- **Date**: 2026-06-02
- **Source test**: `crates/server/tests/bandwidth.rs` →
  `bandwidth_baseline_over_renet_udp_30s_window` (T070), run via
  `cargo test -p server --features udp bandwidth -- --nocapture`.

## Captured metric line

```
BANDWIDTH_BASELINE transport=renet-udp-unsecure clients=2 server_bot_ships=4 entities=84 ticks=900 window_secs=30 mean_bytes_per_client_per_sec=29317.8 peak_bytes_per_client_per_sec=32410 encode_us=33.579 wall_ms=8616
```

| Metric | Value |
|--------|-------|
| Mean bytes/client/sec (out, application payload) | 29317.8 |
| Peak bytes/client/sec (worst 1 s window, out, payload) | 32410 |
| Per-client snapshot encode cost (µs) | 33.579 |
| Entities in baseline world (≈ 6 ships + projectiles) | 84 |

## Fixed scenario parameters

- **Clients**: 2 networked bot clients (fixed scripted input loops: one
  thrust-forward + fire, one strafe + fire) over the **renet UDP** path.
- **Server bot ships**: 4 server-controlled bot ships at fixed spread positions
  (thrust + turn + fire), giving ≈ 6 ships plus their projectiles.
- **Window**: 900 ticks = 30 s simulated @ the fixed 30 Hz authoritative tick
  rate; snapshots broadcast at the baseline **20 Hz** snapshot rate. The 30 s is
  *simulated* (stepped, not slept); `wall_ms` is the real harness wall time.
- **Seed**: fixed initial world seed (fixed positions/intents ⇒ reproducible
  run-to-run).
- **Transport**: renet UDP, unsecure path (the payload byte figure excludes
  UDP/IP + netcode framing per TR-046(b), so it is transport-security
  independent).
- **Encode cost**: timed over the free `encode_snapshot` on the baseline-scale
  world (TR-047), averaged across the 2 clients × 256 samples each.

## Note

Recorded-only — no pass/fail budget gate. This is the figure E009 optimizes
against. Re-running the test re-emits the same `BANDWIDTH_BASELINE` line (modulo
the timing-dependent `encode_us` / `wall_ms` measurements, which vary slightly
with host load; the payload byte figures are deterministic for the fixed seed).
