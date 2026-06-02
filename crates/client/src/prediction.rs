//! Client-side prediction & reconciliation of the **local** ship (OBJ3, AD-005).
//!
//! The client predicts ONLY its own ship by running the **shared `sim`** on its
//! numbered inputs (Principle II / AD-005): the predicted [`World`] is stepped by
//! [`sim::add_fixed_step_systems`] at [`sim::FixedDt`], the *identical* schedule
//! and dt the authoritative server uses, so the two advance **bit-identically**
//! for identical inputs (HINT-003, TR-016, the determinism test T037). Remote
//! entities are NOT simulated here — they are interpolated from snapshots
//! (Phase 6); only the local ship lives in this predicted world.
//!
//! The loop, per the lifecycle:
//! - [`predict_local`] (T034): apply the newest numbered input to the local
//!   ship's [`sim::ShipIntent`] component and step the predicted world once. The
//!   ship moves *this tick*, before any server round-trip (SC-001).
//! - [`InputBuffer`] (T034): retains every unacknowledged numbered input, capped
//!   at [`InputBuffer::CAP`] (TR-027), so reconciliation can replay them.
//! - [`Predictor::reconcile`] (T035): on each snapshot, re-seed the local ship to
//!   the authoritative state and deterministically replay every still-unacked
//!   input through the shared sim; drop acked inputs from the buffer (TR-009).
//! - [`RenderSmoother`] / [`smooth_correction`] (T036): the predicted/authoritative
//!   state is corrected immediately, but the *rendered* ship is not snapped — the
//!   residual (reconciled − previously-rendered) is blended out over ≤
//!   [`MAX_SMOOTH_TICKS`] ticks, no single tick exceeding [`MAX_SNAP_FRACTION`] of
//!   the residual, residual strictly non-increasing (TR-033, no teleport).

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use glam::Vec2;
use protocol::{EntityId, EntityKind, QuantizedIntent, Snapshot};
use sim::components::{AngularVelocity, FlightAssist, Heading, Position, Ship, Velocity};
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

// --- OD-001 provisional feel constants (accepted 2026-06-02; T046 records the
//     full provisional set: RECON_EPS_POS/VEL, MAX_SNAP_FRACTION/FLOOR, and
//     MAX_INTERP_DELTA). These are play-feel product calls, not final derivations;
//     tune them in a networked playtest like the flight `Tuning` values. The
//     MAX_INTERP_DELTA bound is the only one with a kinematic derivation (see its
//     doc comment), but its acceptance as the no-teleport threshold is still
//     provisional pending the networked playtest. ----------------------------

/// Reconciliation convergence threshold for **position**, in sim units (meters).
/// Once the predicted local ship is within this of the authoritative state it
/// counts as "converged" (TR-033 convergence bound, SC-002).
pub const RECON_EPS_POS: f32 = 0.05;

/// Reconciliation convergence threshold for **velocity**, in sim units/s.
pub const RECON_EPS_VEL: f32 = 0.05;

/// No-teleport cap (TR-033, OD-001): the smoothed correction blends the residual
/// over at most [`MAX_SMOOTH_TICKS`] ticks, and **no single tick** may move the
/// rendered local ship by more than this fraction of the *current* residual. At
/// 25% the residual decays geometrically (`0.75^n`) — there is never an
/// instantaneous full-magnitude snap, and the residual is strictly non-increasing.
pub const MAX_SNAP_FRACTION: f32 = 0.25;

/// Absolute floor for a single smoothing step, in sim units. Geometric decay
/// alone would take unbounded ticks to reach exactly zero; once the residual is
/// this small (well under [`RECON_EPS_POS`]) a single step may close it entirely
/// rather than dragging an imperceptible offset out forever. Chosen below the
/// convergence epsilon so it can never cause a visible jump.
pub const MAX_SNAP_FLOOR: f32 = 0.01;

