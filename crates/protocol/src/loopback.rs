//! In-memory loopback transport (TR-003, TR-005) — the loopback-first baseline
//! that lets an embedded server + client communicate with no real sockets
//! (HINT-001; Phase 3 T022 runs an embedded server against this).
//!
//! Properties guaranteed by the **default** [`LoopbackTransport::pair`]:
//! - **Deterministic**: single-threaded, ordered queues; no clocks, no threads.
//! - **Zero-latency**: a `send` is immediately visible to the peer's `recv`.
//! - **Loss-free, in-order**: messages are delivered in send order, none dropped.
//!
//! [`LoopbackTransport::with_loss_jitter`] (T047, TR-036) opts a pair into a
//! configurable **lossy/jittered** medium for tests: a seeded deterministic PRNG
//! (no `rand` global, no wall clock) drives uniform single-packet loss, ±jitter on
//! delivery, and a scripted consecutive-drop burst. It is **reproducible run to
//! run** for a given seed, and the default pair is left exactly as before — only
//! the explicit `with_loss_jitter` constructor turns the knobs on.
//!
//! All signatures honor the seam: only `protocol`/`glam`/`sim`/`std` types
//! appear (SC-006).

use crate::messages::{ConnectionId, Message, NetStats};
use crate::transport::{DisconnectReason, NetTransport};
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;

/// One end of a connection inside the switch: its inbox plus byte counters.
#[derive(Default)]
struct Endpoint {
    /// Messages waiting to be drained by `recv`, oldest first.
    inbox: Vec<Message>,
    /// Messages held back by jitter, each tagged with the logical millisecond at
    /// which it becomes deliverable. Released into `inbox` (in delivery-time then
    /// arrival order) once the switch clock reaches that time. Empty unless a
    /// [`LossJitter`] profile is active (the default pair never touches this).
    delayed: Vec<DelayedMessage>,
    /// Bandwidth bookkeeping (TR-014).
    stats: NetStats,
    /// `true` once disconnected; further sends are dropped.
    closed: bool,
}

/// A message held in [`Endpoint::delayed`] until the switch clock reaches
/// `deliver_at_ms`. `arrival_seq` is the monotonic order it was sent in, so
/// releases that share a delivery time still come out in send order (stable).
struct DelayedMessage {
    /// Logical switch-clock millisecond at which this becomes deliverable.
    deliver_at_ms: u64,
    /// Monotonic send order, the tie-breaker for equal delivery times.
    arrival_seq: u64,
    /// The payload to release.
    msg: Message,
}

/// A tiny seeded, deterministic PRNG (SplitMix64) for the loss/jitter medium
/// (T047). Deliberately self-contained: the loss/jitter test must be reproducible
/// run-to-run, which rules out the `rand` global and any wall-clock seed. Only the
/// caller-supplied seed drives the sequence.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next pseudo-random `u64` (the canonical SplitMix64 step).
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `f64` in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        // Top 53 bits → a uniform mantissa, the standard construction.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `0..=max` (inclusive). `max == 0` yields `0`.
    fn next_in_inclusive(&mut self, max: u64) -> u64 {
        if max == 0 {
            0
        } else {
            self.next_u64() % (max + 1)
        }
    }
}

/// A configurable lossy/jittered delivery profile (T047, TR-036). Applied at the
/// single [`Switch::deliver`] hook; absent on the default lossless pair.
///
/// Determinism: every random decision comes from the seeded [`SplitMix64`], and
/// jitter is measured against the switch's own logical clock
/// ([`Switch::now_ms`]), never the wall clock — so a given seed + driving
/// sequence reproduces byte-for-byte.
struct LossJitter {
    /// Uniform single-packet loss probability in `0.0..=1.0` (baseline 0.05).
    loss: f64,
    /// Jitter half-range in milliseconds: each delivered packet is held an extra
    /// `0..=2*jitter_ms` minus `jitter_ms` → uniform in `[-jitter_ms, +jitter_ms]`,
    /// clamped so delivery never moves earlier than "now" (baseline 50).
    jitter_ms: u64,
    /// Remaining count of a scripted consecutive-drop burst: while > 0 the next
    /// delivery is dropped and the counter decremented, regardless of `loss`. Lets
    /// a test script an exact run of dropped snapshots (TR-036 consecutive-drop).
    scripted_drops: u64,
    /// The seeded PRNG driving loss and jitter (no global / wall-clock entropy).
    rng: SplitMix64,
}

