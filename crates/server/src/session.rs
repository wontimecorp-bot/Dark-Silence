//! Per-connection session state and the connection handshake (TR-002, TR-008,
//! TR-024/025/026).
//!
//! The [`Session`] owns the authoritative connection table: it maps each live
//! [`ConnectionId`] to its per-client bookkeeping ([`ClientState`]) — the
//! last-processed input sequence and last-acked snapshot id — and mints the
//! per-client network entity id. Every connection, loopback or networked, passes
//! through [`Session::handshake`] (T021), so loopback is *not* an authority or
//! validation bypass: only the transport differs (T022).
//!
//! Snapshot delta-coding and the full ack/reconciliation loop are fleshed out in
//! later phases; here the session records the ack anchor each client's snapshot
//! must carry (`acked_input_seq` = that client's last-processed seq, TR-008).

use std::collections::HashMap;

use protocol::{
    ClientInput, ConnectAccepted, ConnectRejected, ConnectionId, EntityId, Message, RejectReason,
    CLIENT_TOKEN_BYTES,
};

use crate::RateConfig;

/// Hard ceiling on concurrent sessions (TR-025). A `Connect` received while the
/// table is full is rejected with [`RejectReason::Full`] and **no slot is
/// allocated** — capacity is reserved only for accepted clients.
pub const MAX_CLIENTS: usize = 8;

/// T052 (TR-022/023): bounded acceptance window for an input's `tick`. An input
/// whose `tick` is older than `server_tick − UNACKED_BUFFER_BOUND` is **stale**.
/// This is the per-client unacknowledged-input buffer bound (TR-027 baseline 64
/// inputs, ~2 s at 30 Hz); an input older than this window is past the point the
/// client could still be replaying, so it is discarded.
pub const UNACKED_BUFFER_BOUND: u32 = 64;

/// T055 (TR-028): per-client inbound message rate ceiling over a one-second
/// window — baseline 4× the 30 Hz send rate. Messages beyond this within the
/// window are dropped and the offender is flagged (TR-031 logging).
pub const INBOUND_RATE_LIMIT_PER_SEC: u32 = 120;

/// T056 (TR-029/030): maximum decodable inbound payload size — the path-MTU
/// baseline. A payload longer than this is treated as malformed (oversize) and
/// dropped without decoding.
pub const MAX_PAYLOAD_BYTES: usize = 1200;

/// T057 (TR-031): idle timeout (seconds) — a session with no received packet for
/// this long is cleanly dropped, freeing ONLY its slot (no slot leak).
pub const IDLE_TIMEOUT_SECS: f32 = 10.0;

/// Capacity of the inspectable [`RejectionLog`] ring (T057). Bounded so the log
/// itself can never grow unboundedly under a flood; the per-category counters are
/// unbounded counts (cheap) but the recent-event detail is capped.
pub const REJECTION_LOG_CAPACITY: usize = 256;

/// Per-client authoritative bookkeeping held in the [`Session`] connection table.
///
/// Tracks the reconciliation anchors the server needs per connection: the
/// highest input sequence it has processed for this client (the value echoed
/// back as `Snapshot::acked_input_seq`, TR-008) and the last snapshot id the
/// client acknowledged (the delta baseline, expanded in Phase 8).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClientState {
    /// The network entity id assigned to this client's owned ship.
    pub entity_id: EntityId,
    /// Highest [`protocol::ClientInput::seq`] the server has processed for this
    /// client. Echoed in every snapshot so the client knows what to replay.
    pub last_processed_input_seq: u32,
    /// Last snapshot id this client acknowledged (delta baseline, TR-013).
    pub last_acked_snapshot_id: u16,
    /// T055 (TR-028): start time (seconds) of the current 1 s rate-limit window.
    rate_window_start: f32,
    /// T055 (TR-028): inbound messages counted in the current window.
    rate_window_count: u32,
    /// T057 (TR-031): server time (seconds) the last packet was received from
    /// this client. Drives the idle timeout.
    last_recv_time: f32,
}

