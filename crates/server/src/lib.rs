//! Headless authoritative server (E003, OBJ1) — the single source of gameplay
//! truth (Principle I).
//!
//! [`ServerApp`] is a synchronous fixed-tick `bevy_ecs` app (AD-002): it owns a
//! [`World`], the **shared** fixed-step [`Schedule`] (registered via
//! [`sim::add_fixed_step_systems`] so server and client run bit-identical logic,
//! Principle II / HINT-003), a [`Box<dyn NetTransport>`], and a [`Session`]. Each
//! tick it drains the transport, validates-and-applies client input, steps the
//! sim once at [`sim::FixedDt`], and (on snapshot ticks) **delta-encodes** a
//! per-client [`Snapshot`] against that client's last-acked baseline, sending it
//! to every client (recv → validate → step → delta-encode → send). The delta
//! encoder, MTU bound, lost-ack keyframe degradation, and bytes/client/sec meter
//! live in [`snapshot`] (OBJ6); each client's baseline is cached per connection.
//!
//! Loopback ([`ServerApp::loopback`]) holds the server end of a
//! [`LoopbackTransport`] pair: an in-process client runs through the **identical**
//! session + validation path as a networked client (TR-003, T022) — only the
//! transport differs. The renet UDP adapter (Phase 4) drops in behind the same
//! `NetTransport` boundary with no change here.
//!
//! The bulk lives in this library so the integration tests
//! (`tests/session.rs`, `tests/rates.rs`) and the thin `main.rs` binary share
//! one implementation.

// ECS systems take tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

mod session;
pub mod snapshot;
pub mod validation;

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use glam::Vec2;
use protocol::{
    ClientInput, ConnectionId, DisconnectReason, EntityId, EntityKind, EntityRecord, FullState,
    LoopbackTransport, Message, NetTransport, QAngle, QVec2, Snapshot,
};
use sim::components::{
    AngularVelocity, CollisionRadius, FlightAssist, Heading, Health, Position, Projectile, Ship,
    Target, TargetKind, Velocity, Weapon,
};
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

pub use session::{
    decode_inbound, ClientState, DropReason, InputDisposition, RateDecision, RejectionCategory,
    RejectionEvent, RejectionLog, Session, IDLE_TIMEOUT_SECS, INBOUND_RATE_LIMIT_PER_SEC,
    MAX_CLIENTS, MAX_PAYLOAD_BYTES, REJECTION_LOG_CAPACITY, UNACKED_BUFFER_BOUND,
};
pub use snapshot::{
    encode_snapshot, encoded_len, BandwidthMeter, EncodeParams, MAX_SNAPSHOT_BYTES,
};
pub use validation::{
    apply_authoritative, fire_allowed, resolve_hit, rewind, validate_input, History,
    TransformSample, ValidatedIntent, HISTORY_LEN, INTERP_DELAY, MAX_REWIND,
};

/// Wire protocol version this server speaks. A [`protocol::Connect`] must match
/// it exactly (TR-024); bumped on any wire-breaking change.
pub const PROTOCOL_VERSION: u16 = 1;

/// Server-announced session rates (TR-044). These are **not negotiated**: the
/// server emits them in every [`protocol::ConnectAccepted`] and the client adopts
/// them. The invariant `snapshot_rate_hz < tick_rate_hz` is enforced at start
/// (snapshots may not outpace the sim) — see [`RateConfig::validate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateConfig {
    /// Fixed simulation tick rate (Hz). The sim steps once per tick.
    pub tick_rate_hz: u16,
    /// Snapshot send rate (Hz). MUST be `< tick_rate_hz` (enforced invariant).
    pub snapshot_rate_hz: u16,
    /// Interpolation delay (ms) the client buffers remote entities by (TR-010).
    pub interp_delay_ms: u16,
}

impl Default for RateConfig {
    /// The server-announced defaults: tick 30 Hz, snapshot 20 Hz, interp 100 ms.
    fn default() -> Self {
        Self {
            tick_rate_hz: 30,
            snapshot_rate_hz: 20,
            interp_delay_ms: 100,
        }
    }
}

impl RateConfig {
    /// Enforce the start invariant (TR-044): the snapshot rate must be strictly
    /// below the tick rate, so snapshots never outpace the authoritative sim.
    /// This is an enforced invariant, not a note — a violation is a config error.
    pub fn validate(&self) -> Result<(), RateConfigError> {
        if self.tick_rate_hz == 0 {
            return Err(RateConfigError::ZeroTickRate);
        }
        if self.snapshot_rate_hz >= self.tick_rate_hz {
            return Err(RateConfigError::SnapshotRateNotBelowTickRate {
                snapshot_rate_hz: self.snapshot_rate_hz,
                tick_rate_hz: self.tick_rate_hz,
            });
        }
        Ok(())
    }

    /// The fixed timestep in seconds derived from the tick rate.
    fn fixed_dt(&self) -> f32 {
        1.0 / self.tick_rate_hz as f32
    }

    /// How many ticks elapse between snapshot sends (≥ 1, since
    /// `snapshot_rate_hz < tick_rate_hz`).
    fn ticks_per_snapshot(&self) -> u32 {
        (self.tick_rate_hz / self.snapshot_rate_hz).max(1) as u32
    }
}

/// Why a [`RateConfig`] failed its start invariant (TR-044).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateConfigError {
    /// `snapshot_rate_hz >= tick_rate_hz` — snapshots would outpace the sim.
    SnapshotRateNotBelowTickRate {
        snapshot_rate_hz: u16,
        tick_rate_hz: u16,
    },
    /// `tick_rate_hz == 0` — the sim cannot step at zero Hz.
    ZeroTickRate,
}

impl std::fmt::Display for RateConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateConfigError::SnapshotRateNotBelowTickRate {
                snapshot_rate_hz,
                tick_rate_hz,
            } => write!(
                f,
                "snapshot_rate_hz ({snapshot_rate_hz}) must be < tick_rate_hz ({tick_rate_hz})"
            ),
            RateConfigError::ZeroTickRate => write!(f, "tick_rate_hz must be > 0"),
        }
    }
}

impl std::error::Error for RateConfigError {}

