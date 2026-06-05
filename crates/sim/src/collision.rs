//! Collision: pure math (swept point-vs-circle CCD, circle–circle contact, the
//! closed-form elastic 2-body impulse, the lethal-ram threshold) plus the ECS
//! systems that apply them — projectile hits and ship↔asteroid rams.
//!
//! Glam-only math, deterministic, engine-agnostic — the authoritative collision
//! math behind the `Physics` trait (ADR-0004). Same inputs → same outputs, so
//! there is never per-frame flicker.

use crate::clock::FixedDt;
use crate::combat::{self, HitFeedback};
use crate::components::AngularVelocity;
use crate::components::{
    hostile, CollisionRadius, CombatRules, Damage, DamageFlash, Destructible, Faction, Heading,
    Health, LastShieldHit, MeshAnchor, Position, PrevPosition, Projectile, ProjectileFaction,
    ProjectileMass, ProjectileOwner, ShieldHitFlash, Ship, Target, TargetKind, Velocity,
};
use crate::damage::{
    apply_damage, core_cell, first_cell_hit, on_cells_carved, DamageEvent, HitKind, Wreck, REACH,
};
use crate::fitting::{
    center_or_anchor, layout_inertia_with, layout_mass_with, FitLayout, HullCatalog, ModuleCatalog,
    CELL_WORLD_SIZE,
};
use crate::motion::{apply_angular_impulse, apply_linear_impulse};
use crate::physics::{Physics, RapierPhysics, SweptHit};
use crate::tuning::Tuning;
use crate::weapon::{damage_event_from_hit, WeaponSource, PROJECTILE_MASS};
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Ship inertial mass for ram impulses (asteroids are heavier, so the ship
/// bounces more).
pub(crate) const SHIP_MASS: f32 = 2.0;
/// Asteroid inertial mass for ram impulses.
pub(crate) const ASTEROID_MASS: f32 = 8.0;

/// Earliest time-of-impact `t ∈ [0, 1]` at which the point sweeping `p0`→`p1`
/// first touches the circle `(center, radius)`, or `None` if it never does
/// within the segment.
///
/// A tangent (closest-approach distance exactly `radius`) counts as a hit
/// (CHK027). A point that starts inside the circle hits at `t = 0`. Because the
/// whole swept segment is tested — not the endpoints — a fast projectile cannot
/// tunnel through a small target between frames (FR-006).
pub fn segment_circle_toi(p0: Vec2, p1: Vec2, center: Vec2, radius: f32) -> Option<f32> {
    let d = p1 - p0;
    let f = p0 - center;
    let a = d.dot(d);
    let r2 = radius * radius;
    if a <= f32::EPSILON {
        return if f.dot(f) <= r2 { Some(0.0) } else { None };
    }
    if f.dot(f) <= r2 {
        return Some(0.0);
    }
    let b = 2.0 * f.dot(d);
    let c = f.dot(f) - r2;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let t = (-b - disc.sqrt()) / (2.0 * a);
    if (0.0..=1.0).contains(&t) {
        Some(t)
    } else {
        None
    }
}

/// Static overlap of two circles: the push-out `normal` (unit vector pointing
/// from `b` toward `a`) and penetration `depth`, or `None` when separate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub normal: Vec2,
    pub depth: f32,
}

/// Detect a circle–circle overlap.
pub fn circle_contact(a: Vec2, a_radius: f32, b: Vec2, b_radius: f32) -> Option<Contact> {
    let delta = a - b;
    let dist = delta.length();
    let sum = a_radius + b_radius;
    if dist < sum {
        let normal = if dist > f32::EPSILON {
            delta / dist
        } else {
            Vec2::X
        };
        Some(Contact {
            normal,
            depth: sum - dist,
        })
    } else {
        None
    }
}

/// Closed-form elastic 2-body collision (restitution = 1). Returns the new
/// velocities `(a, b)`. Conserves total linear momentum; if the bodies are
/// already separating along the contact normal, velocities are unchanged.
pub fn elastic_velocities(
    pa: Vec2,
    va: Vec2,
    ma: f32,
    pb: Vec2,
    vb: Vec2,
    mb: f32,
) -> (Vec2, Vec2) {
    let delta = pa - pb;
    let dist = delta.length();
    let n = if dist > f32::EPSILON {
        delta / dist
    } else {
        Vec2::X
    };
    let vn = (va - vb).dot(n);
    if vn >= 0.0 {
        return (va, vb);
    }
    let inv = 1.0 / ma + 1.0 / mb;
    let j = -2.0 * vn / inv;
    let impulse = n * j;
    (va + impulse / ma, vb - impulse / mb)
}

/// A closing speed at or above the threshold is a lethal ram (boundary
/// inclusive, CHK010).
pub fn is_lethal_ram(closing_speed: f32, threshold: f32) -> bool {
    closing_speed >= threshold
}