impl ClientState {
    /// A fresh client: no inputs processed, no snapshot acked yet. `now` seeds
    /// the rate-limit window and last-received clock so the idle timeout and rate
    /// limit are anchored from admission.
    fn new(entity_id: EntityId, now: f32) -> Self {
        Self {
            entity_id,
            last_processed_input_seq: 0,
            last_acked_snapshot_id: 0,
            rate_window_start: now,
            rate_window_count: 0,
            last_recv_time: now,
        }
    }
}

/// The authoritative session: the live connection table plus the handshake
/// policy (version check, capacity ceiling, ban path).
///
/// One `Session` is owned by the server app and shared across every transport;
/// it is transport-agnostic (it never names a transport type), so the embedded
/// loopback server and a future networked server run the *identical* session and
/// validation path (TR-003, T022).
pub struct Session {
    /// Live connections keyed by [`ConnectionId`]. Capacity is bounded by
    /// [`MAX_CLIENTS`]; a slot exists only for an accepted client.
    clients: HashMap<ConnectionId, ClientState>,
    /// Wire protocol version the server speaks. A [`protocol::Connect`] must
    /// match this **exactly** (TR-024) or it is rejected with
    /// [`RejectReason::Version`].
    protocol_version: u16,
    /// Server-announced session rates emitted in [`ConnectAccepted`] (TR-044).
    rates: RateConfig,
    /// Next network entity id to mint for an accepted client. Monotonic so ids
    /// are stable and deterministic.
    next_entity_id: u32,
    /// Tokens that are pre-banned: a [`protocol::Connect`] carrying one is
    /// rejected with [`RejectReason::Banned`] and closed (T021). The ban
    /// *source*/lifecycle (account service, moderation) is deferred; this is the
    /// reject-and-close behavior, configurable for tests via [`Session::ban_token`].
    banned_tokens: Vec<[u8; CLIENT_TOKEN_BYTES]>,
    /// T057 (TR-031): the inspectable anti-cheat rejection log. Records the
    /// offending id, reason category, and server tick of every rejected /
    /// invalid / malformed event — never raw payloads or thresholds.
    rejections: RejectionLog,
}

impl Session {
    /// Construct an empty session that speaks `protocol_version` and announces
    /// `rates` in every [`ConnectAccepted`].
    pub fn new(protocol_version: u16, rates: RateConfig) -> Self {
        Self {
            clients: HashMap::new(),
            protocol_version,
            rates,
            next_entity_id: 0,
            banned_tokens: Vec::new(),
            rejections: RejectionLog::new(),
        }
    }

    /// Convert a server tick to seconds via the announced tick rate. Used to seed
    /// and advance the per-client rate-limit window and idle clock from the
    /// monotonic server tick (deterministic; no wall-clock dependency in tests).
    fn tick_to_seconds(&self, tick: u32) -> f32 {
        tick as f32 / self.rates.tick_rate_hz.max(1) as f32
    }

    /// Mark a connect token as banned so any [`Session::handshake`] carrying it
    /// is rejected with [`RejectReason::Banned`]. The ban source/lifecycle is
    /// deferred (E004); this exists so the reject-and-close path is testable now.
    pub fn ban_token(&mut self, token: [u8; CLIENT_TOKEN_BYTES]) {
        if !self.banned_tokens.contains(&token) {
            self.banned_tokens.push(token);
        }
    }

    /// Number of live (accepted) connections.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// `true` when the connection table is at the [`MAX_CLIENTS`] ceiling.
    pub fn is_full(&self) -> bool {
        self.clients.len() >= MAX_CLIENTS
    }