/// Per-connection runtime link the server holds outside the [`Session`]: which
/// ECS [`Entity`] backs a connection's ship, and that client's latest decoded
/// intent (staged here, then written to the ship's own [`ShipIntent`] component
/// before each step).
struct ClientLink {
    /// The ECS entity that is this client's owned ship.
    ship: Entity,
    /// The network entity id assigned to the ship (stable across the wire). The
    /// session also records it; held here for direct connection→entity lookup.
    entity_id: EntityId,
    /// The client's latest validated intent, applied to its ship on the next
    /// step. Each ship is driven by its OWN intent (per-entity), so N clients
    /// pilot N ships independently within the single shared step.
    latest_intent: ShipIntent,
}

impl ClientLink {
    /// The wire id of this client's owned ship.
    fn entity_id(&self) -> EntityId {
        self.entity_id
    }
}

/// One entity's **unquantized** authoritative render pose, as read directly from
/// the server `world` by [`ServerApp::render_state`].
///
/// This is the in-process render seam the windowed solo client draws from: for
/// loopback there is no real latency, so rendering the embedded server's world at
/// full `f32` precision (rather than through the quantized snapshot wire +
/// predict/interpolate netcode) gives crisp, in-sync collision and hits. It is
/// keyed by the SAME wire [`EntityId`] the snapshots use, so the client can
/// find-or-spawn and despawn its rendered entities by stable id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderEntity {
    /// Stable wire id (same mapping the snapshots use).
    pub id: EntityId,
    /// What the entity is (picks the client mesh/material).
    pub kind: EntityKind,
    /// Sub-kind tag for targets ([`TargetKind::as_u8`]); `0` for ship/projectile.
    pub flags: u8,
    /// World-space position on the 2D gameplay plane (full `f32`, unquantized).
    pub pos: Vec2,
    /// Facing angle (radians): ship `Heading`, projectile `vel.to_angle()`, `0.0`
    /// for targets.
    pub heading: f32,
    /// Linear velocity (full `f32`, unquantized) — drives the HUD SPD readout for
    /// the local ship.
    pub vel: Vec2,
}

/// The headless authoritative server app.
pub struct ServerApp {
    /// The authoritative ECS world (single source of truth, Principle I).
    world: World,
    /// The shared fixed-step gameplay schedule (server == client, HINT-003).
    schedule: Schedule,
    /// The transport seam — loopback now, renet (Phase 4) later, swapped behind
    /// the same trait (SC-006).
    transport: Box<dyn NetTransport>,
    /// Session table + handshake policy (shared by every transport, T022).
    session: Session,
    /// Server-announced rates and the enforced start invariant.
    rates: RateConfig,
    /// Connection → runtime link (owned ship entity + latest intent).
    links: HashMap<ConnectionId, ClientLink>,
    /// Mints stable network ids for replicated entities (ships, projectiles,
    /// targets) whose `bevy_ecs::Entity` ids must not cross the wire.
    entity_ids: EntityIdAllocator,
    /// The current authoritative server tick (monotonic).
    server_tick: u32,
    /// Rolling snapshot id, incremented per snapshot broadcast (TR-013).
    snapshot_id: u16,
    /// T054 (TR-017): per-entity transform-history ring, keyed by the wire
    /// [`EntityId`]. Sampled every tick (after the step) so a fire/hit can rewind
    /// candidate targets to the firer's viewed time for server-authoritative,
    /// lag-compensated hit resolution. Sized to cover ≥ 500 ms (see
    /// [`validation::HISTORY_LEN`]); a too-old rewind falls back to the oldest
    /// retained sample (no extrapolation).
    history: HashMap<EntityId, History>,
    /// T054 (TR-017): per-connection smoothed round-trip-time estimate (seconds).
    /// Over loopback this stays ≈ 0, so the rewind ≈ the interpolation delay. A
    /// real measurement (snapshot-ack timing) is a WAN-tuning concern explicitly
    /// deferred; this baseline keeps the rewind correct under zero latency.
    rtt: HashMap<ConnectionId, f32>,
    /// T063/T064: per-connection delta baseline cache. Holds the snapshots sent to
    /// each client (by id) until one is acked, plus the currently-acked baseline
    /// the next delta is computed against. A lost ack just leaves the baseline
    /// where it was; an unknown baseline triggers a keyframe (T064).
    baselines: HashMap<ConnectionId, BaselineCache>,
    /// T066 (TR-014): per-connection bytes/client/sec meter, credited the encoded
    /// payload bytes of every snapshot send (the 8b bandwidth-test figure).
    meter: snapshot::BandwidthMeter,
}

impl ServerApp {
    /// Build a server over an arbitrary transport with the given rates,
    /// enforcing the rate invariant at start (TR-044). Returns an error rather
    /// than panicking so callers (and tests) can assert the invariant fires.
    pub fn new(
        transport: Box<dyn NetTransport>,
        rates: RateConfig,
    ) -> Result<Self, RateConfigError> {
        // T019: the rate invariant is enforced at start, not merely noted.
        rates.validate()?;

        let mut world = World::new();
        world.insert_resource(Tuning::default());
        world.insert_resource(FixedDt(rates.fixed_dt()));
        // Intent is per-entity now (a `ShipIntent` component on each ship), not a
        // global resource — so each client ship is piloted by its own input.
        world.insert_resource(HitFeedback::default());

        let mut schedule = Schedule::default();
        // The single shared entry point: server steps the SAME systems in the
        // SAME order as the client (Principle II / HINT-003).
        sim::add_fixed_step_systems(&mut schedule);

        Ok(Self {
            world,
            schedule,
            transport,
            session: Session::new(PROTOCOL_VERSION, rates),
            rates,
            links: HashMap::new(),
            entity_ids: EntityIdAllocator::new(),
            server_tick: 0,
            snapshot_id: 0,
            history: HashMap::new(),
            rtt: HashMap::new(),
            baselines: HashMap::new(),
            meter: snapshot::BandwidthMeter::new(),
        })
    }

    /// Build an embedded server holding the **server end** of a
    /// [`LoopbackTransport`] pair, plus the client end an in-process client uses
    /// to connect (T022). The client connects and exchanges messages through the
    /// **identical** session + validation path as a networked client — loopback
    /// is a transport, not an authority/validation bypass.
    ///
    /// Returns `(server_app, client_transport)`; the client calls
    /// [`NetTransport::connect`] on `client_transport`, then the server observes
    /// it via [`NetTransport::accept`] on its next [`ServerApp::tick`].
    pub fn loopback() -> (ServerApp, LoopbackTransport) {
        Self::loopback_with_rates(RateConfig::default())
            .expect("default RateConfig satisfies the start invariant")
    }