/// Configuration for [`LoopbackTransport::with_loss_jitter`] (T047, TR-036).
/// Plain data so a test can spell out the exact harness parameters TR-036 fixes
/// (5% loss, ±50 ms jitter, a scripted consecutive-drop burst) and a seed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LossJitterConfig {
    /// Uniform single-packet loss probability, `0.0..=1.0` (TR-036 baseline 0.05).
    pub loss: f64,
    /// Jitter half-range in milliseconds; delivery is delayed by a uniform value
    /// in `[-jitter_ms, +jitter_ms]`, clamped non-negative (TR-036 baseline 50).
    pub jitter_ms: u64,
    /// Seed for the deterministic PRNG so the run is reproducible run-to-run.
    pub seed: u64,
}

impl Default for LossJitterConfig {
    /// The TR-036 baseline: 5% loss, ±50 ms jitter, fixed seed, no scripted burst
    /// (call [`LoopbackTransport::script_consecutive_drops`] to add one).
    fn default() -> Self {
        Self {
            loss: 0.05,
            jitter_ms: 50,
            seed: 0xD4D_5113,
        }
    }
}

/// The shared in-memory medium two [`LoopbackTransport`]s talk through. Holds
/// every endpoint and the routing between paired connection ids. Cloned (via
/// `Rc`) into each transport so both observe the same state.
struct Switch {
    /// Per-connection endpoints, keyed by [`ConnectionId`].
    endpoints: HashMap<u32, Endpoint>,
    /// Routing: a message sent *to* key is appended to value's inbox. The pair
    /// is symmetric — client→server and server→client both registered.
    routes: HashMap<u32, u32>,
    /// Connection ids the server has not yet `accept`ed.
    pending_accept: Vec<ConnectionId>,
    /// Next connection id to mint. Monotonic for determinism.
    next_id: u32,
    /// Server connection ids minted per client `connect`, keyed by endpoint
    /// `SocketAddr`, so a loopback client's `connect(addr)` resolves to the
    /// matching server-side endpoint.
    addr_server_conn: HashMap<SocketAddr, u32>,
    /// The loss/jitter profile, or `None` for the default lossless/zero-latency
    /// medium. Only [`LoopbackTransport::with_loss_jitter`] sets this.
    loss_jitter: Option<LossJitter>,
    /// Logical clock in milliseconds, advanced by [`LoopbackTransport::advance`].
    /// Jitter delivery times are measured against it. Stays 0 (and irrelevant) on
    /// the default lossless pair, which delivers immediately.
    now_ms: u64,
    /// Monotonic send counter: the stable tie-breaker so jittered messages that
    /// share a delivery time still release in send order.
    send_seq: u64,
}

impl Switch {
    fn mint_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Append `msg` to the inbox of `to` and credit both sides' byte counters.
    /// The single delivery point: when a [`LossJitter`] profile is active it
    /// applies loss (drop entirely, no bytes credited) and jitter (hold in the
    /// receiver's `delayed` queue until the clock reaches the delivery time). With
    /// no profile (the default pair) it delivers immediately and losslessly,
    /// exactly as before (callers untouched).
    ///
    /// `reliable` bypasses loss/jitter even under an active profile: a real
    /// reliable-ordered channel does not drop or reorder, so the handshake /
    /// teardown messages that go [`NetTransport::send_reliable`] are delivered
    /// immediately and losslessly regardless of the lossy medium — only the
    /// unreliable per-tick input/snapshot traffic is subject to loss/jitter
    /// (TR-005). This keeps a lossy test's handshake deterministic.
    fn deliver(&mut self, from: u32, to: u32, msg: Message, reliable: bool) {
        let bytes = msg.encode().len() as u64;
        // Sender's outbound bytes always count (the bytes left the sender), even
        // if the medium then drops the packet — they really were transmitted.
        if let Some(src) = self.endpoints.get_mut(&from) {
            src.stats.bytes_out += bytes;
        }

        // Decide loss/jitter against the active profile (if any) BEFORE touching
        // the receiver, so the borrow of the profile and of the endpoint don't
        // overlap. Reliable traffic is never lost/jittered (a real reliable
        // channel is lossless + ordered).
        let outcome = if reliable {
            DeliveryOutcome::Immediate
        } else {
            match self.loss_jitter.as_mut() {
                None => DeliveryOutcome::Immediate,
                Some(lj) => lj.decide(self.now_ms),
            }
        };

        // Deliver only to an open receiver; a closed peer drops silently
        // (no inbound bytes credited for an undelivered message).
        let Some(dst) = self.endpoints.get_mut(&to) else {
            return;
        };
        if dst.closed {
            return;
        }
        match outcome {
            DeliveryOutcome::Dropped => {
                // Lost packet: nothing delivered, no inbound bytes credited.
            }
            DeliveryOutcome::Immediate => {
                dst.stats.bytes_in += bytes;
                dst.inbox.push(msg);
            }
            DeliveryOutcome::DelayUntil(deliver_at_ms) => {
                // Bytes are credited when the packet actually leaves the medium
                // into the inbox (`release_due`), so a still-in-flight jittered
                // packet does not inflate `bytes_in` early.
                let arrival_seq = self.send_seq;
                self.send_seq += 1;
                dst.delayed.push(DelayedMessage {
                    deliver_at_ms,
                    arrival_seq,
                    msg,
                });
            }
        }
    }

