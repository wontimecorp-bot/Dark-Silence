//! T048 {TR-036} [COMPLETES TR-036] — remote-entity interpolation under
//! loss/jitter (SC-004).
//!
//! Drives an embedded authoritative `ServerApp` over a **lossy/jittered**
//! [`LoopbackTransport`] ([`LoopbackTransport::with_loss_jitter`], T047): the
//! TR-036 baseline of **5% uniform single-packet loss + ±50 ms jitter**, plus an
//! explicit **scripted consecutive-drop** burst. Two clients connect to one
//! server; one ship is treated as "ours" (the local/excluded ship) and a second
//! ship is the **remote** we interpolate and watch.
//!
//! The remote is driven forward so it actually moves. Each rendered frame the
//! client interpolates the remote ~100 ms in the past from its
//! [`SnapshotBuffer`] and the test asserts the SC-004 objective signals:
//!
//! 1. **No teleport (single-drop + 5%/±50 ms):** between consecutive rendered
//!    frames the interpolated remote never jumps more than [`MAX_INTERP_DELTA`].
//! 2. **Single-drop ride-out:** with exactly one snapshot dropped, the ~100 ms
//!    buffer keeps the remote moving with no visible jump.
//! 3. **Consecutive-drop stall bound:** during a scripted burst the remote
//!    **holds its last interpolated transform** (no extrapolation) and, when
//!    delivery resumes, advances with no jump > [`MAX_INTERP_DELTA`]; a backward
//!    jump or unbounded buffer growth is a failure.
//!
//! `MAX_INTERP_DELTA` is recomputed from the live [`sim::Tuning`] so a tuning
//! change cannot silently invalidate the no-teleport bound.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use client::interpolation::SnapshotBuffer;
use client::prediction::MAX_INTERP_DELTA;
use glam::Vec2;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, LoopbackTransport, LossJitterConfig, Message,
    NetTransport, QuantizedIntent, CLIENT_TOKEN_BYTES,
};
use server::{RateConfig, ServerApp, PROTOCOL_VERSION};
use sim::Tuning;

fn addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

fn forward() -> QuantizedIntent {
    QuantizedIntent {
        forward: 1,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    }
}

/// The bound recomputed from the live tuning + the baseline 20 Hz snapshot rate
/// (`top_speed / 20`). Used to assert the provisional constant matches its stated
/// derivation, so a tuning change cannot silently invalidate it.
fn derived_bound() -> f32 {
    Tuning::default().top_speed() / 20.0
}

/// Connect a client through `transport`, run the handshake, return its conn +
/// assigned ship id. The server must be ticked once after the Connect is sent.
fn connect_and_handshake(
    server: &mut ServerApp,
    transport: &mut LoopbackTransport,
    port: u16,
) -> (ConnectionId, EntityId) {
    let conn = NetTransport::connect(transport, addr(port));
    transport.send_reliable(conn, &connect_msg());
    server.tick();
    for m in transport.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            return (conn, a.client_id);
        }
    }
    panic!("no ConnectAccepted received for port {port}");
}

/// The interpolated position of remote `id` this frame, if present.
fn remote_pos(
    buf: &SnapshotBuffer,
    now_ms: f64,
    interp_delay_ms: f64,
    local_id: EntityId,
    id: EntityId,
) -> Option<Vec2> {
    buf.interpolate_remotes(now_ms, interp_delay_ms, local_id)
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.pos)
}

#[test]
fn provisional_bound_matches_its_derivation() {
    // The recorded constant equals top_speed / 20 Hz (the no-teleport derivation,
    // OD-001/T046) — guards against a silent tuning drift.
    assert!(
        (MAX_INTERP_DELTA - derived_bound()).abs() < 1e-4,
        "MAX_INTERP_DELTA {MAX_INTERP_DELTA} != derived {}",
        derived_bound()
    );
}

