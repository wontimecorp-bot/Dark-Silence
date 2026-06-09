//! Refinement 5 Phase 2 — **lazy voxelization** of the mining structures.
//!
//! A structure (transport / outpost) spawns as the cheap flat-`Health` box it always was, plus a
//! [`VoxelizeOnHit`] marker. While intact it is NOT a carve target (no `FitLayout`) and NOT a
//! voxel-render entity — so it costs ~nothing, at any entity count. The FIRST shot that hits it (in
//! [`crate::collision::collision_detect_system`]) tags it [`PendingVoxelize`] (and is consumed — "the
//! first shot cracks the shell") instead of subtracting flat HP; then [`voxelize_pending_system`]
//! builds its cell hull and swaps it in. From the next tick it carves exactly like a ship (the carve
//! pipeline is hull-agnostic). So only structures actually under fire ever pay the voxel cost.
//!
//! **Determinism:** windowed-`MiningSkirmish`-only — the marker is never present in any headless /
//! determinism / botkit / demo world, and the system is gated on
//! [`ScenarioActive`](crate::ScenarioActive). No RNG.

use bevy_ecs::prelude::*;

use crate::components::{Destructible, Health, RenderScale};
use crate::fitting::{build_layout_with, CellMaterials, Fit, HullCatalog, HullId, ModuleCatalog};

/// Marker on a structure that should become a voxel carve-hull the first time it is damaged. Carries
/// what [`voxelize_pending_system`] needs to build the hull: which catalog hull to use + the
/// per-structural-cell HP. Runtime-local (like `ProjectileOwner`) — never serialized.
#[derive(Component, Clone, Copy, Debug)]
pub struct VoxelizeOnHit {
    /// The station hull (injected into the `HullCatalog` at scenario spawn) this structure carves as.
    pub hull: HullId,
    /// HP seeded onto each structural cell (sizes the structure's toughness; cells × this ≈ old HP).
    pub cell_hp: f32,
}

/// Transient tag set by `collision_detect_system` when a [`VoxelizeOnHit`] structure is first hit;
/// consumed next by [`voxelize_pending_system`]. Separate from the persistent marker so the trigger
/// (collision) and the build (catalog access) are cleanly split across systems.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PendingVoxelize;

/// Fixed-step (gated on `ScenarioActive`, ordered BEFORE `fitted_damage_system`): convert each
/// freshly-tagged structure into a carve-hull. Builds the `FitLayout` from its station hull + the
/// per-cell HP, then swaps the entity from the flat-`Health`/box representation to the fitted one —
/// insert `FitLayout` + `Destructible`, remove `Health` + `RenderScale` + the markers. The structure
/// keeps everything else (its `Position`/`Heading`/`Velocity`/`Faction`, and a transport's mining
/// bundle + turret hosting), so it carries on moving/mining while now carveable, and dies on core.
pub fn voxelize_pending_system(
    mut commands: Commands,
    hulls: Option<Res<HullCatalog>>,
    modules: Option<Res<ModuleCatalog>>,
    pending: Query<(Entity, &VoxelizeOnHit), With<PendingVoxelize>>,
) {
    let (Some(hulls), Some(modules)) = (hulls, modules) else {
        return;
    };
    for (entity, vox) in &pending {
        let Some(hull) = hulls.get(vox.hull) else {
            // Hull unresolvable (shouldn't happen — injected at spawn) → drop the markers so we don't
            // spin on it every tick. The structure simply stays a flat box.
            commands
                .entity(entity)
                .remove::<(VoxelizeOnHit, PendingVoxelize)>();
            continue;
        };
        // Structures' cells are all material 0 (station/disc hulls), so the default catalog resolves
        // material 0 → `vox.cell_hp` (no windowed override needed; only non-zero materials differ).
        let layout = build_layout_with(
            hull,
            &Fit::new(vox.hull),
            &modules,
            vox.cell_hp,
            &CellMaterials::default(),
        );
        commands
            .entity(entity)
            .insert((layout, Destructible))
            .remove::<(Health, RenderScale, VoxelizeOnHit, PendingVoxelize)>();
    }
}