    /// Move every delayed message on `to` whose delivery time has arrived into the
    /// inbox, in (delivery-time, send-order) order, crediting inbound bytes as it
    /// lands. A no-op on the default lossless pair (nothing is ever delayed).
    fn release_due(&mut self, to: u32) {
        let now = self.now_ms;
        let Some(dst) = self.endpoints.get_mut(&to) else {
            return;
        };
        if dst.delayed.is_empty() {
            return;
        }
        // Partition: due vs still-in-flight, preserving the in-flight remainder.
        let mut due: Vec<DelayedMessage> = Vec::new();
        let mut still: Vec<DelayedMessage> = Vec::new();
        for d in dst.delayed.drain(..) {
            if d.deliver_at_ms <= now {
                due.push(d);
            } else {
                still.push(d);
            }
        }
        dst.delayed = still;
        // Stable release order: by delivery time, then original send order — so a
        // jittered reorder is bounded and never spuriously backward beyond jitter.
        due.sort_by(|a, b| {
            a.deliver_at_ms
                .cmp(&b.deliver_at_ms)
                .then(a.arrival_seq.cmp(&b.arrival_seq))
        });
        for d in due {
            dst.stats.bytes_in += d.msg.encode().len() as u64;
            dst.inbox.push(d.msg);
        }
    }
}

/// What the medium decided to do with one packet at [`Switch::deliver`].
enum DeliveryOutcome {
    /// Deliver now, losslessly (the default-pair path).
    Immediate,
    /// Drop entirely (loss or a scripted consecutive-drop).
    Dropped,
    /// Hold until the given logical millisecond (jitter).
    DelayUntil(u64),
}

impl LossJitter {
    /// Decide a single packet's fate at logical time `now_ms`. A scripted
    /// consecutive-drop takes precedence; otherwise roll loss, then jitter.
    fn decide(&mut self, now_ms: u64) -> DeliveryOutcome {
        if self.scripted_drops > 0 {
            self.scripted_drops -= 1;
            return DeliveryOutcome::Dropped;
        }
        // Always consume one loss roll and one jitter roll per packet so the PRNG
        // stream stays aligned to the packet sequence regardless of the outcome
        // (keeps the run reproducible and easy to reason about).
        let loss_roll = self.rng.next_f64();
        let jitter_roll = self.rng.next_in_inclusive(2 * self.jitter_ms);
        if loss_roll < self.loss {
            return DeliveryOutcome::Dropped;
        }
        if self.jitter_ms == 0 {
            return DeliveryOutcome::Immediate;
        }
        // Map `0..=2*jitter_ms` to a signed offset in `[-jitter_ms, +jitter_ms]`,
        // then clamp delivery to no earlier than now (negative offset just speeds
        // it up toward immediate, never into the past).
        let signed = jitter_roll as i64 - self.jitter_ms as i64;
        let deliver_at = now_ms as i64 + signed;
        DeliveryOutcome::DelayUntil(deliver_at.max(now_ms as i64) as u64)
    }
}

/// A single endpoint's view of the shared loopback [`Switch`]. Plays either the
/// client or server role; create a linked pair with [`LoopbackTransport::pair`].
#[derive(Clone)]
pub struct LoopbackTransport {
    switch: Rc<RefCell<Switch>>,
    /// `true` if this transport is the server end (owns the accept queue).
    is_server: bool,
}

impl LoopbackTransport {
    /// Create a linked client↔server pair sharing one in-memory switch. The
    /// returned transports communicate deterministically, with zero latency and
    /// no loss. Tuple order is `(client, server)`.
    ///
    /// The client must still call [`NetTransport::connect`] (with any
    /// [`SocketAddr`] — it is just a registry key for loopback) to establish the
    /// session; the server then observes the new connection via
    /// [`NetTransport::accept`].
    pub fn pair() -> (LoopbackTransport, LoopbackTransport) {
        Self::pair_with(None)
    }

