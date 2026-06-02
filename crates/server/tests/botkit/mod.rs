//! Shared headless bot-harness machinery (8b) — the reusable kit the harness
//! scenarios (`tests/harness.rs`, T038/T068/T071/T072), the loopback↔renet
//! equivalence test (`tests/equivalence.rs`, T069), and the bandwidth baseline
//! (`tests/bandwidth.rs`, T070) all build on.
//!
//! It drives ≥ 2 networked clients (each a [`ScriptedBot`] running a fixed
//! scripted input loop) against ONE embedded authoritative [`ServerApp`], with NO
//! rendering. Each bot holds its **own** client-side `Box<dyn NetTransport>` (the
//! swap seam): the IDENTICAL prediction/reconstruction logic runs over the
//! in-memory loopback (deterministic, zero-latency, lossless) AND over the renet
//! UDP adapter — only the transport underneath differs (SC-006/SC-008). The bots
//! expose **numeric signals only** (predicted state, seq/ack bookkeeping,
//! reconstructed positions, `NetStats`), so every assertion is a hard number,
//! never a visual judgment (TR-015/043).
//!
//! [`inject_mismatch`] (T038/TR-035) is the reproducible forced-divergence seed
//! the reconciliation tier relies on. The renet constructor [`renet_harness`]
//! (udp-gated) runs the netcode connect handshake, then hands the
//! [`BotHarness`] a pump closure that drives renet's update/socket flush each
//! tick — the SAME `BotHarness::step_all` then runs over the real socket path.

#![allow(dead_code)] // each consuming test crate uses a different subset.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use glam::Vec2;
use protocol::{
    apply_delta, ClientInput, Connect, ConnectionId, EntityId, EntityKind, FullState, Message,
    NetStats, NetTransport, QuantizedIntent, Snapshot, SnapshotAck, CLIENT_TOKEN_BYTES,
};
use server::{ServerApp, PROTOCOL_VERSION};
use sim::components::{AngularVelocity, FlightAssist, Heading, Position, Ship, Velocity};
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

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