/// The smoothing window (TR-033): the rendered correction is fully blended out
/// within this many ticks. With [`MAX_SNAP_FRACTION`] = 0.25 the residual is
/// `≤ 0.75^5 ≈ 23.7%` of its start after 5 ticks; the [`MAX_SNAP_FLOOR`] closes
/// the remainder, so a correction never rides out longer than this.
pub const MAX_SMOOTH_TICKS: u32 = 5;

/// No-teleport bound for an **interpolated remote** (OBJ4, TR-010/036, OD-001):
/// the maximum position a remote entity may move between two consecutive rendered
/// frames before it counts as a teleport (a test failure, T048/SC-004).
///
/// Derivation: the fastest a ship can travel is the emergent
/// [`sim::Tuning::top_speed`] (`thrust_force / linear_drag` = 80 sim-units/s with
/// the default tuning). At the baseline 20 Hz snapshot send rate (TR-044) one
/// snapshot interval is `1/20 s`, so the most a remote can legitimately advance
/// across a single snapshot interval is `top_speed / 20 ≈ 4.0` sim units. A
/// remote interpolated between two adjacent snapshots therefore never moves more
/// than this much within one interval; a single rendered-frame jump beyond it
/// (e.g. after a single dropped snapshot the ~100 ms buffer should ride out) is a
/// teleport. The value here is computed for the default tuning to keep it a
/// compile-time constant; the test (T048) recomputes it from the live
/// [`sim::Tuning`] so a tuning change cannot silently invalidate the bound.
///
/// `80.0 / 20.0 = 4.0`.
///
/// Like [`RECON_EPS_POS`]/[`RECON_EPS_VEL`]/[`MAX_SNAP_FRACTION`] this is an
/// **accepted-provisional** OD-001 value (accepted 2026-06-02); it is to be tuned
/// in a networked playtest alongside the flight `Tuning` values, not derived as a
/// final product call.
pub const MAX_INTERP_DELTA: f32 = 80.0 / 20.0;

// --- Numbered input + the unacknowledged-input buffer (T034, TR-007/027) -----

/// A single client input numbered by its monotonic per-client [`seq`]
/// (T033/TR-007). The buffer retains these until the server acks them, so
/// reconciliation can replay everything after the acked seq.
///
/// [`seq`]: NumberedInput::seq
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NumberedInput {
    /// Monotonic per-client sequence number (matches the `seq` sent on the wire).
    pub seq: u32,
    /// The quantized pilot intent for that tick (the same form sent on the wire,
    /// so replay reproduces exactly what the server applied).
    pub intent: QuantizedIntent,
}

/// Holds the client's **unacknowledged** numbered inputs, newest pushed last, so
/// reconciliation can deterministically replay them after re-seeding to the
/// authoritative state (T034/T035, TR-007/009).
///
/// Bounded at [`InputBuffer::CAP`] inputs (TR-027): on overflow the **oldest** is
/// dropped, never grown unboundedly. At 60 Hz that is ~1 s of inputs — far more
/// than any plausible RTT, so a dropped input is already long acked.
#[derive(Default, Debug, Clone)]
pub struct InputBuffer {
    /// Unacked inputs in ascending `seq` order (oldest first, newest last).
    inputs: Vec<NumberedInput>,
}

impl InputBuffer {
    /// Maximum retained unacknowledged inputs (TR-027 baseline 64). The oldest is
    /// dropped on overflow so memory is bounded regardless of ack latency.
    pub const CAP: usize = 64;

    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a freshly produced numbered input. On overflow past
    /// [`InputBuffer::CAP`] the **oldest** input is dropped (TR-027) — never grow
    /// unboundedly.
    pub fn push(&mut self, input: NumberedInput) {
        self.inputs.push(input);
        if self.inputs.len() > Self::CAP {
            // Drop from the front (oldest). One overflow can only ever drop one,
            // since we push one at a time, but `drain` keeps it correct if a
            // caller ever batches.
            let overflow = self.inputs.len() - Self::CAP;
            self.inputs.drain(0..overflow);
        }
    }