    /// Create a linked pair whose medium applies the configurable loss/jitter
    /// profile `config` (T047, TR-036): uniform single-packet loss, ±jitter on
    /// delivery, and (via [`LoopbackTransport::script_consecutive_drops`]) a
    /// scripted consecutive-drop burst — all driven by a **seeded deterministic
    /// PRNG** so a run is reproducible run-to-run (no `rand` global, no wall
    /// clock). Tuple order is `(client, server)`.
    ///
    /// Jitter is measured against a **logical clock** the caller advances with
    /// [`LoopbackTransport::advance`]: a held packet only lands once the clock
    /// reaches its delivery time, so a test drives time explicitly and
    /// deterministically rather than depending on real elapsed time. The default
    /// [`LoopbackTransport::pair`] is unaffected — it stays lossless and
    /// zero-latency.
    pub fn with_loss_jitter(config: LossJitterConfig) -> (LoopbackTransport, LoopbackTransport) {
        let lj = LossJitter {
            loss: config.loss,
            jitter_ms: config.jitter_ms,
            scripted_drops: 0,
            rng: SplitMix64::new(config.seed),
        };
        Self::pair_with(Some(lj))
    }

    fn pair_with(loss_jitter: Option<LossJitter>) -> (LoopbackTransport, LoopbackTransport) {
        let switch = Rc::new(RefCell::new(Switch {
            endpoints: HashMap::new(),
            routes: HashMap::new(),
            pending_accept: Vec::new(),
            next_id: 0,
            addr_server_conn: HashMap::new(),
            loss_jitter,
            now_ms: 0,
            send_seq: 0,
        }));
        let client = LoopbackTransport {
            switch: Rc::clone(&switch),
            is_server: false,
        };
        let server = LoopbackTransport {
            switch,
            is_server: true,
        };
        (client, server)
    }

    /// Advance the shared logical clock by `delta_ms` (T047). Jittered packets
    /// whose delivery time has now arrived become deliverable on the next
    /// [`NetTransport::recv`]. A no-op semantically on the default lossless pair
    /// (which never delays a packet). Either end may drive the clock; both share
    /// it, so the test advances time once per rendered frame.
    pub fn advance(&self, delta_ms: u64) {
        let mut sw = self.switch.borrow_mut();
        sw.now_ms = sw.now_ms.saturating_add(delta_ms);
    }

    /// The current shared logical-clock value in milliseconds (T047).
    pub fn now_ms(&self) -> u64 {
        self.switch.borrow().now_ms
    }

    /// Script the medium to drop the next `count` packets it is asked to deliver,
    /// consecutively, regardless of the loss probability (T047, TR-036
    /// consecutive-drop case). Takes precedence over the random loss roll; once
    /// the burst is exhausted, normal loss/jitter resume. Adds to any burst still
    /// pending. A no-op if no loss/jitter profile is active.
    pub fn script_consecutive_drops(&self, count: u64) {
        let mut sw = self.switch.borrow_mut();
        if let Some(lj) = sw.loss_jitter.as_mut() {
            lj.scripted_drops = lj.scripted_drops.saturating_add(count);
        }
    }
}

impl NetTransport for LoopbackTransport {
    fn connect(&mut self, endpoint: SocketAddr) -> ConnectionId {
        let mut sw = self.switch.borrow_mut();

        // Mint the client-side and server-side endpoints, then cross-route them.
        let client_id = sw.mint_id();
        let server_id = sw.mint_id();

        sw.endpoints.insert(client_id, Endpoint::default());
        sw.endpoints.insert(server_id, Endpoint::default());

        // Symmetric routing: sending on the client id reaches the server id and
        // vice versa.
        sw.routes.insert(client_id, server_id);
        sw.routes.insert(server_id, client_id);

        // Record the server-side id under the endpoint addr and enqueue it for
        // the server's next `accept`.
        sw.addr_server_conn.insert(endpoint, server_id);
        sw.pending_accept.push(ConnectionId(server_id));

        ConnectionId(client_id)
    }

    fn accept(&mut self) -> Vec<ConnectionId> {
        debug_assert!(
            self.is_server,
            "accept() is only meaningful on the server end of a loopback pair"
        );
        let mut sw = self.switch.borrow_mut();
        std::mem::take(&mut sw.pending_accept)
    }