    /// [`ServerApp::loopback`] with explicit rates, surfacing the rate-invariant
    /// error so the rates test (T024) can assert it fires.
    pub fn loopback_with_rates(
        rates: RateConfig,
    ) -> Result<(ServerApp, LoopbackTransport), RateConfigError> {
        let (client, server) = LoopbackTransport::pair();
        let app = ServerApp::new(Box::new(server), rates)?;
        Ok((app, client))
    }

    /// The current authoritative server tick.
    pub fn server_tick(&self) -> u32 {
        self.server_tick
    }

    /// The server-announced session rates.
    pub fn rates(&self) -> RateConfig {
        self.rates
    }

    /// Read-only access to the session (connection table, ack anchors).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Mutable access to the session — used to configure policy before the loop
    /// runs (e.g. banning a token to exercise the reject-and-close path, T021).
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Read-only access to the authoritative world (for tests/inspection).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Mutable access to the authoritative world (for tests/inspection and the
    /// deterministic forced-mismatch harness, T038/TR-035). A real deployment
    /// mutates the world only through the validated tick loop; this accessor
    /// exists so a test can script a reproducible one-tick authoritative override
    /// (an injected divergence the client did not predict) without an
    /// authority bypass in the production path.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The ECS [`Entity`] backing the ship owned by `conn`, if it is a live
    /// client. Pairs with [`ServerApp::world_mut`] so a test can address the
    /// authoritative ship directly (e.g. to inject a scripted override, T038).
    pub fn client_ship_entity(&self, conn: ConnectionId) -> Option<Entity> {
        self.links.get(&conn).map(|link| link.ship)
    }

    /// The ECS [`Entity`] backing the ship with the given wire [`EntityId`] (the
    /// id a client learns from its [`protocol::ConnectAccepted`]). Pairs with
    /// [`ServerApp::world_mut`] so a test that holds a *client-side* loopback conn
    /// (not the server-side one) can still address the authoritative ship by the
    /// network id it was told (T037/T038).
    pub fn ship_entity_for(&self, id: EntityId) -> Option<Entity> {
        self.links
            .values()
            .find(|link| link.entity_id == id)
            .map(|link| link.ship)
    }

    /// The wire [`EntityId`] of the ship owned by `conn`, if it is a live client.
    /// This is the id that connection's [`protocol::ConnectAccepted`] carried and
    /// the id its ship appears under in every snapshot.
    pub fn client_ship_id(&self, conn: ConnectionId) -> Option<EntityId> {
        self.links.get(&conn).map(ClientLink::entity_id)
    }

    /// Run the fixed-tick loop forever (the server main loop). Each iteration is
    /// one [`ServerApp::tick`]; pacing to wall-clock at `tick_rate_hz` is the
    /// caller's concern (a real deployment sleeps to the tick boundary). Kept
    /// minimal here so the loop body — not timing — is the unit under test.
    pub fn run(&mut self) -> ! {
        loop {
            self.tick();
        }
    }

    /// Advance the authoritative world by exactly one tick (AD-002):
    /// `accept` new connections → `recv` + validate-and-apply each client's input
    /// → step the shared sim once → broadcast a snapshot on snapshot ticks.
    ///
    /// Exposed (not just [`ServerApp::run`]) so tests can drive the server
    /// deterministically one tick at a time.
    pub fn tick(&mut self) {
        // Drive any socket-backed transport's pump one fixed step BEFORE draining
        // (a no-op for the in-memory loopback transport, which is synchronous; the
        // renet UDP adapter overrides `NetTransport::pump` to run its netcode
        // update + socket flush). This lets the SAME tick loop run over loopback
        // and renet unchanged (SC-006/SC-008) — inbound UDP packets are applied to
        // renet's server before `accept`/`recv` reads them this tick.
        self.transport
            .pump(std::time::Duration::from_secs_f32(self.rates.fixed_dt()));
        self.accept_new_connections();
        self.drain_and_apply_inputs();
        self.step_sim();
        // T054 (TR-017): after the authoritative step, sample every replicated
        // entity's transform into its history ring so a later fire can rewind
        // candidate targets to the firer's viewed time.
        self.capture_transform_history();
        self.server_tick += 1;
        // T057 (TR-031): drop sessions idle past the timeout — only those slots,
        // leaving every other client and authoritative state untouched.
        self.drop_timed_out_clients();
        // Snapshot cadence is slower than the tick rate (invariant TR-044), so
        // we only broadcast every `ticks_per_snapshot` ticks.
        if self
            .server_tick
            .is_multiple_of(self.rates.ticks_per_snapshot())
        {
            self.broadcast_snapshots();
        }
    }

    /// Admit connections that arrived since the last tick. Each `Connect` runs
    /// the shared [`Session::handshake`]; on accept a ship is spawned and a
    /// [`protocol::ConnectAccepted`] is sent reliably, on reject a
    /// [`protocol::ConnectRejected`] is sent and the connection is closed (the
    /// reject-and-close path, T021).
    fn accept_new_connections(&mut self) {
        let new_conns = self.transport.accept();
        for conn in new_conns {
            // The client's first reliable message must be a `Connect`.
            for msg in self.transport.recv(conn) {
                if let Message::Connect(connect) = msg {
                    match self.session.handshake(conn, &connect, self.server_tick) {
                        Ok(accepted) => {
                            let ship = self.spawn_client_ship();
                            // Bind the wire id to the ship entity up front so the
                            // id the client was told (`accepted.client_id`) is the
                            // id its ship carries in every snapshot.
                            self.entity_ids.bind(ship, accepted.client_id);
                            self.links.insert(
                                conn,
                                ClientLink {
                                    ship,
                                    entity_id: accepted.client_id,
                                    latest_intent: ShipIntent::default(),
                                },
                            );
                            self.transport
                                .send_reliable(conn, &Message::ConnectAccepted(accepted));
                        }
                        Err(rejected) => {
                            self.transport
                                .send_reliable(conn, &Message::ConnectRejected(rejected));
                            // Reject-and-close: a refused connect holds no slot
                            // and the connection is torn down (T021).
                            self.transport
                                .disconnect(conn, DisconnectReason::ServerClosed);
                        }
                    }
                }
                // Non-Connect first messages are ignored this phase (full
                // protocol-error handling is a later task).
            }
        }
    }

    /// Drain each live connection's inbox and route every message through the
    /// authoritative validate-and-apply path. Loopback and networked clients use
    /// this identical path (no bypass, T022/TR-018).
    ///
    /// Each inbound message is first metered against the per-client inbound rate
    /// limit (T055/TR-028); a throttled message is dropped (and the offender
    /// flagged) before it can mutate any state. A `ClientInput` then flows through
    /// the seq/tick intake classifier (T052/TR-022/023) and per-field validation
    /// (T050/TR-020) in [`ServerApp::validate_and_apply`].
    ///
    /// (The transport already turns bytes into a typed [`Message`] via the same
    /// `Message::decode` that backs [`session::decode_inbound`]; the byte-level
    /// malformed/oversize guard (T056/TR-029/030) lives in `decode_inbound` and is
    /// the function the renet adapter's receive path routes through, so malformed
    /// bytes never reach this typed path.)
    fn drain_and_apply_inputs(&mut self) {
        let conns: Vec<ConnectionId> = self.sorted_conns();
        for conn in conns {
            for msg in self.transport.recv(conn) {
                // T055 (TR-028): meter inbound rate; drop + flag the excess. This
                // also refreshes the per-client idle clock (TR-031).
                if self.session.note_inbound(conn, self.server_tick) == RateDecision::Throttle {
                    continue;
                }
                match msg {
                    Message::ClientInput(input) => self.validate_and_apply(conn, input),
                    Message::SnapshotAck(ack) => {
                        self.session.record_snapshot_ack(conn, ack.last_snapshot_id);
                        // T063: promote this client's delta baseline to the full
                        // state of the snapshot it just acked, so the next delta is
                        // computed against a KNOWN-received baseline (never an
                        // unacked one). An ack for a snapshot we no longer hold is
                        // ignored (the cache keeps only recent sends).
                        if let Some(cache) = self.baselines.get_mut(&conn) {
                            cache.promote(ack.last_snapshot_id);
                        }
                    }
                    Message::Disconnect(_) => self.disconnect_client(conn),
                    // A late `Connect` on a live connection is ignored; other
                    // s→c message kinds are not expected from a client.
                    _ => {}
                }
            }
        }
    }

    /// Authoritative input validation + application chokepoint (Principle I,
    /// TR-001/011/012/018/020/022/023). Every client input — loopback or
    /// networked — flows through here; loopback is not a bypass.
    ///
    /// 1. **Intake classification (T052/TR-022/023):** classify the input by its
    ///    `seq`/`tick`. A replay/duplicate (or already-superseded out-of-order
    ///    seq) or a stale input is discarded with NO state mutation (it is logged
    ///    for anti-cheat; the seq is *not* re-applied, so each seq is processed at
    ///    most once).
    /// 2. **Per-field validation (T050/TR-020):** clamp the analog axes to the
    ///    valid `-1..=1` range and accept `toggle_assist`; produce a
    ///    [`validation::ValidatedIntent`] that structurally carries no client
    ///    position/hit claim (TR-012).
    /// 3. **Apply (T053):** stage the validated intent on THIS client's own ship
    ///    (per-entity). Motion comes from the server `sim`; the fire is gated by
    ///    the authoritative weapon cooldown in `weapon_fire_system` (T051).
    ///    Anchor the per-client ack at the processed seq (TR-008).
    ///
    /// Public so the validation suite (T058–T060) can drive the authoritative
    /// chokepoint directly with a server-side [`ConnectionId`] and assert the
    /// transport-agnostic validate→classify→apply behavior (loopback is not a
    /// bypass). A real deployment reaches it only via the tick loop's
    /// [`ServerApp::drain_and_apply_inputs`]; this accessor adds no authority
    /// bypass — it runs the identical validation path.
    pub fn validate_and_apply(&mut self, conn: ConnectionId, input: ClientInput) {
        // T052 (TR-022/023): discard replay/stale at intake — never partial-apply.
        if let Some(state) = self.session.client(conn) {
            match Session::classify_input(&state, &input, self.server_tick) {
                InputDisposition::Apply => {}
                InputDisposition::Replay => {
                    self.session
                        .log_rejection(conn, RejectionCategory::Replay, self.server_tick);
                    return;
                }
                InputDisposition::Stale => {
                    self.session
                        .log_rejection(conn, RejectionCategory::Stale, self.server_tick);
                    return;
                }
            }
        }

        // The redundant tail is newest-first; `inputs[0]` is the latest intent.
        let Some(newest) = input.inputs.first().copied() else {
            return;
        };
        // T050 (TR-020): per-field validation — clamp analog axes, accept flags.
        // A clamp is a silently-bounded apply (TR-020); record it as an observed
        // anomaly without rejecting the input.
        if newest.forward.unsigned_abs() > 1
            || newest.strafe.unsigned_abs() > 1
            || newest.turn.unsigned_abs() > 1
        {
            self.session
                .log_rejection(conn, RejectionCategory::Clamped, self.server_tick);
        }
        let validated = validation::validate_input(&newest);
        if let Some(link) = self.links.get_mut(&conn) {
            // T053: apply ONLY the validated intent to this client's ship. The
            // sim governs the resulting motion / constraint resolution (TR-019).
            link.latest_intent = validation::apply_authoritative(validated);
        }
        // Anchor the per-client ack at the newest processed seq (TR-008). This
        // also advances `last_processed_input_seq`, so a later duplicate of this
        // seq is caught as a Replay (each seq processed at most once, TR-023).
        self.session.record_processed_input(conn, input.seq);
    }

    /// T054 (TR-017): sample every replicated entity's authoritative transform
    /// into its history ring at the current server time, so a later fire can
    /// rewind candidate targets to the firer's viewed time. Bounded per entity by
    /// [`validation::HISTORY_LEN`]; entities no longer present are pruned so the
    /// map cannot leak.
    fn capture_transform_history(&mut self) {
        let now = self.server_tick as f32 * self.rates.fixed_dt();
        let mut present: Vec<EntityId> = Vec::new();

        // Ships and targets are the candidate hit targets the firer rewinds.
        let mut ships = self
            .world
            .query_filtered::<(Entity, &Position, &CollisionRadius), With<Ship>>();
        let ship_rows: Vec<(Entity, Vec2, f32)> = ships
            .iter(&self.world)
            .map(|(e, p, r)| (e, p.0, r.0))
            .collect();
        let mut targets = self
            .world
            .query_filtered::<(Entity, &Position, &CollisionRadius), With<Target>>();
        let target_rows: Vec<(Entity, Vec2, f32)> = targets
            .iter(&self.world)
            .map(|(e, p, r)| (e, p.0, r.0))
            .collect();

        for (entity, pos, radius) in ship_rows.into_iter().chain(target_rows) {
            let id = self.entity_ids.id_for(entity);
            present.push(id);
            self.history.entry(id).or_default().push(now, pos, radius);
        }

        // Prune history for entities that no longer exist (no leak).
        self.history.retain(|id, _| present.contains(id));
    }

    /// T057 (TR-031): drop every session idle past [`session::IDLE_TIMEOUT_SECS`]
    /// — only those slots — and log each as an idle timeout. Remaining clients and
    /// the authoritative world are untouched (no slot leak).
    fn drop_timed_out_clients(&mut self) {
        let timed_out = self.session.timed_out(self.server_tick);
        for conn in timed_out {
            self.session
                .log_rejection(conn, RejectionCategory::IdleTimeout, self.server_tick);
            self.disconnect_client(conn);
        }
    }

    /// T054 (TR-012/017): server-authoritative, lag-compensated hit resolution
    /// entry for `shooter`'s shot sweeping `shot_prev → shot_now` against the
    /// candidate `target` (both by wire [`EntityId`]).
    ///
    /// Rewinds the target's recorded transform to the shooter's viewed time
    /// (`now − min(interp_delay + that shooter's RTT, 500 ms)`) and resolves the
    /// hit with the sim swept segment-circle primitive. Hits are resolved against
    /// the rewound position, never a client-asserted one (TR-012). Returns the
    /// time-of-impact along the shot segment on a hit, or `None`. Pure read of the
    /// history rings — mutates nothing.
    pub fn resolve_authoritative_hit(
        &self,
        shooter: ConnectionId,
        target: EntityId,
        shot_prev: Vec2,
        shot_now: Vec2,
    ) -> Option<f32> {
        let now = self.server_tick as f32 * self.rates.fixed_dt();
        let rtt = self.rtt.get(&shooter).copied().unwrap_or(0.0);
        let history = self.history.get(&target)?;
        validation::resolve_hit(history, shot_prev, shot_now, now, rtt)
    }

    /// Read-only access to a target's transform-history ring (T054), addressed by
    /// wire [`EntityId`] — for tests/inspection of the lag-compensation rewind.
    pub fn entity_history(&self, id: EntityId) -> Option<&History> {
        self.history.get(&id)
    }

    /// The smoothed RTT estimate (seconds) for `conn` (T054). Over loopback this
    /// is `0.0` (the default), so the rewind interval ≈ the interpolation delay.
    pub fn client_rtt(&self, conn: ConnectionId) -> f32 {
        self.rtt.get(&conn).copied().unwrap_or(0.0)
    }

    /// Record a smoothed RTT measurement (seconds) for `conn` (T054). Baseline
    /// exponential smoothing; the measurement source (snapshot-ack timing) and WAN
    /// tuning are deferred — this keeps the per-client estimate inspectable now.
    pub fn observe_rtt(&mut self, conn: ConnectionId, sample_secs: f32) {
        let sample = sample_secs.max(0.0);
        let smoothed = self
            .rtt
            .get(&conn)
            .map(|prev| prev * 0.875 + sample * 0.125)
            .unwrap_or(sample);
        self.rtt.insert(conn, smoothed);
    }

    /// Step the shared sim exactly once.
    ///
    /// Intent is **per-entity**: each client's staged [`ShipIntent`] is written
    /// onto its OWN ship's `ShipIntent` component before the step, so the shared
    /// gameplay systems pilot every ship from its own input within the single
    /// shared step (SC-001 / TR-002). The systems are unchanged (Principle II /
    /// HINT-003) — only the data they read is now sourced per-ship.
    fn step_sim(&mut self) {
        // Push each client's latest staged intent onto its ship's component.
        let staged: Vec<(Entity, ShipIntent)> = self
            .links
            .values()
            .map(|link| (link.ship, link.latest_intent))
            .collect();
        for (ship, intent) in staged {
            if let Some(mut component) = self.world.get_mut::<ShipIntent>(ship) {
                *component = intent;
            }
        }

        self.schedule.run(&mut self.world);

        // A toggle is edge-triggered: consume it so it does not re-fire next step
        // — on both the staged buffer and the ship's component.
        for link in self.links.values_mut() {
            link.latest_intent.toggle_assist = false;
            if let Some(mut component) = self.world.get_mut::<ShipIntent>(link.ship) {
                component.toggle_assist = false;
            }
        }
    }

    /// Build and send a **delta-coded** snapshot (unreliable) to every client
    /// (T063/T064/T065/T066).
    ///
    /// The authoritative full state is built once from the `sim` world (T065:
    /// server transforms/velocities only — no client-asserted data is on this
    /// path). Each client then gets a delta against ITS OWN last-acked baseline:
    /// only changed entities in `entities`, disappeared ids in `removed`, tagged
    /// with that client's `baseline_id`. A client whose acked baseline is unknown
    /// (lost ack, or never acked) gets a full keyframe (delta-from-nothing) so it
    /// re-baselines gracefully (T064). Every snapshot is MTU-bounded inside
    /// [`snapshot::encode_snapshot`]. Each send credits its encoded payload bytes
    /// to that connection's [`snapshot::BandwidthMeter`] (T066) AND to the
    /// transport's `NetStats` (via the transport's own send accounting).
    fn broadcast_snapshots(&mut self) {
        // The snapshot's identity is its server tick, mapped to the u16 wire id
        // the ack/baseline fields carry (avoiding the `0` = "nothing acked" and
        // `KEYFRAME_BASELINE` sentinels). The tick is already on the wire
        // (`Snapshot::server_tick`), so the client can ack THIS snapshot by id
        // without a separate wire field. The rolling counter is advanced too for
        // back-compatible inspection, but identity is the tick.
        self.snapshot_id = self.snapshot_id.wrapping_add(1);
        let snapshot_id = protocol::snapshot_wire_id(self.server_tick);

        // T065: build the authoritative full state ONCE from the sim world.
        let current = self.build_full_state();
        let now_secs = self.server_tick as f32 * self.rates.fixed_dt();

        let conns: Vec<ConnectionId> = self.sorted_conns();
        for conn in conns {
            // The client's own ship id + position drive the priority origin and the
            // never-drop guard (T064).
            let recipient_id = self.client_ship_id(conn);
            let recipient_pos = recipient_id
                .and_then(|id| current.get(id))
                .map(|r| r.pos.dequantize_pos())
                .unwrap_or(Vec2::ZERO);

            // The baseline this client last acked (None ⇒ unknown ⇒ keyframe, T064).
            let cache = self.baselines.entry(conn).or_default();
            let (baseline_id, keyframe) = match cache.acked_baseline() {
                Some(id) => (id, false),
                None => (Snapshot::KEYFRAME_BASELINE, true),
            };
            let baseline = cache.acked_state().clone();

            let params = snapshot::EncodeParams {
                server_tick: self.server_tick,
                acked_input_seq: self.session.acked_input_seq(conn),
                baseline_id,
                keyframe,
                recipient_id,
                recipient_pos,
            };
            let snap = snapshot::encode_snapshot(&current, &baseline, params);

            // Record the full state THIS snapshot represents under its id, so when
            // the client later acks it the baseline can be promoted to it (T063).
            // The MTU guard may have shed entities; the client reconstructs only
            // what it actually received, so the cached baseline-for-this-id is the
            // baseline-plus-applied-delta the CLIENT will hold, not the full world.
            let sent_state = protocol::apply_delta(&baseline, &snap);
            cache.record_sent(snapshot_id, sent_state);

            // T066: meter the encoded payload bytes for this connection.
            let bytes = snapshot::encoded_len(&snap) as u64;
            self.meter.record_send(conn, now_secs, bytes);

            self.transport
                .send_unreliable(conn, &Message::Snapshot(snap));
        }
    }

    /// Read-only access to the per-client bandwidth meter (T066, TR-014). The 8b
    /// bandwidth scenario reads mean/peak bytes/client/sec off this.
    pub fn bandwidth_meter(&self) -> &snapshot::BandwidthMeter {
        &self.meter
    }

    /// Drive the underlying transport's pump one step (a no-op for loopback, a
    /// netcode update + socket flush for the renet UDP adapter). [`ServerApp::tick`]
    /// already pumps once at its top so inbound packets are applied before the
    /// drain; this accessor lets the 8b renet harness flush OUTBOUND snapshots
    /// after `tick` queued them (renet only flushes queued messages inside its own
    /// update/send), so the bot transports can then receive them within the same
    /// harness step. A no-op for loopback, so callers run unchanged over either
    /// transport (SC-006/SC-008).
    pub fn pump_transport(&mut self, dt: std::time::Duration) {
        self.transport.pump(dt);
    }

    /// Build the authoritative full entity set for a snapshot from the `sim` world
    /// (T065). Reads ONLY server-authoritative transforms/velocities — no
    /// client-asserted data is on this path (the server reads only its own `sim`
    /// world; there is no client input argument here). Returns a
    /// [`protocol::FullState`] the delta encoder deltas against each client's
    /// baseline.
    fn build_full_state(&mut self) -> FullState {
        FullState::from_records(self.full_records())
    }

    /// The current authoritative full entity set (server-`sim` only, T065) — the
    /// public read the 8b bandwidth baseline uses to time per-client snapshot
    /// **encode cost** (TR-047) over the real baseline world via the free
    /// [`snapshot::encode_snapshot`] function. Same record set
    /// [`ServerApp::broadcast_snapshots`] deltas against each client's baseline;
    /// exposed read-only (it mints wire ids for any unbound entities, mirroring a
    /// broadcast, but mutates no world state).
    pub fn current_full_state(&mut self) -> FullState {
        self.build_full_state()
    }

    /// The current authoritative per-entity render state, **unquantized** (full
    /// `f32` precision, no wire round-trip) — the in-process render read the
    /// windowed solo client draws from directly (no predict/interpolate netcode on
    /// that path; the embedded server IS the authoritative sim, so its world is the
    /// crisp, in-sync source of truth for collision and hits).
    ///
    /// One [`RenderEntity`] per replicated Ship / Projectile / Target, keyed by the
    /// SAME wire [`EntityId`] mapping ([`EntityIdAllocator::id_for`]) the snapshots
    /// use, so the client can reconcile its rendered entities by stable id. Mirrors
    /// [`ServerApp::full_records`] but skips quantization: `flags` carries the
    /// target sub-kind ([`TargetKind::as_u8`]; `0` for ship/projectile) and
    /// `heading` is the ship `Heading` (projectile heading is derived from velocity
    /// direction, target heading is `0.0`).
    ///
    /// Additive and client-only: no test depends on it, and it mutates no world
    /// state (it only mints wire ids for any not-yet-seen entity, exactly as a
    /// snapshot build would).
    pub fn render_state(&mut self) -> Vec<RenderEntity> {
        let mut out = Vec::new();

        // Ships (carry a `Heading`).
        let mut ships = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity, &Heading), With<Ship>>();
        let ship_rows: Vec<(Entity, Vec2, Vec2, f32)> = ships
            .iter(&self.world)
            .map(|(e, p, v, h)| (e, p.0, v.0, h.0))
            .collect();
        for (entity, pos, vel, heading) in ship_rows {
            out.push(RenderEntity {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Ship,
                flags: 0,
                pos,
                heading,
                vel,
            });
        }

        // Projectiles (heading derived from velocity direction).
        let mut projectiles = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity), With<Projectile>>();
        let proj_rows: Vec<(Entity, Vec2, Vec2)> = projectiles
            .iter(&self.world)
            .map(|(e, p, v)| (e, p.0, v.0))
            .collect();
        for (entity, pos, vel) in proj_rows {
            out.push(RenderEntity {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Projectile,
                flags: 0,
                pos,
                heading: vel.to_angle(),
                vel,
            });
        }

