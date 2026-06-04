//! T070 {TR-042,TR-046} [COMPLETES TR-042] [COMPLETES TR-046] — the fixed,
//! repeatable bandwidth baseline session over the renet UDP path (SC-005).
//!
//! TR-042 fixes the baseline session: **2 networked bot clients + 4
//! server-controlled bot ships** (≈ 6 ships plus their projectiles), each bot a
//! **fixed scripted input loop**, run for a **fixed 30 s window** at the baseline
//! 20 Hz snapshot rate from a fixed initial world seed. This test runs that
//! session over the real renet UDP adapter (T041 path) and measures, per the
//! reproducibility spec (TR-046):
//!   - **mean + peak bytes/client/sec, direction = out, application payload** via
//!     the server's [`BandwidthMeter`] / the transport `NetStats` (encoded
//!     `protocol` message bytes — EXCLUDING UDP/IP + netcode framing, TR-046(b));
//!   - **per-client snapshot encode cost** (µs) timed over the baseline world via
//!     the free `encode_snapshot` (TR-047).
//!
//! The figures are emitted as a **structured test log line** so SC-005 is
//! asserted-as-satisfied by the figure's PRESENCE. This is **recorded-only**:
//! there is NO pass/fail budget gate (the per-client byte budget is E009). The
//! test asserts only that a figure was captured and that the scenario parameters
//! held (2 bots + 4 ships, 900 ticks, fixed seed).
//!
//! The 30 s window is **simulated**: the fixed 30 Hz tick is stepped 30 Hz × 30 s
//! = 900 ticks (snapshots broadcast at the 20 Hz rate); the renet pump is driven
//! with dt = 1/30 plus minimal real micro-sleeps for socket delivery — it does
//! NOT sleep a real 30 s. The payload byte figure excludes transport headers, so
//! the **unsecure** renet path is used for speed (noted; the payload figure is
//! transport-security-independent, TR-046(b)).
#![cfg(feature = "udp")]

mod botkit;

use std::time::Instant;

use botkit::{forward_intent, renet_harness, InputScript};
use glam::Vec2;
use server::{encode_snapshot, EncodeParams};

/// The fixed 30 s window as ticks at the 30 Hz authoritative rate (TR-042).
const WINDOW_SECS: f32 = 30.0;
const TICK_RATE_HZ: u32 = 30;
const TICKS: u32 = (WINDOW_SECS as u32) * TICK_RATE_HZ; // 900 ticks
/// Number of server-controlled bot ships (TR-042: 4, alongside the 2 bot clients).
const SERVER_BOT_SHIPS: usize = 4;
/// How many times the encode is timed per client to smooth the per-client
/// encode-cost figure (recorded-only; not a gate).
const ENCODE_SAMPLES: u32 = 256;

/// The two networked bot clients' scripted input loops (fixed, repeatable): one
/// thrusts forward + fires (so projectiles populate the world), one strafes +
/// fires. Fixed loop ⇒ a reproducible session run-to-run.
fn client_scripts() -> Vec<InputScript> {
    let forward_fire = protocol::QuantizedIntent {
        fire: true,
        ..forward_intent()
    };
    let strafe_fire = protocol::QuantizedIntent {
        forward: 0,
        strafe: 1,
        turn: 0,
        fire: true,
        toggle_assist: false,
        afterburner: false,
    };
    vec![
        InputScript::constant(forward_fire),
        InputScript::constant(strafe_fire),
    ]
}

/// A fixed scripted intent for a server-controlled bot ship (thrust + fire so it
/// moves and emits projectiles — populating the ≈ 6 ships + projectiles world).
fn bot_ship_intent() -> sim::ShipIntent {
    sim::ShipIntent {
        forward: 1.0,
        strafe: 0.0,
        turn: 1.0,
        fire: true,
        toggle_assist: false,
        afterburner: false,
    }
}