    fn send_reliable(&mut self, conn: ConnectionId, msg: &Message) {
        // Reliable channel: delivered losslessly and in order even under an active
        // loss/jitter profile (a real reliable channel never drops/reorders). On
        // the default lossless pair this is identical to `send_unreliable`.
        let mut sw = self.switch.borrow_mut();
        let Some(&dst) = sw.routes.get(&conn.0) else {
            return;
        };
        sw.deliver(conn.0, dst, msg.clone(), /*reliable=*/ true);
    }

    fn send_unreliable(&mut self, conn: ConnectionId, msg: &Message) {
        let mut sw = self.switch.borrow_mut();
        let Some(&dst) = sw.routes.get(&conn.0) else {
            // No route (never connected / already torn down): drop silently.
            return;
        };
        sw.deliver(conn.0, dst, msg.clone(), /*reliable=*/ false);
    }

    fn recv(&mut self, conn: ConnectionId) -> Vec<Message> {
        let mut sw = self.switch.borrow_mut();
        // First release any jittered packets whose delivery time has arrived
        // (no-op on the default lossless pair, which never delays anything).
        sw.release_due(conn.0);
        match sw.endpoints.get_mut(&conn.0) {
            Some(ep) => std::mem::take(&mut ep.inbox),
            None => Vec::new(),
        }
    }

    fn disconnect(&mut self, conn: ConnectionId, _reason: DisconnectReason) {
        let mut sw = self.switch.borrow_mut();
        if let Some(ep) = sw.endpoints.get_mut(&conn.0) {
            ep.closed = true;
            // Drop any undelivered inbound messages on a closed endpoint —
            // both already-landed and still-in-flight (jittered) packets.
            ep.inbox.clear();
            ep.delayed.clear();
        }
    }