        // Targets (the sub-kind rides in `flags` via `TargetKind::as_u8`).
        let mut targets = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity, &TargetKind), With<Target>>();
        let target_rows: Vec<(Entity, Vec2, Vec2, u8)> = targets
            .iter(&self.world)
            .map(|(e, p, v, k)| (e, p.0, v.0, k.as_u8()))
            .collect();
        for (entity, pos, vel, kind_flag) in target_rows {
            out.push(RenderEntity {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Target,
                flags: kind_flag,
                pos,
                heading: 0.0,
                vel,
            });
        }

        out
    }

    /// The flat authoritative record list (ships, projectiles, targets), quantized
    /// for the wire (TR-013). Pulled out of the old full-state path; the delta
    /// encoder now consumes this via [`ServerApp::build_full_state`].
    fn full_records(&mut self) -> Vec<EntityRecord> {
        let mut records = Vec::new();

        // Ships.
        let mut ships = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity, &Heading), With<Ship>>();
        let ship_rows: Vec<(Entity, Vec2, Vec2, f32)> = ships
            .iter(&self.world)
            .map(|(e, p, v, h)| (e, p.0, v.0, h.0))
            .collect();
        for (entity, pos, vel, heading) in ship_rows {
            records.push(EntityRecord {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Ship,
                pos: QVec2::quantize_pos(pos),
                vel: QVec2::quantize_vel(vel),
                heading: QAngle::quantize(heading),
                flags: 0,
            });
        }

        // Projectiles (no heading component — derived from velocity direction).
        let mut projectiles = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity), With<Projectile>>();
        let proj_rows: Vec<(Entity, Vec2, Vec2)> = projectiles
            .iter(&self.world)
            .map(|(e, p, v)| (e, p.0, v.0))
            .collect();
        for (entity, pos, vel) in proj_rows {
            records.push(EntityRecord {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Projectile,
                pos: QVec2::quantize_pos(pos),
                vel: QVec2::quantize_vel(vel),
                heading: QAngle::quantize(vel.to_angle()),
                flags: 0,
            });
        }

        // Targets (dummies, asteroids, seekers). The target sub-kind rides in
        // `flags` so the client can pick the right mesh (the wire `EntityKind`
        // only says "Target"); see `TargetKind::as_u8`.
        let mut targets = self
            .world
            .query_filtered::<(Entity, &Position, &Velocity, &TargetKind), With<Target>>();
        let target_rows: Vec<(Entity, Vec2, Vec2, u8)> = targets
            .iter(&self.world)
            .map(|(e, p, v, k)| (e, p.0, v.0, k.as_u8()))
            .collect();
        for (entity, pos, vel, kind_flag) in target_rows {
            records.push(EntityRecord {
                id: self.entity_ids.id_for(entity),
                kind: EntityKind::Target,
                pos: QVec2::quantize_pos(pos),
                vel: QVec2::quantize_vel(vel),
                heading: QAngle::quantize(0.0),
                flags: kind_flag,
            });
        }

        records
    }

    /// Spawn a **server-controlled bot ship** at `pos` driven by a fixed
    /// `intent` every tick — the AI ships the bandwidth baseline scenario adds
    /// alongside the connected bot clients (TR-042: 2 networked bots + 4
    /// server-controlled bot ships ≈ 6 ships + projectiles). The ship carries the
    /// same gameplay bundle as a client ship (so the shared sim drives it
    /// identically) plus a fixed [`ShipIntent`]; because the per-tick step only
    /// re-stages intents for *connected-client* ships, this bot ship keeps its
    /// fixed intent across ticks (it thrusts/fires on its own). Returns the spawned
    /// entity so a test can address it; it appears in snapshots under an
    /// auto-minted wire id like any other replicated entity.
    pub fn spawn_bot_ship(&mut self, pos: Vec2, intent: ShipIntent) -> Entity {
        let tuning = *self.world.resource::<Tuning>();
        self.world
            .spawn((
                Ship,
                intent,
                Position(pos),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                Health(100.0),
                FlightAssist::On,
                CollisionRadius(0.8),
                Weapon {
                    cooldown: 0.0,
                    fire_rate: tuning.fire_rate,
                    muzzle_speed: tuning.muzzle_speed,
                },
            ))
            .id()
    }

    /// Populate the authoritative world with the E002 starter targets — two
    /// static dummies, two drifting asteroids, and one player-seeking AI — so the
    /// embedded loopback server has something to fight over (Principle I/VII). The
    /// shared fixed-step systems (`ai::seek_system`, weapon/collision/combat,
    /// already registered via [`sim::add_fixed_step_systems`]) move, damage, and
    /// despawn these authoritatively; they replicate to the client as
    /// [`EntityKind::Target`] records (see [`ServerApp::full_records`]) and are
    /// rendered there as interpolated remotes.
    ///
    /// Values mirror the E002 client scene exactly so the look/feel is unchanged.
    /// **Client-only**: no test calls this, so the existing entity set the session
    /// tests depend on is unaffected (additive). Call it once, before the client
    /// connects, so the first snapshot already carries the targets.
    pub fn spawn_demo_world(&mut self) {
        // Two static practice dummies.
        self.spawn_target(
            TargetKind::Dummy,
            Vec2::new(11.0, 4.0),
            Vec2::ZERO,
            0.9,
            20.0,
        );
        self.spawn_target(
            TargetKind::Dummy,
            Vec2::new(15.0, -5.0),
            Vec2::ZERO,
            0.9,
            20.0,
        );
        // Two drifting asteroids (constant velocity; the sim integrates the drift).
        self.spawn_target(
            TargetKind::Asteroid,
            Vec2::new(-13.0, 7.0),
            Vec2::new(2.5, -1.2),
            0.9,
            40.0,
        );
        self.spawn_target(
            TargetKind::Asteroid,
            Vec2::new(-7.0, -11.0),
            Vec2::new(1.0, 2.0),
            0.9,
            40.0,
        );
        // One seeker — `ai::seek_system` thrusts it toward the player ship each
        // tick (it queries `With<Ship>`, satisfied by the connected client's ship).
        self.spawn_target(
            TargetKind::Seeker,
            Vec2::new(22.0, 16.0),
            Vec2::ZERO,
            0.7,
            30.0,
        );
    }

    /// Spawn one authoritative target with the `sim` gameplay components the shared
    /// fixed-step systems read: `ai::seek_system` reads `TargetKind`/`Position`/
    /// `Velocity`, collision/combat read `CollisionRadius`/`Health`. Matches the
    /// E002 client scene component set (minus rendering). Helper for
    /// [`ServerApp::spawn_demo_world`].
    fn spawn_target(
        &mut self,
        kind: TargetKind,
        pos: Vec2,
        vel: Vec2,
        radius: f32,
        health: f32,
    ) -> Entity {
        self.world
            .spawn((
                Target,
                kind,
                Position(pos),
                Velocity(vel),
                CollisionRadius(radius),
                Health(health),
            ))
            .id()
    }

    /// Spawn a fresh authoritative ship for a newly connected client, reusing the
    /// `sim` gameplay components and [`Tuning`] (mirrors the client scene minus
    /// rendering). The flight-model (`FlightAssist::On`) is the default feel.
    fn spawn_client_ship(&mut self) -> Entity {
        let tuning = *self.world.resource::<Tuning>();
        self.world
            .spawn((
                Ship,
                // Per-entity intent: this client's input is written onto its own
                // ship's component each step, so N ships are piloted independently.
                ShipIntent::default(),
                Position(Vec2::ZERO),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                AngularVelocity(0.0),
                Health(100.0),
                FlightAssist::On,
                CollisionRadius(0.8),
                Weapon {
                    cooldown: 0.0,
                    fire_rate: tuning.fire_rate,
                    muzzle_speed: tuning.muzzle_speed,
                },
            ))
            .id()
    }

    /// Tear a client down: despawn its ship, drop its link + RTT estimate, and
    /// free its session slot (so the capacity ceiling reflects only live clients,
    /// T021/T057). The despawned ship's history ring is pruned on the next
    /// [`ServerApp::capture_transform_history`] (the entity is gone), so no
    /// per-client bookkeeping leaks (TR-031: no slot leak).
    fn disconnect_client(&mut self, conn: ConnectionId) {
        if let Some(link) = self.links.remove(&conn) {
            if let Ok(entity) = self.world.get_entity_mut(link.ship) {
                entity.despawn();
            }
        }
        self.rtt.remove(&conn);
        // T063: drop the per-client delta baseline cache so it cannot leak across
        // a disconnect (a reused connection id re-baselines via a keyframe).
        self.baselines.remove(&conn);
        self.session.remove(conn);
        self.transport
            .disconnect(conn, DisconnectReason::ClientClosed);
    }

    /// Live connection ids in a stable (sorted) order, so per-tick iteration over
    /// connections is deterministic regardless of `HashMap` order (HINT-003).
    fn sorted_conns(&self) -> Vec<ConnectionId> {
        let mut conns: Vec<ConnectionId> = self.links.keys().copied().collect();
        conns.sort_by_key(|c| c.0);
        conns
    }
}

