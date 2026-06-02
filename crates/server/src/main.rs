//! Headless authoritative server binary (E003, OBJ1).
//!
//! A thin entry point: the implementation lives in the `server` library
//! ([`server::ServerApp`]) so the integration tests and this binary share one
//! code path. A real deployment swaps in the renet transport (Phase 4) and adds
//! wall-clock pacing; this `main` runs the loopback server with the announced
//! defaults so the crate produces a runnable binary.

use server::{RateConfig, ServerApp};

fn main() {
    let rates = RateConfig::default();
    // T019: enforce the rate invariant at start (defensive — `ServerApp::new`
    // also enforces it, but failing here gives a clean operator-facing message).
    if let Err(e) = rates.validate() {
        eprintln!("invalid server rate config: {e}");
        std::process::exit(1);
    }
    let (mut server, _client) = ServerApp::loopback();
    server.run();
}
