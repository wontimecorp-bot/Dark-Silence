//! Collision: pure math (swept point-vs-circle CCD, circle–circle contact, the
//! closed-form elastic 2-body impulse, the lethal-ram threshold) plus the ECS
//! systems that apply them — projectile hits and ship↔asteroid rams.
//!
//! Glam-only math, deterministic, engine-agnostic — the authoritative collision
//! math behind the `Physics` trait (ADR-0004). Same inputs → same outputs, so
//! there is never per-frame flicker.

use crate::clock::FixedDt;
use crate::combat::{self, HitFeedback};
use crate::components::{
    CollisionRadius, Damage, DamageFlash, Heading, Health, LastShieldHit, Position, PrevPosition,
    Projectile, ProjectileOwner, ShieldHitFlash, Ship, Target, TargetKind, Velocity,
};
use crate::damage::{
    apply_damage, on_section_destroyed, shatter_ship, DamageEvent, HitKind, HullStructure, Wreck,
};
use crate::fitting::{Fit, FitLayout, HullCatalog, SectionId};
use crate::physics::{Physics, RapierPhysics, SweptHit};
use crate::tuning::Tuning;
use crate::weapon::{damage_event_from_hit, WeaponSource};
use bevy_ecs::prelude::*;
use glam::Vec2;