    /// Iterate the live connections and their state.
    pub fn iter(&self) -> impl Iterator<Item = (ConnectionId, ClientState)> + '_ {
        self.clients.iter().map(|(&id, &state)| (id, state))
    }

    /// Look up a connection's per-client state.
    pub fn client(&self, conn: ConnectionId) -> Option<ClientState> {
        self.clients.get(&conn).copied()
    }

    /// Decide whether to accept the `connect` handshake (TR-024/025/026). Pure
    /// policy with no side effects, so [`Session::handshake`] can run the check
    /// before allocating a slot (a `Full`/`Version`/`Banned` connect must NOT
    /// consume capacity).
    fn evaluate(&self, connect: &protocol::Connect) -> Result<(), ConnectRejected> {
        // Exact-match version (TR-024). No negotiation: a mismatch is rejected.
        if connect.protocol_version != self.protocol_version {
            return Err(ConnectRejected {
                reason: RejectReason::Version,
            });
        }
        // Reserved ban path (TR-026): reject-and-close on a banned token.
        if self.banned_tokens.contains(&connect.client_token) {
            return Err(ConnectRejected {
                reason: RejectReason::Banned,
            });
        }
        // Capacity ceiling (TR-025): reject at capacity, allocating no slot.
        if self.is_full() {
            return Err(ConnectRejected {
                reason: RejectReason::Full,
            });
        }
        Ok(())
    }

    /// Run the connection handshake for `conn` (TR-024/025/026).
    ///
    /// On success the connection is admitted: a network entity id is minted, a
    /// [`ClientState`] slot is allocated in the table, and a [`ConnectAccepted`]
    /// carrying that id, the server-announced rates, and `server_tick` is
    /// returned. On rejection **no slot is allocated** (capacity is reserved for
    /// accepted clients only) and the caller must send the [`ConnectRejected`]
    /// and close the connection (the `Banned` reject-and-close path, T021).
    ///
    /// Rejection precedence: version mismatch → ban → capacity.
    pub fn handshake(
        &mut self,
        conn: ConnectionId,
        connect: &protocol::Connect,
        server_tick: u32,
    ) -> Result<ConnectAccepted, ConnectRejected> {
        // A duplicate handshake on an already-admitted connection returns its
        // existing acceptance rather than minting a second slot.
        if let Some(state) = self.clients.get(&conn) {
            return Ok(self.accepted_for(state.entity_id, server_tick));
        }

        self.evaluate(connect)?;

        let entity_id = EntityId(self.next_entity_id);
        self.next_entity_id += 1;
        self.clients.insert(
            conn,
            ClientState::new(entity_id, self.tick_to_seconds(server_tick)),
        );
        Ok(self.accepted_for(entity_id, server_tick))
    }

    /// Build the [`ConnectAccepted`] for an admitted client from the
    /// server-announced rates (TR-044 — no negotiation; the client adopts these).
    fn accepted_for(&self, client_id: EntityId, server_tick: u32) -> ConnectAccepted {
        ConnectAccepted {
            client_id,
            tick_rate_hz: self.rates.tick_rate_hz,
            snapshot_rate_hz: self.rates.snapshot_rate_hz,
            interp_delay_ms: self.rates.interp_delay_ms,
            server_tick,
        }
    }

    /// Remove a connection from the table (clean disconnect or reject-close).
    /// Returns the removed state, if the connection was live.
    pub fn remove(&mut self, conn: ConnectionId) -> Option<ClientState> {
        self.clients.remove(&conn)
    }

    /// Record that the server processed input `seq` for `conn`, advancing the
    /// per-client ack anchor. Monotonic: a stale/duplicate seq is ignored so the
    /// echoed `acked_input_seq` never moves backward (TR-008).
    pub fn record_processed_input(&mut self, conn: ConnectionId, seq: u32) {
        if let Some(state) = self.clients.get_mut(&conn) {
            if seq > state.last_processed_input_seq {
                state.last_processed_input_seq = seq;
            }
        }
    }

    /// Record the latest snapshot id `conn` acknowledged (the delta baseline).
    /// Monotonic over the `u16` id space (wrap handling lands with delta coding
    /// in Phase 8).
    pub fn record_snapshot_ack(&mut self, conn: ConnectionId, snapshot_id: u16) {
        if let Some(state) = self.clients.get_mut(&conn) {
            state.last_acked_snapshot_id = snapshot_id;
        }
    }

    /// The ack anchor a snapshot to `conn` must carry: the highest input seq the
    /// server has processed for that client (TR-008). `0` for an unknown
    /// connection (it will receive no snapshot).
    pub fn acked_input_seq(&self, conn: ConnectionId) -> u32 {
        self.clients
            .get(&conn)
            .map(|s| s.last_processed_input_seq)
            .unwrap_or(0)
    }

    // --- T052: seq/tick intake classification (TR-022/023) --------------------

    /// T052 (TR-022/023): classify an inbound [`ClientInput`] at intake, BEFORE
    /// any state mutation, into [`InputDisposition`].
    ///
    /// Pure decision (takes the client's current [`ClientState`], not `&self`), so
    /// it is unit-testable in isolation:
    /// - `seq <= last_processed_input_seq` → [`InputDisposition::Replay`]: a
    ///   replay/duplicate (the redundant tail means already-seen seqs recur, so an
    ///   out-of-order input whose `seq` was already processed is also a replay,
    ///   TR-023) — discard, never partial-apply.
    /// - `tick` older than the acceptance window
    ///   (`server_tick − UNACKED_BUFFER_BOUND`) → [`InputDisposition::Stale`] —
    ///   discard.
    /// - otherwise → [`InputDisposition::Apply`].
    ///
    /// Replay is checked before staleness so a duplicate of a recent input is
    /// reported as a replay, not stale. Each `seq` is therefore processed at most
    /// once (advancing `last_processed_input_seq` on apply is the caller's job via
    /// [`Session::record_processed_input`]).
    pub fn classify_input(
        state: &ClientState,
        input: &ClientInput,
        server_tick: u32,
    ) -> InputDisposition {
        // Replay / duplicate / already-superseded out-of-order seq (TR-022/023).
        if input.seq <= state.last_processed_input_seq {
            return InputDisposition::Replay;
        }
        // Stale: older than the bounded acceptance window (TR-022/027). Saturating
        // so an early-game `server_tick < bound` never underflows (window is then
        // the whole history → nothing is stale yet).
        let oldest_acceptable = server_tick.saturating_sub(UNACKED_BUFFER_BOUND);
        if input.tick < oldest_acceptable {
            return InputDisposition::Stale;
        }
        InputDisposition::Apply
    }

    // --- T055: per-client inbound message rate limit (TR-027/028) -------------

    /// T055 (TR-028): record one inbound message from `conn` at server tick
    /// `server_tick` and decide whether it is within the per-client rate budget.
    ///
    /// Counts messages in a sliding one-second window (reset when the window
    /// elapses). The first [`INBOUND_RATE_LIMIT_PER_SEC`] messages in a window are
    /// [`RateDecision::Allow`]ed; the rest are [`RateDecision::Throttle`]d (dropped
    /// by the caller this window) and the offender is flagged in the
    /// [`RejectionLog`]. This also refreshes the idle clock (a received message,
    /// even a throttled one, proves the connection is alive — TR-031). Independent
    /// of the fire-rate gate (TR-021).
    ///
    /// Returns [`RateDecision::Allow`] for an unknown connection's safety (it has
    /// no slot to flood); callers route only live-connection traffic here.
    pub fn note_inbound(&mut self, conn: ConnectionId, server_tick: u32) -> RateDecision {
        let now = self.tick_to_seconds(server_tick);
        let entity_id = self.clients.get(&conn).map(|s| s.entity_id);
        let decision = {
            let Some(state) = self.clients.get_mut(&conn) else {
                return RateDecision::Allow;
            };
            // The connection is alive — refresh the idle clock (TR-031).
            state.last_recv_time = now;
            // Roll the window if a second has elapsed since it started.
            if now - state.rate_window_start >= 1.0 {
                state.rate_window_start = now;
                state.rate_window_count = 0;
            }
            state.rate_window_count += 1;
            if state.rate_window_count > INBOUND_RATE_LIMIT_PER_SEC {
                RateDecision::Throttle
            } else {
                RateDecision::Allow
            }
        };
        if decision == RateDecision::Throttle {
            self.rejections
                .record(entity_id, RejectionCategory::RateLimited, server_tick);
        }
        decision
    }

    // --- T057: idle timeout + rejection log (TR-031) --------------------------

    /// T057 (TR-031): the connections whose idle timeout has elapsed at
    /// `server_tick` — no received packet for [`IDLE_TIMEOUT_SECS`]. Pure query
    /// (does not remove them); the caller drops ONLY these sessions (their slots),
    /// leaving every other client and the authoritative state untouched (no slot
    /// leak). Returns a stable, sorted list so teardown is deterministic.
    pub fn timed_out(&self, server_tick: u32) -> Vec<ConnectionId> {
        let now = self.tick_to_seconds(server_tick);
        let mut out: Vec<ConnectionId> = self
            .clients
            .iter()
            .filter(|(_, s)| now - s.last_recv_time >= IDLE_TIMEOUT_SECS)
            .map(|(&conn, _)| conn)
            .collect();
        out.sort_by_key(|c| c.0);
        out
    }

    /// T057 (TR-031): seconds since the last received packet from `conn` at
    /// `server_tick` (the idle age). `None` for an unknown connection.
    pub fn idle_seconds(&self, conn: ConnectionId, server_tick: u32) -> Option<f32> {
        self.clients
            .get(&conn)
            .map(|s| self.tick_to_seconds(server_tick) - s.last_recv_time)
    }

    /// T057 (TR-031): log a rejected / invalid / malformed event for `conn` under
    /// `category` at `server_tick`. Records only the offending entity id, the
    /// category, and the tick — never the raw payload or any validation threshold
    /// (no exploitable leak). For an unknown connection the entity id is `None`.
    pub fn log_rejection(
        &mut self,
        conn: ConnectionId,
        category: RejectionCategory,
        server_tick: u32,
    ) {
        let entity_id = self.clients.get(&conn).map(|s| s.entity_id);
        self.rejections.record(entity_id, category, server_tick);
    }

    /// Read-only access to the inspectable [`RejectionLog`] (T057) — for tests and
    /// anti-cheat tooling to assert what was rejected and that no payloads leaked.
    pub fn rejections(&self) -> &RejectionLog {
        &self.rejections
    }
}

