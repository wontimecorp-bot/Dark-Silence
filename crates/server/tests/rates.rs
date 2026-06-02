//! T024 — [COMPLETES TR-044] rate-config invariant + announced defaults.
//!
//! Two facts under test:
//! 1. A server configured with `snapshot_rate >= tick_rate` is rejected at start
//!    (the enforced invariant fires — it is not a note).
//! 2. The `ConnectAccepted` a client receives announces the 30/20/100 defaults,
//!    which the client adopts (no negotiation).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use protocol::{Connect, Message, NetTransport, CLIENT_TOKEN_BYTES};
use server::{RateConfig, RateConfigError, ServerApp, PROTOCOL_VERSION};

fn addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000)
}

#[test]
fn snapshot_rate_at_or_above_tick_rate_is_rejected_at_start() {
    // Equal rates: snapshots would keep pace with the sim — rejected.
    let equal = RateConfig {
        tick_rate_hz: 30,
        snapshot_rate_hz: 30,
        interp_delay_ms: 100,
    };
    assert_eq!(
        equal.validate(),
        Err(RateConfigError::SnapshotRateNotBelowTickRate {
            snapshot_rate_hz: 30,
            tick_rate_hz: 30,
        })
    );

    // Above: snapshots would outpace the sim — rejected.
    let above = RateConfig {
        tick_rate_hz: 30,
        snapshot_rate_hz: 31,
        interp_delay_ms: 100,
    };
    assert!(above.validate().is_err());

    // The invariant fires through the actual server construction path, too:
    // building a server with an invalid config returns the error, not an app.
    let built = ServerApp::loopback_with_rates(above);
    assert!(
        matches!(
            built,
            Err(RateConfigError::SnapshotRateNotBelowTickRate { .. })
        ),
        "ServerApp must refuse to start when snapshot_rate >= tick_rate"
    );

    // A valid config (snapshot strictly below tick) constructs fine.
    assert!(RateConfig::default().validate().is_ok());
    assert!(ServerApp::loopback_with_rates(RateConfig::default()).is_ok());
}

#[test]
fn connect_accepted_announces_30_20_100_defaults() {
    let defaults = RateConfig::default();
    assert_eq!(defaults.tick_rate_hz, 30);
    assert_eq!(defaults.snapshot_rate_hz, 20);
    assert_eq!(defaults.interp_delay_ms, 100);
    assert!(
        defaults.snapshot_rate_hz < defaults.tick_rate_hz,
        "defaults satisfy the start invariant"
    );

    // A connecting client adopts the announced rates verbatim (no negotiation):
    // the `ConnectAccepted` carries exactly the server defaults.
    let (mut server, mut client) = ServerApp::loopback();
    let conn = client.connect(addr());
    client.send_reliable(
        conn,
        &Message::Connect(Connect {
            protocol_version: PROTOCOL_VERSION,
            client_token: [0u8; CLIENT_TOKEN_BYTES],
        }),
    );
    server.tick();

    let accepted = client
        .recv(conn)
        .into_iter()
        .find_map(|m| match m {
            Message::ConnectAccepted(a) => Some(a),
            _ => None,
        })
        .expect("client receives ConnectAccepted");

    assert_eq!(accepted.tick_rate_hz, 30, "announced tick rate");
    assert_eq!(accepted.snapshot_rate_hz, 20, "announced snapshot rate");
    assert_eq!(accepted.interp_delay_ms, 100, "announced interp delay");
}
