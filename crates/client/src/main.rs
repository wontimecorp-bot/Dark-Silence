//! Dark Silence game client binary — a one-line shell over the `client` library.
//!
//! All client logic (rendering, input, prediction, reconciliation, scene setup)
//! lives in the library (`lib.rs`) so the integration tests under `tests/` can
//! drive the netcode layer headlessly. This binary just launches the windowed
//! app via [`client::run`].

use bevy::prelude::AppExit;

fn main() -> AppExit {
    client::run()
}