/// Fixed-step projectile collision (FR-006/FR-007): each projectile is swept
/// from its previous to current position against every target circle. On the
/// first hit the target takes damage, the projectile despawns, and hit feedback
/// is raised. Damage is order-independent across simultaneous hits.
///
/// **Unfitted-only now (E007, INV-D17)**: the target query gains `Without<FitLayout>`
/// so this legacy flat-`Health` path resolves **only** unfitted targets
/// (dummies/asteroids/E002 bots). Fitted ships (those carrying a [`FitLayout`]) are
/// handled by [`fitted_damage_system`] (the per-module E007 pipeline) instead, so
/// no target is processed by both. The `Without<FitLayout>` filter is a **no-op**
/// for every existing unfitted target, so the E002/E003 behavior is byte-for-byte
/// identical (the flat `apply_damage` clamp + despawn).
pub fn collision_detect_system(
    mut commands: Commands,
    mut feedback: ResMut<HitFeedback>,
    rules: Option<Res<CombatRules>>,
    projectiles: Query<
        (
            Entity,
            &Position,
            &PrevPosition,
            &Damage,
            Option<&ProjectileFaction>,
        ),
        With<Projectile>,
    >,
    mut targets: Query<
        (&Position, &CollisionRadius, &mut Health, Option<&Faction>),
        (With<Target>, Without<FitLayout>),
    >,
) {
    let friendly_fire = rules.is_some_and(|r| r.friendly_fire);
    let physics = RapierPhysics::new();
    for (projectile, pos, prev, dmg, proj_faction) in &projectiles {
        for (tpos, radius, mut health, target_faction) in &mut targets {
            // Mining-skirmish friend/foe gate: a FACTIONED shot skips a non-enemy target. A no-op
            // for an unfactioned shot (`proj_faction` is `None`) → today's free-for-all, so every
            // determinism/botkit/test world (no factioned projectiles) is byte-identical.
            if let Some(pf) = proj_faction {
                if !hostile(pf.0, target_faction.copied(), friendly_fire) {
                    continue;
                }
            }
            if physics
                .swept_cast(prev.0, pos.0, tpos.0, radius.0)
                .is_some()
            {
                health.0 = combat::apply_damage(health.0, dmg.0);
                feedback.hit_flash = combat::FLASH_TIME;
                commands.entity(projectile).despawn();
                break; // a projectile strikes at most one target
            }
        }
    }
}

/// A recorded projectile→fitted-target hit, collected in the query phase so the
/// exclusive `apply_damage` (which takes `&mut World`) runs after the borrow ends.
struct FittedHit {
    /// The projectile to despawn.
    projectile: Entity,
    /// The fitted ship struck.
    target: Entity,
    /// The typed event built from the shot, in the target's **hull-local cell-space**.
    event: DamageEvent,
    /// Unit direction (world space) from the target centre toward the world impact
    /// point — surfaced from the otherwise-discarded `hit.point`/`tpos` so the client
    /// can flash the shield deflector AT the impact (FIX 0a). Falls back to the
    /// reverse shot direction when the centre-to-impact vector is degenerate.
    shield_dir: Vec2,
    /// Phase M4 — the projectile's **momentum** `PROJECTILE_MASS · velocity` (world frame), the
    /// impulse deposited on the struck body (linear + off-centre torque).
    impulse: Vec2,
    /// Phase M4 — the **arm** `world_impact − target_centre` (world frame): the contact point
    /// relative to the target's centre of mass, so an off-centre hit imparts spin (`arm × impulse`).
    arm: Vec2,
}

