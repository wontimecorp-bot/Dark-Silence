//! Shared simulation crate — the single source of gameplay truth.
//!
//! Both the authoritative server (Tier 0 per-tick integration) and the client
//! (prediction) run this exact code, and the transit layer (Tier 1) uses its
//! closed-form evaluator. The load-bearing invariant of the whole tiered design
//! lives in [`motion`]: the per-tick integrator and the analytic evaluator must
//! agree, so an entity demoted to a closed-form trajectory and later promoted
//! back into the live sim reappears exactly where the math said it would.
//!
//! E002 grows this crate with the single-player flight & combat gameplay —
//! flight dynamics, swept collision, weapon, combat, and seek AI — all as
//! headless `bevy_ecs` systems so the Bevy client stays a thin shell (ADR-0013).

// ECS systems take tuple queries with `With`/`Without` filters; that idiom trips
// `clippy::type_complexity` with no readability win, so allow it crate-wide.
#![allow(clippy::type_complexity)]

pub mod ai;
pub mod clock;
pub mod collision;
pub mod combat;
pub mod components;
pub mod damage;
pub mod fitting;
pub mod flight;
pub mod intent;
pub mod motion;
pub mod physics;
pub mod tuning;
pub mod weapon;

pub use clock::FixedDt;
pub use combat::HitFeedback;
pub use components::{
    AngularVelocity, CollisionRadius, Damage, FlightAssist, Heading, Health, Lifetime, Position,
    PrevPosition, Projectile, ProjectileOwner, Ship, Target, TargetKind, Velocity, Weapon,
};
pub use fitting::{
    build_layout, cell_map, derive_ship_stats, hardpoint_arc, load_preset, module_at,
    preview_stats, recompute_ship_stats_system, resolve_hit, save_preset, CellOccupant, FitLayout,
    FitPreset, HitResolution, PresetId, ShipStats, WeaponProfile,
};
pub use intent::ShipIntent;
pub use motion::{analytic, integrate, simulate, BodyState};
pub use physics::{Physics, RapierPhysics, SweptHit};
pub use tuning::Tuning;

use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule};

/// Register the shared fixed-step gameplay systems, in their **canonical order**,
/// onto a caller-owned [`Schedule`] (Principle II, HINT-003).
///
/// This is the single entry point both the authoritative server (E003) and the
/// client must use to advance the sim, so the two run **bit-identical** logic in
/// the same order — the determinism guarantee the reconciliation/prediction layer
/// (and the Phase 5 determinism test) depends on. It is purely additive: it
/// registers the existing `pub fn` gameplay systems unchanged (it does not modify
/// any system's behavior), `.chain()`ed so the order is deterministic.
///
/// The canonical order mirrors the client's `FixedUpdate` pipeline, minus the
/// client-only render-capture system (which is not gameplay):
///
/// 1. [`ai::seek_system`]
/// 2. [`flight::ship_motion_system`]
/// 3. [`weapon::weapon_fire_system`]
/// 4. [`weapon::projectile_step_system`]
/// 5. [`collision::collision_detect_system`]
/// 6. [`collision::ram_collision_system`]
/// 7. [`combat::destruction_system`]
/// 8. [`combat::feedback_decay_system`]
///
/// The caller is responsible for inserting the resources the systems read
/// ([`FixedDt`], [`Tuning`], [`HitFeedback`]) into the `World` before running
/// the schedule, and for attaching a [`ShipIntent`] **component** to every
/// piloted ship (intent is per-entity, not a global resource — the server drives
/// N independently-controlled ships in one shared step).
pub fn add_fixed_step_systems(schedule: &mut Schedule) {
    schedule.add_systems(
        (
            ai::seek_system,
            flight::ship_motion_system,
            weapon::weapon_fire_system,
            weapon::projectile_step_system,
            collision::collision_detect_system,
            collision::ram_collision_system,
            combat::destruction_system,
            combat::feedback_decay_system,
        )
            .chain(),
    );
}