    /// Drop every input the server has acknowledged: those with `seq <=
    /// acked_seq` are removed (TR-009), leaving only the inputs still in flight
    /// that reconciliation must replay.
    pub fn drop_acked(&mut self, acked_seq: u32) {
        self.inputs.retain(|i| i.seq > acked_seq);
    }

    /// The still-unacked inputs in replay order (ascending `seq`).
    pub fn unacked(&self) -> &[NumberedInput] {
        &self.inputs
    }

    /// The newest buffered input, if any (the one [`predict_local`] applies).
    pub fn newest(&self) -> Option<NumberedInput> {
        self.inputs.last().copied()
    }

    /// Number of buffered (unacked) inputs.
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}

// --- The predicted world (T034/T035) -----------------------------------------

/// Owns the client's **predicted local-ship** simulation: a `bevy_ecs` [`World`]
/// holding only the local ship, advanced by the **shared** fixed-step
/// [`Schedule`] (registered via [`sim::add_fixed_step_systems`], identical to the
/// server's, HINT-003). This is the prediction half of OBJ3; the rendering and
/// interpolation of remotes is wired separately (Phase 6).
pub struct Predictor {
    /// The predicted ECS world (local ship only — remotes are interpolated, not
    /// simulated, per AD-005).
    world: World,
    /// The shared fixed-step gameplay schedule — the SAME systems in the SAME
    /// order as the server, so prediction and authority advance bit-identically.
    schedule: Schedule,
    /// The local ship entity in the predicted world.
    ship: Entity,
}

impl Predictor {
    /// Build a predictor whose predicted world holds a single local ship seeded
    /// to `initial`, stepped by the shared sim at `dt`.
    ///
    /// The world is seeded with the SAME resources and ship components the server
    /// spawns ([`Tuning`], [`FixedDt`], [`HitFeedback`], and a full ship bundle)
    /// so the shared systems behave identically on both ends (Principle II). The
    /// `initial` state lets a test seed the predicted world bit-identically to the
    /// server's (T037).
    pub fn new(initial: ShipInit, dt: f32) -> Self {
        let mut world = World::new();
        world.insert_resource(Tuning::default());
        world.insert_resource(FixedDt(dt));
        world.insert_resource(HitFeedback::default());

        let ship = world
            .spawn((
                Ship,
                ShipIntent::default(),
                Position(initial.pos),
                Velocity(initial.vel),
                Heading(initial.heading),
                AngularVelocity(initial.angular_velocity),
                initial.assist,
            ))
            .id();

        let mut schedule = Schedule::default();
        // The single shared entry point — identical to the server's step
        // (Principle II / HINT-003), so the determinism guarantee holds.
        sim::add_fixed_step_systems(&mut schedule);

        Self {
            world,
            schedule,
            ship,
        }
    }

    /// The local ship's current predicted kinematic + flight state.
    pub fn ship_state(&self) -> ShipState {
        ShipState::read(&self.world, self.ship)
    }

    /// Read-only access to the predicted world (tests/inspection).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The local ship entity in the predicted world.
    pub fn ship(&self) -> Entity {
        self.ship
    }

    /// Apply `input` to the local ship and step the predicted world once via the
    /// shared sim (T034, TR-007). The ship moves **this tick**, with no server
    /// round-trip — the immediate-response prediction path (SC-001).
    ///
    /// Records `input` in `buffer` (capped, TR-027) so reconciliation can replay
    /// it later. Equivalent to [`predict_local`]; provided as a method for the
    /// owned-world ergonomic path.
    pub fn predict(&mut self, buffer: &mut InputBuffer, input: NumberedInput) {
        predict_local(&mut self.world, &mut self.schedule, self.ship, input);
        buffer.push(input);
    }