/// Build the hull-local carve entry ray for a world-space projectile hit on a fitted
/// target — anchored at **where the bullet visually struck**, not through the core
/// (the FIX: the old version routed every shot through the grid/core centre using
/// only the shot *direction*, so an off-centre shot still carved a channel down the
/// middle; with the fine dense hull that is visibly wrong).
///
/// `apply_damage` carves `ev.point → ev.point + ev.dir·REACH` across the target's
/// **hull-local cell-space** (cells at integer coords, centres at `coord + 0.5`,
/// cell-space axes: `x = col`, `y = row`). This maps the real world hit into that
/// space:
///
/// 1. **Direction** (`dir_cell`): rotate the shot's world travel direction `world_dir`
///    into the hull frame by `-heading` (the inverse ship rotation), giving ship-local
///    `(forward, lateral)`, then map it into cell-space axes (`x = col`, `y = row`). The
///    carve bores inward along the shot line from the impact.
/// 2. **Entry point** (`entry_cell_pos`): map the world impact offset from the ship
///    centre into cell-space — `offset_world = world_impact − world_centre`
///    (the contact point's offset from the ship centre, world units); `offset_local =
///    Rot(−heading) · offset_world` (un-rotate into the hull frame, so `offset_local.x`
///    is ship-local **forward** and `offset_local.y` is ship-local **lateral**);
///    `entry_cell_pos = offset_local / CELL_WORLD_SIZE + center` mapped onto the
///    cell-space axes to **match the render** ([`build_hull_mesh`] maps local **X
///    (forward) ← row**, local **Y (lateral) ← col**): therefore **col ← lateral**
///    (`offset_local.y`) and **row ← forward** (`offset_local.x`). `center` is the
///    per-target cell-space anchor (the caller mirrors the client's `hull_mesh_center`:
///    cell-COM for a `Wreck`, grid centre for a live ship).
///
/// The carve ray STARTS at this impact cell (on the hull edge the shot met) and goes
/// inward along `dir_cell`, so the channel BEGINS where the bullet hit. The existing
/// [`carve_path`](crate::damage) cell-trace walks the present cells from there inward;
/// only the ray's lateral position changes — it is no longer forced through the centre.
///
/// **Consequence (intended, more realistic):** an off-centre shot carves where it hits
/// (e.g. cuts a wing) and does NOT auto-bore to the core; killing requires hitting the
/// core region (centre) — a centre-aimed burst still bores to the core from any
/// approach, while a flank burst severs a piece.
fn hull_local_entry_ray(
    world_dir: Vec2,
    world_impact: Vec2,
    world_centre: Vec2,
    heading: f32,
    center: Vec2,
) -> (Vec2, Vec2) {
    let inv_rot = Vec2::from_angle(-heading);
    // Inverse ship rotation: world travel direction → hull-local `(forward, lateral)`
    // (`.x` = ship-local +X/forward, `.y` = ship-local +Y/lateral).
    let dir_local = if world_dir.length_squared() > f32::EPSILON {
        inv_rot.rotate(world_dir).normalize_or_zero()
    } else {
        Vec2::ZERO
    };
    // Impact offset from the ship centre, un-rotated into the hull frame (so
    // `offset_local.x` = forward, `offset_local.y` = lateral), scaled from world units to
    // cells, and centred on the target's cell-space `center` → the cell-space position the
    // bullet visually struck.
    //
    // `center` is the per-target cell-space point that the entity's `Position`/render is
    // anchored on, computed by the caller to MATCH the client's `hull_mesh_center`
    // (`crates/client/src/net.rs`): the **cell-COM** (`mean(col+0.5, row+0.5)`) for a
    // `Wreck` (its `Position` is its cell-COM and its cells render around it), and the
    // **grid centre** (`grid_dims·0.5`) for a live ship (its `Position` sits at the grid
    // centre). Threading it here — rather than always using the grid centre — makes the
    // carve enter where the cells actually are for an off-centre wreck piece (otherwise the
    // ray entered empty space → `NoModule`/MISS, nothing carved).
    let offset_world = world_impact - world_centre;
    let offset_local = inv_rot.rotate(offset_world);
    // Cell/trace space is (x = col, y = row); the render maps forward → row, lateral →
    // col (`build_hull_mesh`: local X/nose ← row, local Y/wing ← col). Match it so a
    // lateral wing hit lands on the col axis (the wing), not the row (fore/aft) axis.
    let entry_cell_pos = Vec2::new(
        offset_local.y / CELL_WORLD_SIZE + center.x, // x = col  <- lateral
        offset_local.x / CELL_WORLD_SIZE + center.y, // y = row  <- forward
    );
    // The carve direction in cell space: x = col <- lateral = dir_local.y; y = row <-
    // forward = dir_local.x — the same swap, so carve_path AND the armor-angle (entry-cell
    // radial vs ev.dir) both operate consistently in cell space.
    let dir_cell = Vec2::new(dir_local.y, dir_local.x).normalize_or_zero();
    (entry_cell_pos, dir_cell)
}

