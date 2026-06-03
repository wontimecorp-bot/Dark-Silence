//! The destruction worker (US3, Phase 5, FR-014/017).
//!
//! When a section reaches `0` health, [`on_section_destroyed`] (T028) removes its
//! cells from the ship's [`FitLayout`] (coarse, cell-ready granularity — INV-D08),
//! then runs the connectivity flood-fill **once** and severs any region the removal
//! disconnected from the ship's core (INV-D07/D15).
//!
//! **Event-driven, never per-frame (INV-D08)**: connectivity ([`connected_region`])
//! runs solely here, at a destruction event — never on a tick where nothing was
//! destroyed. This fn is **not** registered in the schedule (that is T039/Phase 8);
//! it is called directly (by `apply_damage` later, and by the Phase 5 unit tests).

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use super::content::SalvageConfig;
use super::layers::HullStructure;
use super::salvage::salvage_layout;
use super::sever::{
    connected_region, core_cell, disconnected_regions, sever_chunk, Wreck, WreckOrigin,
};
use crate::fitting::{Fit, FitLayout, HullCatalog, ModuleCatalog, SectionId};

/// Handle a section reaching `0` structural health (T028, FR-014/015/016/017): remove
/// the section's cells from the ship's [`FitLayout`], then run the connectivity check
/// and sever any disconnected region into a drifting chunk.
///
/// Steps:
///
/// 1. Resolve the ship's hull ([`Fit`] on the entity + [`HullCatalog`]) and find the
///    hull cells whose [`GridCell::section`](crate::fitting::GridCell::section)
///    equals `section`.
/// 2. **Remove** those cells from the ship's [`FitLayout`] — the coarse,
///    cell-ready removal (INV-D08: the ONLY place connectivity runs; event-driven,
///    never per-frame). Record whether the core cell sat in the destroyed section.
/// 3. Compute the new [`core_cell`]. If there are no cells left **or** the core was
///    in the destroyed section, the **whole ship is destroyed** (INV-D15): flag it
///    (set [`HullStructure::current`] to `0` and attach a
///    [`Wreck`]`{ origin: DestroyedShip, .. }` marker) and return early — the
///    persistent-wreck spawn + salvage are Phase 6 (T032).
/// 4. Else run [`connected_region`] and, for **each** disconnected region (a maximal
///    4-connected component of the remaining cells NOT attached to the core), call
///    [`sever_chunk`]. Regions are iterated in a deterministic order (sorted by each
///    region's smallest-cell representative — [`disconnected_regions`]).
///
/// Runs once per destruction event (INV-D08). Server-authoritative (INV-D16). Total:
/// a missing fit / hull / layout is a no-op (never a panic).
pub fn on_section_destroyed(world: &mut World, ship: Entity, section: SectionId) {
    // --- 1. Resolve the hull + the destroyed section's cells -------------------
    let Some(fit) = world.get::<Fit>(ship).cloned() else {
        return;
    };
    let Some(hulls) = world.get_resource::<HullCatalog>() else {
        return;
    };
    let Some(hull) = hulls.get(fit.hull).cloned() else {
        return;
    };

    // The cells the destroyed section authored on the hull grid.
    let section_cells: Vec<_> = hull
        .cells
        .iter()
        .filter(|gc| gc.section == section)
        .map(|gc| gc.coord)
        .collect();

    // --- 2. Remove those cells from the live FitLayout (coarse, INV-D08) -------
    // Record whether the core sat in the destroyed section BEFORE removal — the
    // core-sever test (INV-D15) compares the pre-removal core against the section.
    let core_before = world.get::<FitLayout>(ship).and_then(core_cell);
    let core_in_destroyed_section = core_before.is_some_and(|c| section_cells.contains(&c));

    if let Some(mut layout) = world.get_mut::<FitLayout>(ship) {
        for cell in &section_cells {
            layout.cells.remove(cell);
        }
    } else {
        // No layout to operate on — nothing further to do.
        return;
    }

    // --- 3. Whole-ship-destroyed check (INV-D15) -------------------------------
    let core_after = world.get::<FitLayout>(ship).and_then(core_cell);
    if core_after.is_none() || core_in_destroyed_section {
        destroy_ship(world, ship);
        return;
    }
    // SAFETY: `core_after` is `Some` here (checked above).
    let core = core_after.expect("core present after the non-whole-ship branch");

    // --- 4. Flood-fill once + sever each disconnected region -------------------
    let Some(layout) = world.get::<FitLayout>(ship).cloned() else {
        return;
    };
    let attached = connected_region(&layout, core);
    let regions = disconnected_regions(&layout, &attached);
    for region in regions {
        // A non-empty region severs as one chunk (a lone orphan cell severs cleanly,
        // INV-D09); skip the degenerate empty set defensively.
        if !region.is_empty() {
            sever_chunk(world, ship, &region);
        }
    }
}

/// Mark the whole ship destroyed (INV-D15): zero its structural backstop and attach
/// a [`Wreck`]`{ origin: DestroyedShip, contents, .. }` (T032, FR-020).
///
/// The destroyed ship entity **retains** its residual [`FitLayout`] + body
/// components, so it **is** the persistent, lootable wreck — no separate entity is
/// spawned. The salvage [`contents`](Wreck::contents) are decided **once, here, at
/// the destruction event** ([`salvage_layout`] over the residual layout): a clean
/// sever yields an intact module, a through-killed module yields scrap, and an
/// over-killed ship (deeply-negative / zeroed health, or a structural-only residual)
/// still yields ≥ a `Scrap` floor (the INV-D09 guard inside `salvage_layout`) so the
/// wreck is never empty loot.
///
/// A minimal test world without the [`ModuleCatalog`]/[`SalvageConfig`] resources
/// falls back to empty contents (no panic) — the wreck marker still attaches.
fn destroy_ship(world: &mut World, ship: Entity) {
    if let Some(mut hs) = world.get_mut::<HullStructure>(ship) {
        hs.current = 0.0;
    }

    // Decide the lootable contents from the ship's residual layout (the modules that
    // survived to the wreck, with their live health), resource-absent → empty.
    let residual_layout = world.get::<FitLayout>(ship).cloned();
    let contents = match (
        residual_layout.as_ref(),
        world.get_resource::<ModuleCatalog>(),
        world.get_resource::<SalvageConfig>(),
    ) {
        (Some(layout), Some(catalog), Some(cfg)) => salvage_layout(layout, catalog, cfg),
        _ => Vec::new(),
    };

    // Attach the destroyed-ship wreck (idempotent — overwrite if present). The entity
    // itself, keeping its residual `FitLayout` + body, IS the persistent wreck.
    world.entity_mut(ship).insert(Wreck {
        origin: WreckOrigin::DestroyedShip,
        contents,
        claimed: false,
    });
}