/// T052 (TR-022/023): the intake decision for one [`ClientInput`]. Every variant
/// other than [`InputDisposition::Apply`] is **discard-only** — the input mutates
/// no authoritative state (its `seq` may still be recorded as seen so a later
/// duplicate is caught, per TR-039).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDisposition {
    /// In-window, not yet processed — apply it.
    Apply,
    /// `seq <= last_processed` (duplicate / already-superseded out-of-order) —
    /// discard (TR-022/023).
    Replay,
    /// `tick` older than the acceptance window — discard (TR-022).
    Stale,
}

/// T055 (TR-028): the per-message rate-limit decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateDecision {
    /// Within the per-client budget this window — process the message.
    Allow,
    /// Over the budget — drop this message and flag the offender (TR-031).
    Throttle,
}

/// T056 (TR-029/030): why an inbound byte payload was dropped at the intake
/// boundary. Returned by [`decode_inbound`] so the caller can log it (T057). No
/// variant carries the raw payload — only the structural reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Payload length exceeded [`MAX_PAYLOAD_BYTES`] (oversize-vs-MTU, TR-029).
    Oversize,
    /// `Message::decode` failed — malformed / truncated / undecodable (TR-030).
    Malformed,
}

/// T056 (TR-029/030): the byte-intake guard.
///
/// Decodes `bytes` into a [`Message`], or returns a [`DropReason`] to drop it
/// without ever mutating authoritative state. It drops when the payload exceeds
/// the MTU bound ([`MAX_PAYLOAD_BYTES`], TR-029) or when [`Message::decode`] fails
/// (malformed / truncated / undecodable, TR-030). It MUST NOT clamp — clamping is
/// only for decodable out-of-range analog fields (T050). Pure and total: crafted
/// bad bytes can never panic here (the underlying `bitcode::decode` returns
/// `Result`, never panics), so this is the single place malformed bytes are
/// turned into a safe, logged drop. The renet adapter's receive path decodes via
/// the same `Message::decode`, so malformed bytes never reach the gameplay layer.
pub fn decode_inbound(bytes: &[u8]) -> Result<Message, DropReason> {
    // Oversize first (TR-029): never even attempt to decode an over-MTU payload.
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(DropReason::Oversize);
    }
    // Malformed / truncated / undecodable (TR-030). `decode` returns Err, so a
    // crafted byte string cannot panic — it becomes a safe drop.
    Message::decode(bytes).map_err(|_| DropReason::Malformed)
}