/// Per-connection delta baseline cache (T063/T064).
///
/// The server delta-codes each client's snapshot against the snapshot that client
/// **last acked** — never an unacked one — so a lost ack re-baselines gracefully
/// (T064). To do that it must remember, per client, the full reconstructed state
/// of each snapshot it sent until one is acked:
///
/// - [`BaselineCache::record_sent`] stores the full state a sent snapshot id
///   represents (bounded ring — only the most recent sends are kept);
/// - [`BaselineCache::promote`] advances the acked baseline to a sent snapshot's
///   stored full state when the client acks that id;
/// - [`BaselineCache::acked_baseline`] / [`BaselineCache::acked_state`] are the id
///   plus full state the next delta is computed against (`None` id ⇒ the client
///   has acked nothing yet ⇒ the encoder emits a keyframe).
#[derive(Default)]
struct BaselineCache {
    /// The snapshot id of the currently-acked baseline (`None` until the client
    /// acks its first snapshot — that case forces a keyframe).
    acked_id: Option<u16>,
    /// The full reconstructed state of the acked baseline — the state the client
    /// currently holds, which the next delta is computed against. Empty until the
    /// first ack.
    acked: FullState,
    /// Recent sent snapshots (id → the full state that send represents), oldest
    /// first. Bounded by [`BaselineCache::SENT_RING`] so it cannot leak.
    sent: Vec<(u16, FullState)>,
}

