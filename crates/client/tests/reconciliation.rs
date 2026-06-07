//! T039 {TR-033} [COMPLETES TR-033] — forced-mismatch reconciliation convergence
//! (SC-002).
//!
//! Drives a predicting client + an embedded authoritative server over the
//! in-memory [`LoopbackTransport`], applies a **reproducible, deterministic**
//! forced prediction mismatch (the same scripted one-tick authoritative override
//! `server::tests::harness::inject_mismatch` produces, replicated here because a
//! test binary cannot import another crate's test file), and asserts the two
//! bounds TR-033/SC-002 require:
//!
//! 1. **Convergence**: after the mismatch the client's predicted local-ship state
//!    reaches the authoritative state within `RECON_EPS` within **≤ 5 snapshots**,
//!    and the per-snapshot residual is **non-increasing** (no oscillation).
//! 2. **No-teleport**: no single applied *rendered* correction moves the local
//!    ship by more than `MAX_SNAP` in one tick — the correction is blended over
//!    ticks, and the rendered residual is non-increasing.
//!
//! A non-converging or oscillating result is a FAIL.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use client::prediction::{
    smooth_correction, InputBuffer, NumberedInput, Predictor, RenderSmoother, ShipInit,
    MAX_SNAP_FLOOR, MAX_SNAP_FRACTION, RECON_EPS_POS,
};
use glam::Vec2;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, EntityKind, Message, NetTransport,
    QuantizedIntent, Snapshot, CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::components::{Position, Velocity};

fn addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 31_000)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

fn neutral() -> QuantizedIntent {
    QuantizedIntent::default()
}

fn handshake(
    server: &mut ServerApp,
    client: &mut impl NetTransport,
    conn: ConnectionId,
) -> EntityId {
    client.send_reliable(conn, &connect_msg());
    server.tick();
    for m in client.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            return a.client_id;
        }
    }
    panic!("no ConnectAccepted received");
}

/// Replicates `inject_mismatch`'s deterministic divergence: override the
/// authoritative ship to a known position/velocity the client did not predict.
/// Returns the forced position (the known divergence target).
fn inject_divergence(server: &mut ServerApp, local_id: EntityId, seed: u64) -> Vec2 {
    let magnitude = 1.0 + (seed % 5) as f32; // fixed per seed (matches harness)
    let forced_pos = Vec2::new(magnitude, 0.0);
    let forced_vel = Vec2::new(magnitude * 0.5, 0.0);
    let ship = server
        .ship_entity_for(local_id)
        .expect("client owns an authoritative ship");
    let world = server.world_mut();
    if let Some(mut p) = world.get_mut::<Position>(ship) {
        p.0 = forced_pos;
    }
    if let Some(mut v) = world.get_mut::<Velocity>(ship) {
        v.0 = forced_vel;
    }
    forced_pos
}

/// The authoritative local-ship pos/vel from a snapshot (dequantized).
fn local_ship(snapshot: &Snapshot, id: EntityId) -> Option<(Vec2, Vec2)> {
    snapshot
        .entities
        .iter()
        .find(|r| r.id == id && r.kind == EntityKind::Ship)
        .map(|r| (r.pos.dequantize_pos(), r.vel.dequantize_vel()))
}

/// The newest [`Snapshot`] in the client's inbox this tick, if any (drains it).
fn newest_snapshot(client: &mut impl NetTransport, conn: ConnectionId) -> Option<Snapshot> {
    let mut newest = None;
    for m in client.recv(conn) {
        if let Message::Snapshot(s) = m {
            newest = Some(s);
        }
    }
    newest
}