    /// Reconcile the predicted local ship against an authoritative `snapshot`
    /// (T035, TR-009/016).
    ///
    /// Re-seeds the local ship to the snapshot's authoritative state (the
    /// [`EntityRecord`] whose id is `local_id`, dequantized), drops every input
    /// the snapshot acks (`seq <= acked_input_seq`) from `buffer`, then
    /// **deterministically replays** every remaining unacked input through the
    /// shared sim — reproducing the predicted state on top of the authoritative
    /// base. The authoritative/predicted state is corrected *immediately*; the
    /// rendered offset is smoothed separately ([`smooth_correction`], T036).
    ///
    /// Returns `true` if the snapshot carried the local ship (a reconcile
    /// happened); `false` if the local ship was absent from the snapshot (nothing
    /// to re-seed from — the predicted state is left untouched).
    ///
    /// [`EntityRecord`]: protocol::EntityRecord
    pub fn reconcile(
        &mut self,
        snapshot: &Snapshot,
        local_id: EntityId,
        buffer: &mut InputBuffer,
    ) -> bool {
        reconcile(
            &mut self.world,
            &mut self.schedule,
            self.ship,
            snapshot,
            local_id,
            buffer,
        )
    }
}

/// Seed parameters for the predicted local ship — mirrors the server's spawned
/// ship so the predicted world is bit-identical at `t == 0` (T037).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipInit {
    /// Initial position.
    pub pos: Vec2,
    /// Initial velocity.
    pub vel: Vec2,
    /// Initial heading (radians).
    pub heading: f32,
    /// Initial angular velocity (rad/s).
    pub angular_velocity: f32,
    /// Initial flight-assist mode.
    pub assist: FlightAssist,
}

impl Default for ShipInit {
    /// The default matches the server's `spawn_client_ship` ship pose: at rest at
    /// the origin, heading 0, flight-assist On.
    fn default() -> Self {
        Self {
            pos: Vec2::ZERO,
            vel: Vec2::ZERO,
            heading: 0.0,
            angular_velocity: 0.0,
            assist: FlightAssist::On,
        }
    }
}

/// A snapshot of the local ship's `sim` state, for comparison and reconciliation.
/// Holds the full integrated state so a determinism test can compare exact f32
/// bit patterns (T037) and a convergence test can measure position/velocity
/// residuals (T039).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShipState {
    /// Position in sim units.
    pub pos: Vec2,
    /// Velocity in sim units/s.
    pub vel: Vec2,
    /// Heading in radians.
    pub heading: f32,
    /// Angular velocity in rad/s.
    pub angular_velocity: f32,
}

impl ShipState {
    /// Read the ship's state out of `world`.
    pub fn read(world: &World, ship: Entity) -> Self {
        let pos = world
            .get::<Position>(ship)
            .map(|p| p.0)
            .unwrap_or(Vec2::ZERO);
        let vel = world
            .get::<Velocity>(ship)
            .map(|v| v.0)
            .unwrap_or(Vec2::ZERO);
        let heading = world.get::<Heading>(ship).map(|h| h.0).unwrap_or(0.0);
        let angular_velocity = world
            .get::<AngularVelocity>(ship)
            .map(|a| a.0)
            .unwrap_or(0.0);
        Self {
            pos,
            vel,
            heading,
            angular_velocity,
        }
    }

    /// Bit-identical comparison (epsilon = 0): every f32 must match by exact bit
    /// pattern (T037 / SC-007). NaN-safe (compares raw bits), and distinguishes
    /// `+0.0`/`-0.0` — which is exactly the strictness the determinism guarantee
    /// requires.
    pub fn bit_identical(&self, other: &Self) -> bool {
        self.pos.x.to_bits() == other.pos.x.to_bits()
            && self.pos.y.to_bits() == other.pos.y.to_bits()
            && self.vel.x.to_bits() == other.vel.x.to_bits()
            && self.vel.y.to_bits() == other.vel.y.to_bits()
            && self.heading.to_bits() == other.heading.to_bits()
            && self.angular_velocity.to_bits() == other.angular_velocity.to_bits()
    }
}

