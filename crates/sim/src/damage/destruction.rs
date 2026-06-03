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

use std::collections::BTreeSet;

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;

use super::content::SalvageConfig;
use super::layers::HullStructure;
use super::salvage::salvage_layout;
use super::sever::{
    connected_region, core_cell, disconnected_regions, sever_chunk, Wreck, WreckOrigin,
};
use crate::components::{CollisionRadius, Target};
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

/// Break a doomed ship apart so it is **visibly destroyed** — the live-combat death
/// trigger for a ship whose structural backstop ([`HullStructure`]) was depleted by
/// sustained fire (the E007 live-demo gap: `apply_damage`'s entry-routing rarely
/// kills a *module* cell on a head-on shot, so `on_section_destroyed` never fired
/// and the enemy sat at `0` hull forever — alive and intact).
///
/// Resolves the ship's hull ([`Fit`] + [`HullCatalog`]), finds the **core section**
/// (the section owning the [`core_cell`] / max-depth cell), and destroys every
/// **NON-core** section FIRST — each disconnected region the removal isolates severs
/// into a drifting [`WreckChunk`](super::sever::WreckChunk) — THEN destroys the core
/// section last (the whole-ship [`Wreck`], [`destroy_ship`]). Net effect: the ship
/// sheds several debris chunks AND becomes a persistent wreck — VISIBLY destroyed.
///
/// **Why this order matters (the E007 visibility fix)**: the core cell is typically
/// `SectionId(0)` (the centered reactor). Destroying sections in `SectionId` order
/// would hit the core FIRST, take the whole-ship path, and `break` — so NO non-core
/// section ever severs and nothing visibly flies apart. By destroying non-core
/// sections first, every disconnected region severs into a drifting chunk before the
/// core's whole-ship wreck lands.
///
/// **Idempotent / total**: a no-op if the ship is already a [`Wreck`], its sections
/// are already gone, or its fit/hull is unresolvable — [`on_section_destroyed`] is
/// itself defensive (each call bails on a missing fit/hull/layout, never panics), so
/// re-shattering an already-shattered ship does nothing further. Server-authoritative
/// (INV-D16): only the combat system on the server calls this.
pub fn shatter_ship(world: &mut World, ship: Entity) {
    // Already a wreck → nothing to shatter further (idempotent).
    if world.get::<Wreck>(ship).is_some() {
        return;
    }

    // Resolve the hull and collect its DISTINCT sections in a deterministic order
    // (`BTreeSet` sorts by `SectionId`). A missing fit/hull is a no-op (no panic).
    let Some(fit) = world.get::<Fit>(ship).cloned() else {
        return;
    };
    let Some(hulls) = world.get_resource::<HullCatalog>() else {
        return;
    };
    let Some(hull) = hulls.get(fit.hull).cloned() else {
        return;
    };
    let sections: BTreeSet<SectionId> = hull.cells.iter().map(|gc| gc.section).collect();

    // Find the CORE section: the section owning the live `core_cell` (max-depth
    // interior cell). Destroying it is the whole-ship path, so it must go LAST.
    // If there is no core (no cells left) the loop below is a no-op and the trailing
    // `destroy_ship` still marks the wreck.
    let core_section = world
        .get::<FitLayout>(ship)
        .and_then(core_cell)
        .and_then(|core| hull.cells.iter().find(|gc| gc.coord == core))
        .map(|gc| gc.section);

    // Order the NON-core sections so the LARGEST section (by authored cell count) is
    // destroyed first, ties broken by `SectionId` for determinism. On the revise-A
    // dense hulls the big shared `STRUCTURAL_SECTION` (the plating body, far more cells
    // than any single one-cell module section) therefore severs FIRST — its removal
    // isolates the still-alive module cells (the wing weapon, the aft thruster, …) into
    // drifting chunks BEFORE the core's whole-ship wreck lands, so the ship visibly
    // flies apart. (On the older coarse layout destroying the one-cell module sections
    // already fragmented the body; on the denser, more-connected body only the big
    // structural removal disconnects anything — hence largest-first.) This refines only
    // the within-non-core ORDER; the core-last whole-ship invariant + the per-section
    // sever mechanism are unchanged.
    let mut cell_count: std::collections::BTreeMap<SectionId, usize> =
        std::collections::BTreeMap::new();
    for gc in &hull.cells {
        *cell_count.entry(gc.section).or_insert(0) += 1;
    }
    let mut noncore: Vec<SectionId> = sections
        .iter()
        .copied()
        .filter(|s| Some(*s) != core_section)
        .collect();
    // Largest section first (descending cell count), then ascending SectionId on a tie.
    noncore.sort_by(|a, b| {
        cell_count
            .get(b)
            .cmp(&cell_count.get(a))
            .then_with(|| a.cmp(b))
    });

    // Destroy every NON-core section (largest-first, deterministic). As each section's
    // cells are removed, connectivity severs every region disconnected from the core
    // into a drifting chunk. The core section is skipped here so the ship sheds its
    // debris BEFORE the whole-ship wreck lands. If a non-core destruction ever takes the
    // whole-ship path itself (it severed the core via cascade), stop — the ship already
    // carries a `Wreck`.
    for section in noncore {
        on_section_destroyed(world, ship, section);
        if world.get::<Wreck>(ship).is_some() {
            return;
        }
    }

    // Finally, destroy the core section (the whole-ship `DestroyedShip` wreck). If the
    // core section was unresolvable (no cells), `destroy_ship` still attaches the
    // wreck marker so the ship is never left alive-but-shattered.
    match core_section {
        Some(section) => on_section_destroyed(world, ship, section),
        None => destroy_ship(world, ship),
    }
    // Belt-and-braces: if destroying the core section did not take the whole-ship path
    // (e.g. the core section had multiple cells and one survived), mark the wreck so a
    // shattered ship is ALWAYS a wreck (it can no longer be a live target).
    if world.get::<Wreck>(ship).is_none() {
        destroy_ship(world, ship);
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
///
/// **Live-combat marker strip (E007 visibility)**: a destroyed ship must STOP being
/// a live, pristine target. After attaching the [`Wreck`], this removes
/// [`Target`], [`CollisionRadius`], and [`FitLayout`] from the entity so it is no
/// longer hit by [`fitted_damage_system`](crate::fitted_damage_system) (no more
/// repeated "KILL" as later shots land) and `render_state`'s drifting-debris query
/// (`With<Wreck> Without<Target> Without<Ship> Without<Projectile>`) emits it as a
/// wreck rather than a pristine ship. The salvage `contents` are computed from the
/// residual [`FitLayout`] **before** the strip, so loot is unaffected. The body
/// ([`Position`](crate::components::Position)/[`Velocity`](crate::components::Velocity)/
/// [`Heading`](crate::components::Heading)/[`AngularVelocity`](crate::components::AngularVelocity))
/// + [`Wreck`] are kept, so the wreck persists as a drifting physical hulk.
fn destroy_ship(world: &mut World, ship: Entity) {
    if let Some(mut hs) = world.get_mut::<HullStructure>(ship) {
        hs.current = 0.0;
    }

    // Decide the lootable contents from the ship's residual layout (the modules that
    // survived to the wreck, with their live health), resource-absent → empty. Computed
    // BEFORE the `FitLayout` strip below so the salvage walk sees the residual modules.
    let residual_layout = world.get::<FitLayout>(ship).cloned();
    let contents = match (
        residual_layout.as_ref(),
        world.get_resource::<ModuleCatalog>(),
        world.get_resource::<SalvageConfig>(),
    ) {
        (Some(layout), Some(catalog), Some(cfg)) => salvage_layout(layout, catalog, cfg),
        _ => Vec::new(),
    };

    // Attach the destroyed-ship wreck (idempotent — overwrite if present), then strip
    // the live-combat markers so the dead ship stops being a damageable target and
    // stops rendering as a pristine ship (it renders as drifting debris instead). The
    // entity keeps its body + `Wreck`, so it IS the persistent, lootable wreck.
    let mut entity = world.entity_mut(ship);
    entity.insert(Wreck {
        origin: WreckOrigin::DestroyedShip,
        contents,
        claimed: false,
    });
    entity.remove::<Target>();
    entity.remove::<CollisionRadius>();
    entity.remove::<FitLayout>();
}
