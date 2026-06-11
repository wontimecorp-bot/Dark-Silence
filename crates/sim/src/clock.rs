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

/// The current authoritative fixed-step tick — monotonic from 0, published by
/// the server (`ServerApp` mirrors its tick counter into this resource before
/// each schedule step).
///
/// The AI substrate (00008-ship-ai) reads it for tick-stamped bookkeeping —
/// `AoiTier.since_tick` hysteresis (T005) and, later, scheduler
/// `last_think_tick` cadences. Pure data: nothing writes gameplay state from
/// it, and every reader is `resource_exists`-gated, so worlds that never
/// insert it (client prediction, legacy sim tests) simply skip those systems.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CurrentTick(pub u64);
