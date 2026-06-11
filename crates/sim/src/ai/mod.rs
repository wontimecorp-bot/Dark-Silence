//! Ship AI (E011, 00008-ship-ai): tiered autonomous behaviors at scale.
//!
//! Module root for the new AI substrate — utility-FSM brains ([`brain`]),
//! context-map steering ([`steering`]), perception + faction sensor networks
//! ([`perception`]), squad/wing command ([`squad`]), AOI sim-LOD tiers
//! ([`lod`]), and the live-editable [`AiTuning`] resource ([`tuning`]). All new
//! systems are additive and `ScenarioActive`-gated (TR-016), registered in
//! `add_fixed_step_systems`; the determinism/golden worlds never run them.
//!
//! This file ALSO retains the legacy seeking-target AI byte-frozen (AD-005,
//! HINT-005): a pure steering helper plus the fixed-step target-motion system.
//! Seekers thrust toward the player; asteroids drift at constant velocity;
//! dummies stay put. Observable behaviour (a thrust vector pointing at the
//! target) is what the spec requires (CHK013). The golden `demo_enemies_smoke`
//! depends on it; the new substrate runs parallel + gated, never through it.

pub mod brain;
pub mod command;
pub mod ident;
pub mod lod;
pub mod perception;
pub mod role;
pub mod squad;
pub mod steering;
pub mod strategy;
pub mod tuning;

#[cfg(feature = "ai_debug")]
pub use brain::debug_capture::{AiDebugCapture, AimDrive, FireReason};
pub use brain::{
    ai_execute_system, ai_think_system, archetype_refresh_system, cadence_for_tier,
    classify_archetype, default_combat_stance, default_movement_profile, hull_fraction,
    primary_fire_group, ram_utility, score_behavior, select_behavior, standoff_distance,
    weapon_range, AiBrain, AiEvent, Behavior, CombatStance, FitArchetype, MovementProfile,
    RethinkQueue,
};
pub use command::{OrderKind, PlayerOrder};
pub use ident::{ai_despawn_sweep_system, phase_bucket, AiIdAllocator, AiStableId};
pub use lod::{
    classify_aoi_system, far_hostile_scan_system, glide_collapse_system, glide_motion_system,
    AoiTier, GlideState, Gliding, HostileContact, PlayerShip, Tier,
};
pub use perception::{
    faction_key, perception_scan_system, scan_cadence_for_tier, sensor_network_system, Contact,
    ContactList, LinkState, NetworkComponent, SensorNetworks,
};
pub use role::{
    role_trigger_system, sweep_route, Posture, RoleGoal, ScenarioRole, FIRED_UPON_WINDOW_TICKS,
};
pub use squad::{spawn_squad, squad_think_system, FormationDef, Squad, SquadOrder};
pub use strategy::{
    strategic_plan_system, wing_plan_system, Objective, SquadObjective, WingObjective,
};
pub use tuning::AiTuning;

use crate::clock::FixedDt;
use crate::components::{Position, Ship, Target, TargetKind, Velocity};
use crate::motion::{integrate, BodyState};
use crate::tuning::Tuning;
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Acceleration that steers `seeker` toward `target` at magnitude `thrust`.
/// Zero when the two coincide (avoids normalizing a zero vector to NaN).
pub fn seek_accel(seeker: Vec2, target: Vec2, thrust: f32) -> Vec2 {
    let d = target - seeker;
    let len = d.length();
    if len > f32::EPSILON {
        d / len * thrust
    } else {
        Vec2::ZERO
    }
}

/// Fixed-step motion for all targets (FR-008/FR-012). Seekers accelerate toward
/// the player; asteroids and dummies receive zero acceleration, so an asteroid
/// drifts on its constant velocity and a dummy (zero velocity) stays put. All
/// reuse the E001 `integrate` keystone.
pub fn seek_system(
    tuning: Res<Tuning>,
    dt: Res<FixedDt>,
    ship_q: Query<&Position, With<Ship>>,
    mut targets: Query<(&mut Position, &mut Velocity, &TargetKind), (With<Target>, Without<Ship>)>,
) {
    let dt = dt.0;
    let player = ship_q.iter().next().map(|p| p.0);
    for (mut pos, mut vel, kind) in &mut targets {
        // The mining transport owns its FULL Newtonian integration in `mining_transport_system`
        // (Refinement 3), so this generic Target integrator must not also advance it. Transports
        // exist only in the windowed mining scenario, so this is a byte-identical no-op everywhere
        // else (determinism / botkit / demo worlds spawn no Transport-kind targets).
        if matches!(*kind, TargetKind::Transport) {
            continue;
        }
        let accel = match (*kind, player) {
            (TargetKind::Seeker, Some(player_pos)) => {
                seek_accel(pos.0, player_pos, tuning.thrust_force / tuning.mass)
            }
            _ => Vec2::ZERO,
        };
        let stepped = integrate(BodyState::new(pos.0, vel.0), accel, dt);
        pos.0 = stepped.pos;
        vel.0 = stepped.vel;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeks_toward_target() {
        let a = seek_accel(Vec2::ZERO, Vec2::new(10.0, 0.0), 5.0);
        assert!((a - Vec2::new(5.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn coincident_is_zero_not_nan() {
        let p = Vec2::new(1.0, 1.0);
        assert_eq!(seek_accel(p, p, 5.0), Vec2::ZERO);
    }
}
