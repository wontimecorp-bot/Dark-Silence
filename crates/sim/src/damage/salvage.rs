//! Intact-vs-scrap wreck salvage (US4, Phase 6, FR-018/019/020).
//!
//! A wreck (a destroyed ship or a severed chunk) yields loot per residual module:
//! a **clean sever** (the module's own `health` at/above the intact threshold, its
//! surrounding structure severed away rather than penetrated through) yields an
//! [`IntactModule`](SalvageOutcome::IntactModule) — re-equippable, operational
//! (FR-018); a **destroyed / penetrated-through** module (health below the
//! threshold, in the limit `0`) yields [`Scrap`](SalvageOutcome::Scrap) (FR-019).
//! An **over-killed** wreck (no module cells survived) still yields ≥ a scrap floor
//! so it is never empty loot (INV-D09).
//!
//! **Compute-once, read-many**: the layout→contents decision is made exactly once,
//! at the moment the wreck spawns ([`salvage_layout`] over the residual
//! [`FitLayout`]), and stored on the [`Wreck`](super::sever::Wreck)`.contents`. The
//! public [`salvage`] accessor is the **read surface** E013 consumes — it returns
//! that precomputed list, it does **not** re-decide. Claiming is single-resolution
//! ([`Wreck::claim`](super::sever::Wreck::claim), INV-D10): the loot is handed out
//! exactly once. Salvage **reads** E006 module health/`health_max`; it runs no
//! combat (server-authoritative: the server emits/resolves wrecks, E013 prices them
//! — pricing is not this surface).
//!
//! All salvage balance is content, not code (FR-022): the clean-sever boundary
//! (INV-D12), the per-mass scrap conversion, and the over-kill floor (INV-D09) live
//! on [`SalvageConfig`](super::content::SalvageConfig).

use serde::{Deserialize, Serialize};

use super::content::SalvageConfig;
use crate::fitting::{CellOccupant, FitLayout, Module, ModuleCatalog, ModuleRef};

/// The two-tier loot a wreck/chunk yields per residual module (data-model.md
/// `SalvageItem`; contracts/damage-api.md `SalvageOutcome`, FR-018/019/020).
///
/// A **clean sever** (the module's own `health >= INTACT_THRESHOLD` and its
/// surrounding structure was severed away, not penetrated through) yields an
/// [`IntactModule`](SalvageOutcome::IntactModule) — re-equippable, operational
/// (FR-018). A **destroyed / penetrated-through** module yields
/// [`Scrap`](SalvageOutcome::Scrap) (`amount > 0`, FR-019); an over-kill still
/// leaves ≥ a scrap floor (never zero loot, INV-D09). The clean-sever-vs-scrap
/// **decision** ([`intact_threshold`]/[`salvage`]) is Phase 6 (T031, INV-D12); this
/// epic only fixes the **shape** so the wreck types compile.
///
/// Derive discipline matches the rest of `sim`: serde as the replication (E003) /
/// persistence (E004) seam — present, not exercised this epic; value semantics.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SalvageOutcome {
    /// A clean-severed, still-operational module — its re-equippable identity
    /// (E006 [`ModuleRef`]). Decided in Phase 6 (FR-018, INV-D12).
    IntactModule(ModuleRef),
    /// A scalar scrap quantity (`> 0`) from a destroyed / through-killed module, or
    /// the over-kill floor (FR-019, INV-D09). Decided in Phase 6.
    Scrap(f32),
}

/// The clean-sever-vs-through-kill boundary (T031, FR-018, INV-D12): `true` iff this
/// module salvages **intact**.
///
/// A module is intact iff its live `health >= intact_fraction * module.health_max`
/// (the threshold is content — [`SalvageConfig::intact_fraction`]). At/above the
/// threshold the surrounding structure was cut away cleanly, leaving the module
/// operational; **below** it (a penetrated-through / destroyed module, in the limit
/// `health == 0`) the module is wrecked → scrap, never intact. This is what makes a
/// careful clean sever strictly better than "blast it apart" (a through-kill can
/// never beat a clean sever — FR-018/019).
pub fn intact_threshold(occupant: &CellOccupant, module: &Module, cfg: &SalvageConfig) -> bool {
    occupant.health >= cfg.intact_fraction * module.health_max
}

/// The scrap quantity a through-killed `module` yields: `mass * scrap_per_mass`,
/// floored at `scrap_floor` so a scrap outcome is never below the minimum (INV-D09).
fn scrap_amount(module: &Module, cfg: &SalvageConfig) -> f32 {
    (module.mass * cfg.scrap_per_mass).max(cfg.scrap_floor)
}

/// Walk a residual [`FitLayout`] into its salvage contents (T031, FR-018/019/020) —
/// the **compute** step, run once at wreck spawn.
///
/// For each **occupied** cell (`module.is_some()`) that resolves in the
/// [`ModuleCatalog`], decide via [`intact_threshold`]: at/above the threshold →
/// [`IntactModule`](SalvageOutcome::IntactModule)`(ModuleRef::new(slot, id))`
/// (clean sever, FR-018); below → [`Scrap`](SalvageOutcome::Scrap)`(scrap_amount)`
/// (through-kill, FR-019). Cells iterate in [`FitLayout`] `BTreeMap` order so the
/// contents are deterministic (Principle II). An empty/structural cell, or a module
/// id that does not resolve in the catalog, contributes nothing.
///
/// **Over-kill floor (INV-D09)**: if no module cell survived (the resulting `Vec` is
/// empty — e.g. a wreck of only structural cells, or every module's id missing from
/// the catalog), push a single [`Scrap`](SalvageOutcome::Scrap)`(scrap_floor)` so a
/// wreck **never** yields zero loot.
pub fn salvage_layout(
    layout: &FitLayout,
    catalog: &ModuleCatalog,
    cfg: &SalvageConfig,
) -> Vec<SalvageOutcome> {
    let mut contents = Vec::new();
    for occ in layout.cells.values() {
        let Some(module_id) = occ.module else {
            // An empty / structural cell carries no salvageable device.
            continue;
        };
        let Some(module) = catalog.get(module_id) else {
            // A dangling module id (catalog row missing): no salvage, no panic.
            continue;
        };
        if intact_threshold(occ, module, cfg) {
            contents.push(SalvageOutcome::IntactModule(ModuleRef::new(
                occ.slot, module_id,
            )));
        } else {
            contents.push(SalvageOutcome::Scrap(scrap_amount(module, cfg)));
        }
    }

    // Over-kill floor (INV-D09): a wreck is never zero loot.
    if contents.is_empty() {
        contents.push(SalvageOutcome::Scrap(cfg.scrap_floor));
    }
    contents
}

/// The salvage **read surface** (T031, contracts/damage-api.md §4, FR-018/019/020) —
/// the list E013 consumes to price a wreck.
///
/// Returns the wreck's **precomputed** [`SalvageOutcome`] list (`contents.clone()`).
/// The layout→contents decision was made **once at spawn** ([`salvage_layout`] over
/// the residual layout) and stored on the [`Wreck`](super::sever::Wreck); this
/// accessor only reads it — it does not re-walk the layout or re-decide intact vs
/// scrap. Looting (the single-resolution claim, INV-D10) is
/// [`Wreck::claim`](super::sever::Wreck::claim).
pub fn salvage(wreck: &super::sever::Wreck) -> Vec<SalvageOutcome> {
    wreck.contents.clone()
}