/// T057 (TR-031): the category of a logged rejection — coarse enough to drive
/// anti-cheat without leaking which exact threshold or value triggered it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectionCategory {
    /// An input was clamped (out-of-range analog field, TR-020). Recorded as an
    /// observed anomaly, though the input is still applied (clamped).
    Clamped,
    /// A replayed / duplicate / already-superseded-out-of-order input (TR-022/023).
    Replay,
    /// A stale input (tick outside the acceptance window, TR-022).
    Stale,
    /// A fire intent rejected by the cooldown gate (TR-021).
    FireGated,
    /// An inbound payload exceeded the MTU bound (TR-029).
    Oversize,
    /// An undecodable / malformed / truncated payload (TR-030).
    Malformed,
    /// A message that exceeded the per-client inbound rate limit (TR-028).
    RateLimited,
    /// A session dropped for idle timeout (TR-031).
    IdleTimeout,
}

/// One recorded rejection event (T057, TR-031). Carries ONLY anti-cheat-safe
/// context: the offending entity id (if the connection had a slot), the reason
/// category, and the server tick. It deliberately carries NO raw payload and NO
/// validation threshold, so the log cannot be mined to learn the server's exact
/// bounds (no exploitable leak).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RejectionEvent {
    /// The offending client's entity id, or `None` if it had no live slot.
    pub entity_id: Option<EntityId>,
    /// The coarse reason category.
    pub category: RejectionCategory,
    /// The server tick the rejection occurred at.
    pub server_tick: u32,
}

