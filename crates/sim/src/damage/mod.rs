//! Damage & destruction — the unified, data-driven typed-damage domain (E007,
//! ADR-0008/0012).
//!
//! All damage/penetration/destruction/severing/salvage logic lives here in the
//! shared `sim` crate (Principle II) so the authoritative server resolves combat
//! on the exact code path the client predicts on. This module is the entry point;
//! it re-exports the public surface as the submodules grow across phases.
//!
//! The model is the ADR-0008 unified stack: typed [`Channel`]s of damage flow
//! through an ordered [`DefenseLayer`] stack (Shields → Armor → Hull → Systems),
//! mitigated at each layer by a data-driven [`ResistanceMatrix`] (content, not
//! code — FR-022). The armor gate runs the angle/penetration math
//! ([`resolve_penetration`]) and routes surviving damage to the module behind the
//! entry point. Destruction is at section/module granularity now and
//! **cell-upgrade-ready** (a later content/resolution upgrade, not a refactor).
//!
//! Derive discipline matches the rest of `sim` (`components.rs`, the E006 fitting
//! domain): `Serialize`/`Deserialize` is present as a **seam** for replication
//! (E003) and persistence (E004) — these types are not serialized or stored this
//! epic (data-model.md serde note). Value-semantics derives give round-trip
//! equality.
//!
//! Current surface (Phase 1 Setup + Phase 2 Foundational substrate):
//! - [`event`] — the typed [`DamageEvent`] packet + the [`Channel`] axis (FR-001).
//! - [`resist`] — the [`DefenseLayer`] stack + the [`ResistanceMatrix`] resource
//!   and its [`layer_resist`] lookup (FR-004/022).
//! - [`content`] — the data-driven tuning content: the seed matrix
//!   ([`default_resistance_matrix`]) + the penetration/shield/stat-scaling config
//!   resources (FR-004/005/006/007/008/010/012/022).
//! - [`penetration`] — the armor-angle gate [`resolve_penetration`] →
//!   [`PenetrationResult`] (FR-005/006/007/008).
//! - [`layers`] — the per-ship defense-layer state components ([`Shields`],
//!   [`SectionArmor`], [`HullStructure`], [`SectionHealth`], [`ArmorFacet`],
//!   [`DamageContext`]) **plus** the US1 traversal: the entry-point resolution
//!   ([`resolve_entry_point`]/[`route_behind`]) and the full
//!   [`apply_damage`] pipeline ([`DamageOutcome`]/[`HitKind`]) — Shields → Armor →
//!   Hull → Systems (FR-002/003/004/009/011).
//! - [`shields`] — shield absorption ([`shield_absorb`]) + the powered regen/decay
//!   system ([`shield_regen_system`]/[`regen_shield`]) (US1, FR-010).
//!
//! Phase 5 (US3) adds destruction + connectivity severing:
//! - [`destruction`] — the event-driven destruction worker
//!   ([`on_section_destroyed`], INV-D08) that removes a destroyed section's cells
//!   and triggers the connectivity check (US3, FR-014/017).
//! - [`sever`] — the destruction-time connectivity flood-fill
//!   ([`connected_region`]) + the COM-momentum-inheriting chunk split
//!   ([`sever_chunk`], INV-D07) + the wreck types ([`Wreck`]/[`WreckChunk`]/
//!   [`WreckOrigin`]) and the core-cell convention ([`core_cell`]) (US3, FR-015/016).
//!
//! Phase 6 (US4) adds intact-vs-scrap wreck salvage:
//! - [`salvage`] — the [`SalvageOutcome`] loot shape + the intact-vs-scrap decision
//!   ([`intact_threshold`]/[`salvage::salvage_layout`], INV-D12), the read accessor
//!   [`salvage`], and the single-resolution claim ([`Wreck::claim`]) — over the
//!   data-driven [`SalvageConfig`] (FR-018/019/020, INV-D09/D10/D12). The wreck spawn
//!   sites ([`sever_chunk`]/[`destruction::on_section_destroyed`]) populate the
//!   persistent `Wreck.contents` at spawn.

pub mod content;
pub mod destruction;
pub mod event;
pub mod layers;
pub mod penetration;
pub mod resist;
pub mod salvage;
pub mod sever;
pub mod shields;

pub use content::{
    default_resistance_matrix, ArmorMaterial, PenetrationConfig, SalvageConfig, ShieldConfig,
    StatScalingConfig,
};
pub use destruction::on_section_destroyed;
pub use event::{Channel, DamageEvent};
pub use layers::{
    apply_damage, resolve_entry_point, route_behind, ArmorFacet, DamageContext, DamageOutcome,
    HitKind, HullStructure, SectionArmor, SectionHealth, Shields,
};
pub use penetration::{resolve_penetration, PenetrationResult};
pub use resist::{layer_resist, DefenseLayer, ResistanceMatrix};
pub use salvage::{intact_threshold, salvage, salvage_layout, SalvageOutcome};
pub use sever::{connected_region, core_cell, sever_chunk, Wreck, WreckChunk, WreckOrigin};
pub use shields::{regen_shield, shield_absorb, shield_regen_system};
