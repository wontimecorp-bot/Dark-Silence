//! Dark Silence game client — E002 single-player flight & combat slice.
//!
//! A **thin Bevy shell** over the shared `sim` crate (ADR-0013): rendering,
//! keyboard input, the HUD, and scene setup only. All gameplay state and logic
//! (motion, collision, weapon, combat, AI) live in `sim` as headless
//! `bevy_ecs` systems; this client advances them in `FixedUpdate`
//! (`Time<Fixed>`, 60 Hz) decoupled from rendering, and interpolates the
//! rendered `Transform`s between fixed steps (FR-004, Principle II/VII).

// Bevy systems use tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

mod camera;
mod hud;
mod input;
mod render_sync;
mod scene;

use bevy::prelude::*;
use sim::{FixedDt, HitFeedback, ShipIntent, Tuning};

/// The fixed simulation tick rate (FR-016): 60 Hz, decoupled from render FPS.
const TICK_HZ: f64 = 60.0;

fn main() -> AppExit {
    App::new()
        .add_plugins(DefaultPlugins)
        // Fixed-step clock for the sim, and the matching dt the sim systems read.
        .insert_resource(Time::<Fixed>::from_hz(TICK_HZ))
        .insert_resource(FixedDt((1.0 / TICK_HZ) as f32))
        .insert_resource(Tuning::default())
        .insert_resource(ShipIntent::default())
        .insert_resource(HitFeedback::default())
        .add_systems(
            Startup,
            (scene::setup_scene, camera::setup_camera, hud::setup_hud),
        )
        // Input runs before the fixed step so intents apply the same frame.
        .add_systems(PreUpdate, (input::read_input, input::toggle_assist))
        // The authoritative gameplay pipeline, in order, at a fixed timestep.
        .add_systems(
            FixedUpdate,
            (
                sim::ai::seek_system,
                sim::flight::ship_motion_system,
                sim::weapon::weapon_fire_system,
                sim::weapon::projectile_step_system,
                sim::collision::collision_detect_system,
                sim::collision::ram_collision_system,
                sim::combat::destruction_system,
                sim::combat::feedback_decay_system,
                render_sync::capture_sim_state,
            )
                .chain(),
        )
        // Per-frame rendering: attach projectile visuals, interpolate, follow.
        .add_systems(
            Update,
            (
                (
                    render_sync::add_projectile_visuals,
                    render_sync::interpolate_transforms,
                    render_sync::update_aim_pip,
                    camera::follow_camera,
                )
                    .chain(),
                camera::zoom_camera,
                hud::update_hud,
            ),
        )
        .run()
}