/// T057 (TR-031): the inspectable, bounded anti-cheat rejection log.
///
/// Holds a per-category running **count** (cheap, unbounded counts) plus a bounded
/// ring of the most recent [`REJECTION_LOG_CAPACITY`] [`RejectionEvent`]s. The
/// ring is capped so the log itself can never grow unboundedly under a flood
/// (TR-027 spirit). Nothing here records raw payloads or exact thresholds, so it
/// is safe to expose (no exploitable leak, TR-031).
#[derive(Clone, Debug, Default)]
pub struct RejectionLog {
    events: std::collections::VecDeque<RejectionEvent>,
    counts: HashMap<RejectionCategory, u64>,
}

impl RejectionLog {
    /// An empty log.
    pub fn new() -> Self {
        Self {
            events: std::collections::VecDeque::with_capacity(REJECTION_LOG_CAPACITY),
            counts: HashMap::new(),
        }
    }

    /// Record a rejection, bumping its category count and pushing it onto the
    /// bounded ring (evicting the oldest event when full).
    fn record(
        &mut self,
        entity_id: Option<EntityId>,
        category: RejectionCategory,
        server_tick: u32,
    ) {
        *self.counts.entry(category).or_insert(0) += 1;
        if self.events.len() == REJECTION_LOG_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(RejectionEvent {
            entity_id,
            category,
            server_tick,
        });
    }

    /// Total rejections recorded across all categories.
    pub fn total(&self) -> u64 {
        self.counts.values().sum()
    }

    /// Running count for one category (anti-cheat metric).
    pub fn count(&self, category: RejectionCategory) -> u64 {
        self.counts.get(&category).copied().unwrap_or(0)
    }

