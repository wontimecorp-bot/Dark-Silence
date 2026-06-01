//! Shared simulation crate — the single source of gameplay truth.
//!
//! Both the authoritative server (Tier 0 per-tick integration) and the client
//! (prediction) run this exact code, and the transit layer (Tier 1) uses its
//! closed-form evaluator. The load-bearing invariant of the whole tiered design
//! lives in [`motion`]: the per-tick integrator and the analytic evaluator must
//! agree, so an entity demoted to a closed-form trajectory and later promoted
//! back into the live sim reappears exactly where the math said it would.

pub mod motion;

pub use motion::{analytic, integrate, simulate, BodyState};