#[test]
fn remote_interpolation_no_teleport_under_loss_and_jitter() {
    // TR-036 baseline: 5% loss + ±50 ms jitter, seeded for reproducibility.
    let cfg = LossJitterConfig {
        loss: 0.05,
        jitter_ms: 50,
        seed: 0xBADC0FFEE,
    };
    let (mut client, server_transport) = LoopbackTransport::with_loss_jitter(cfg);
    let mut server = ServerApp::new(Box::new(server_transport), RateConfig::default())
        .expect("default rates valid");

    // Two clients on one server: ours (excluded) + the remote we watch.
    let (local_conn, local_id) = connect_and_handshake(&mut server, &mut client, 40_001);
    let (remote_conn, remote_id) = connect_and_handshake(&mut server, &mut client, 40_002);
    assert_ne!(local_id, remote_id, "two distinct ships");

    let rates = server.rates();
    let tick_ms = 1000.0 / rates.tick_rate_hz as f64;
    let interp_delay_ms = rates.interp_delay_ms as f64;
    let bound = derived_bound();

    let mut buf = SnapshotBuffer::new(rates.tick_rate_hz);
    let mut now_ms = 0.0_f64;
    let mut last_remote: Option<Vec2> = None;
    let mut max_jump = 0.0_f32;
    let mut samples = 0u32;

    // Run a few seconds of ticks. The remote thrusts forward the whole time, so it
    // is genuinely moving; we never thrust our own ship (it stays at origin). The
    // loop index doubles as the monotonic input `seq`.
    let total_ticks = rates.tick_rate_hz as u32 * 4;
    for seq in 1..=total_ticks {
        // Both clients send input. The remote thrusts; we coast.
        client.send_unreliable(
            remote_conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![forward()])),
        );
        server.tick();

        // Advance the jitter clock one tick of real time.
        client.advance(tick_ms as u64);
        now_ms += tick_ms;

        // Drain OUR connection's inbox into the snapshot buffer (stale/dup gated).
        // The buffer must never regress (monotonic) and never grow past CAP.
        for m in client.recv(local_conn) {
            if let Message::Snapshot(s) = m {
                buf.push(s);
            }
        }
        assert!(
            buf.len() <= SnapshotBuffer::CAP,
            "buffer must stay bounded (no unbounded growth): {}",
            buf.len()
        );

        // Render the remote ~100 ms in the past and check the per-frame jump.
        if let Some(pos) = remote_pos(&buf, now_ms, interp_delay_ms, local_id, remote_id) {
            if let Some(prev) = last_remote {
                let jump = (pos - prev).length();
                max_jump = max_jump.max(jump);
                samples += 1;
                assert!(
                    jump <= bound + 1e-3,
                    "interpolated remote teleported: jump {jump} > MAX_INTERP_DELTA {bound} \
                     (now_ms={now_ms})"
                );
            }
            last_remote = Some(pos);
        }
    }

    // Drain the remote's own inbox so the test doesn't leak the queue (and so the
    // server-side delivery to the remote is exercised symmetrically).
    let _ = client.recv(remote_conn);

    assert!(
        samples > 20,
        "the run produced too few render samples: {samples}"
    );
    // The remote really moved (the max jump is a real, non-trivial signal), but
    // stayed under the bound the whole run.
    assert!(
        max_jump > 0.0,
        "the remote must have visibly moved across frames"
    );
    assert!(
        max_jump <= bound + 1e-3,
        "max observed remote jump {max_jump} exceeds MAX_INTERP_DELTA {bound}"
    );
    eprintln!(
        "T048 single-drop/5%/±50ms: samples={samples} max_jump={max_jump:.4} \
         MAX_INTERP_DELTA={bound:.4}"
    );
}

#[test]
fn single_dropped_snapshot_is_ridden_out_with_no_jump() {
    // Pure single-drop: no random loss, no jitter — script exactly ONE dropped
    // snapshot mid-run and assert the ~100 ms buffer keeps the remote advancing
    // smoothly with no jump beyond the bound (the buffer holds enough future
    // snapshots to interpolate across the gap).
    let cfg = LossJitterConfig {
        loss: 0.0,
        jitter_ms: 0,
        seed: 1,
    };
    let (mut client, server_transport) = LoopbackTransport::with_loss_jitter(cfg);
    let mut server = ServerApp::new(Box::new(server_transport), RateConfig::default())
        .expect("default rates valid");

    let (local_conn, local_id) = connect_and_handshake(&mut server, &mut client, 41_001);
    let (remote_conn, remote_id) = connect_and_handshake(&mut server, &mut client, 41_002);

    let rates = server.rates();
    let tick_ms = 1000.0 / rates.tick_rate_hz as f64;
    let interp_delay_ms = rates.interp_delay_ms as f64;
    let bound = derived_bound();

    let mut buf = SnapshotBuffer::new(rates.tick_rate_hz);
    let mut now_ms = 0.0_f64;
    let mut last_remote: Option<Vec2> = None;
    let mut max_jump = 0.0_f32;

    let total_ticks = rates.tick_rate_hz as u32 * 3;
    let drop_at = total_ticks / 2;
    // `seq` is the monotonic input number; the tick index is `seq - 1`.
    for seq in 1..=total_ticks {
        let t = seq - 1;
        // Script a single dropped delivery right before the mid-run tick.
        if t == drop_at {
            client.script_consecutive_drops(1);
        }
        client.send_unreliable(
            remote_conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![forward()])),
        );
        server.tick();
        client.advance(tick_ms as u64);
        now_ms += tick_ms;

        for m in client.recv(local_conn) {
            if let Message::Snapshot(s) = m {
                buf.push(s);
            }
        }

        if let Some(pos) = remote_pos(&buf, now_ms, interp_delay_ms, local_id, remote_id) {
            if let Some(prev) = last_remote {
                let jump = (pos - prev).length();
                max_jump = max_jump.max(jump);
                assert!(
                    jump <= bound + 1e-3,
                    "single dropped snapshot caused a teleport: jump {jump} > {bound} (t={t})"
                );
            }
            last_remote = Some(pos);
        }
    }
    eprintln!("T048 single-drop ride-out: max_jump={max_jump:.4} MAX_INTERP_DELTA={bound:.4}");
}