impl BaselineCache {
    /// How many recently-sent snapshots to retain per client awaiting an ack.
    /// Comfortably covers the ack RTT at the 20 Hz snapshot rate (well over a
    /// second of unacked snapshots) while staying bounded (no leak).
    const SENT_RING: usize = 64;

    /// Record that snapshot `id` (representing `state`, the full state the client
    /// reconstructs from baseline+delta) was sent. Bounded: the oldest is dropped
    /// past [`BaselineCache::SENT_RING`].
    fn record_sent(&mut self, id: u16, state: FullState) {
        self.sent.push((id, state));
        if self.sent.len() > Self::SENT_RING {
            let overflow = self.sent.len() - Self::SENT_RING;
            self.sent.drain(0..overflow);
        }
    }

    /// Promote the acked baseline to the stored full state of snapshot `id` (the
    /// client just acked it). A no-op if we no longer hold that id (the ack is for
    /// a snapshot aged out of the ring) or if it is not newer than the current
    /// baseline (a stale/duplicate ack never moves the baseline backward).
    fn promote(&mut self, id: u16) {
        // Ignore a stale/duplicate ack: never move the baseline backward.
        if let Some(current) = self.acked_id {
            if id == current {
                return;
            }
        }
        if let Some((_, state)) = self.sent.iter().find(|(sent_id, _)| *sent_id == id) {
            self.acked_id = Some(id);
            self.acked = state.clone();
        }
    }

