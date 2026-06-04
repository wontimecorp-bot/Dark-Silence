//! T037 {TR-034,TR-016,TR-032} — bit-identical determinism (SC-007).
//!
//! The load-bearing guarantee of the whole prediction/reconciliation design: the
//! **same shared `sim`** runs on the authoritative server and on the client's
//! predicted world, so for an identical seed + identical numbered input stream
//! the two advance **bit-identically** (epsilon = 0) over the deterministic
//! in-memory loopback harness (TR-016 / TR-032(a)). This re-exercises the E001
//! bit-identical primitive at the E003 split — it does not re-prove f32
//! reproducibility, it asserts the *same* `sim` code path drives both ends.
//!
//! Both sides are seeded to the IDENTICAL initial ship (the server's spawned
//! ship and a standalone predicted world built the same way) and fed the SAME
//! `seq`-ordered [`ClientInput`] stream. After a fixed tick count their raw
//! `sim` state (exact f32 bit patterns of position/velocity/heading/omega) must
//! match bit-for-bit. The seed, initial entities, input stream, and tick count
//! are all fixed so the comparison is reproducible run-to-run.
//!
//! The comparison reads the server's **raw** authoritative world (not its
//! quantized snapshot, which is lossy by design) so the test is a genuine
//! bit-for-bit check (epsilon = 0), per the task.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;
use glam::Vec2;
use protocol::{
    ClientInput, Connect, ConnectionId, EntityId, Message, NetTransport, QuantizedIntent,
    CLIENT_TOKEN_BYTES,
};
use server::{RateConfig, ServerApp, PROTOCOL_VERSION};
use sim::components::{AngularVelocity, FlightAssist, Heading, Health, Position, Ship, Velocity};
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

fn addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30_000)
}

fn connect_msg() -> Message {
    Message::Connect(Connect {
        protocol_version: PROTOCOL_VERSION,
        client_token: [0u8; CLIENT_TOKEN_BYTES],
    })
}

/// The raw `sim` state of a ship, compared by **exact f32 bit pattern**
/// (epsilon = 0). This mirrors the client's `ShipState::bit_identical` so the two
/// crates agree on what "bit-identical" means (SC-007).
#[derive(Clone, Copy, Debug)]
struct RawShipState {
    pos: Vec2,
    vel: Vec2,
    heading: f32,
    angular_velocity: f32,
}

impl RawShipState {
    fn read(world: &World, ship: Entity) -> Self {
        Self {
            pos: world.get::<Position>(ship).unwrap().0,
            vel: world.get::<Velocity>(ship).unwrap().0,
            heading: world.get::<Heading>(ship).unwrap().0,
            angular_velocity: world.get::<AngularVelocity>(ship).unwrap().0,
        }
    }

    /// Exact bit-pattern equality (NaN-safe, sign-of-zero sensitive) — the
    /// epsilon = 0 guarantee SC-007 demands.
    fn bit_identical(&self, other: &Self) -> bool {
        self.pos.x.to_bits() == other.pos.x.to_bits()
            && self.pos.y.to_bits() == other.pos.y.to_bits()
            && self.vel.x.to_bits() == other.vel.x.to_bits()
            && self.vel.y.to_bits() == other.vel.y.to_bits()
            && self.heading.to_bits() == other.heading.to_bits()
            && self.angular_velocity.to_bits() == other.angular_velocity.to_bits()
    }
}

/// Build a standalone **client-predicted** world holding a single ship seeded
/// IDENTICALLY to the server's spawned ship, stepped by the SAME shared schedule
/// at the SAME `dt`. This is exactly what `client::prediction::Predictor` does;
/// it is reconstructed here so the determinism test has no cross-crate
/// dev-dependency cycle, while still driving the genuine shared `sim` (the same
/// `sim::add_fixed_step_systems` the server uses).
fn build_predicted_world(dt: f32) -> (World, Schedule, Entity) {
    let mut world = World::new();
    world.insert_resource(Tuning::default());
    world.insert_resource(FixedDt(dt));
    world.insert_resource(HitFeedback::default());

    // Same bundle the server's `spawn_client_ship` uses (minus the runtime-local
    // Weapon/CollisionRadius, which do not affect the kinematic state compared).
    let ship = world
        .spawn((
            Ship,
            ShipIntent::default(),
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            Health(100.0),
            FlightAssist::On,
        ))
        .id();

    let mut schedule = Schedule::default();
    // The single shared entry point — IDENTICAL to the server's step.
    sim::add_fixed_step_systems(&mut schedule);

    (world, schedule, ship)
}