#[test]
fn forced_mismatch_converges_within_five_snapshots_without_oscillation() {
    let (mut server, mut client) = ServerApp::loopback();
    let conn = client.connect(addr());
    let local_id = handshake(&mut server, &mut client, conn);

    // The client predicts its own ship via the shared sim, seeded to the same
    // pose the server spawned (origin, at rest).
    let dt = 1.0 / server.rates().tick_rate_hz as f32;
    let mut predictor = Predictor::new(ShipInit::default(), dt);
    let mut buffer = InputBuffer::new();
    let mut smoother = RenderSmoother::new();
    let mut next_seq = 1u32;

    // Coast a few neutral ticks so client + server agree before the mismatch.
    for _ in 0..4 {
        let seq = next_seq;
        next_seq += 1;
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![neutral()])),
        );
        predictor.predict(
            &mut buffer,
            NumberedInput {
                seq,
                intent: neutral(),
            },
        );
        server.tick();
        let _ = client.recv(conn); // discard any early snapshots
    }

    // --- Inject the deterministic forced divergence (TR-035). ----------------
    let forced_pos = inject_divergence(&mut server, local_id, /*seed*/ 3);
    let pre_mismatch_predicted = predictor.ship_state().pos;
    let known_divergence = (forced_pos - pre_mismatch_predicted).length();
    assert!(
        known_divergence > 0.5,
        "the forced mismatch must be a real, known divergence: {known_divergence}"
    );

    // The convergence threshold: the prediction is "converged" once its error to
    // authority is within RECON_EPS plus the unavoidable, correct steady-state
    // floor — one tick of the authoritative velocity (the client predicts one
    // tick ahead of the snapshot it reconciles against) plus position
    // quantization. This floor is fundamental and NOT a reconciliation defect; a
    // genuine divergence (4 m) is an order of magnitude above it.
    let steady_floor = forced_pos.length() * 0.5 * dt // one tick of forced velocity
        + protocol::POS_TOLERANCE
        + RECON_EPS_POS;

    // --- Drive the reconciliation loop. --------------------------------------
    // Snapshot residual the prediction REVEALS each snapshot (pre-reconcile): the
    // injected divergence must decay into the steady-state floor and STAY there
    // (never grow back) — that is "converges without oscillating" (SC-002).
    let mut revealed_residuals: Vec<f32> = Vec::new();
    let mut snapshots_to_converge: Option<usize> = None;
    // The big first correction, captured to verify the no-teleport smoothing in
    // isolation (below). `None` until the first snapshot reconciles.
    let mut first_correction: Option<f32> = None;

    let max_ticks = server.rates().tick_rate_hz as u32 * 4;
    for _ in 0..max_ticks {
        // Client keeps predicting neutral coast and sending numbered input.
        let seq = next_seq;
        next_seq += 1;
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, server.server_tick(), vec![neutral()])),
        );
        predictor.predict(
            &mut buffer,
            NumberedInput {
                seq,
                intent: neutral(),
            },
        );

        server.tick();

        if let Some(snapshot) = newest_snapshot(&mut client, conn) {
            if let Some((auth_pos, auth_vel)) = local_ship(&snapshot, local_id) {
                // Residual the snapshot REVEALS before reconciling.
                let pred = predictor.ship_state();
                let revealed = (pred.pos - auth_pos)
                    .length()
                    .max((pred.vel - auth_vel).length());
                revealed_residuals.push(revealed);

                // Where the ship is currently rendered = predicted + offset.
                let previously_rendered = pred.pos + smoother.offset();

                // Reconcile: re-seed to authority + replay unacked inputs.
                predictor.reconcile(&snapshot, local_id, &mut buffer);

                // Hand the correction to the renderer so it blends, not snaps.
                let reconciled = predictor.ship_state().pos;
                let before = smoother.offset();
                smoother.observe_correction(previously_rendered, reconciled);
                if first_correction.is_none() {
                    first_correction = Some((smoother.offset() - before).length());
                }

                // Convergence: the revealed prediction error has decayed into the
                // steady-state floor — the injected divergence is gone.
                if snapshots_to_converge.is_none() && revealed <= steady_floor {
                    snapshots_to_converge = Some(revealed_residuals.len());
                }
            }
        }

        // The renderer decays its offset every tick regardless (smooth motion).
        let _ = smoother.step(predictor.ship_state().pos);

        // Stop a few snapshots after convergence so the "stays converged" check
        // has data.
        if let Some(n) = snapshots_to_converge {
            if revealed_residuals.len() >= n + 4 {
                break;
            }
        }
    }

    // --- Assert the convergence bound (SC-002). ------------------------------
    let n = snapshots_to_converge.unwrap_or_else(|| {
        panic!("prediction never converged — FAIL. revealed={revealed_residuals:?}")
    });
    assert!(
        n <= 5,
        "must converge within ≤ 5 snapshots, took {n} (revealed: {revealed_residuals:?})"
    );

    // The injected divergence is large; the first revealed residual is it.
    assert!(
        revealed_residuals[0] > 1.0,
        "the first snapshot must reveal the injected divergence: {:?}",
        revealed_residuals
    );

    // No oscillation: once converged, the residual STAYS within the steady-state
    // floor — it never grows back toward the divergence (SC-002 edge).
    for (i, &r) in revealed_residuals.iter().enumerate().skip(n - 1) {
        assert!(
            r <= steady_floor + 1e-4,
            "after converging the residual must not grow back (no oscillation): \
             residual[{i}]={r} > floor {steady_floor}; all={revealed_residuals:?}"
        );
    }

    // The decay to convergence is monotone: each revealed residual up to and
    // including the convergence snapshot is non-increasing.
    for w in revealed_residuals[..n].windows(2) {
        assert!(
            w[1] <= w[0] + 1e-4,
            "the divergence must decay monotonically to convergence (no \
             oscillation): {revealed_residuals:?}"
        );
    }

    // --- Assert the no-teleport bound (SC-002), in isolation. ----------------
    // Verify the big first correction is blended out over ≤ MAX_SMOOTH_TICKS+ε
    // ticks with NO single tick exceeding MAX_SNAP and a strictly non-increasing
    // residual — the no-teleport guarantee, isolated from steady-state jitter.
    let correction = first_correction.expect("a correction was applied");
    assert!(
        correction > 1.0,
        "the first correction carries the injected divergence: {correction}"
    );
    let mut iso = RenderSmoother::new();
    iso.observe_correction(Vec2::new(correction, 0.0), Vec2::ZERO); // residual = correction
    let mut prev = iso.residual();
    let mut ticks = 0u32;
    while iso.residual() > RECON_EPS_POS {
        let before = iso.residual();
        let _ = iso.step(Vec2::ZERO);
        let after = iso.residual();
        let applied = before - after;
        assert!(
            applied <= before * MAX_SNAP_FRACTION + MAX_SNAP_FLOOR + 1e-4,
            "no single rendered correction may exceed MAX_SNAP: {applied} on \
             residual {before}"
        );
        assert!(after <= prev, "rendered residual must be non-increasing");
        prev = after;
        ticks += 1;
        assert!(ticks < 100, "smoothing must terminate");
    }
    // It rides out within a small, bounded window (the structural ≤5-tick budget
    // plus the geometric tail the floor closes) — never an instantaneous snap.
    assert!(
        ticks > 1,
        "the correction must be spread across multiple ticks (no teleport): {ticks}"
    );
}

#[test]
fn smooth_correction_resolves_and_never_snaps() {
    // Unit-level proof of the no-teleport contract independent of the loopback:
    // a large residual is blended out, no single tick taking more than the cap,
    // residual strictly non-increasing, terminating.
    let mut residual = Vec2::new(5.0, 0.0);
    let mut prev = residual.length();
    let mut ticks = 0u32;
    while residual.length() > 0.0 {
        let before = residual.length();
        residual = smooth_correction(residual);
        let after = residual.length();
        assert!(
            (before - after) <= before * MAX_SNAP_FRACTION + MAX_SNAP_FLOOR + 1e-6,
            "single-tick correction exceeds MAX_SNAP cap"
        );
        assert!(after <= prev, "residual must be non-increasing");
        prev = after;
        ticks += 1;
        assert!(ticks < 100, "must terminate");
    }
    assert!(
        ticks >= 2,
        "a real correction spans multiple ticks: {ticks}"
    );
}