#[test]
fn bandwidth_baseline_over_renet_udp_30s_window() {
    // --- Assemble the fixed baseline session over the renet UDP path. --------
    // 2 networked bot clients (renet) ...
    let mut harness = renet_harness(client_scripts());
    assert_eq!(
        harness.bot_count(),
        2,
        "TR-042 fixes the session at 2 networked bot clients"
    );
    assert_eq!(
        harness.server.rates().tick_rate_hz as u32,
        TICK_RATE_HZ,
        "the baseline runs at the fixed 30 Hz tick rate"
    );

    // ... + 4 server-controlled bot ships at fixed spread positions (≈ 6 ships).
    // Fixed positions/intents ⇒ a fixed initial world seed (TR-042).
    for k in 0..SERVER_BOT_SHIPS {
        let angle = std::f32::consts::TAU * (k as f32) / (SERVER_BOT_SHIPS as f32);
        let pos = Vec2::new(20.0 * angle.cos(), 20.0 * angle.sin());
        harness.server.spawn_bot_ship(pos, bot_ship_intent());
    }

    // --- Run the fixed 30 s window (900 ticks; SIMULATED, not a real 30 s). ---
    let wall_start = Instant::now();
    harness.run_ticks(TICKS);
    let wall_elapsed = wall_start.elapsed();

    // --- Measure mean + peak bytes/client/sec (out, application payload). -----
    // The server `BandwidthMeter` credits the encoded payload bytes of every
    // snapshot SEND per connection (direction = out); the transport `NetStats`
    // agree the bytes really crossed the wire. Mean over the fixed 30 s window =
    // total payload bytes / window seconds; peak = the meter's worst 1 s window.
    let n = harness.bot_count();
    let mut per_client_mean = Vec::with_capacity(n);
    let mut per_client_peak = Vec::with_capacity(n);
    for i in 0..n {
        let (_rolling_mean, peak, total) = harness.bot_bandwidth(i);
        let mean_over_window = total as f32 / WINDOW_SECS;
        per_client_mean.push(mean_over_window);
        per_client_peak.push(peak);

        // The transport `NetStats` (bytes the client actually RECEIVED, out from
        // the server) confirm real payload bytes crossed the renet UDP wire.
        let stats = harness.bot_stats(i);
        assert!(
            stats.bytes_in > 0,
            "bot {i} received snapshot payload bytes over the renet UDP wire: {stats:?}"
        );
        assert!(
            total > 0,
            "bot {i} was credited outbound snapshot payload bytes (meter): {total}"
        );
    }

    // Fleet figures: mean = average per-client mean; peak = worst single-client
    // 1 s window (bytes/client/sec, out, payload).
    let mean_bytes_per_client_per_sec = per_client_mean.iter().copied().sum::<f32>() / n as f32;
    let peak_bytes_per_client_per_sec = per_client_peak.iter().copied().max().unwrap_or(0);

    // --- Measure per-client snapshot ENCODE cost (µs) over the baseline world. -
    // TR-047: per-client encode cost is timed over the free `encode_snapshot` on
    // the real baseline-scale world (≈ 6 ships + projectiles). Recorded-only.
    let current = harness.server.current_full_state();
    let entity_count = current.len();
    let mut encode_us_total = 0.0f64;
    for i in 0..n {
        let recipient_id = harness.bot_local_id(i);
        let recipient_pos = current
            .get(recipient_id)
            .map(|r| r.pos.dequantize_pos())
            .unwrap_or(Vec2::ZERO);
        let params = EncodeParams {
            server_tick: harness.server.server_tick(),
            acked_input_seq: 0,
            baseline_id: protocol::Snapshot::KEYFRAME_BASELINE,
            keyframe: true,
            recipient_id: Some(recipient_id),
            recipient_pos,
        };
        let start = Instant::now();
        for _ in 0..ENCODE_SAMPLES {
            let snap = encode_snapshot(&current, &protocol::FullState::new(), params);
            std::hint::black_box(&snap);
        }
        let per_client_us = start.elapsed().as_secs_f64() * 1.0e6 / ENCODE_SAMPLES as f64;
        encode_us_total += per_client_us;
    }
    let encode_us = encode_us_total / n as f64;

    // --- Emit the figures as a structured test log line (TR-046(c), SC-005). ---
    // The PRESENCE of this line is the SC-005 artifact. `--nocapture` surfaces it;
    // it is also asserted-as-captured below.
    println!(
        "BANDWIDTH_BASELINE transport=renet-udp-unsecure clients={n} \
         server_bot_ships={SERVER_BOT_SHIPS} entities={entity_count} ticks={TICKS} \
         window_secs={WINDOW_SECS} \
         mean_bytes_per_client_per_sec={mean_bytes_per_client_per_sec:.1} \
         peak_bytes_per_client_per_sec={peak_bytes_per_client_per_sec} \
         encode_us={encode_us:.3} wall_ms={:.0}",
        wall_elapsed.as_secs_f64() * 1000.0
    );

    // --- Recorded-only assertions: a figure was captured + params held. -------
    // NO pass/fail BUDGET gate (the byte budget is E009). We assert only that the
    // figures are REAL (non-zero, finite) and the scenario parameters were fixed.
    assert!(
        mean_bytes_per_client_per_sec.is_finite() && mean_bytes_per_client_per_sec > 0.0,
        "a real mean bytes/client/sec figure must be captured: \
         {mean_bytes_per_client_per_sec}"
    );
    assert!(
        peak_bytes_per_client_per_sec > 0,
        "a real peak bytes/client/sec figure must be captured: \
         {peak_bytes_per_client_per_sec}"
    );
    assert!(
        encode_us.is_finite() && encode_us > 0.0,
        "a real per-client encode-cost figure (µs) must be captured: {encode_us}"
    );
    // Scenario parameters held: ≈ 6 ships (2 clients + 4 bot ships) + projectiles.
    assert!(
        entity_count >= 6,
        "the baseline world holds ≥ 6 ships (2 clients + 4 bot ships) plus \
         projectiles: entities={entity_count}"
    );
    assert_eq!(
        harness.server.session().client_count(),
        2,
        "the 2 networked bot clients stayed connected for the whole window"
    );
}