/// Fixed-step per-module damage for **fitted** targets (E007, T038/T039;
/// FR-001/021, INV-D16/D17) — the exclusive-`&mut World` successor to the legacy
/// flat-`Health` path for ships carrying a [`FitLayout`].
///
/// It runs the **same** swept-ray CCD as [`collision_detect_system`]
/// ([`RapierPhysics::swept_cast`] — no new geometry, no tunnel, FR-021) but routes
/// each hit through the E007 [`apply_damage`] pipeline (Shields → Armor gate → carve,
/// Phase 2). It is exclusive because `apply_damage` needs `&mut World`; the
/// projectile/target sweep is therefore collected **first** (the query borrow
/// released) and the mutations applied after.
///
/// Steps:
/// 1. **Sweep** every `(Projectile, WeaponSource)` vs every carve-target
///    (`With<FitLayout>, With<Destructible>` — live ships AND destructible wreckage),
///    skipping self-hits via [`ProjectileOwner`] (E002), and record the **first** target
///    struck per projectile (lowest `toi`).
/// 2. For each recorded hit: capture the target's pre-carve [`core_cell`], transform
///    the world hit into the target's hull-local cell-space ([`hull_local_entry_ray`]),
///    build the [`DamageEvent`] ([`damage_event_from_hit`]), and apply it
///    ([`apply_damage`] — which **carves** the cells along the shot ray out of the live
///    `FitLayout`). Set the hit flash + the legibility tag
///    ([`HitFeedback::last_kind`], FR-024) and despawn the projectile.
/// 3. **Carve destruction event (Phase 2)**: if the carve removed any cell
///    (`outcome.destroyed`), call [`on_cells_carved`] with the pre-carve core. For a
///    **live ship** it runs the connectivity flood-fill **once** (INV-D08) and (a) severs
///    each region the carve disconnected from the core into a drifting [`WreckChunk`]
///    while the ship lives, and (b) destroys the whole ship
///    ([`destroy_ship`](crate::damage::on_cells_carved)) when the **core cell** was carved
///    away or left coreless. A whole-ship death raises the kill flash. For a **wreck**
///    (already dead) it instead severs further-disconnected pieces and **despawns** the
///    entity once its cells are fully carved away — never a re-kill (see
///    [`on_cells_carved`]). This is the Phase 2 death model: a ship dies when its core is
///    destroyed/severed — the old `HullStructure`-depletion `shatter_ship` trigger is
///    retired (carving-to-core is now the death).
///
/// **Graceful degradation (INV-D16)**: a world with no fitted targets (the E002/
/// E003/determinism worlds) records no hits and is a no-op; `apply_damage` /
/// `on_cells_carved` themselves bail (return `NoModule` / no-op) when an E007
/// resource or catalog is absent, so a world missing them never panics. Server-
/// authoritative — only the server runs this; the client predicts the same path.
pub fn fitted_damage_system(world: &mut World) {
    // --- 1. Collect hits (query borrow released before any &mut World use) -------
    let physics = RapierPhysics::new();
    let mut hits: Vec<FittedHit> = Vec::new();

    // Snapshot the carve-targets once (Entity, world pos, radius, heading, grid).
    // **Destructibility is the explicit per-entity gate** (`With<Destructible>`): a target
    // is carve-eligible iff it carries `FitLayout` + `CollisionRadius` + `Destructible`.
    // This deliberately drops the old `&Fit` requirement and the `Without<Wreck>` gate —
    // live ships AND wreckage (severed chunks / destroyed-ship hulks, which keep a residual
    // `FitLayout` + a collider + `Destructible`) carve through the SAME path. Wreckage is
    // not re-killed: the wreck-aware `on_cells_carved` branch handles a `Wreck` target
    // separately (sever-further / despawn-when-empty, no `destroy_ship`). Removing
    // `Destructible` per entity makes that entity inert (the user's toggle).
    let mut target_q = world.query_filtered::<(
        Entity,
        &Position,
        &CollisionRadius,
        &Heading,
        &FitLayout,
        Option<&Wreck>,
        Option<&MeshAnchor>,
        Option<&Faction>,
    ), (With<FitLayout>, With<Destructible>)>();
    // Resolve each target's grid dims from its **`FitLayout.hull`** (→ `HullCatalog`),
    // not a `Fit` — the `Fit`-independent lookup that lets wreckage (no `Fit`) carve too.
    // The grid is the cell-space the carve maps the real impact into (see
    // `hull_local_entry_ray`). A target whose hull is unresolvable is skipped in the apply
    // loop (no panic, INV-D16).
    let hull_dims: std::collections::BTreeMap<Entity, (u16, u16)> = {
        let hulls = world.get_resource::<HullCatalog>();
        target_q
            .iter(world)
            .filter_map(|(e, _, _, _, layout, _, _, _)| {
                hulls
                    .and_then(|h| h.get(layout.hull))
                    .map(|hull| (e, hull.grid_dims))
            })
            .collect()
    };
    // Per-target cell-space **center** for the carve entry ray — computed to MATCH the
    // client's `hull_mesh_center` (`crates/client/src/net.rs`) so the carve enters where
    // the cells actually render:
    //   * `Wreck` target → its **frozen [`MeshAnchor`]** (the cell-COM captured AT SEVER /
    //     the hulk's grid centre), so carving a cell does not move where the piece anchors
    //     (Fix #6). A severed chunk's `Position` is that anchor's world point, and its cells
    //     render around it. (Absent anchor — a hand-built test wreck — falls back to the live
    //     cell-COM, unchanged.)
    //   * live ship (no `Wreck`) → the **grid centre** `(cols·0.5, rows·0.5)` — its
    //     `Position` sits at the grid centre (byte-identical to the previous behaviour).
    // Deterministic: the `BTreeMap` cells iterate in sorted `Cell` order, the query order is
    // stable, and the result is keyed by `Entity` in a `BTreeMap`. A target whose hull is
    // unresolvable has no entry and is skipped in the apply loop alongside `hull_dims`.
    let centers: std::collections::BTreeMap<Entity, Vec2> = target_q
        .iter(world)
        .filter_map(|(e, _, _, _, layout, wreck, anchor, _)| {
            let is_wreck = wreck.is_some();
            // A live ship needs its resolved grid dims (skip the target if unresolvable,
            // matching `hull_dims`); a `Wreck` uses its frozen anchor / cell-COM and ignores
            // the dims. Single-sourced with `apply_damage`'s armor-angle reference via
            // `center_or_anchor`.
            let grid_dims = if is_wreck {
                (0, 0)
            } else {
                *hull_dims.get(&e)?
            };
            let anchor = anchor.map(|a| a.0);
            Some((e, center_or_anchor(anchor, layout, grid_dims, is_wreck)))
        })
        .collect();
    // Snapshot each target's selection inputs, INCLUDING a clone of its `FitLayout` so the
    // narrow-phase (below) can run the cell-precise crossing test (`first_cell_hit`) without
    // re-borrowing the world. `FitLayout` is `Clone` (a `BTreeMap` of small `Copy`
    // occupants); the clone is released before any `&mut World` use, like the rest of the
    // collected hit data.
    let targets: Vec<(Entity, Vec2, f32, f32, FitLayout, Option<Faction>)> = target_q
        .iter(world)
        .map(|(e, p, r, h, layout, _, _, faction)| {
            (e, p.0, r.0, h.0, layout.clone(), faction.copied())
        })
        .collect();

    // Mining-skirmish friend/foe: a factioned shot only carves an ENEMY fitted target (mirrors the
    // flat path). Read before the projectile borrow; `Option<Res>`/default so a world without the
    // rules (every determinism/test world) keeps today's behavior.
    let friendly_fire = world
        .get_resource::<CombatRules>()
        .is_some_and(|r| r.friendly_fire);

    let mut proj_q = world.query_filtered::<(
        Entity,
        &Position,
        &PrevPosition,
        &Velocity,
        &Damage,
        &WeaponSource,
        Option<&ProjectileOwner>,
        Option<&ProjectileMass>,
        Option<&ProjectileFaction>,
    ), With<Projectile>>();
    for (projectile, pos, prev, vel, dmg, src, owner, pmass, pfac) in proj_q.iter(world) {
        let owner_e = owner.map(|o| o.0);
        // Phase M5: the per-weapon slug mass the shot carries (falls back to the global
        // `PROJECTILE_MASS` for a projectile spawned without one — e.g. a minimal-world test shot).
        let proj_mass = pmass.map(|m| m.0).unwrap_or(PROJECTILE_MASS);
        // **Cell-precise hit selection** (the targeting-bug fix): a projectile hits a target
        // ONLY if its swept segment actually crosses one of that target's present CELLS —
        // not merely the loose `CollisionRadius` circle. The circle (sized to the whole
        // footprint) is kept as a cheap **broad-phase reject**; the truth is the cells.
        //
        // Among the broad-phase survivors, pick the target with the lowest **cell-crossing
        // toi** (the first CELL the ray reaches across all candidates), NOT the lowest
        // circle toi. A target whose cells the ray never crosses is NOT hit (the projectile
        // passes it). So a shot aimed at a small piece sitting beside a ship — which crosses
        // the ship's big circle first but never a ship cell — carves the PIECE (whose cell
        // it crosses), and the ship keeps all its cells.
        //
        // The cell test is single-sourced with the carve: it maps the world segment into the
        // target's cell-space (`hull_local_entry_ray`, the SAME per-target `center` the carve
        // uses) and runs `first_cell_hit` (the SAME swept-vs-inscribed-circle test
        // `carve_path` walks) over the REACH segment — so the selected target is guaranteed
        // to carve a cell (no empty path, no fallback).
        //
        // Carries the chosen target's world centre `tpos` + the broad-phase circle impact
        // point `world_impact` (shield-impact dir, FIX 0a) and the already-computed cell-
        // space entry ray `(point, dir_cell)` so the apply phase reuses them.
        let mut best: Option<(Entity, f32, Vec2, Vec2, Vec2, Vec2)> = None; // (target, cell_toi, tpos, world_impact, point, dir_cell)
        for (target, tpos, radius, heading, layout, target_faction) in &targets {
            if owner_e == Some(*target) {
                continue; // self-hit prevention (E002 ProjectileOwner)
            }
            // Mining-skirmish friend/foe gate: a FACTIONED shot skips a non-enemy fitted target. A
            // no-op for an unfactioned shot (`pfac` is `None`) → every determinism/test world (no
            // factioned projectiles) is byte-identical.
            if let Some(pf) = pfac {
                if !hostile(pf.0, *target_faction, friendly_fire) {
                    continue;
                }
            }
            // Broad-phase: cheap circle reject — skip targets the projectile clearly misses.
            let Some(hit) = physics.swept_cast(prev.0, pos.0, *tpos, *radius) else {
                continue;
            };
            // The carve entry ray (and `center`) must resolve, else this target is skipped in
            // the apply loop anyway — so it cannot be hit (INV-D16, no panic).
            if !hull_dims.contains_key(target) {
                continue;
            }
            let Some(&center) = centers.get(target) else {
                continue;
            };
            // Map the world impact into the target's cell-space (same mapping the carve uses).
            let (point, dir_cell) = hull_local_entry_ray(vel.0, hit.point, *tpos, *heading, center);
            // Narrow-phase: does the cell-space segment actually cross one of THIS target's
            // present cells? If not, the projectile passes this target (not a hit).
            let Some((_, cell_toi)) = first_cell_hit(layout, point, point + dir_cell * REACH)
            else {
                continue;
            };
            // Pick the first CELL reached across all candidates (lowest cell-crossing toi).
            let take = match best {
                None => true,
                Some((_, best_toi, _, _, _, _)) => cell_toi < best_toi,
            };
            if take {
                best = Some((*target, cell_toi, *tpos, hit.point, point, dir_cell));
            }
        }
        if let Some((target, cell_toi, tpos, world_impact, point, dir_cell)) = best {
            // FIX 0a: the world impact direction from the ship centre to the swept circle hit
            // point — the data `update_shield_bubble` flashes at. Fall back to the reverse
            // shot direction (`-vel`) when the centre→impact vector is degenerate (e.g. a
            // hit at the exact centre), so the flash always has a sensible facing.
            let shield_dir = {
                let from_centre = (world_impact - tpos).normalize_or_zero();
                if from_centre != Vec2::ZERO {
                    from_centre
                } else {
                    (-vel.0).normalize_or_zero()
                }
            };
            let local_hit = SweptHit {
                toi: cell_toi,
                point,
            };
            let event = damage_event_from_hit(&local_hit, src, dmg.0, dir_cell, owner_e);
            hits.push(FittedHit {
                projectile,
                target,
                event,
                shield_dir,
                // Phase M4/M5: momentum the shot carries = its per-weapon mass·velocity (world),
                // and the contact arm from the target centre → off-centre hits spin the body.
                impulse: proj_mass * vel.0,
                arm: world_impact - tpos,
            });
        }
    }

    // --- 2. Apply each hit through the E007 pipeline (exclusive &mut World) -------
    for FittedHit {
        projectile,
        target,
        event,
        shield_dir,
        impulse,
        arm,
    } in hits
    {
        // Capture the pre-carve core cell BEFORE apply_damage carves the layout — the
        // carve may remove the core cell, and the death model needs the original core
        // to detect "core destroyed".
        let pre_carve_core = world.get::<FitLayout>(target).and_then(core_cell);

        // --- Phase M4/M5: transfer the shot's momentum to the struck body --------------
        // Apply the projectile's impulse (`impulse = projectile_mass·velocity`) to the target
        // BEFORE the carve, so a piece the carve severs off carries the kick (`sever_chunk` reads
        // the post-kick parent velocity). Linear `Δv = J/M`; off-centre `Δω = (arm × J)/I` → tumble.
        // M5: mass `M` ([`layout_mass`]) and inertia `I` ([`layout_inertia`]) are the body's REAL
        // per-cell masses (module cells = their module mass, structural = `STRUCT_CELL_MASS`) over
        // the CURRENT (pre-carve) cells — the SAME mass basis `derive_ship_stats` gives flight, so
        // a live ship and the wreck it becomes share one mass (no jump on death), and a heavy
        // reactor/armor body resists knockback + tumble more than light plating. Mass + inertia are
        // computed up-front so the `FitLayout`/`ModuleCatalog` borrows drop before the velocity
        // writes. A catalog-less minimal test world (no `ModuleCatalog`) skips the impulse; a target
        // with no `Velocity`/`AngularVelocity` is skipped per-write. Deterministic (sorted-cell fold).
        // M6: the structural-cell mass is live-tunable (dev panel); copy it out first (no borrow).
        let struct_cell_mass = world
            .get_resource::<crate::tuning::SimTuning>()
            .copied()
            .unwrap_or_default()
            .struct_cell_mass;
        let mass_inertia = world.get_resource::<ModuleCatalog>().and_then(|modules| {
            world.get::<FitLayout>(target).map(|layout| {
                (
                    layout_mass_with(layout, modules, struct_cell_mass),
                    layout_inertia_with(layout, modules, struct_cell_mass),
                )
            })
        });
        if let Some((mass, inertia)) = mass_inertia {
            let mass = mass.max(f32::MIN_POSITIVE);
            if let Some(mut vel) = world.get_mut::<Velocity>(target) {
                vel.0 = apply_linear_impulse(vel.0, impulse, mass);
            }
            if let Some(mut omega) = world.get_mut::<AngularVelocity>(target) {
                omega.0 = apply_angular_impulse(omega.0, arm, impulse, inertia);
            }
        }

        let outcome = apply_damage(world, target, event);

        // Feedback (FR-024): flash + the legibility tag the HUD reads.
        if let Some(mut feedback) = world.get_resource_mut::<HitFeedback>() {
            feedback.hit_flash = combat::FLASH_TIME;
            feedback.last_kind = Some(outcome.result);
        }

        // Per-entity visual feedback (E007 live-demo): refresh the struck target's
        // timers so the client can react this frame. The hull-hit `DamageFlash` timing
        // seam is still refreshed (the client no longer scale-pulses from it — the
        // user-disliked "zoom" is gone — but the timing remains available). A hit that
        // the shield ABSORBED ([`HitKind::ShieldAbsorbed`]) additionally refreshes a
        // `ShieldHitFlash` so the client blooms a brief cyan deflector shimmer for the
        // split-second the shot strikes the still-up shield (then it fades; no flash
        // once the shield is depleted and shots reach the hull). Insert-or-overwrite —
        // a fresh hit refreshes the timer; the decay systems bleed each back to 0.
        // Skipped if the target was despawned this hit (no entity to flag).
        if let Ok(mut entity) = world.get_entity_mut(target) {
            entity.insert(DamageFlash(combat::FLASH_TIME));
            if outcome.result == HitKind::ShieldAbsorbed {
                entity.insert(ShieldHitFlash(combat::SHIELD_FLASH_TIME));
                // FIX 0a: record WHERE the shield was hit so the client flashes the
                // deflector at the impact point (not over the whole ship). Same timer
                // window as `ShieldHitFlash`; the two decay in lock-step.
                entity.insert(LastShieldHit {
                    dir: shield_dir,
                    timer: combat::SHIELD_FLASH_TIME,
                });
            }
        }

        // --- 3. Carve destruction event (Phase 2): sever + core-death --------------
        // The carve removed cells from the live FitLayout. Run the connectivity
        // flood-fill ONCE (INV-D08): on a LIVE ship, sever each region the carve
        // disconnected from the core into a drifting chunk while the ship lives, and
        // destroy the whole ship when the CORE cell was carved away/severed (the Phase 2
        // death model — raises the kill flash). On a WRECK (already dead) it severs
        // further-disconnected pieces and despawns the emptied entity instead — no
        // re-kill. `on_cells_carved` dispatches on the `Wreck` tag and is total. Skipped
        // if the target despawned this hit.
        if outcome.destroyed && world.get_entity(target).is_ok() {
            let ship_destroyed = on_cells_carved(world, target, pre_carve_core);
            if ship_destroyed {
                if let Some(mut feedback) = world.get_resource_mut::<HitFeedback>() {
                    feedback.destroy_flash = combat::FLASH_TIME;
                }
            }
        }

        // The projectile strikes at most one target, then despawns (E002 parity).
        if let Ok(e) = world.get_entity_mut(projectile) {
            e.despawn();
        }
    }
}

