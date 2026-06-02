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
pub mod flight;
pub mod intent;
pub mod motion;
pub mod physics;
pub mod tuning;
pub mod weapon;

pub use clock::FixedDt;
pub use combat::HitFeedback;
pub use components::{
    CollisionRadius, Damage, FlightAssist, Heading, Health, Lifetime, Position, PrevPosition,
    Projectile, ProjectileOwner, Ship, Target, TargetKind, Velocity, Weapon,
};
pub use intent::ShipIntent;
pub use motion::{analytic, integrate, simulate, BodyState};
pub use physics::{Physics, RapierPhysics, SweptHit};
pub use tuning::Tuning;