/// Step the predicted world once for `intent` — mirrors the server's `step_sim`:
/// write the intent onto the ship's component, run the shared schedule, then
/// consume the edge-triggered toggle.
fn predict_step(world: &mut World, schedule: &mut Schedule, ship: Entity, intent: QuantizedIntent) {
    if let Some(mut c) = world.get_mut::<ShipIntent>(ship) {
        *c = ShipIntent::from(intent);
    }
    schedule.run(world);
    if let Some(mut c) = world.get_mut::<ShipIntent>(ship) {
        c.toggle_assist = false;
    }
}

/// The FIXED numbered input stream both sides are driven by — a scripted mix of
/// thrust, strafe, and turn so the comparison exercises the full flight model
/// (translation, angular inertia, the shared power budget), not just a trivial
/// coast. Reproducible run-to-run.
fn fixed_input_stream() -> Vec<QuantizedIntent> {
    let mk = |forward: i8, strafe: i8, turn: i8| QuantizedIntent {
        forward,
        strafe,
        turn,
        fire: false,
        toggle_assist: false,
        afterburner: false,
    };
    let mut stream = Vec::new();
    // 60 ticks of a deterministic, varied manoeuvre.
    for i in 0..60u32 {
        let intent = match i % 6 {
            0 => mk(1, 0, 0),   // thrust forward
            1 => mk(1, 0, 1),   // forward + turn left (power-share kicks in)
            2 => mk(0, 1, 1),   // strafe left + turn
            3 => mk(-1, 0, -1), // reverse + turn right
            4 => mk(0, -1, 0),  // strafe right
            _ => mk(1, 1, -1),  // forward + strafe + turn right
        };
        stream.push(intent);
    }
    stream
}

/// Drain the loopback client's inbox and return its assigned ship [`EntityId`].
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

#[test]
fn server_and_predicted_sim_are_bit_identical_over_fixed_input_stream() {
    // The server runs at its default 30 Hz; the predicted world MUST use the same
    // fixed dt to advance bit-identically (TR-016).
    let rates = RateConfig::default();
    let dt = 1.0 / rates.tick_rate_hz as f32;

    let (mut server, mut client) = ServerApp::loopback();
    let conn = client.connect(addr());
    let local_id = handshake(&mut server, &mut client, conn);

    // Standalone predicted world, seeded IDENTICALLY to the server's ship.
    let (mut pred_world, mut pred_schedule, pred_ship) = build_predicted_world(dt);

    // The server's ship entity (raw authoritative state lives here). Looked up by
    // the wire id the client learned — the loopback `conn` is the client-side
    // handle, not the server-side one the links are keyed by.
    let server_ship = server
        .ship_entity_for(local_id)
        .expect("the accepted client owns an authoritative ship");

    // Sanity: both start bit-identical (same seed).
    let server_start = RawShipState::read(server.world(), server_ship);
    let pred_start = RawShipState::read(&pred_world, pred_ship);
    assert!(
        server_start.bit_identical(&pred_start),
        "seeds must start bit-identical: server={server_start:?} pred={pred_start:?}"
    );

    // Feed the IDENTICAL seq-ordered input stream to BOTH ends, one tick each.
    let stream = fixed_input_stream();
    let tick_count = stream.len();
    for (i, intent) in stream.iter().enumerate() {
        let seq = (i + 1) as u32;
        let tick = i as u32;

        // Server: send the numbered input, then advance exactly one tick. The
        // server applies the newest input from the (single-entry) tail.
        client.send_unreliable(
            conn,
            &Message::ClientInput(ClientInput::new(seq, tick, vec![*intent])),
        );
        server.tick();
        // Drain any broadcast snapshots so the client inbox does not grow (we
        // compare the server's RAW world, not the lossy snapshot).
        let _ = client.recv(conn);

        // Predicted: apply the SAME intent and step the SAME shared schedule once.
        predict_step(&mut pred_world, &mut pred_schedule, pred_ship, *intent);

        // After every tick the two raw states must remain bit-identical.
        let s = RawShipState::read(server.world(), server_ship);
        let p = RawShipState::read(&pred_world, pred_ship);
        assert!(
            s.bit_identical(&p),
            "server and predicted sim diverged at tick {i} (seq {seq}): \
             server={s:?} predicted={p:?}"
        );
    }

    // Final state: bit-identical after the full fixed tick count, AND non-trivial
    // (the ship actually moved, so the test is not passing on a no-op).
    let final_server = RawShipState::read(server.world(), server_ship);
    let final_pred = RawShipState::read(&pred_world, pred_ship);
    assert!(
        final_server.bit_identical(&final_pred),
        "server and predicted sim must be bit-identical after {tick_count} ticks: \
         server={final_server:?} predicted={final_pred:?}"
    );
    assert!(
        final_server.pos.length() > 1.0,
        "the scripted manoeuvre must actually move the ship (no-op would pass \
         trivially): final pos={:?}",
        final_server.pos
    );

    // The client id was learned and the ship is addressable by it (handshake path
    // exercised end to end).
    assert!(server.ship_entity_for(local_id).is_some());
}