/// Apply `input` to `ship` and step the shared sim once (T034, TR-007).
///
/// Writes the numbered input's quantized intent onto the ship's [`sim::ShipIntent`]
/// component, then runs `schedule` (built by [`sim::add_fixed_step_systems`])
/// against `world` exactly once at the world's [`sim::FixedDt`]. This is the
/// per-tick prediction step: the local ship advances immediately, with no server
/// round-trip (the immediate-response path, SC-001).
///
/// The toggle-assist flag is **edge-triggered** and consumed after the step
/// (matching the server's `step_sim`), so a single toggle does not re-fire every
/// replay tick.
pub fn predict_local(
    world: &mut World,
    schedule: &mut Schedule,
    ship: Entity,
    input: NumberedInput,
) {
    if let Some(mut intent) = world.get_mut::<ShipIntent>(ship) {
        *intent = ShipIntent::from(input.intent);
    }
    schedule.run(world);
    // Consume the edge-triggered toggle so it does not re-fire (server parity).
    if let Some(mut intent) = world.get_mut::<ShipIntent>(ship) {
        intent.toggle_assist = false;
    }
}

/// Re-seed `ship` to the authoritative state in `snapshot` and deterministically
/// replay every still-unacked buffered input through the shared sim (T035,
/// TR-009/016). See [`Predictor::reconcile`] for the contract.
pub fn reconcile(
    world: &mut World,
    schedule: &mut Schedule,
    ship: Entity,
    snapshot: &Snapshot,
    local_id: EntityId,
    buffer: &mut InputBuffer,
) -> bool {
    // Find the authoritative record for the local ship in the snapshot.
    let Some(record) = snapshot
        .entities
        .iter()
        .find(|r| r.id == local_id && r.kind == EntityKind::Ship)
    else {
        // The local ship is not in this snapshot — nothing authoritative to
        // re-seed from. Leave the predicted state untouched (do NOT teleport to
        // a guessed state). Still prune acked inputs so the buffer stays bounded.
        buffer.drop_acked(snapshot.acked_input_seq);
        return false;
    };

    // Re-seed the local ship to the authoritative (dequantized) state. Velocity
    // and heading come straight from the record; angular velocity is not carried
    // in the wire record, so it is reconstructed by the replay below (the shared
    // flight model derives omega each tick from the turn input).
    let auth_pos = record.pos.dequantize_pos();
    let auth_vel = record.vel.dequantize_vel();
    let auth_heading = record.heading.dequantize();
    if let Some(mut pos) = world.get_mut::<Position>(ship) {
        pos.0 = auth_pos;
    }
    if let Some(mut vel) = world.get_mut::<Velocity>(ship) {
        vel.0 = auth_vel;
    }
    if let Some(mut heading) = world.get_mut::<Heading>(ship) {
        heading.0 = auth_heading;
    }

    // Drop everything the snapshot acked; only inputs after the ack are replayed.
    buffer.drop_acked(snapshot.acked_input_seq);

    // Deterministically replay the remaining unacked inputs through the shared
    // sim — the SAME schedule the server stepped, so the predicted state lands
    // back where prediction had it (minus the corrected divergence) (TR-009/016).
    let replay: Vec<NumberedInput> = buffer.unacked().to_vec();
    for input in replay {
        predict_local(world, schedule, ship, input);
    }
    true
}

// --- Smoothed render correction (T036, TR-033) -------------------------------

/// Smooths the *rendered* local ship toward its reconciled state without ever
/// snapping (T036, TR-033, OD-001).
///
/// Reconciliation ([`reconcile`]) corrects the authoritative/predicted state
/// **immediately**. To avoid a visible teleport, the renderer does not jump to
/// that state: it carries a decaying `offset` (rendered − authoritative) that is
/// bled out over ≤ [`MAX_SMOOTH_TICKS`] ticks, each tick closing at most
/// [`MAX_SNAP_FRACTION`] of the remaining offset (plus the [`MAX_SNAP_FLOOR`]
/// endgame). The residual is therefore strictly non-increasing and never moves
/// the rendered ship by more than the no-teleport cap in a single tick.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct RenderSmoother {
    /// The current rendered-minus-authoritative offset, in sim units. Decays to
    /// zero; the rendered position is `authoritative + offset`.
    offset: Vec2,
}