pub fn connect_msg() -> Message {
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

/// A fixed scripted input loop for a [`ScriptedBot`]: a cycle of quantized pilot
/// intents applied one per tick (wrapping when exhausted), so a bot's behavior is
/// fully deterministic and reproducible run-to-run (TR-043).
#[derive(Clone, Debug)]
pub struct InputScript {
    /// The cycle of intents, applied one per tick in order (wraps at the end).
    steps: Vec<QuantizedIntent>,
    /// The next index into `steps` to apply.
    cursor: usize,
}

impl InputScript {
    /// A script that cycles `steps` (must be non-empty). One intent is consumed
    /// per tick; after the last it wraps to the first.
    pub fn new(steps: Vec<QuantizedIntent>) -> Self {
        assert!(!steps.is_empty(), "an input script needs at least one step");
        Self { steps, cursor: 0 }
    }

    /// A script that holds a single fixed intent every tick.
    pub fn constant(intent: QuantizedIntent) -> Self {
        Self::new(vec![intent])
    }

    /// The neutral (no-op) intent — coast in place.
    pub fn neutral_intent() -> QuantizedIntent {
        QuantizedIntent {
            forward: 0,
            strafe: 0,
            turn: 0,
            fire: false,
            toggle_assist: false,
        }
    }

    /// Take the next scripted intent, advancing (and wrapping) the cursor.
    fn next(&mut self) -> QuantizedIntent {
        let intent = self.steps[self.cursor];
        self.cursor = (self.cursor + 1) % self.steps.len();
        intent
    }
}

/// The bot's predicted local-ship simulation — a minimal predicted world stepped
/// by the **shared** `sim` schedule (the same one the server and the real client
/// run), so the bot predicts its own ship bit-identically to authority for
/// identical inputs (Principle II / HINT-003). Mirrors the real client's
/// `Predictor` without depending on the `client` crate (no dependency cycle).
struct BotPredictor {
    world: World,
    schedule: Schedule,
    ship: Entity,
}

impl BotPredictor {
    /// A predicted world holding one local ship at rest at the origin, seeded with
    /// the same resources/components the server spawns (so the shared systems
    /// behave identically).
    fn new(dt: f32) -> Self {
        let mut world = World::new();
        world.insert_resource(Tuning::default());
        world.insert_resource(FixedDt(dt));
        world.insert_resource(HitFeedback::default());
        let ship = world
            .spawn((
                Ship,
                ShipIntent::default(),
                Position(Vec2::ZERO),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                FlightAssist::On,
            ))
            .id();
        let mut schedule = Schedule::default();
        sim::add_fixed_step_systems(&mut schedule);
        Self {
            world,
            schedule,
            ship,
        }
    }

    /// Apply `intent` and step the shared sim once (predict this tick).
    fn predict(&mut self, intent: QuantizedIntent) {
        if let Some(mut i) = self.world.get_mut::<ShipIntent>(self.ship) {
            *i = ShipIntent::from(intent);
        }
        self.schedule.run(&mut self.world);
        if let Some(mut i) = self.world.get_mut::<ShipIntent>(self.ship) {
            i.toggle_assist = false;
        }
    }

    /// Re-seed the predicted ship to an authoritative state (reconcile base).
    fn reseed(&mut self, pos: Vec2, vel: Vec2, heading: f32) {
        if let Some(mut p) = self.world.get_mut::<Position>(self.ship) {
            p.0 = pos;
        }
        if let Some(mut v) = self.world.get_mut::<Velocity>(self.ship) {
            v.0 = vel;
        }
        if let Some(mut h) = self.world.get_mut::<Heading>(self.ship) {
            h.0 = heading;
        }
    }

    fn pos(&self) -> Vec2 {
        self.world
            .get::<Position>(self.ship)
            .map(|p| p.0)
            .unwrap_or(Vec2::ZERO)
    }

    fn vel(&self) -> Vec2 {
        self.world
            .get::<Velocity>(self.ship)
            .map(|v| v.0)
            .unwrap_or(Vec2::ZERO)
    }
}

/// One headless networked client driven by a fixed scripted input loop (T068).
///
/// Exposes **numeric signals only** — predicted local state, seq/ack bookkeeping,
/// reconstructed-state counts, and `NetStats` — so a scenario asserts hard
/// numbers, never a rendered/visual judgment. The local ship is predicted through
/// the shared sim; received snapshots are delta-reconstructed via
/// `protocol::apply_delta` exactly as the real client does.
///
/// 8b: the bot holds its **own** client-side transport behind a `Box<dyn
/// NetTransport>` (the swap seam) rather than a concrete loopback end, so the
/// IDENTICAL `ScriptedBot` reconstruction/prediction logic runs over loopback
/// (T071/T072) AND over the renet UDP adapter (T069/T070) — only the transport
/// underneath differs (SC-006/SC-008). For loopback the boxed transport is a
/// clone of the shared switch end (zero-latency, lossless); for renet it is this
/// bot's own `RenetTransport` client (pumped each tick).
pub struct ScriptedBot {
    /// The bot's own client-side transport (loopback clone or renet client).
    transport: Box<dyn NetTransport>,
    /// The bot's own connection handle (client side).
    conn: ConnectionId,
    /// The bot's own ship's network id, learned at handshake.
    local_id: EntityId,
    /// The fixed scripted input loop.
    script: InputScript,
    /// Monotonic per-client input sequence number (next to send).
    next_seq: u32,
    /// The bot's predicted local-ship simulation (shared sim, AD-005).
    predictor: BotPredictor,
    /// Running acked delta baseline (id + reconstructed full state), folded by
    /// `apply_delta` exactly as the real client reconstructs (T063).
    acked_baseline_id: Option<u16>,
    /// The reconstructed full state of the acked baseline.
    baseline: FullState,
    /// The most recent reconstructed full state (latest snapshot applied).
    latest_full: FullState,
    /// The highest input seq the server has acked for this bot (from the snapshot
    /// `acked_input_seq`) — the reconciliation anchor.
    last_acked_input_seq: u32,
    /// Count of snapshots successfully reconstructed (a numeric liveness signal).
    snapshots_reconstructed: u32,
    /// The `baseline_id` of the most recent snapshot received (numeric signal:
    /// `KEYFRAME_BASELINE` means the server keyframed this bot).
    last_baseline_id: u16,
}

impl ScriptedBot {
    /// The bot's own ship network id (the entity it predicts and reconciles).
    pub fn local_id(&self) -> EntityId {
        self.local_id
    }

    /// The bot's predicted local-ship position (sim units) — a numeric signal.
    pub fn predicted_pos(&self) -> Vec2 {
        self.predictor.pos()
    }

    /// The bot's predicted local-ship velocity (sim units/s) — a numeric signal.
    pub fn predicted_vel(&self) -> Vec2 {
        self.predictor.vel()
    }

    /// The next input sequence number the bot will send (seq bookkeeping signal).
    pub fn next_seq(&self) -> u32 {
        self.next_seq
    }

    /// The highest input seq the server has acked for this bot (reconciliation
    /// anchor; a numeric ack-bookkeeping signal).
    pub fn last_acked_input_seq(&self) -> u32 {
        self.last_acked_input_seq
    }

    /// Number of snapshots the bot has successfully reconstructed (liveness).
    pub fn snapshots_reconstructed(&self) -> u32 {
        self.snapshots_reconstructed
    }

    /// The `baseline_id` of the most recent snapshot — `Snapshot::KEYFRAME_BASELINE`
    /// iff the server keyframed this bot (a lost-ack / first-snapshot signal).
    pub fn last_baseline_id(&self) -> u16 {
        self.last_baseline_id
    }

    /// The number of entities in the bot's most recently reconstructed full state
    /// (so a scenario can assert it sees the other bots' ships).
    pub fn visible_entity_count(&self) -> usize {
        self.latest_full.len()
    }

    /// The reconstructed authoritative position of `id` in the bot's latest full
    /// state (dequantized), if present — used to assert the bot reconstructs the
    /// SAME world the server holds (equivalence, 8b).
    pub fn reconstructed_pos(&self, id: EntityId) -> Option<Vec2> {
        self.latest_full.get(id).map(|r| r.pos.dequantize_pos())
    }
}

/// The server-side connection id the bot's snapshots are metered under. For
/// loopback the switch mints the server conn id immediately after the client conn
/// id (`client_conn + 1`, see `LoopbackTransport::connect`); for renet the server
/// mints its own conn ids in accept order. Recorded per bot at handshake so the
/// bandwidth meter (keyed by the SERVER-side id) is read correctly for either
/// transport.
type ServerConn = ConnectionId;

/// The headless multi-client bot harness (T068, generalized in 8b).
///
/// Owns one embedded authoritative [`ServerApp`] and ≥ 2 [`ScriptedBot`]s. Each
/// bot holds its OWN client-side `Box<dyn NetTransport>` (the swap seam), so the
/// SAME harness drives the bots over the in-memory loopback (default; T071/T072)
/// AND over the renet UDP adapter (T069/T070) — only the transport differs
/// (SC-006/SC-008). No rendering. [`BotHarness::step_all`] advances one full tick
/// across every bot; the bots' numeric accessors are the only signals a scenario
/// reads.
pub struct BotHarness {
    /// The embedded authoritative server.
    pub server: ServerApp,
    /// The bots, in connection order.
    bots: Vec<ScriptedBot>,
    /// Per-bot SERVER-side connection id (the key the bandwidth meter uses).
    server_conns: Vec<ServerConn>,
    /// The transport pump: how a tick is driven between the bots and the server
    /// (a no-op for loopback, a renet `update(dt)` + micro-sleep for renet).
    pump: Pump,
    /// The fixed step (seconds) the renet pump advances per tick.
    dt_secs: f32,
}

/// The per-tick renet pump driver: given the bots, the server, and the fixed
/// step (seconds), it flushes bot inputs out, runs the authoritative tick, and
/// delivers snapshots back (via each transport's `pump`). Boxed so the non-udp
/// build needs no renet types here and the renet harness can capture its own
/// state. Aliased to keep [`Pump`] / [`BotHarness::from_parts`] off the
/// `clippy::type_complexity` radar.
pub type PumpFn = Box<dyn FnMut(&mut [ScriptedBot], &mut ServerApp, f32)>;

/// How [`BotHarness::step_all`] moves bytes between the bot transports and the
/// server transport for a tick. Loopback is synchronous (a send is immediately
/// visible to the peer's recv), so it needs no pump. Renet drives the netcode
/// pump (`update(dt)` on every client + the server) plus a tiny real sleep so the
/// OS can ferry datagrams between the bound sockets.
enum Pump {
    /// In-memory loopback: synchronous, no pump needed.
    Loopback,
    /// Renet UDP: a renet pump driver supplied by the renet harness constructor.
    Renet(PumpFn),
}

impl BotHarness {
    /// Build a **loopback** harness with one bot per `scripts` entry (≥ 2 required
    /// by T068), each connected and handshaked against the embedded server so it
    /// knows its own ship id. The server's announced tick rate seeds each bot's
    /// predicted fixed step. This is the default, deterministic, zero-latency
    /// path (T071/T072); the renet variant is [`renet_harness`].
    pub fn new(scripts: Vec<InputScript>) -> Self {
        assert!(
            scripts.len() >= 2,
            "the bot harness drives ≥ 2 networked clients (T068)"
        );
        let (mut server, client) = ServerApp::loopback();
        let dt = 1.0 / server.rates().tick_rate_hz as f32;

        // Connect every bot (each on a clone of the shared client transport, so
        // they share the server's switch), then tick once so the server accepts
        // them all and replies with each bot's ship id.
        let mut pending: Vec<(LoopbackEnd, ConnectionId, InputScript)> = Vec::new();
        for (i, script) in scripts.into_iter().enumerate() {
            let mut t = client.clone();
            let conn = t.connect(bot_addr(i as u64));
            t.send_reliable(conn, &connect_msg());
            pending.push((t, conn, script));
        }
        server.tick(); // accept all + reply

        let mut bots = Vec::new();
        let mut server_conns = Vec::new();
        for (mut t, conn, script) in pending {
            let mut local_id = None;
            for m in t.recv(conn) {
                if let Message::ConnectAccepted(a) = m {
                    local_id = Some(a.client_id);
                }
            }
            let local_id = local_id.expect("each bot is accepted and told its ship id");
            // Loopback mints the server conn id immediately after the client one.
            server_conns.push(ConnectionId(conn.0 + 1));
            bots.push(ScriptedBot {
                transport: Box::new(t),
                conn,
                local_id,
                script,
                next_seq: 1,
                predictor: BotPredictor::new(dt),
                acked_baseline_id: None,
                baseline: FullState::new(),
                latest_full: FullState::new(),
                last_acked_input_seq: 0,
                snapshots_reconstructed: 0,
                last_baseline_id: 0,
            });
        }

        Self {
            server,
            bots,
            server_conns,
            pump: Pump::Loopback,
            dt_secs: dt,
        }
    }

    /// Construct a harness from already-handshaked parts (the renet constructor
    /// builds these after driving the netcode connect pump). `server_conns[i]` is
    /// bot `i`'s SERVER-side connection id (the bandwidth-meter key) and `pump`
    /// drives the per-tick netcode pump.
    pub fn from_parts(
        server: ServerApp,
        bots: Vec<ScriptedBot>,
        server_conns: Vec<ServerConn>,
        pump: PumpFn,
        dt_secs: f32,
    ) -> Self {
        assert_eq!(
            bots.len(),
            server_conns.len(),
            "one server conn per bot is required"
        );
        Self {
            server,
            bots,
            server_conns,
            pump: Pump::Renet(pump),
            dt_secs,
        }
    }

    /// Number of bots the harness drives.
    pub fn bot_count(&self) -> usize {
        self.bots.len()
    }

    /// Read-only access to bot `i`'s numeric signals.
    pub fn bot(&self, i: usize) -> &ScriptedBot {
        &self.bots[i]
    }

    /// Read-only access to all bots.
    pub fn bots(&self) -> &[ScriptedBot] {
        &self.bots
    }

    /// The bot's own ship network id (so a test can assert the dropped bot's ship
    /// was removed from authority after a disconnect, T071).
    pub fn bot_local_id(&self, i: usize) -> EntityId {
        self.bots[i].local_id
    }

    /// The per-client bytes/sec figures the server's meter recorded for bot `i`
    /// (mean/peak/total) — a numeric bandwidth signal for 8b. Keyed by the bot's
    /// recorded SERVER-side connection id (loopback or renet).
    pub fn bot_bandwidth(&self, i: usize) -> (f32, u64, u64) {
        let server_conn = self.server_conns[i];
        let meter = self.server.bandwidth_meter();
        (
            meter.mean_bytes_per_sec(server_conn),
            meter.peak_window_bytes(server_conn),
            meter.total_bytes(server_conn),
        )
    }

    /// Bot `i`'s transport-level `NetStats` (bytes in/out) — a numeric signal.
    pub fn bot_stats(&mut self, i: usize) -> NetStats {
        let conn = self.bots[i].conn;
        self.bots[i].transport.stats(conn)
    }

    /// Advance the whole harness by exactly one authoritative tick (T068):
    /// 1. every bot builds + sends its next scripted numbered `ClientInput`, and
    ///    predicts its own ship through the shared sim (immediate response);
    /// 2. the transport pump + server tick (recv → validate → step → maybe
    ///    broadcast); for loopback the send is already visible, for renet the pump
    ///    drives the netcode update + a micro-sleep for socket delivery;
    /// 3. every bot drains its inbox, **reconstructs** each delta snapshot
    ///    (`apply_delta`), acks it, reconciles its predicted ship against the
    ///    newest reconstructed authoritative state, and records numeric signals.
    pub fn step_all(&mut self) {
        let server_tick = self.server.server_tick();

        // 1) Each bot sends its scripted input and predicts locally.
        for bot in &mut self.bots {
            let intent = bot.script.next();
            let seq = bot.next_seq;
            bot.next_seq += 1;
            let input = ClientInput::new(seq, server_tick, vec![intent]);
            bot.transport
                .send_unreliable(bot.conn, &Message::ClientInput(input));
            bot.predictor.predict(intent);
        }

        // 2) Advance authority one tick. For renet, the pump drives the netcode
        //    update on every client + the server (and a micro-sleep) so the bytes
        //    actually cross the sockets; for loopback the send above is already
        //    visible and the pump is a no-op.
        match &mut self.pump {
            Pump::Loopback => {
                self.server.tick();
            }
            Pump::Renet(pump) => {
                pump(&mut self.bots, &mut self.server, self.dt_secs);
            }
        }

        // 3) Each bot drains, reconstructs, acks, reconciles.
        for bot in &mut self.bots {
            let messages = bot.transport.recv(bot.conn);
            let mut newest_full: Option<Snapshot> = None;
            for m in messages {
                let Message::Snapshot(delta) = m else {
                    continue;
                };
                bot.last_baseline_id = delta.baseline_id;
                // Reconstruct the full state from baseline + delta (the same
                // `apply_delta` the real client uses).
                let full_state = if delta.is_keyframe() {
                    apply_delta(&FullState::new(), &delta)
                } else if bot.acked_baseline_id == Some(delta.baseline_id) {
                    apply_delta(&bot.baseline, &delta)
                } else if bot.acked_baseline_id.is_none() && delta.baseline_id == 0 {
                    apply_delta(&FullState::new(), &delta)
                } else {
                    // Unreconstructable (baseline we never acked): drop it.
                    continue;
                };

                // Adopt + ack (so the server advances this bot's delta baseline).
                bot.acked_baseline_id = Some(delta.wire_id());
                bot.baseline = full_state.clone();
                bot.latest_full = full_state.clone();
                bot.snapshots_reconstructed += 1;
                bot.last_acked_input_seq = delta.acked_input_seq;
                bot.transport.send_unreliable(
                    bot.conn,
                    &Message::SnapshotAck(SnapshotAck {
                        last_snapshot_id: delta.wire_id(),
                    }),
                );

                // The reconstructed full snapshot the reconcile consumes.
                newest_full = Some(Snapshot {
                    server_tick: delta.server_tick,
                    acked_input_seq: delta.acked_input_seq,
                    baseline_id: delta.baseline_id,
                    entities: full_state.to_records(),
                    removed: Vec::new(),
                });
            }

            // Reconcile the predicted local ship against the newest authoritative
            // state (re-seed only; replay of unacked inputs is the client crate's
            // job — the bot's signal is the reconstructed authoritative value).
            if let Some(snap) = newest_full {
                if let Some(rec) = snap
                    .entities
                    .iter()
                    .find(|r| r.id == bot.local_id && r.kind == EntityKind::Ship)
                {
                    bot.predictor.reseed(
                        rec.pos.dequantize_pos(),
                        rec.vel.dequantize_vel(),
                        rec.heading.dequantize(),
                    );
                }
            }
        }
    }

    /// Drive `ticks` full harness steps (convenience for scenarios).
    pub fn run_ticks(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.step_all();
        }
    }

    /// T071: bot `i` sends a clean `Disconnect` (reliable), the way a real client
    /// drops mid-session. The server frees ONLY that slot on its next drain (its
    /// ship despawns, its session/baseline/meter bookkeeping is dropped) and every
    /// remaining client continues. The dropped bot stays in the harness's `bots`
    /// vec so the test can confirm it stops being served, but it sends nothing
    /// further (its scripted loop is left untouched; the test simply stops driving
    /// expectations for it).
    pub fn disconnect_bot(&mut self, i: usize) {
        let bot = &mut self.bots[i];
        bot.transport.send_reliable(
            bot.conn,
            &Message::Disconnect(protocol::Disconnect {
                reason: protocol::DisconnectReason::ClientClosed,
            }),
        );
    }
}