#[test]
fn consecutive_drop_burst_stalls_then_resumes_without_teleport() {
    // Scripted consecutive-drop burst (TR-036): the remote must HOLD its last
    // interpolated transform during the burst (freeze, no extrapolation), the
    // buffer must not regress or grow unboundedly, and on resume the remote must
    // advance with no jump > MAX_INTERP_DELTA and never backward.
    let cfg = LossJitterConfig {
        loss: 0.0,
        jitter_ms: 0,
        seed: 2,
    };
    let (mut client, server_transport) = LoopbackTransport::with_loss_jitter(cfg);
    let mut server = ServerApp::new(Box::new(server_transport), RateConfig::default())
        .expect("default rates valid");

    let (local_conn, local_id) = connect_and_handshake(&mut server, &mut client, 42_001);
    let (remote_conn, remote_id) = connect_and_handshake(&mut server, &mut client, 42_002);

    let rates = server.rates();
    let tick_ms = 1000.0 / rates.tick_rate_hz as f64;
    let interp_delay_ms = rates.interp_delay_ms as f64;
    let bound = derived_bound();

    let mut buf = SnapshotBuffer::new(rates.tick_rate_hz);
    let mut now_ms = 0.0_f64;
    let mut last_remote: Option<Vec2> = None;
    let mut max_jump = 0.0_f32;
    let mut max_backward: f32 = 0.0; // any backward-x movement is a failure signal
    let mut stalled_frames = 0u32;

    // A burst long enough to outlast the buffered window so a stall is *expected*
    // and asserted as the accepted outcome (freeze, not teleport).
    let burst = 12u64;
    let total_ticks = rates.tick_rate_hz as u32 * 3;
    let burst_at = rates.tick_rate_hz as u32; // ~1 s in
                                              // `seq` is the monotonic input number; the tick index is `seq - 1`.
    for seq in 1..=total_ticks {
        let t = seq - 1;
        if t == burst_at {
            client.script_consecutive_drops(burst);
        }
        client.send_unreliable(
            remote_conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![forward()])),
        );
        server.tick();
        client.advance(tick_ms as u64);
        now_ms += tick_ms;

        for m in client.recv(local_conn) {
            if let Message::Snapshot(s) = m {
                buf.push(s);
            }
        }
        assert!(
            buf.len() <= SnapshotBuffer::CAP,
            "buffer must stay bounded during a burst: {}",
            buf.len()
        );

        if let Some(pos) = remote_pos(&buf, now_ms, interp_delay_ms, local_id, remote_id) {
            if let Some(prev) = last_remote {
                let dx = pos.x - prev.x;
                let jump = (pos - prev).length();
                max_jump = max_jump.max(jump);
                // A forward-thrusting remote must never move backward in x; any
                // backward step is a buffer regression / bad interpolation (fail).
                if dx < 0.0 {
                    max_backward = max_backward.max(-dx);
                }
                // A held (frozen) frame is the accepted stall outcome.
                if jump <= 1e-4 {
                    stalled_frames += 1;
                }
                assert!(
                    jump <= bound + 1e-3,
                    "burst resume teleported: jump {jump} > MAX_INTERP_DELTA {bound} (t={t})"
                );
            }
            last_remote = Some(pos);
        }
    }

    // The burst outlasted the buffer, so a freeze (stall) must have been observed —
    // that is the *accepted* outcome, asserted as held frames.
    assert!(
        stalled_frames > 0,
        "a long consecutive-drop burst must produce an observable stall (freeze)"
    );
    // The stall is a freeze, never a backward jump (TR-036 failure condition).
    assert!(
        max_backward < 1e-3,
        "remote moved backward during/after the burst (buffer regression): {max_backward}"
    );
    eprintln!(
        "T048 consecutive-drop: stalled_frames={stalled_frames} max_jump={max_jump:.4} \
         max_backward={max_backward:.6} MAX_INTERP_DELTA={bound:.4}"
    );
}