impl RenderSmoother {
    /// A smoother with no pending correction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a correction: set the rendered offset to the residual between where
    /// the ship was **previously rendered** and the just-reconciled authoritative
    /// state, so the rendered ship decays from its old pose to the new one rather
    /// than jumping.
    ///
    /// This **composes** a mid-decay correction without discarding the remaining
    /// decay: the caller computes `previously_rendered` as `predicted_pos +
    /// self.offset()`, so it ALREADY contains the in-flight offset. The residual
    /// that keeps the rendered ship exactly where it is — old offset plus any new
    /// divergence — is therefore `previously_rendered − reconciled`, and we **set**
    /// the offset to it. Using `+=` would re-add the in-flight offset (it is
    /// already inside `previously_rendered`), double-counting it; since the live
    /// loop reconciles every tick the offset would then diverge (~1.5×/tick) and
    /// fling the rendered ship off-screen (regression test
    /// `render_smoother_stays_bounded_under_repeated_in_sync_corrections`).
    pub fn observe_correction(&mut self, previously_rendered: Vec2, reconciled: Vec2) {
        self.offset = previously_rendered - reconciled;
    }

    /// Advance the smoothing one tick: shrink the offset by [`smooth_correction`]
    /// and return the new rendered position `authoritative + offset` for this
    /// tick. The rendered ship moves toward the authoritative state by the bounded
    /// step, never the full residual at once (no teleport, TR-033).
    pub fn step(&mut self, authoritative: Vec2) -> Vec2 {
        self.offset = smooth_correction(self.offset);
        authoritative + self.offset
    }

    /// The remaining rendered offset magnitude (0 once fully converged).
    pub fn residual(&self) -> f32 {
        self.offset.length()
    }

    /// The remaining rendered offset vector (rendered = authoritative + offset).
    /// Zero once fully converged.
    pub fn offset(&self) -> Vec2 {
        self.offset
    }
}