    /// The id of the currently-acked baseline (`None` ⇒ nothing acked ⇒ keyframe).
    fn acked_baseline(&self) -> Option<u16> {
        self.acked_id
    }

    /// The full state of the currently-acked baseline (empty until the first ack).
    fn acked_state(&self) -> &FullState {
        &self.acked
    }
}

/// Mints a stable [`EntityId`] for each ECS [`Entity`] the snapshot replicates.
///
/// `bevy_ecs::Entity` generational ids are runtime-local and must not cross the
/// wire; this maps each one to a network id (TR-013). A client's ship is bound to
/// the id it was handed in [`protocol::ConnectAccepted`] (so the client can find
/// itself); other entities get monotonic ids on first sight.
struct EntityIdAllocator {
    map: HashMap<Entity, EntityId>,
    next: u32,
}

impl EntityIdAllocator {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            next: 0,
        }
    }

    /// Reserve `id` for `entity` (the ship id handed to its owning client). Keeps
    /// the monotonic counter ahead of any bound id so a later auto-minted id can
    /// never collide with it.
    fn bind(&mut self, entity: Entity, id: EntityId) {
        self.map.insert(entity, id);
        self.next = self.next.max(id.0 + 1);
    }

    /// The network id for `entity`, minting a new one on first sight.
    fn id_for(&mut self, entity: Entity) -> EntityId {
        if let Some(id) = self.map.get(&entity) {
            return *id;
        }
        let id = EntityId(self.next);
        self.next += 1;
        self.map.insert(entity, id);
        id
    }
}