/// A distinct (deterministic) loopback endpoint key per bot index so repeated bots
/// in one harness do not alias on the switch.
fn bot_addr(index: u64) -> SocketAddr {
    let port = 41_000u16.wrapping_add((index % 1000) as u16);
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// A forward-thrust intent (a common scripted input across the scenarios).
pub fn forward_intent() -> QuantizedIntent {
    QuantizedIntent {
        forward: 1,
        strafe: 0,
        turn: 0,
        fire: false,
        toggle_assist: false,
    }
}

// =============================================================================
// Renet UDP harness (8b — T069/T070). Gated behind the `udp` feature so the
// default (loopback-only) build never pulls in renet.
// =============================================================================

#[cfg(feature = "udp")]
mod renet {
    use super::*;
    use ::protocol::RenetTransport;
    use std::net::UdpSocket;
    use std::time::Duration;

    /// Upper bound on netcode-connect drive iterations before giving up (generous;
    /// well past the handshake at ~16 ms steps).
    const CONNECT_MAX_ITERS: usize = 2000;
    /// A tiny real sleep so the OS ferries datagrams between the bound 127.0.0.1
    /// sockets within a drive iteration (the connect handshake + per-tick pump).
    const STEP_SLEEP: Duration = Duration::from_micros(200);
    /// How many micro-pump rounds the per-tick renet pump runs each direction so a
    /// snapshot reliably lands within the same harness step (bounded, not a real
    /// 30 s wall-clock — the run is 900 ticks).
    const PUMP_ROUNDS: usize = 3;

    /// Build a renet-UDP-backed [`BotHarness`]: one **unsecure** renet server (the
    /// payload byte figure is transport-security-independent, so unsecure is used
    /// for speed — T070) and `scripts.len()` renet client bots, all on ephemeral
    /// 127.0.0.1 sockets (TR-041). Drives the netcode connect handshake to
    /// completion, learns each bot's ship id, resolves each bot's SERVER-side conn
    /// id from the session (for the bandwidth meter), and installs a per-tick pump
    /// that drives renet's update/socket flush so `BotHarness::step_all` runs
    /// unchanged over the real socket path.
    pub fn renet_harness(scripts: Vec<InputScript>) -> BotHarness {
        assert!(
            scripts.len() >= 2,
            "the bot harness drives ≥ 2 networked clients (T068)"
        );

        // --- Server on an ephemeral 127.0.0.1 UDP port. ----------------------
        let server_socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("bind renet server udp socket on 127.0.0.1");
        let server_addr = server_socket
            .local_addr()
            .expect("renet server socket has a local addr");
        let server_transport = RenetTransport::unsecure_server(server_socket, server_addr)
            .expect("build unsecure renet server");
        let mut server = ServerApp::new(Box::new(server_transport), Default::default())
            .expect("default RateConfig satisfies the start invariant");
        let dt = 1.0 / server.rates().tick_rate_hz as f32;
        let dt_dur = Duration::from_secs_f32(dt);

        // --- One renet client per bot. ---------------------------------------
        let n = scripts.len();
        let mut clients: Vec<RenetTransport> = Vec::with_capacity(n);
        let mut client_conns: Vec<ConnectionId> = Vec::with_capacity(n);
        for i in 0..n {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .expect("bind renet client udp socket on 127.0.0.1");
            let mut client = RenetTransport::unsecure_client(socket, server_addr, 100 + i as u64)
                .expect("build unsecure renet client");
            let conn = client.connect(server_addr);
            clients.push(client);
            client_conns.push(conn);
        }

        // --- Drive the netcode connect handshake to completion. --------------
        // Pump server + every client until all clients are connected at the renet
        // layer, then run the protocol Connect/ConnectAccepted handshake.
        for _ in 0..CONNECT_MAX_ITERS {
            for c in clients.iter_mut() {
                let _ = c.update(dt_dur);
            }
            server.pump_transport(dt_dur);
            if clients.iter().all(|c| c.is_connected()) {
                break;
            }
            std::thread::sleep(STEP_SLEEP);
        }
        assert!(
            clients.iter().all(|c| c.is_connected()),
            "all renet clients must connect to the server over UDP"
        );

        // Each client sends its protocol `Connect` (reliable); the server admits
        // it on a tick and replies `ConnectAccepted`. Drive until every bot has
        // learned its ship id.
        for (i, c) in clients.iter_mut().enumerate() {
            c.send_reliable(client_conns[i], &connect_msg());
        }
        let mut local_ids: Vec<Option<EntityId>> = vec![None; n];
        for _ in 0..CONNECT_MAX_ITERS {
            for c in clients.iter_mut() {
                let _ = c.update(dt_dur);
            }
            server.tick(); // accept + spawn + reply (pumps the server transport)
            for (i, c) in clients.iter_mut().enumerate() {
                let _ = c.update(dt_dur);
                for m in c.recv(client_conns[i]) {
                    if let Message::ConnectAccepted(a) = m {
                        local_ids[i] = Some(a.client_id);
                    }
                }
            }
            if local_ids.iter().all(|id| id.is_some()) {
                break;
            }
            std::thread::sleep(STEP_SLEEP);
        }
        let local_ids: Vec<EntityId> = local_ids
            .into_iter()
            .map(|id| id.expect("every renet bot must be accepted and told its ship id"))
            .collect();

        // --- Resolve each bot's SERVER-side conn id from the session. --------
        // The session maps a server-side `ConnectionId` to the `ClientState` whose
        // `entity_id` is exactly the `client_id` the bot learned, so we key the
        // bandwidth meter correctly regardless of renet's accept order.
        let server_conns: Vec<ConnectionId> = local_ids
            .iter()
            .map(|wire_id| {
                server
                    .session()
                    .iter()
                    .find(|(_, st)| st.entity_id == *wire_id)
                    .map(|(conn, _)| conn)
                    .expect("each accepted bot has a server-side session slot")
            })
            .collect();

        // --- Assemble the bots (same `ScriptedBot` logic as loopback). -------
        let mut bots = Vec::with_capacity(n);
        for ((client, conn), (local_id, script)) in clients
            .into_iter()
            .zip(client_conns)
            .zip(local_ids.into_iter().zip(scripts))
        {
            bots.push(ScriptedBot {
                transport: Box::new(client),
                conn,
                local_id,
                script,
                next_seq: 1,
                predictor: BotPredictor::new(dt),
                acked_baseline_id: None,
                baseline: FullState::new(),
                latest_full: FullState::new(),
                last_acked_input_seq: 0,
                snapshots_reconstructed: 0,
                last_baseline_id: 0,
            });
        }

        // --- The per-tick renet pump. ----------------------------------------
        // Flush bot inputs onto the wire, run the server tick (recv → step →
        // queue snapshots), then flush + deliver the snapshots back to the bots —
        // a few micro-pump rounds + tiny sleeps each direction so the datagrams
        // actually traverse the sockets within the step. Bounded (PUMP_ROUNDS),
        // NOT a real 30 s wall-clock.
        let pump: PumpFn = Box::new(
            move |bots: &mut [ScriptedBot], server: &mut ServerApp, dt: f32| {
                let dt_dur = Duration::from_secs_f32(dt);
                // Push the just-queued bot inputs/acks out toward the server.
                for _ in 0..PUMP_ROUNDS {
                    for bot in bots.iter_mut() {
                        bot.transport.pump(dt_dur);
                    }
                    server.pump_transport(dt_dur);
                    std::thread::sleep(STEP_SLEEP);
                }
                // The authoritative step: recv inputs → step sim → queue snapshots.
                server.tick();
                // Flush queued snapshots out and deliver them to the bots.
                for _ in 0..PUMP_ROUNDS {
                    server.pump_transport(dt_dur);
                    for bot in bots.iter_mut() {
                        bot.transport.pump(dt_dur);
                    }
                    std::thread::sleep(STEP_SLEEP);
                }
            },
        );

        BotHarness::from_parts(server, bots, server_conns, pump, dt)
    }
}

// Re-exported for the udp-gated equivalence/bandwidth tests. `allow(unused)`
// because the `harness` test crate (loopback-only) compiles `botkit` too but
// never uses the renet constructor.
#[cfg(feature = "udp")]
#[allow(unused_imports)]
pub use renet::renet_harness;
