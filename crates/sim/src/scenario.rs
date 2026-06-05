//! Scenario gating for the first real game mode (the 2-faction asteroid-mining skirmish).
//!
//! [`ScenarioActive`] is a marker **resource** the windowed client's embedded server inserts via
//! `ServerApp::spawn_scenario`. The headless determinism / botkit / unit-test worlds never call
//! `spawn_scenario`, so the resource is absent there and every scenario-gameplay system
//! (`run_if(resource_exists::<ScenarioActive>)` — mining transport AI, turrets, respawn) is skipped
//! → those worlds step **bit-identically**. Scenario systems also only ever touch scenario-only
//! components (`MiningTransport`, `Turret`, …), so even without the gate they would be empty-query
//! no-ops; the gate is the belt-and-suspenders guard documented in the plan's determinism doctrine.

use bevy_ecs::prelude::Resource;
use glam::Vec2;

use crate::components::Faction;

/// Present iff a real scenario world is live (inserted by `ServerApp::spawn_scenario`). Gates the
/// scenario-gameplay systems so headless/test worlds (which never spawn a scenario) stay
/// bit-identical.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScenarioActive;

/// Per-faction player spawn points (Phase 5) — where an auto-joining human spawns (near their home
/// refinery outpost). A world resource set by `ServerApp::spawn_scenario` for the mining skirmish;
/// absent in Sandbox (so the player stays unfactioned at the origin there).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct FactionSpawns {
    pub red: Vec2,
    pub blue: Vec2,
}

impl FactionSpawns {
    /// The spawn point for `faction`.
    pub fn for_faction(&self, faction: Faction) -> Vec2 {
        match faction {
            Faction::Red => self.red,
            Faction::Blue => self.blue,
        }
    }
}
