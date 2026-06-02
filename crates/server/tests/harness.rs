//! T038 {TR-035} — the loopback bot harness, seeded with a reproducible forced
//! prediction mismatch (`inject_mismatch`).
//!
//! This file is the *start* of the headless bot harness extended in Phase 8
//! (T068+): a deterministic, no-rendering driver of an embedded [`ServerApp`]
//! over the in-memory [`LoopbackTransport`]. It is kept cleanly structured so
//! later scenarios (responsiveness, invalid-input rejection, loss/jitter
//! interpolation, bandwidth baseline, disconnect-mid-session) bolt on without a
//! rewrite.
//!
//! [`inject_mismatch`] produces the **reproducible, deterministic forced
//! divergence** TR-035 requires: a scripted one-tick authoritative override of
//! the local ship's state, applied at a fixed tick, so the next authoritative
//! snapshot disagrees with the client's neutral-coast prediction by a KNOWN
//! magnitude. Fixed seed/inputs ⇒ the mismatch magnitude and the resulting
//! convergence are repeatable run-to-run.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use glam::Vec2;
use protocol::{
    Connect, ConnectionId, EntityId, EntityKind, Message, NetTransport, QVec2, Snapshot,
    CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::components::{Position, Velocity};

/// A loopback session (embedded server + connected client) carrying a scripted,
/// reproducible forced prediction mismatch. The handle the harness hands back so
/// a test (T039, and Phase 8 scenarios) can drive ticks, pull snapshots, and
/// assert convergence against the known divergence.
pub struct MismatchHarness {
    /// The embedded authoritative server.
    pub server: ServerApp,
    /// The client end of the loopback transport.
    pub client: LoopbackEnd,
    /// This client's connection id on the loopback switch.
    pub conn: ConnectionId,
    /// The network id of the client's own (local) ship.
    pub local_id: EntityId,
    /// The authoritative position the server's ship was *forced* to at injection
    /// (the override the client did not predict).
    pub forced_pos: Vec2,
    /// The authoritative velocity the server's ship was forced to at injection.
    pub forced_vel: Vec2,
    /// The position the client's neutral-coast prediction held at injection (its
    /// pre-mismatch predicted state) — so the test knows the divergence magnitude.
    pub predicted_pos: Vec2,
    /// The server tick at which the override was applied (fixed by the seed).
    pub injected_at_tick: u32,
}

/// The client transport type (the loopback end the bot drives). Aliased so the
/// Phase 8 harness can swap the concrete type without touching call sites.
pub type LoopbackEnd = protocol::LoopbackTransport;

impl MismatchHarness {
    /// The known magnitude of the forced position divergence at injection: how
    /// far the authoritative state was pushed from the client's prediction. The
    /// reconciliation convergence test (T039/SC-002) checks the predicted ship
    /// closes this gap within `RECON_EPS` in ≤ 5 snapshots without oscillating.
    pub fn divergence_magnitude(&self) -> f32 {
        (self.forced_pos - self.predicted_pos).length()
    }

    /// Drain the client's inbox and return the newest [`Snapshot`], if any.
    pub fn latest_snapshot(&mut self) -> Option<Snapshot> {
        let mut newest = None;
        for m in self.client.recv(self.conn) {
            if let Message::Snapshot(s) = m {
                newest = Some(s);
            }
        }
        newest
    }

    /// The authoritative position/velocity of the local ship in `snapshot`
    /// (dequantized), if present.
    pub fn local_ship_in(&self, snapshot: &Snapshot) -> Option<(Vec2, Vec2)> {
        snapshot
            .entities
            .iter()
            .find(|r| r.id == self.local_id && r.kind == EntityKind::Ship)
            .map(|r| (r.pos.dequantize_pos(), r.vel.dequantize_vel()))
    }
}

fn addr(seed: u64) -> SocketAddr {
    // Derive a distinct (but deterministic) loopback endpoint key from the seed so
    // repeated harness instances in one test process do not alias.
    let port = 20_000u16.wrapping_add((seed % 1000) as u16);
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// Build a loopback session and inject a **reproducible, deterministic** forced
/// prediction mismatch on the local ship (T038, TR-035).
///
/// Scripted by `seed` so the divergence magnitude and convergence are repeatable
/// run-to-run:
/// 1. an embedded [`ServerApp`] is created and one client connects and is
///    accepted (the client learns its own ship id);
/// 2. the server is ticked a fixed number of **neutral** ticks (no input) so the
///    ship coasts predictably — the client's prediction holds the same coast;
/// 3. at the fixed injection tick the authoritative ship is **overridden**
///    directly in the world to a position/velocity offset the client never
///    predicted (the magnitude derived from `seed`, so it is known and fixed).
///
/// The next snapshot the server broadcasts therefore disagrees with the client's
/// prediction by exactly `forced - predicted`. The seed makes the whole thing
/// deterministic: same seed ⇒ same override ⇒ same mismatch.
pub fn inject_mismatch(seed: u64) -> MismatchHarness {
    let (mut server, mut client) = ServerApp::loopback();

    // --- Connect + accept (the client learns its own ship id). ---------------
    let conn = client.connect(addr(seed));
    client.send_reliable(conn, &connect_msg());
    server.tick(); // tick 1: accept → spawn ship
    let mut local_id = None;
    for m in client.recv(conn) {
        if let Message::ConnectAccepted(a) = m {
            local_id = Some(a.client_id);
        }
    }
    let local_id = local_id.expect("client must be accepted and told its ship id");

    // --- Coast a fixed number of neutral ticks (no input). -------------------
    // The ship stays at the origin at rest (the client predicts the same coast),
    // so the divergence comes purely from the scripted override below.
    const NEUTRAL_TICKS: u32 = 4;
    for _ in 0..NEUTRAL_TICKS {
        server.tick();
    }

    // The client's neutral-coast prediction: still at the origin, at rest.
    let predicted_pos = Vec2::ZERO;

    // --- Inject the scripted authoritative override. -------------------------
    // The override magnitude is derived from the seed so it is fixed per seed but
    // distinct across seeds. A modest, well-inside-quantization-range offset on
    // +x with a matching velocity (so the authoritative state is self-consistent:
    // a ship that genuinely got pushed/rammed).
    let magnitude = 1.0 + (seed % 5) as f32; // 1.0..=5.0 sim units, fixed per seed
    let forced_pos = Vec2::new(magnitude, 0.0);
    let forced_vel = Vec2::new(magnitude * 0.5, 0.0);

    // Looked up by the wire id the client learned — the loopback `conn` is the
    // client-side handle, not the server-side one the links are keyed by.
    let ship_entity = server
        .ship_entity_for(local_id)
        .expect("the connected client owns an authoritative ship");
    {
        let world = server.world_mut();
        if let Some(mut pos) = world.get_mut::<Position>(ship_entity) {
            pos.0 = forced_pos;
        }
        if let Some(mut vel) = world.get_mut::<Velocity>(ship_entity) {
            vel.0 = forced_vel;
        }
    }
    let injected_at_tick = server.server_tick();

    MismatchHarness {
        server,
        client,
        conn,
        local_id,
        forced_pos,
        forced_vel,
        predicted_pos,
        injected_at_tick,
    }
}

// --- The harness's own sanity tests ------------------------------------------
//
// These prove the injection is deterministic and produces a KNOWN, non-trivial
// divergence (TR-035) — the property T039's convergence test relies on.

#[test]
fn inject_mismatch_is_deterministic_for_a_fixed_seed() {
    let a = inject_mismatch(7);
    let b = inject_mismatch(7);
    assert_eq!(a.forced_pos, b.forced_pos, "same seed ⇒ same override pos");
    assert_eq!(a.forced_vel, b.forced_vel, "same seed ⇒ same override vel");
    assert_eq!(
        a.injected_at_tick, b.injected_at_tick,
        "same seed ⇒ same injection tick"
    );
    assert_eq!(
        a.divergence_magnitude(),
        b.divergence_magnitude(),
        "same seed ⇒ same known divergence magnitude"
    );
}

#[test]
fn inject_mismatch_produces_a_known_nonzero_divergence() {
    let h = inject_mismatch(3);
    assert!(
        h.divergence_magnitude() > 0.5,
        "the forced mismatch must be a real, measurable divergence: {}",
        h.divergence_magnitude()
    );
    // The override is reflected in the authoritative world the very next read.
    let entity = h.server.ship_entity_for(h.local_id).unwrap();
    let auth = h.server.world().get::<Position>(entity).unwrap().0;
    assert_eq!(
        auth, h.forced_pos,
        "the authoritative ship really holds the overridden state"
    );
}

#[test]
fn injected_override_appears_in_the_next_snapshot() {
    let mut h = inject_mismatch(2);

    // Drive until a snapshot is broadcast (snapshot rate < tick rate) and confirm
    // it carries the OVERRIDDEN authoritative state — the value that disagrees
    // with the client's neutral-coast prediction.
    let ticks_per_snapshot = h.server.rates().tick_rate_hz; // generous upper bound
    let mut seen = None;
    for _ in 0..ticks_per_snapshot {
        h.server.tick();
        if let Some(s) = h.latest_snapshot() {
            seen = Some(s);
            break;
        }
    }
    let snapshot = seen.expect("a snapshot must be broadcast");
    let (snap_pos, _snap_vel) = h
        .local_ship_in(&snapshot)
        .expect("the local ship is present in the snapshot");

    // The snapshot position is the overridden one, advanced by a few ticks of the
    // forced velocity — i.e. it is on the +x side, far from the predicted origin.
    assert!(
        snap_pos.x > 0.5,
        "the snapshot reflects the injected divergence: snap_pos={snap_pos:?}"
    );
    // It must NOT match the client's neutral-coast prediction (the origin).
    let from_prediction = (snap_pos - h.predicted_pos).length();
    assert!(
        from_prediction > 0.5,
        "the snapshot must disagree with the client's prediction by a known \
         amount: {from_prediction}"
    );

    // Quantization round-trips the override within tolerance (sanity on QVec2).
    let requantized = QVec2::quantize_pos(h.forced_pos).dequantize_pos();
    assert!((requantized - h.forced_pos).length() < 0.5);
}