/// Shrink a render-correction residual by one smoothing tick (T036, TR-033).
///
/// Closes the larger of [`MAX_SNAP_FRACTION`] of the residual or the
/// [`MAX_SNAP_FLOOR`] absolute step, never more than the residual itself. The
/// returned residual is therefore **strictly smaller** than the input whenever
/// the input is non-zero (non-increasing, no oscillation), and a single tick
/// never moves the rendered ship by more than the no-teleport cap.
///
/// With a 25% fraction the residual decays as `0.75^n`, reaching ~23.7% after 5
/// ticks; the absolute floor then closes the tail so the correction fully
/// resolves within the [`MAX_SMOOTH_TICKS`] window rather than asymptotically.
pub fn smooth_correction(residual: Vec2) -> Vec2 {
    let len = residual.length();
    if len == 0.0 {
        return Vec2::ZERO;
    }
    // The bounded step this tick: a fraction of the residual, or the absolute
    // floor, whichever is larger — but never more than the whole residual.
    let step = (len * MAX_SNAP_FRACTION).max(MAX_SNAP_FLOOR).min(len);
    let remaining = len - step;
    if remaining <= 0.0 {
        Vec2::ZERO
    } else {
        // Scale the residual vector down to the new (smaller) length, preserving
        // direction so the rendered ship slides straight toward the authority.
        residual * (remaining / len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral() -> QuantizedIntent {
        QuantizedIntent {
            forward: 0,
            strafe: 0,
            turn: 0,
            fire: false,
            toggle_assist: false,
        }
    }

    fn forward() -> QuantizedIntent {
        QuantizedIntent {
            forward: 1,
            strafe: 0,
            turn: 0,
            fire: false,
            toggle_assist: false,
        }
    }

    #[test]
    fn input_buffer_caps_at_64_dropping_oldest() {
        let mut buf = InputBuffer::new();
        for seq in 1..=100 {
            buf.push(NumberedInput {
                seq,
                intent: neutral(),
            });
        }
        assert_eq!(buf.len(), InputBuffer::CAP, "buffer bounded at CAP");
        // Oldest dropped: the surviving inputs are the newest 64 (seq 37..=100).
        assert_eq!(buf.unacked().first().unwrap().seq, 100 - 64 + 1);
        assert_eq!(buf.unacked().last().unwrap().seq, 100);
    }

    #[test]
    fn drop_acked_removes_only_acked_seqs() {
        let mut buf = InputBuffer::new();
        for seq in 1..=10 {
            buf.push(NumberedInput {
                seq,
                intent: neutral(),
            });
        }
        buf.drop_acked(4);
        assert_eq!(buf.len(), 6);
        assert_eq!(buf.unacked().first().unwrap().seq, 5);
    }

    #[test]
    fn predict_local_moves_the_ship_immediately() {
        let mut p = Predictor::new(ShipInit::default(), 1.0 / 60.0);
        let mut buf = InputBuffer::new();
        let before = p.ship_state().pos;
        p.predict(
            &mut buf,
            NumberedInput {
                seq: 1,
                intent: forward(),
            },
        );
        let after = p.ship_state().pos;
        assert!(
            after.x > before.x,
            "forward thrust must move the predicted ship along +x this tick"
        );
        assert_eq!(
            buf.len(),
            1,
            "the predicted input is buffered for reconcile"
        );
    }

    #[test]
    fn render_smoother_stays_bounded_under_repeated_in_sync_corrections() {
        // Mirrors the live client loop: a snapshot arrives and reconciles EVERY
        // tick, so `observe_correction` is called every tick with
        // `previously_rendered = authoritative + current_offset`. When prediction
        // matches authority (in sync) the rendered offset must DECAY toward zero,
        // never grow. A `+=` (rather than set) double-counts the in-flight offset
        // and the rendered ship diverges off-screen — this guards that regression.
        let mut s = RenderSmoother::new();
        let auth = Vec2::new(5.0, 0.0);
        // One real divergence to put an offset in flight (initial residual 1.0).
        s.observe_correction(Vec2::new(6.0, 0.0), auth);
        let mut max_seen = 0.0_f32;
        for _ in 0..120 {
            let rendered = s.step(auth); // render frame: decay + read
            max_seen = max_seen.max((rendered - auth).length());
            // Tick: in-sync correction — predicted == authoritative.
            let previously_rendered = auth + s.offset();
            s.observe_correction(previously_rendered, auth);
        }
        assert!(
            s.residual() < RECON_EPS_POS,
            "offset must decay to ~0 under in-sync corrections, got {}",
            s.residual()
        );
        assert!(
            max_seen <= 1.0 + 1e-3,
            "rendered offset must never exceed the initial divergence; peaked at {max_seen}"
        );
    }

    #[test]
    fn smooth_correction_is_non_increasing_and_bounded() {
        let mut residual = Vec2::new(10.0, 0.0);
        let start = residual.length();
        let mut prev = start;
        let mut ticks = 0;
        while residual.length() > 0.0 {
            let before = residual.length();
            residual = smooth_correction(residual);
            let after = residual.length();
            let step = before - after;
            // No single tick may exceed the no-teleport cap.
            assert!(
                step <= before * MAX_SNAP_FRACTION + MAX_SNAP_FLOOR + 1e-6,
                "single-tick correction {step} exceeds MAX_SNAP cap"
            );
            // Strictly non-increasing (no oscillation).
            assert!(after <= prev, "residual must not grow: {after} > {prev}");
            prev = after;
            ticks += 1;
            assert!(ticks < 100, "smoothing must terminate");
        }
        assert!(ticks > 1, "a real correction is spread over multiple ticks");
    }
}
