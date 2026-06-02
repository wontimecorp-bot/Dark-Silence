//! The fixed simulation timestep, as a resource the `sim` systems read.
//!
//! Keeping `dt` in a `sim`-owned resource (rather than reading Bevy's
//! `Time<Fixed>`) keeps the `sim` crate free of `bevy_time` and makes the
//! headless integration tests trivially deterministic: they just insert a
//! `FixedDt`. The client sets this to match its `Time<Fixed>` rate (FR-016).

use bevy_ecs::prelude::Resource;

/// The fixed simulation timestep in seconds. Default is 60 Hz (`1/60 s`).
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct FixedDt(pub f32);

impl Default for FixedDt {
    fn default() -> Self {
        Self(1.0 / 60.0)
    }
}