/// Fixed-step decay of the per-entity [`DamageFlash`] hit-pop timer toward `0`
/// (E007 live-demo visual feedback).
///
/// Bleeds every entity's `DamageFlash` down by the fixed `dt` each step, clamped at
/// `0`, exactly like the global [`HitFeedback`] decay — so the client's hit-pop is
/// brief and deterministic (server and client tick the same timer). It does not
/// remove the component at `0` (a depleted flash is simply invisible client-side and
/// is refreshed in place on the next hit), keeping the system allocation-free. A
/// world with no `DamageFlash` entities is a no-op.
pub fn damage_flash_decay_system(dt: Res<FixedDt>, mut q: Query<&mut DamageFlash>) {
    let dt = dt.0;
    for mut flash in &mut q {
        if flash.0 > 0.0 {
            flash.0 = (flash.0 - dt).max(0.0);
        }
    }
}

/// Fixed-step decay of the per-entity [`ShieldHitFlash`] deflector-shimmer timer —
/// and the directional [`LastShieldHit`] fade timer (FIX 0a) — toward `0` (E007
/// live-demo shield visual).
///
/// Bleeds every entity's `ShieldHitFlash` down by the fixed `dt` each step, clamped
/// at `0`, exactly like [`damage_flash_decay_system`] — so the client's cyan shield
/// flash is a brief, deterministic bloom-and-fade (server and client tick the same
/// timer). The [`LastShieldHit::timer`] is decayed in **lock-step** by the same `dt`
/// (they are refreshed together on a shield-absorbed hit), so the directional flash
/// fades identically; its `dir` is left in place once the timer hits `0` (a depleted
/// flash is simply invisible client-side and is overwritten by the next hit). Neither
/// component is removed at `0`, keeping the system allocation-free. A world with no
/// such entities is a no-op (graceful degradation, INV-D16).
pub fn shield_hit_flash_decay_system(
    dt: Res<FixedDt>,
    mut q: Query<&mut ShieldHitFlash>,
    mut dir_q: Query<&mut LastShieldHit>,
) {
    let dt = dt.0;
    for mut flash in &mut q {
        if flash.0 > 0.0 {
            flash.0 = (flash.0 - dt).max(0.0);
        }
    }
    for mut hit in &mut dir_q {
        if hit.timer > 0.0 {
            hit.timer = (hit.timer - dt).max(0.0);
        }
    }
}