    fn stats(&self, conn: ConnectionId) -> NetStats {
        let sw = self.switch.borrow();
        sw.endpoints
            .get(&conn.0)
            .map(|ep| ep.stats)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{Connect, SnapshotAck};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7777)
    }

    fn connect_msg() -> Message {
        Message::Connect(Connect {
            protocol_version: 1,
            client_token: [0u8; crate::messages::CLIENT_TOKEN_BYTES],
        })
    }

    #[test]
    fn client_connect_is_observed_by_server_accept() {
        let (mut client, mut server) = LoopbackTransport::pair();
        let _client_conn = client.connect(addr());
        let accepted = server.accept();
        assert_eq!(accepted.len(), 1, "server must observe one new connection");
        // A second accept drains nothing new.
        assert!(server.accept().is_empty());
    }

    #[test]
    fn messages_deliver_in_send_order_lossless() {
        let (mut client, mut server) = LoopbackTransport::pair();
        let client_conn = client.connect(addr());
        let server_conn = server.accept()[0];

        // Client → server: send three, expect three in order.
        let a = connect_msg();
        let b = Message::SnapshotAck(SnapshotAck {
            last_snapshot_id: 1,
        });
        let c = Message::SnapshotAck(SnapshotAck {
            last_snapshot_id: 2,
        });
        client.send_reliable(client_conn, &a);
        client.send_unreliable(client_conn, &b);
        client.send_unreliable(client_conn, &c);

        let got = server.recv(server_conn);
        assert_eq!(got, vec![a, b, c], "messages must arrive in send order");
        // Inbox drained.
        assert!(server.recv(server_conn).is_empty());
    }

    #[test]
    fn stats_count_encoded_bytes_both_ways() {
        let (mut client, mut server) = LoopbackTransport::pair();
        let client_conn = client.connect(addr());
        let server_conn = server.accept()[0];

        let msg = connect_msg();
        let n = msg.encode().len() as u64;
        client.send_reliable(client_conn, &msg);

        assert_eq!(client.stats(client_conn).bytes_out, n);
        assert_eq!(client.stats(client_conn).bytes_in, 0);
        assert_eq!(server.stats(server_conn).bytes_in, n);
        assert_eq!(server.stats(server_conn).bytes_out, 0);
    }

    #[test]
    fn disconnect_stops_delivery() {
        let (mut client, mut server) = LoopbackTransport::pair();
        let client_conn = client.connect(addr());
        let server_conn = server.accept()[0];

        server.disconnect(server_conn, DisconnectReason::ServerClosed);
        client.send_unreliable(client_conn, &connect_msg());
        assert!(
            server.recv(server_conn).is_empty(),
            "a closed endpoint must not receive further messages"
        );
    }

    // --- T047: loss/jitter medium --------------------------------------------

    fn snap(id: u16) -> Message {
        Message::SnapshotAck(SnapshotAck {
            last_snapshot_id: id,
        })
    }

    #[test]
    fn default_pair_is_unchanged_lossless_zero_latency() {
        // Sanity: the default constructor must still deliver immediately with no
        // loss and no need to advance a clock (existing behavior, T047 guard).
        let (mut client, mut server) = LoopbackTransport::pair();
        let cc = client.connect(addr());
        let sc = server.accept()[0];
        for i in 0..50 {
            client.send_unreliable(cc, &snap(i));
        }
        let got = server.recv(sc);
        assert_eq!(got.len(), 50, "default pair drops nothing");
        assert_eq!(got[0], snap(0));
        assert_eq!(got[49], snap(49));
    }

    #[test]
    fn loss_jitter_is_deterministic_for_a_seed() {
        // Same seed + same driving sequence → identical delivered set, run to run.
        fn run() -> Vec<u16> {
            let cfg = LossJitterConfig {
                loss: 0.05,
                jitter_ms: 50,
                seed: 12345,
            };
            let (mut client, mut server) = LoopbackTransport::with_loss_jitter(cfg);
            let cc = client.connect(addr());
            let sc = server.accept()[0];
            let mut delivered = Vec::new();
            for i in 0..200u16 {
                client.send_unreliable(cc, &snap(i));
                // Advance well past the jitter window each step so everything that
                // survived loss has landed by the end.
                client.advance(200);
                for m in server.recv(sc) {
                    if let Message::SnapshotAck(a) = m {
                        delivered.push(a.last_snapshot_id);
                    }
                }
            }
            delivered
        }
        assert_eq!(run(), run(), "a seeded loss/jitter run is reproducible");
    }

    #[test]
    fn loss_drops_roughly_the_configured_fraction() {
        // 5% loss over a large sample lands near 5% (loose bound; just guards the
        // knob is actually wired, not an exact statistical claim).
        let cfg = LossJitterConfig {
            loss: 0.05,
            jitter_ms: 0,
            seed: 99,
        };
        let (mut client, mut server) = LoopbackTransport::with_loss_jitter(cfg);
        let cc = client.connect(addr());
        let sc = server.accept()[0];
        let n = 2000u16;
        let mut delivered = 0usize;
        for i in 0..n {
            client.send_unreliable(cc, &snap(i));
            delivered += server.recv(sc).len();
        }
        let dropped = n as usize - delivered;
        let frac = dropped as f64 / n as f64;
        assert!(
            (0.02..0.09).contains(&frac),
            "≈5% loss expected, observed {frac}"
        );
    }

    #[test]
    fn jitter_holds_a_packet_until_the_clock_advances() {
        // With pure jitter (no loss) a packet may be held; once the clock passes
        // the worst-case jitter every sent packet has been delivered exactly once.
        let cfg = LossJitterConfig {
            loss: 0.0,
            jitter_ms: 50,
            seed: 7,
        };
        let (mut client, mut server) = LoopbackTransport::with_loss_jitter(cfg);
        let cc = client.connect(addr());
        let sc = server.accept()[0];
        client.send_unreliable(cc, &snap(1));
        // Before advancing past max jitter the packet may still be in flight; after
        // advancing well past it, it must have arrived exactly once.
        client.advance(100);
        let mut got = server.recv(sc);
        client.advance(100);
        got.extend(server.recv(sc));
        assert_eq!(got, vec![snap(1)], "no-loss jitter delivers exactly once");
    }

    #[test]
    fn scripted_consecutive_drops_drop_exactly_that_many() {
        let cfg = LossJitterConfig {
            loss: 0.0, // isolate the scripted burst from random loss
            jitter_ms: 0,
            seed: 1,
        };
        let (mut client, mut server) = LoopbackTransport::with_loss_jitter(cfg);
        let cc = client.connect(addr());
        let sc = server.accept()[0];
        client.script_consecutive_drops(3);
        let mut delivered = Vec::new();
        for i in 0..6u16 {
            client.send_unreliable(cc, &snap(i));
            for m in server.recv(sc) {
                if let Message::SnapshotAck(a) = m {
                    delivered.push(a.last_snapshot_id);
                }
            }
        }
        // The first three are the scripted burst; 3,4,5 survive (no other loss).
        assert_eq!(delivered, vec![3, 4, 5]);
    }
}