    /// Iterate the retained recent events, oldest first.
    pub fn events(&self) -> impl Iterator<Item = &RejectionEvent> + '_ {
        self.events.iter()
    }

    /// The most recently recorded event, if any.
    pub fn last(&self) -> Option<&RejectionEvent> {
        self.events.back()
    }

    /// Number of retained recent events (capped at [`REJECTION_LOG_CAPACITY`]).
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the log has recorded nothing yet.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(version: u16, token: [u8; CLIENT_TOKEN_BYTES]) -> protocol::Connect {
        protocol::Connect {
            protocol_version: version,
            client_token: token,
        }
    }

    fn session() -> Session {
        Session::new(1, RateConfig::default())
    }

    #[test]
    fn handshake_accepts_matching_version_and_allocates_slot() {
        let mut s = session();
        let accepted = s
            .handshake(ConnectionId(0), &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 7)
            .expect("matching version is accepted");
        assert_eq!(accepted.client_id, EntityId(0));
        assert_eq!(accepted.server_tick, 7);
        assert_eq!(s.client_count(), 1);
    }

    #[test]
    fn handshake_rejects_version_mismatch_without_allocating() {
        let mut s = session();
        let rejected = s
            .handshake(ConnectionId(0), &connect(2, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .expect_err("mismatched version is rejected");
        assert_eq!(rejected.reason, RejectReason::Version);
        assert_eq!(s.client_count(), 0, "a rejected connect allocates no slot");
    }

    #[test]
    fn handshake_rejects_at_capacity_without_allocating() {
        let mut s = session();
        for i in 0..MAX_CLIENTS {
            s.handshake(
                ConnectionId(i as u32),
                &connect(1, [0u8; CLIENT_TOKEN_BYTES]),
                0,
            )
            .expect("under capacity");
        }
        assert!(s.is_full());
        let rejected = s
            .handshake(ConnectionId(99), &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .expect_err("at capacity is rejected");
        assert_eq!(rejected.reason, RejectReason::Full);
        assert_eq!(
            s.client_count(),
            MAX_CLIENTS,
            "a Full reject allocates no extra slot"
        );
    }

    #[test]
    fn handshake_rejects_banned_token_and_closes() {
        let mut s = session();
        let banned = [9u8; CLIENT_TOKEN_BYTES];
        s.ban_token(banned);
        let rejected = s
            .handshake(ConnectionId(0), &connect(1, banned), 0)
            .expect_err("banned token is rejected");
        assert_eq!(rejected.reason, RejectReason::Banned);
        assert_eq!(s.client_count(), 0);
    }

    #[test]
    fn processed_input_seq_is_monotonic_and_echoed() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        s.record_processed_input(conn, 5);
        assert_eq!(s.acked_input_seq(conn), 5);
        // A stale seq never moves the anchor backward.
        s.record_processed_input(conn, 3);
        assert_eq!(s.acked_input_seq(conn), 5);
        s.record_processed_input(conn, 9);
        assert_eq!(s.acked_input_seq(conn), 9);
    }

    #[test]
    fn snapshot_ack_is_recorded_per_client() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        s.record_snapshot_ack(conn, 42);
        assert_eq!(s.client(conn).unwrap().last_acked_snapshot_id, 42);
    }

    // --- T052: seq/tick classification ---------------------------------------

    fn input(seq: u32, tick: u32) -> ClientInput {
        ClientInput::new(
            seq,
            tick,
            vec![protocol::QuantizedIntent {
                forward: 0,
                strafe: 0,
                turn: 0,
                fire: false,
                toggle_assist: false,
                afterburner: false,
            }],
        )
    }

    fn admitted_state() -> ClientState {
        let mut st = ClientState::new(EntityId(0), 0.0);
        st.last_processed_input_seq = 5;
        st
    }

    #[test]
    fn classify_applies_a_fresh_in_window_input() {
        let st = admitted_state();
        assert_eq!(
            Session::classify_input(&st, &input(6, 100), 100),
            InputDisposition::Apply
        );
    }

    #[test]
    fn classify_replay_for_seq_at_or_below_last_processed() {
        let st = admitted_state();
        assert_eq!(
            Session::classify_input(&st, &input(5, 100), 100),
            InputDisposition::Replay,
            "a seq equal to last-processed is a duplicate"
        );
        assert_eq!(
            Session::classify_input(&st, &input(4, 100), 100),
            InputDisposition::Replay,
            "a lower seq is a replay"
        );
    }

    #[test]
    fn classify_stale_for_tick_outside_the_window() {
        let st = admitted_state();
        // server_tick 200, window bound 64 → oldest acceptable tick = 136.
        assert_eq!(
            Session::classify_input(&st, &input(6, 135), 200),
            InputDisposition::Stale
        );
        assert_eq!(
            Session::classify_input(&st, &input(6, 136), 200),
            InputDisposition::Apply,
            "the window boundary tick is still accepted"
        );
    }

    #[test]
    fn classify_replay_takes_precedence_over_stale() {
        let st = admitted_state();
        // Both old seq AND old tick: reported as Replay (checked first).
        assert_eq!(
            Session::classify_input(&st, &input(1, 0), 200),
            InputDisposition::Replay
        );
    }

    // --- T055: inbound rate limit --------------------------------------------

    #[test]
    fn rate_limit_allows_up_to_the_budget_then_throttles() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        // All within tick 0 (same 1 s window): the first 120 allowed.
        for _ in 0..INBOUND_RATE_LIMIT_PER_SEC {
            assert_eq!(s.note_inbound(conn, 0), RateDecision::Allow);
        }
        assert_eq!(
            s.note_inbound(conn, 0),
            RateDecision::Throttle,
            "the 121st message in the window is throttled"
        );
        assert_eq!(s.rejections().count(RejectionCategory::RateLimited), 1);
    }

    #[test]
    fn rate_limit_window_resets_after_one_second() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        for _ in 0..INBOUND_RATE_LIMIT_PER_SEC {
            s.note_inbound(conn, 0);
        }
        assert_eq!(s.note_inbound(conn, 0), RateDecision::Throttle);
        // 30 ticks at 30 Hz == 1 s later → the window rolls and the budget resets.
        assert_eq!(s.note_inbound(conn, 30), RateDecision::Allow);
    }

    // --- T056: decode_inbound guard ------------------------------------------

    #[test]
    fn decode_inbound_accepts_a_valid_encoded_message() {
        let msg = Message::Disconnect(protocol::Disconnect {
            reason: protocol::DisconnectReason::ClientClosed,
        });
        let bytes = msg.encode();
        assert_eq!(decode_inbound(&bytes), Ok(msg));
    }

    #[test]
    fn decode_inbound_rejects_oversize_without_decoding() {
        let bytes = vec![0u8; MAX_PAYLOAD_BYTES + 1];
        assert_eq!(decode_inbound(&bytes), Err(DropReason::Oversize));
    }

    #[test]
    fn decode_inbound_rejects_malformed_bytes_without_panicking() {
        // Crafted garbage: must be a safe Malformed drop, never a panic.
        assert_eq!(
            decode_inbound(&[0xFF, 0xFF, 0xFF, 0xFF, 0x7F]),
            Err(DropReason::Malformed)
        );
        // Empty / truncated payload is also a safe drop.
        assert_eq!(decode_inbound(&[]), Err(DropReason::Malformed));
    }

    // --- T057: idle timeout + rejection log ----------------------------------

    #[test]
    fn idle_timeout_lists_only_the_silent_session() {
        let mut s = session();
        let (a, b) = (ConnectionId(0), ConnectionId(1));
        s.handshake(a, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        s.handshake(b, &connect(1, [1u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        // 330 ticks at 30 Hz == 11 s. `b` sends a packet at tick 330 (alive);
        // `a` stays silent since admission at tick 0.
        s.note_inbound(b, 330);
        assert_eq!(
            s.timed_out(330),
            vec![a],
            "only the silent session times out; the active one is unaffected"
        );
    }

    #[test]
    fn idle_timeout_does_not_fire_before_the_window() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        // 270 ticks == 9 s < 10 s timeout.
        assert!(s.timed_out(270).is_empty());
    }

    #[test]
    fn rejection_log_records_category_tick_and_entity_no_payload() {
        let mut s = session();
        let conn = ConnectionId(0);
        s.handshake(conn, &connect(1, [0u8; CLIENT_TOKEN_BYTES]), 0)
            .unwrap();
        s.log_rejection(conn, RejectionCategory::Replay, 42);
        s.log_rejection(conn, RejectionCategory::Malformed, 43);
        assert_eq!(s.rejections().total(), 2);
        assert_eq!(s.rejections().count(RejectionCategory::Replay), 1);
        let last = s.rejections().last().expect("an event was logged");
        // Only id + category + tick are retained (the type structurally cannot
        // carry a raw payload or a threshold).
        assert_eq!(last.entity_id, Some(EntityId(0)));
        assert_eq!(last.category, RejectionCategory::Malformed);
        assert_eq!(last.server_tick, 43);
    }

    #[test]
    fn rejection_log_is_bounded() {
        let mut log = RejectionLog::new();
        for tick in 0..(REJECTION_LOG_CAPACITY as u32 + 50) {
            log.record(None, RejectionCategory::Malformed, tick);
        }
        assert_eq!(log.len(), REJECTION_LOG_CAPACITY, "ring never overgrows");
        // The count is unbounded (cheap) and reflects every record.
        assert_eq!(
            log.count(RejectionCategory::Malformed),
            REJECTION_LOG_CAPACITY as u64 + 50
        );
    }
}