/// Fixed-step ship↔asteroid rams (FR-009/FR-010): on contact, exchange momentum
/// via the closed-form elastic impulse (motion stays sim-authoritative, AD-003);
/// if the closing speed is lethal, deplete the ship's health (destruction is
/// handled by `combat::destruction_system`).
pub fn ram_collision_system(
    tuning: Res<Tuning>,
    // Phase M6: `Option` so a minimal world (no `SimTuning`) degrades to the const ram masses.
    sim: Option<Res<crate::tuning::SimTuning>>,
    mut ship_q: Query<(&Position, &mut Velocity, &mut Health, &CollisionRadius), With<Ship>>,
    mut asteroids: Query<
        (&Position, &mut Velocity, &CollisionRadius, &TargetKind),
        (With<Target>, Without<Ship>),
    >,
) {
    let sim = sim.map(|s| *s).unwrap_or_default();
    let Some((ship_pos, mut ship_vel, mut ship_health, ship_radius)) = ship_q.iter_mut().next()
    else {
        return;
    };
    let ship_pos = ship_pos.0;
    let ship_radius = ship_radius.0;
    let physics = RapierPhysics::new();

    for (apos, mut avel, aradius, kind) in &mut asteroids {
        if *kind != TargetKind::Asteroid {
            continue;
        }
        if physics
            .contact(ship_pos, ship_radius, apos.0, aradius.0)
            .is_some()
        {
            let closing = (ship_vel.0 - avel.0).length();
            let (new_ship, new_ast) = elastic_velocities(
                ship_pos,
                ship_vel.0,
                sim.ship_ram_mass,
                apos.0,
                avel.0,
                sim.asteroid_ram_mass,
            );
            ship_vel.0 = new_ship;
            avel.0 = new_ast;
            if is_lethal_ram(closing, tuning.lethal_ram_speed) {
                ship_health.0 = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec2, b: Vec2, tol: f32) -> bool {
        (a - b).length() <= tol
    }

    #[test]
    fn swept_hits_small_fast_target_no_tunnel() {
        let hit = segment_circle_toi(
            Vec2::new(-100.0, 0.0),
            Vec2::new(100.0, 0.0),
            Vec2::ZERO,
            0.5,
        );
        let t = hit.expect("fast sweep across the circle must register a hit");
        assert!((0.0..=1.0).contains(&t));
        assert!(t < 0.5, "entry should be on the approaching half");
    }

    #[test]
    fn swept_misses_when_path_clears_circle() {
        assert_eq!(
            segment_circle_toi(
                Vec2::new(-100.0, 5.0),
                Vec2::new(100.0, 5.0),
                Vec2::ZERO,
                0.5
            ),
            None
        );
    }

    #[test]
    fn grazing_tangent_counts_as_hit() {
        let r = 1.0;
        assert!(
            segment_circle_toi(Vec2::new(-10.0, r), Vec2::new(10.0, r), Vec2::ZERO, r).is_some()
        );
    }

    #[test]
    fn point_starting_inside_hits_at_zero() {
        assert_eq!(
            segment_circle_toi(Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::ZERO, 1.0),
            Some(0.0)
        );
    }

    #[test]
    fn circle_contact_detects_overlap_and_separation() {
        let c = circle_contact(Vec2::ZERO, 1.0, Vec2::new(1.5, 0.0), 1.0).expect("overlap");
        // Normal points from `b` (at +x) toward `a` (at origin), i.e. -x.
        assert!(close(c.normal, Vec2::new(-1.0, 0.0), 1e-4));
        assert!((c.depth - 0.5).abs() < 1e-4);
        assert_eq!(
            circle_contact(Vec2::ZERO, 1.0, Vec2::new(3.0, 0.0), 1.0),
            None
        );
    }

    #[test]
    fn elastic_equal_mass_headon_conserves_momentum_and_separates() {
        let (pa, va, pb, vb) = (
            Vec2::new(-1.0, 0.0),
            Vec2::new(2.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(-1.0, 0.0),
        );
        let (na, nb) = elastic_velocities(pa, va, 1.0, pb, vb, 1.0);
        assert!(close(va + vb, na + nb, 1e-4));
        let n = (pa - pb).normalize();
        assert!((na - nb).dot(n) >= 0.0);
    }

    #[test]
    fn elastic_separating_bodies_unchanged() {
        let (va, vb) = (Vec2::new(-1.0, 0.0), Vec2::new(1.0, 0.0));
        assert_eq!(
            elastic_velocities(Vec2::new(-1.0, 0.0), va, 1.0, Vec2::new(1.0, 0.0), vb, 1.0),
            (va, vb)
        );
    }

    #[test]
    fn lethal_ram_threshold_is_inclusive() {
        assert!(is_lethal_ram(40.0, 40.0));
        assert!(!is_lethal_ram(39.99, 40.0));
    }

    #[test]
    fn thin_target_still_hit_at_small_radius() {
        // A very small/thin target on a long fast sweep must still register
        // (the no-tunneling guarantee holds down to small radii — CHK028).
        let hit = segment_circle_toi(
            Vec2::new(-200.0, 0.0),
            Vec2::new(200.0, 0.0),
            Vec2::ZERO,
            0.1,
        );
        assert!(hit.is_some(), "swept test must hit even a thin target");
    }
}