/// Ship inertial mass for ram impulses (asteroids are heavier, so the ship
/// bounces more).
const SHIP_MASS: f32 = 1.0;
/// Asteroid inertial mass for ram impulses.
const ASTEROID_MASS: f32 = 6.0;

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
    projectiles: Query<(Entity, &Position, &PrevPosition, &Damage), With<Projectile>>,
    mut targets: Query<
        (&Position, &CollisionRadius, &mut Health),
        (With<Target>, Without<FitLayout>),
    >,
) {
    let physics = RapierPhysics::new();
    for (projectile, pos, prev, dmg) in &projectiles {
        for (tpos, radius, mut health) in &mut targets {
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
}

/// Build the hull-local entry ray for a world-space projectile hit on a fitted
/// target (the local-space transform the E007 pipeline expects, T038).
///
/// `apply_damage`/`resolve_entry_point` trace `ev.point → ev.point + ev.dir·REACH`
/// across the target's **hull-local cell-space** (cells at integer coords, centers
/// at `coord + 0.5`). This maps the world hit into that space:
///
/// 1. **Direction**: rotate the projectile's world travel direction `world_dir`
///    into the hull frame by `-heading` (the inverse ship rotation), giving the
///    incoming `dir_local`.
/// 2. **Entry point**: place it on the far side of the grid **opposite** the
///    incoming direction, through the grid center, so the local ray sweeps the
///    whole chassis along `dir_local` and lands on the first occupied cell it
///    crosses (mirroring the `REACH`-extended sweep `resolve_entry_point` does).
///    `grid_center = (cols, rows)/2`; the entry sits `span` back along `-dir_local`
///    where `span` over-covers the grid diagonal.
///
/// This is the **simplest correct** transform for the e2e (the brief's documented
/// option): it does not depend on the world hit's offset within the target circle
/// (which the coarse unit-cell grid cannot resolve sub-cell anyway), only on the
/// incoming direction, so a shot fired *at* a fitted ship reliably resolves to a
/// real module along its flight line. World scale ↔ local cell scale is decoupled:
/// the local ray is constructed in cell-space directly.
fn hull_local_entry_ray(world_dir: Vec2, heading: f32, grid_dims: (u16, u16)) -> (Vec2, Vec2) {
    let dir_local = if world_dir.length_squared() > f32::EPSILON {
        // Inverse ship rotation: world → hull-local.
        Vec2::from_angle(-heading)
            .rotate(world_dir)
            .normalize_or_zero()
    } else {
        Vec2::ZERO
    };
    let grid_center = Vec2::new(grid_dims.0 as f32 * 0.5, grid_dims.1 as f32 * 0.5);
    // Over-cover the grid diagonal so the entry sits fully outside the chassis on
    // the incoming side; the ray then crosses the centre along `dir_local`.
    let span = (grid_dims.0.max(grid_dims.1) as f32) + 2.0;
    let point = grid_center - dir_local * span;
    (point, dir_local)
}

/// Map a struck cell to the [`SectionId`] it belongs to on `ship`'s hull (the same
/// hull lookup `on_section_destroyed` uses, T038), so a destroyed module triggers
/// its section's destruction chain. `None` for a cell the hull never authored.
fn section_of_struck_cell(world: &World, ship: Entity, cell: (u16, u16)) -> Option<SectionId> {
    let fit = world.get::<Fit>(ship)?;
    let hulls = world.get_resource::<HullCatalog>()?;
    let hull = hulls.get(fit.hull)?;
    hull.cells
        .iter()
        .find(|gc| gc.coord == cell)
        .map(|gc| gc.section)
}

/// Fixed-step per-module damage for **fitted** targets (E007, T038/T039;
/// FR-001/021, INV-D16/D17) — the exclusive-`&mut World` successor to the legacy
/// flat-`Health` path for ships carrying a [`FitLayout`].
///
/// It runs the **same** swept-ray CCD as [`collision_detect_system`]
/// ([`RapierPhysics::swept_cast`] — no new geometry, no tunnel, FR-021) but routes
/// each hit through the E007 [`apply_damage`] pipeline (Shields → Armor → Hull →
/// Systems, per-module health). It is exclusive because `apply_damage` needs
/// `&mut World`; the projectile/target sweep is therefore collected **first** (the
/// query borrow released) and the mutations applied after.
///
/// Steps:
/// 1. **Sweep** every `(Projectile, WeaponSource)` vs every fitted target
///    (`With<FitLayout>`), skipping self-hits via [`ProjectileOwner`] (E002), and
///    record the **first** target struck per projectile (lowest `toi`).
/// 2. For each recorded hit: transform the world hit into the target's hull-local
///    cell-space ([`hull_local_entry_ray`]), build the [`DamageEvent`]
///    ([`damage_event_from_hit`]), and apply it ([`apply_damage`]). Set the hit/
///    destroy flash + the legibility tag ([`HitFeedback::last_kind`], FR-024) and
///    despawn the projectile.
/// 3. **Destruction trigger**: if the outcome destroyed a struck module, map its
///    cell → [`SectionId`] and call [`on_section_destroyed`] (connectivity → sever
///    → wreck → salvage). MVP coupling: a destroyed module destroys its section
///    (coarse, documented).
/// 4. **Hull-death trigger (live-combat death)**: AFTER the per-module destruction
///    block, if the target still exists, is not already a [`Wreck`], and its
///    [`HullStructure::current`] has reached `0`, call [`shatter_ship`] and raise the
///    kill flash. This is the additive death trigger that makes sustained fire
///    reliably DESTROY a fitted enemy: on a head-on shot the entry module is the
///    centre cell with nothing behind it, so penetrating damage spills into
///    `HullStructure` and almost never kills a *module* cell (so step 3 rarely
///    fires) — without this the enemy sits at `0` hull forever, alive and intact.
///    It does NOT touch [`apply_damage`]'s core routing or the E007 invariant tests.
///
/// **Graceful degradation (INV-D16)**: a world with no fitted targets (the E002/
/// E003/determinism worlds) records no hits and is a no-op; `apply_damage` /
/// `on_section_destroyed` themselves bail (return `NoModule` / no-op) when an E007
/// resource or catalog is absent, so a world missing them never panics. Server-
/// authoritative — only the server runs this; the client predicts the same path.
pub fn fitted_damage_system(world: &mut World) {
    // --- 1. Collect hits (query borrow released before any &mut World use) -------
    let physics = RapierPhysics::new();
    let mut hits: Vec<FittedHit> = Vec::new();

    // Snapshot the fitted targets once (Entity, world pos, radius, heading, grid).
    let mut target_q = world.query_filtered::<(
        Entity,
        &Position,
        &CollisionRadius,
        &Heading,
        &FitLayout,
        &Fit,
    ), With<FitLayout>>();
    // Resolve each fitted target's grid dims from its hull (so the local ray spans
    // the chassis); a target whose hull is unresolvable is skipped (no panic).
    let hull_dims: std::collections::BTreeMap<Entity, (u16, u16)> = {
        let hulls = world.get_resource::<HullCatalog>();
        target_q
            .iter(world)
            .filter_map(|(e, _, _, _, _, fit)| {
                hulls
                    .and_then(|h| h.get(fit.hull))
                    .map(|hull| (e, hull.grid_dims))
            })
            .collect()
    };
    let targets: Vec<(Entity, Vec2, f32, f32)> = target_q
        .iter(world)
        .map(|(e, p, r, h, _, _)| (e, p.0, r.0, h.0))
        .collect();

    let mut proj_q = world.query_filtered::<(
        Entity,
        &Position,
        &PrevPosition,
        &Velocity,
        &Damage,
        &WeaponSource,
        Option<&ProjectileOwner>,
    ), With<Projectile>>();
    for (projectile, pos, prev, vel, dmg, src, owner) in proj_q.iter(world) {
        let owner_e = owner.map(|o| o.0);
        // First target struck along the sweep (lowest toi), skipping self-hits.
        // Carry the target heading (for the local-ray transform) AND the target's
        // world centre `tpos` (for the shield-impact direction, FIX 0a) so the apply
        // phase has both without re-querying.
        let mut best: Option<(Entity, f32, Vec2, SweptHit)> = None;
        for &(target, tpos, radius, heading) in &targets {
            if owner_e == Some(target) {
                continue; // self-hit prevention (E002 ProjectileOwner)
            }
            let Some(hit) = physics.swept_cast(prev.0, pos.0, tpos, radius) else {
                continue;
            };
            let take = match best {
                None => true,
                Some((_, _, _, ref b)) => hit.toi < b.toi,
            };
            if take {
                best = Some((target, heading, tpos, hit));
            }
        }
        if let Some((target, heading, tpos, hit)) = best {
            let Some(&dims) = hull_dims.get(&target) else {
                // Hull unresolvable for this target: skip (no panic, INV-D16).
                continue;
            };
            // FIX 0a: the world impact direction from the ship centre to the swept hit
            // point — the data `update_shield_bubble` flashes at. Fall back to the
            // reverse shot direction (`-vel`) when the centre→impact vector is
            // degenerate (e.g. a hit at the exact centre), so the flash always has a
            // sensible facing.
            let shield_dir = {
                let from_centre = (hit.point - tpos).normalize_or_zero();
                if from_centre != Vec2::ZERO {
                    from_centre
                } else {
                    (-vel.0).normalize_or_zero()
                }
            };
            let (point, dir_local) = hull_local_entry_ray(vel.0, heading, dims);
            let local_hit = SweptHit {
                toi: hit.toi,
                point,
            };
            let event = damage_event_from_hit(&local_hit, src, dmg.0, dir_local, owner_e);
            hits.push(FittedHit {
                projectile,
                target,
                event,
                shield_dir,
            });
        }
    }

    // --- 2. Apply each hit through the E007 pipeline (exclusive &mut World) -------
    for FittedHit {
        projectile,
        target,
        event,
        shield_dir,
    } in hits
    {
        let outcome = apply_damage(world, target, event);

        // Feedback (FR-024): flash + the legibility tag the HUD reads.
        if let Some(mut feedback) = world.get_resource_mut::<HitFeedback>() {
            feedback.hit_flash = combat::FLASH_TIME;
            feedback.last_kind = Some(outcome.result);
            if outcome.destroyed {
                feedback.destroy_flash = combat::FLASH_TIME;
            }
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

        // --- 3. Destruction trigger (the chain T040 exercises) -------------------
        // A destroyed module destroys its section (MVP coupling): map the struck
        // cell → SectionId and run connectivity → sever → wreck → salvage.
        if outcome.destroyed {
            if let Some(struck) = outcome.struck {
                let cell = world.get::<FitLayout>(target).and_then(|l| {
                    l.cells
                        .iter()
                        .find(|(_, occ)| {
                            occ.slot == struck.slot && occ.module == Some(struck.module)
                        })
                        .map(|(&c, _)| c)
                });
                if let Some(cell) = cell {
                    if let Some(section) = section_of_struck_cell(world, target, cell) {
                        on_section_destroyed(world, target, section);
                    }
                }
            }
        }

        // --- 4. Hull-death trigger (the live-combat death the demo needs) ----------
        // On a head-on shot the entry module is the centre cell with nothing behind
        // it, so penetrating damage spills into HullStructure and almost never kills
        // a module cell — so step 3 rarely fires. When sustained fire finally drains
        // HullStructure to 0, nothing else destroys the ship, so it would sit at 0
        // hull forever (alive + intact). Shatter it here: if the target still exists,
        // isn't already a Wreck, and its structural backstop is depleted, break it
        // apart (sheds chunks → persistent wreck) and raise the kill flash.
        let depleted = world
            .get::<HullStructure>(target)
            .is_some_and(|hs| hs.current <= 0.0);
        let already_wreck = world.get::<Wreck>(target).is_some();
        if depleted && !already_wreck && world.get_entity(target).is_ok() {
            shatter_ship(world, target);
            if let Some(mut feedback) = world.get_resource_mut::<HitFeedback>() {
                feedback.destroy_flash = combat::FLASH_TIME;
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
    mut ship_q: Query<(&Position, &mut Velocity, &mut Health, &CollisionRadius), With<Ship>>,
    mut asteroids: Query<
        (&Position, &mut Velocity, &CollisionRadius, &TargetKind),
        (With<Target>, Without<Ship>),
    >,
) {
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
                SHIP_MASS,
                apos.0,
                avel.0,
                ASTEROID_MASS,
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
