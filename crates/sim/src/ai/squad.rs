//! AI squads (T016–T018, OBJ3): the `Squad`/`SquadOrder`/`FormationDef`
//! hierarchical command layer — a squad brain decides at squad cadence and its
//! members execute via O(1) local steering (TR-009), paced to the slowest
//! essential member with member-death re-derive, squad-of-1 degrade, and a
//! wing parent seam (TR-010).
//!
//! **Cost shape (TR-009)**: decisions are O(squads) — one assignment pass per
//! squad per think (O(members) cheap field writes, no scoring); each member
//! then executes its standing assignment through `ai_execute_system`'s O(1)
//! steering math ([`formation_keep`](crate::ai::steering::formation_keep) /
//! waypoint follow). Total decision cost scales with squad count, not ship
//! count.
//!
//! **AD-004 (steering mix)**: Mid-tier members already execute squad orders as
//! plain order-vector steering — `formation_keep` is a closed-form slot/order
//! vector with (at most) the danger mask layered on top; NO per-member
//! interest-map build happens here. Full 16-slot context maps remain
//! Active-tier individual-brain territory (T007/T025).
//!
//! **Design decisions (documented)**:
//! - **Squad entity carries a `Position`** = the centroid of its members,
//!   updated by [`squad_think_system`] every tick — squads have no body of
//!   their own, but `classify_aoi_system` and the coarse index key off
//!   `Position`, so the centroid is how a squad gets an [`AoiTier`] (data-model
//!   §`AoiTier`: per squad/aggregate entity too).
//! - **Leader designation**: the squad designates `members[0]` — the FIRST
//!   alive member in the spawner-authored, never-reordered member order — as
//!   the formation leader. The leader's brain flies the order goal
//!   ([`Behavior::Waypoint`]); every other member gets
//!   [`Behavior::FormationKeep`] with `leader = members[0]` and its assigned
//!   slot offset. When the leader dies, the prune shifts `members[0]` and the
//!   same-tick re-derive promotes the next member deterministically.
//! - **Pace anchor → throttle cap**: the squad paces to the slowest ESSENTIAL
//!   member (v1: ALL members are essential; the screen/non-essential split
//!   lands with role composition). Member speed = `ShipStats::top_speed()`
//!   (`thrust_force / linear_drag`, the same estimate `classify_archetype`
//!   uses) when fitted, else the base [`Tuning`] world fallback. The leader's
//!   [`AiBrain::throttle_cap`] is set to `anchor_speed / leader_top_speed`
//!   (clamped to `[0, 1]`) so the formation never outruns its anchor;
//!   `ai_execute_system` applies the cap multiplicatively to forward intent.
//! - **Squad-of-1 degrade** (Q6): at `members.len() == 1` the last member's
//!   brain degrades to an INDIVIDUAL goal derived from the order (`MoveTo` →
//!   `Waypoint`, `Engage` → `Engage` + target, else `Hold`), with
//!   `leader = None`, `formation_slot = None`. The squad ENTITY remains —
//!   inert but tracked — the simplest deterministic v1 disposition (no
//!   despawn/dangling-wing edge; the data-model's full despawn-on-degrade is
//!   an acceptable later tightening).
//! - **Empty squad**: `members.len() == 0` despawns the squad entity via
//!   `Commands` (nothing left to command; keeps the world free of zombie
//!   brains).
//! - **No runtime re-clustering** (Q6): membership is spawner-authored and
//!   only ever PRUNED (member death) — never reordered, merged, or re-split.

use bevy_ecs::entity::Entities;
use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::brain::{cadence_for_tier, AiBrain, AiEvent, Behavior, RethinkQueue};
use crate::ai::ident::{phase_bucket, AiIdAllocator, AiStableId};
use crate::ai::lod::{AoiTier, GlideState, Tier};
use crate::ai::role::ScenarioRole;
use crate::ai::tuning::AiTuning;
use crate::clock::CurrentTick;
use crate::components::Position;
use crate::fitting::ShipStats;
use crate::tuning::Tuning;

// ---------------------------------------------------------------------------
// T016 — Squad / SquadOrder / FormationDef
// ---------------------------------------------------------------------------

/// The squad-level decision its members execute via O(1) steering (data-model
/// §`Squad.order`). v1 orders are EXTERNALLY authored (scenario/tests, T033);
/// the squad brain's job is assignment + translation, not order selection.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SquadOrder {
    /// Station-keep: members hold (zero goal).
    #[default]
    Hold,
    /// Fly the formation to a point: leader waypoints there, wingmen keep slot.
    MoveTo(Vec2),
    /// Attack a target: every member gets `Engage` + the target (the OBJ4
    /// engage-source until perception lands, T029; execution arm = T025).
    Engage(Entity),
    /// Re-form on the leader where it stands: leader holds, wingmen keep slot.
    FormUp,
    /// Withdraw the formation to a fallback point (movement-wise a `MoveTo`;
    /// the survival-posture distinction matters from T025 on).
    Withdraw(Vec2),
}

/// Leader-frame formation slot offsets (data-model §`Squad.formation`):
/// `slots[i]` is a body-frame offset from the leader (+X = leader's nose),
/// rotated by the leader's heading at execution time
/// ([`crate::ai::steering::formation_keep`]). Slot 0 is the leader's own spot
/// (the origin) in both built-in constructors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormationDef {
    /// Leader-frame slot offsets; members map to slots by member index.
    pub slots: Vec<Vec2>,
}

impl FormationDef {
    /// A V-shaped wedge of `n` slots, `spacing` apart: slot 0 at the origin
    /// (leader), then alternating port/starboard echelons trailing behind —
    /// slot `i` sits `row = ⌈i/2⌉` rows back at `(-row·spacing, ±row·spacing)`
    /// (odd `i` = port `+Y`, even `i` = starboard `-Y`). Pure integer-derived
    /// arithmetic: deterministic for any `(n, spacing)`.
    pub fn wedge(n: usize, spacing: f32) -> Self {
        let mut slots = Vec::with_capacity(n);
        for i in 0..n {
            if i == 0 {
                slots.push(Vec2::ZERO);
                continue;
            }
            let row = i.div_ceil(2) as f32;
            let side = if i % 2 == 1 { 1.0 } else { -1.0 };
            slots.push(Vec2::new(-row * spacing, side * row * spacing));
        }
        Self { slots }
    }

    /// A line abreast of `n` slots, `spacing` apart: slot 0 at the origin
    /// (leader), then alternating port/starboard on the leader's beam —
    /// slot `i` at `(0, ±⌈i/2⌉·spacing)` (odd = port, even = starboard).
    /// Deterministic for any `(n, spacing)`.
    pub fn line_abreast(n: usize, spacing: f32) -> Self {
        let mut slots = Vec::with_capacity(n);
        for i in 0..n {
            if i == 0 {
                slots.push(Vec2::ZERO);
                continue;
            }
            let row = i.div_ceil(2) as f32;
            let side = if i % 2 == 1 { 1.0 } else { -1.0 };
            slots.push(Vec2::new(0.0, side * row * spacing));
        }
        Self { slots }
    }
}

/// The squad brain — a component on its OWN entity (data-model §`Squad`),
/// spawned by [`spawn_squad`] alongside an [`AiStableId`], an [`AoiTier`] and
/// a centroid [`Position`] (see the module docs). `Clone + Debug`, no
/// `Serialize` (V-9).
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Squad {
    /// Spawner-authored members in stable order (Q6) — decision order. Pruned
    /// on member death (sweep + [`squad_think_system`]), never reordered.
    pub members: Vec<Entity>,
    /// The standing squad-level order (externally set in v1).
    pub order: SquadOrder,
    /// Slowest essential member — the pace source (TR-010). Re-derived on
    /// every squad think and on membership change; pruned on despawn (V-1).
    pub pace_anchor: Option<Entity>,
    /// Parent wing entity (large mixed fleets, TR-010); `None` = independent.
    /// v1 carries the seam only — wing brains land with fleet composition.
    pub wing: Option<Entity>,
    /// Leader-frame slot offsets; member index `i` maps to slot `i` (the
    /// leader holds slot 0 implicitly and flies the order goal instead).
    pub formation: FormationDef,
    /// Cached pace: the anchor's estimated top speed (world units/s).
    pub anchor_speed: f32,
    /// Last tick this squad completed a think (scheduler bookkeeping).
    pub last_think_tick: u64,
    /// Fallback-cadence slot, derived from the squad's [`AiStableId`] (V-4).
    pub phase_bucket: u16,
    /// Member count at the last completed think — the membership-change
    /// detector that forces a same-tick re-derive when a member dies
    /// (lifecycle row `Active → Active'`). Initialized to `u32::MAX` by
    /// [`spawn_squad`] (a count no real squad can have) so the FIRST
    /// `squad_think_system` run after spawn always assigns, independent of
    /// the cadence phase. Pure bookkeeping; nothing on the decision path
    /// reads it beyond the inequality.
    pub last_member_count: u32,
}

/// Spawn a squad entity over `members` (T016): the [`Squad`] component plus an
/// allocated [`AiStableId`] (squads participate in stable ordering + phase
/// buckets like ships do), a default-`Dormant` [`AoiTier`] (squads are
/// AOI-classified too) and a [`Position`] seeded to the members' current
/// centroid (the documented squad-Position decision — kept fresh by
/// [`squad_think_system`] each tick).
///
/// Usable from tests and the server scenario authoring path (T033) alike;
/// inserts a default [`AiIdAllocator`] if the world has none. Members missing
/// a `Position` simply don't contribute to the seed centroid (an empty/
/// position-less member list seeds the origin).
pub fn spawn_squad(
    world: &mut World,
    members: &[Entity],
    formation: FormationDef,
    order: SquadOrder,
) -> Entity {
    let mut sum = Vec2::ZERO;
    let mut counted = 0u32;
    for &m in members {
        if let Some(p) = world.get::<Position>(m) {
            sum += p.0;
            counted += 1;
        }
    }
    let centroid = if counted > 0 {
        sum / counted as f32
    } else {
        Vec2::ZERO
    };
    let bucket_count = world
        .get_resource::<AiTuning>()
        .map_or(AiTuning::default().fallback_bucket_count, |t| {
            t.fallback_bucket_count
        });
    world.init_resource::<AiIdAllocator>();
    let id = world.resource_mut::<AiIdAllocator>().allocate();
    world
        .spawn((
            Squad {
                members: members.to_vec(),
                order,
                pace_anchor: None,
                wing: None,
                formation,
                anchor_speed: 0.0,
                last_think_tick: 0,
                phase_bucket: phase_bucket(id, bucket_count),
                last_member_count: u32::MAX, // Sentinel: first run always thinks.
            },
            id,
            AoiTier::default(),
            Position(centroid),
        ))
        .id()
}

// ---------------------------------------------------------------------------
// T017/T018 — squad brain: assignment, pace anchor, lifecycle
// ---------------------------------------------------------------------------

/// One member's desired brain state under the current squad order — the
/// compare-before-write unit (only a REAL change writes the brain and pushes
/// [`AiEvent::OrderChanged`], so steady-state squads are event-quiet).
struct MemberAssignment {
    behavior: Behavior,
    target: Option<Entity>,
    waypoint: Option<Vec2>,
    leader: Option<Entity>,
    formation_slot: Option<Vec2>,
    throttle_cap: f32,
}

/// The slot offset for member index `i`: direct index when the formation has
/// enough slots, index-modulo reuse on shortfall (deterministic; the V-7
/// in-range guarantee holds because the offset is resolved HERE — brains store
/// the resolved `Vec2`, never an index), origin when the formation is empty.
fn slot_offset(formation: &FormationDef, index: usize) -> Vec2 {
    if formation.slots.is_empty() {
        Vec2::ZERO
    } else {
        formation.slots[index % formation.slots.len()]
    }
}

/// Translate the squad order into member `index`'s assignment (T017).
///
/// Leader (`index == 0`): `MoveTo`/`Withdraw` → fly the goal as a `Waypoint`
/// with the pace throttle cap; `FormUp`/`Hold` → hold position; `Engage` →
/// engage like everyone else. Wingmen: `Engage` → engage; any other order →
/// `FormationKeep` on the leader at their slot offset.
fn member_assignment(
    order: SquadOrder,
    index: usize,
    leader: Entity,
    slot: Vec2,
    leader_cap: f32,
) -> MemberAssignment {
    let idle = MemberAssignment {
        behavior: Behavior::Hold,
        target: None,
        waypoint: None,
        leader: None,
        formation_slot: None,
        throttle_cap: 1.0,
    };
    if let SquadOrder::Engage(target) = order {
        // v1: distribute the target to EVERY member (execution arm = T025;
        // selecting Engage is harmless now — it coasts until then).
        return MemberAssignment {
            behavior: Behavior::Engage,
            target: Some(target),
            ..idle
        };
    }
    if index == 0 {
        // The leader flies the order goal; pace comes from the throttle cap.
        return match order {
            SquadOrder::MoveTo(goal) | SquadOrder::Withdraw(goal) => MemberAssignment {
                behavior: Behavior::Waypoint,
                waypoint: Some(goal),
                throttle_cap: leader_cap,
                ..idle
            },
            // FormUp: the leader IS the form-up point — hold where it stands.
            SquadOrder::FormUp | SquadOrder::Hold => idle,
            SquadOrder::Engage(_) => unreachable!("handled above"),
        };
    }
    match order {
        // Every formation-movement order is FormationKeep for wingmen — the
        // O(1) order-vector execution (AD-004).
        SquadOrder::MoveTo(_) | SquadOrder::Withdraw(_) | SquadOrder::FormUp => MemberAssignment {
            behavior: Behavior::FormationKeep,
            leader: Some(leader),
            formation_slot: Some(slot),
            ..idle
        },
        SquadOrder::Hold => idle,
        SquadOrder::Engage(_) => unreachable!("handled above"),
    }
}

/// The solo degrade (T018, Q6): a squad of ONE translates its order into the
/// last member's own individual goal — `MoveTo` → `Waypoint`, `Engage` →
/// `Engage` + target, else `Hold` — with no leader and no slot (the documented
/// squad-of-1 rule).
fn solo_assignment(order: SquadOrder) -> MemberAssignment {
    let idle = MemberAssignment {
        behavior: Behavior::Hold,
        target: None,
        waypoint: None,
        leader: None,
        formation_slot: None,
        throttle_cap: 1.0,
    };
    match order {
        SquadOrder::MoveTo(goal) => MemberAssignment {
            behavior: Behavior::Waypoint,
            waypoint: Some(goal),
            ..idle
        },
        SquadOrder::Engage(target) => MemberAssignment {
            behavior: Behavior::Engage,
            target: Some(target),
            ..idle
        },
        SquadOrder::Hold | SquadOrder::FormUp | SquadOrder::Withdraw(_) => idle,
    }
}

/// The squad brain (T017/T018, TR-009/TR-010): per squad, in [`AiStableId`]
/// order (V-3), every tick refresh the squad entity's centroid [`Position`];
/// then, when due, run the assignment pass.
///
/// **Due** = membership changed since the last think (a member died — the
/// lifecycle's same-tick re-derive trigger) OR the squad's fallback cadence
/// fires (`(now + phase_bucket) % cadence_for_tier(squad AoiTier) == 0` — the
/// same scheduler discipline as `ai_think_system`; a squad without an
/// [`AoiTier`] runs at the Mid cadence, the squad's nominal band).
///
/// **The think** (all O(members) cheap writes, no scoring — TR-009):
/// 1. prune despawned members (defense in depth; `ai_despawn_sweep_system`
///    already pruned this tick when scheduled — V-1),
/// 2. `len == 0` → despawn the squad entity (documented),
/// 3. re-derive the pace anchor: slowest member by estimated top speed
///    (`ShipStats::top_speed()` or the base [`Tuning`] fallback; ties keep the
///    FIRST member in stable order),
/// 4. `len == 1` → solo degrade ([`solo_assignment`]),
/// 5. else assign: `members[0]` = leader (flies the order, throttle-capped to
///    `anchor_speed / leader_top_speed`), `members[i>0]` = wingman at slot `i`
///    — writing each member's brain ONLY when its assignment actually changed
///    and pushing [`AiEvent::OrderChanged`] for exactly those members (no
///    event spam; the member brains' own think re-scores the same tick since
///    this system runs before `ai_think_system`).
///
/// Registered in the `ScenarioActive`-gated AI set after `classify_aoi_system`
/// (it reads the squad's fresh tier) and before `archetype_refresh_system` /
/// `ai_think_system` (squad orders constrain member brains the same tick). No
/// golden world spawns a `Squad`, so the goldens stay bit-identical.
#[allow(clippy::too_many_arguments)] // One param per seam: resources + the two disjoint queries.
pub fn squad_think_system(
    tuning: Res<AiTuning>,
    base: Res<Tuning>,
    tick: Res<CurrentTick>,
    entities: &Entities,
    mut queue: ResMut<RethinkQueue>,
    mut commands: Commands,
    mut squads: Query<(
        Entity,
        &AiStableId,
        &mut Squad,
        &mut Position,
        Option<&AoiTier>,
        Option<&GlideState>,
    )>,
    mut members: Query<
        (
            &Position,
            Option<&ShipStats>,
            &mut AiBrain,
            // T032 (TR-015) composition: a member carrying a `ScenarioRole`
            // is EXEMPT from the squad's goal assignment — the script
            // outranks the squad order (the role re-asserts its own goal at
            // think time, so writing the squad goal here would only thrash).
            // Centroid + pace-anchor derivation still count roled members.
            Option<&ScenarioRole>,
        ),
        Without<Squad>,
    >,
) {
    let now = tick.0;

    // V-3 stable order (squad ids are unique, so the sort is total).
    let mut order_pass: Vec<(AiStableId, Entity)> =
        squads.iter().map(|(e, id, ..)| (*id, e)).collect();
    order_pass.sort_unstable();

    for (_, squad_entity) in order_pass {
        let Ok((_, _, mut squad, mut squad_pos, aoi, gliding)) = squads.get_mut(squad_entity)
        else {
            continue;
        };

        // Defense-in-depth prune (the V-1 sweep owns the canonical pass).
        if squad.members.iter().any(|m| !entities.contains(*m)) {
            squad.members.retain(|m| entities.contains(*m));
        }

        // Centroid Position upkeep — EVERY tick (the documented decision: the
        // AOI classifier + coarse index see the squad through this Position).
        // Summed in stable member order; write only on a real move. While the
        // squad GLIDES (T019), `glide_motion_system` OWNS this Position
        // (members are projections of it — re-deriving the centroid from them
        // would only feed f32 mean noise back into the bit-exact glide path),
        // so the upkeep is skipped.
        if gliding.is_none() {
            let mut sum = Vec2::ZERO;
            let mut counted = 0u32;
            for &m in &squad.members {
                if let Ok((pos, _, _, _)) = members.get(m) {
                    sum += pos.0;
                    counted += 1;
                }
            }
            if counted > 0 {
                let centroid = sum / counted as f32;
                if squad_pos.0 != centroid {
                    squad_pos.0 = centroid;
                }
            }
        }

        // Due gate: membership change forces a same-tick re-derive (TR-010);
        // otherwise the squad thinks on its tier cadence like any brain.
        let membership_changed = squad.last_member_count != squad.members.len() as u32;
        let tier = aoi.map_or(Tier::Mid, |a| a.tier);
        let cadence = cadence_for_tier(tier, &tuning);
        let cadence_due = (now + u64::from(squad.phase_bucket)).is_multiple_of(cadence);
        if !membership_changed && !cadence_due {
            continue;
        }

        squad.last_think_tick = now;
        squad.last_member_count = squad.members.len() as u32;

        // Empty squad: nothing left to command — despawn (documented).
        if squad.members.is_empty() {
            commands.entity(squad_entity).despawn();
            continue;
        }

        // T018 pace anchor: slowest member (all essential in v1), speed from
        // ShipStats when fitted else the base-Tuning fallback; strict `<` so
        // ties keep the first member in stable order.
        let mut anchor: Option<(Entity, f32)> = None;
        for &m in &squad.members {
            let Ok((_, stats, _, _)) = members.get(m) else {
                continue;
            };
            let speed = stats.map_or(base.top_speed(), ShipStats::top_speed);
            if anchor.is_none_or(|(_, best)| speed < best) {
                anchor = Some((m, speed));
            }
        }
        if let Some((entity, speed)) = anchor {
            squad.pace_anchor = Some(entity);
            squad.anchor_speed = speed;
        }

        // A dangling Engage target degrades the standing order to Hold so the
        // assignment below never re-asserts a despawned Entity into member
        // brains (V-1 spirit; the sweep applies the same rule).
        if let SquadOrder::Engage(t) = squad.order {
            if !entities.contains(t) {
                squad.order = SquadOrder::Hold;
            }
        }

        // Q6 squad-of-1 degrade: the last member becomes an individual brain.
        // (T032: a scenario-roled member keeps its script instead.)
        if squad.members.len() == 1 {
            let m = squad.members[0];
            if let Ok((_, _, mut brain, role)) = members.get_mut(m) {
                if role.is_none() {
                    apply_assignment(m, &mut brain, &solo_assignment(squad.order), &mut queue);
                }
            }
            continue;
        }

        // Assignment pass: leader = members[0], wingmen at slot i (TR-009).
        let leader = squad.members[0];
        let leader_top = members
            .get(leader)
            .map_or(base.top_speed(), |(_, stats, _, _)| {
                stats.map_or(base.top_speed(), ShipStats::top_speed)
            });
        let leader_cap = if leader_top > 0.0 {
            (squad.anchor_speed / leader_top).clamp(0.0, 1.0)
        } else {
            1.0
        };
        for index in 0..squad.members.len() {
            let m = squad.members[index];
            let slot = slot_offset(&squad.formation, index);
            let desired = member_assignment(squad.order, index, leader, slot, leader_cap);
            if let Ok((_, _, mut brain, role)) = members.get_mut(m) {
                // T032: scenario-roled members are exempt (script > squad).
                if role.is_none() {
                    apply_assignment(m, &mut brain, &desired, &mut queue);
                }
            }
        }
    }
}

/// Compare-before-write of one member's assignment: an UNCHANGED assignment is
/// a pure read (no change-detection flag, no event); a changed one rewrites
/// the squad-owned brain fields and pushes exactly one
/// [`AiEvent::OrderChanged`] for the member (T017's no-spam rule).
fn apply_assignment(
    member: Entity,
    brain: &mut Mut<AiBrain>,
    desired: &MemberAssignment,
    queue: &mut RethinkQueue,
) {
    let unchanged = brain.behavior == desired.behavior
        && brain.target == desired.target
        && brain.waypoint == desired.waypoint
        && brain.leader == desired.leader
        && brain.formation_slot == desired.formation_slot
        && brain.throttle_cap == desired.throttle_cap;
    if unchanged {
        return;
    }
    brain.behavior = desired.behavior;
    brain.target = desired.target;
    brain.waypoint = desired.waypoint;
    brain.leader = desired.leader;
    brain.formation_slot = desired.formation_slot;
    brain.throttle_cap = desired.throttle_cap;
    queue.push(member, AiEvent::OrderChanged);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::brain::ai_execute_system;
    use crate::ai::ident::ai_despawn_sweep_system;
    use crate::components::{Heading, Velocity};
    use crate::intent::ShipIntent;
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    // --- helpers -----------------------------------------------------------

    /// A world with every resource the squad seam reads, at tick 0.
    fn squad_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(Tuning::default());
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick(0));
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        // The real registration order: the V-1 sweep prunes BEFORE the squad
        // brain reads/re-derives.
        schedule.add_systems((ai_despawn_sweep_system, squad_think_system).chain());
        (world, schedule)
    }

    fn step(world: &mut World, schedule: &mut Schedule, tick: u64) {
        world.resource_mut::<CurrentTick>().0 = tick;
        schedule.run(world);
    }

    fn spawn_member(world: &mut World, pos: Vec2) -> Entity {
        world
            .spawn((
                Position(pos),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                ShipIntent::default(),
                AiBrain::default(),
            ))
            .id()
    }

    fn brain_of(world: &World, e: Entity) -> AiBrain {
        *world.get::<AiBrain>(e).expect("member carries AiBrain")
    }

    fn squad_of(world: &World, e: Entity) -> Squad {
        world.get::<Squad>(e).expect("squad entity").clone()
    }

    /// A real derived fighter fit (the brain.rs test pattern) as the base for
    /// synthetic stat overrides.
    fn fighter_stats() -> ShipStats {
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{
            build_layout, derive_ship_stats, seed_catalogs, Fit, SlotId, HULL_FIGHTER,
        };
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let layout = build_layout(hull, &fit, &modules);
        derive_ship_stats(hull, &fit, &modules, &layout)
    }

    /// Fighter stats pinned to an exact top speed (drag normalized to 1).
    fn stats_with_top_speed(top_speed: f32) -> ShipStats {
        let mut s = fighter_stats();
        s.linear_drag = 1.0;
        s.thrust_force = top_speed;
        s
    }

    // --- T016: formation constructors --------------------------------------

    /// The wedge/line constructors are deterministic and shaped as documented:
    /// slot 0 = leader origin, alternating port/starboard rows.
    #[test]
    fn formation_constructors_are_deterministic_and_shaped() {
        let w = FormationDef::wedge(5, 10.0);
        assert_eq!(
            w.slots,
            vec![
                Vec2::ZERO,
                Vec2::new(-10.0, 10.0),
                Vec2::new(-10.0, -10.0),
                Vec2::new(-20.0, 20.0),
                Vec2::new(-20.0, -20.0),
            ]
        );
        let l = FormationDef::line_abreast(4, 8.0);
        assert_eq!(
            l.slots,
            vec![
                Vec2::ZERO,
                Vec2::new(0.0, 8.0),
                Vec2::new(0.0, -8.0),
                Vec2::new(0.0, 16.0),
            ]
        );
        // Deterministic: a second construction is identical; n = 0 is empty.
        assert_eq!(w, FormationDef::wedge(5, 10.0));
        assert_eq!(l, FormationDef::line_abreast(4, 8.0));
        assert!(FormationDef::wedge(0, 10.0).slots.is_empty());
    }

    // --- T016/T017: spawn + first think assigns ----------------------------

    /// `spawn_squad` carries the documented components (stable id, AOI tier,
    /// centroid Position) and the first think designates the leader, maps
    /// wingmen to slots, derives the pace anchor, and pushes `OrderChanged`
    /// for every member whose assignment was set.
    #[test]
    fn spawn_and_first_think_assigns_leader_slots_anchor() {
        let (mut world, mut schedule) = squad_world();
        let goal = Vec2::new(200.0, 0.0);
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let m2 = spawn_member(&mut world, Vec2::new(20.0, 0.0));
        let formation = FormationDef::wedge(3, 10.0);
        let se = spawn_squad(
            &mut world,
            &[m0, m1, m2],
            formation.clone(),
            SquadOrder::MoveTo(goal),
        );

        // T016 spawn contract: stable id + AOI tier + centroid Position.
        assert!(
            world.get::<AiStableId>(se).is_some(),
            "squad gets a stable id"
        );
        assert!(
            world.get::<AoiTier>(se).is_some(),
            "squad is AOI-classified"
        );
        assert_eq!(
            world.get::<Position>(se).unwrap().0,
            Vec2::new(10.0, 0.0),
            "squad Position seeded to the member centroid"
        );

        step(&mut world, &mut schedule, 0); // Sentinel forces the first think.

        // Leader = members[0]: flies the goal, no slot, pace cap 1.0 (equal
        // speeds → the anchor ties to the leader itself).
        let lead = brain_of(&world, m0);
        assert_eq!(lead.behavior, Behavior::Waypoint);
        assert_eq!(lead.waypoint, Some(goal));
        assert_eq!(lead.leader, None);
        assert_eq!(lead.formation_slot, None);
        assert_eq!(lead.throttle_cap, 1.0);

        // Wingmen: FormationKeep on the leader at slots 1/2.
        for (m, slot) in [(m1, formation.slots[1]), (m2, formation.slots[2])] {
            let b = brain_of(&world, m);
            assert_eq!(b.behavior, Behavior::FormationKeep);
            assert_eq!(b.leader, Some(m0));
            assert_eq!(b.formation_slot, Some(slot));
            assert_eq!(b.throttle_cap, 1.0);
        }

        // Pace anchor: all members share the base fallback speed → first wins.
        let s = squad_of(&world, se);
        assert_eq!(s.pace_anchor, Some(m0));
        assert_eq!(s.anchor_speed, Tuning::default().top_speed());
        assert_eq!(s.last_think_tick, 0);

        // OrderChanged pushed for exactly the assigned members.
        let queue = world.resource::<RethinkQueue>();
        for m in [m0, m1, m2] {
            assert_eq!(queue.get(m), Some(AiEvent::OrderChanged));
        }

        // Centroid upkeep: move a member, run an off-cadence tick — Position
        // follows even without a think.
        world.get_mut::<Position>(m2).unwrap().0 = Vec2::new(50.0, 30.0);
        step(&mut world, &mut schedule, 1);
        assert_eq!(
            world.get::<Position>(se).unwrap().0,
            Vec2::new(20.0, 10.0),
            "centroid refreshed every tick"
        );
    }

    /// Steady state is event-quiet: a cadence-due re-think with an unchanged
    /// assignment writes nothing and pushes no `OrderChanged` (no spam).
    #[test]
    fn unchanged_assignment_pushes_no_events() {
        let (mut world, mut schedule) = squad_world();
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::wedge(2, 10.0),
            SquadOrder::MoveTo(Vec2::new(100.0, 0.0)),
        );

        step(&mut world, &mut schedule, 0); // First think assigns.
        world.resource_mut::<RethinkQueue>().clear();
        let before = (brain_of(&world, m0), brain_of(&world, m1));

        // Find the next cadence-due tick (squad spawned Dormant, no players →
        // dormant cadence) and re-think there.
        let squad = squad_of(&world, se);
        let cadence = cadence_for_tier(Tier::Dormant, &AiTuning::default());
        let due = (1..=2 * cadence)
            .find(|t| (t + u64::from(squad.phase_bucket)).is_multiple_of(cadence))
            .expect("a due tick exists within two cadence periods");
        step(&mut world, &mut schedule, due);

        assert_eq!(squad_of(&world, se).last_think_tick, due, "think ran");
        assert!(
            world.resource::<RethinkQueue>().is_empty(),
            "unchanged assignment → zero OrderChanged events"
        );
        assert_eq!(
            (brain_of(&world, m0), brain_of(&world, m1)),
            before,
            "brains untouched"
        );
    }

    /// Engage distributes the target to EVERY member (the OBJ4 engage source
    /// until perception lands); FormUp holds the leader and slots the wingmen;
    /// Hold idles everyone.
    #[test]
    fn engage_formup_and_hold_orders_translate() {
        let (mut world, mut schedule) = squad_world();
        let hostile = world.spawn(Position(Vec2::new(500.0, 0.0))).id();
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::line_abreast(2, 10.0),
            SquadOrder::Engage(hostile),
        );

        step(&mut world, &mut schedule, 0);
        for m in [m0, m1] {
            let b = brain_of(&world, m);
            assert_eq!(b.behavior, Behavior::Engage, "Engage on every member");
            assert_eq!(b.target, Some(hostile));
            assert_eq!(b.leader, None);
            assert_eq!(b.formation_slot, None);
        }

        // FormUp: membership is unchanged, so force the re-think via the
        // sentinel (external-order changes apply at the next due think).
        world.get_mut::<Squad>(se).unwrap().order = SquadOrder::FormUp;
        world.get_mut::<Squad>(se).unwrap().last_member_count = u32::MAX;
        step(&mut world, &mut schedule, 1);
        assert_eq!(
            brain_of(&world, m0).behavior,
            Behavior::Hold,
            "leader holds"
        );
        let wing = brain_of(&world, m1);
        assert_eq!(wing.behavior, Behavior::FormationKeep);
        assert_eq!(wing.leader, Some(m0));

        // Hold: everyone idles, squad references cleared.
        world.get_mut::<Squad>(se).unwrap().order = SquadOrder::Hold;
        world.get_mut::<Squad>(se).unwrap().last_member_count = u32::MAX;
        step(&mut world, &mut schedule, 2);
        for m in [m0, m1] {
            let b = brain_of(&world, m);
            assert_eq!(b.behavior, Behavior::Hold);
            assert_eq!((b.target, b.leader, b.formation_slot), (None, None, None));
        }
    }

    // --- T018: pace anchor --------------------------------------------------

    /// The pace anchor is the SLOWEST member (v1: all essential), and the
    /// leader's throttle cap is `anchor_speed / leader_top_speed`.
    #[test]
    fn pace_anchor_is_slowest_member_and_caps_leader() {
        let (mut world, mut schedule) = squad_world();
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let m2 = spawn_member(&mut world, Vec2::new(20.0, 0.0));
        world.entity_mut(m0).insert(stats_with_top_speed(80.0));
        world.entity_mut(m1).insert(stats_with_top_speed(30.0));
        world.entity_mut(m2).insert(stats_with_top_speed(60.0));
        let se = spawn_squad(
            &mut world,
            &[m0, m1, m2],
            FormationDef::wedge(3, 10.0),
            SquadOrder::MoveTo(Vec2::new(300.0, 0.0)),
        );

        step(&mut world, &mut schedule, 0);
        let s = squad_of(&world, se);
        assert_eq!(s.pace_anchor, Some(m1), "slowest member anchors the pace");
        assert_eq!(s.anchor_speed, 30.0);
        assert_eq!(
            brain_of(&world, m0).throttle_cap,
            30.0 / 80.0,
            "leader throttle-capped to the anchor's pace"
        );
        assert_eq!(
            brain_of(&world, m1).throttle_cap,
            1.0,
            "wingmen keep full throttle (formation-keep self-paces)"
        );
    }

    // --- T018: lifecycle ----------------------------------------------------

    /// Member death (mid-run despawn) re-derives slots + the pace anchor and
    /// promotes the next member to leader — pushing `OrderChanged` to the
    /// affected members. The squad does NOT dissolve (Q6).
    #[test]
    fn member_death_rederives_leader_slots_and_anchor() {
        let (mut world, mut schedule) = squad_world();
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let m2 = spawn_member(&mut world, Vec2::new(20.0, 0.0));
        world.entity_mut(m0).insert(stats_with_top_speed(30.0));
        world.entity_mut(m1).insert(stats_with_top_speed(80.0));
        world.entity_mut(m2).insert(stats_with_top_speed(60.0));
        let goal = Vec2::new(300.0, 0.0);
        let formation = FormationDef::wedge(3, 10.0);
        let se = spawn_squad(
            &mut world,
            &[m0, m1, m2],
            formation.clone(),
            SquadOrder::MoveTo(goal),
        );

        step(&mut world, &mut schedule, 0);
        assert_eq!(squad_of(&world, se).pace_anchor, Some(m0), "m0 anchors");
        world.resource_mut::<RethinkQueue>().clear();

        // Kill the leader (also the anchor): the same-tick membership-change
        // re-derive must promote m1 and re-anchor — off-cadence (tick 1).
        world.despawn(m0);
        step(&mut world, &mut schedule, 1);

        let s = squad_of(&world, se);
        assert_eq!(s.members, vec![m1, m2], "pruned, order preserved");
        assert_eq!(
            s.pace_anchor,
            Some(m2),
            "anchor re-derived to the new slowest"
        );
        assert_eq!(s.anchor_speed, 60.0);
        assert_eq!(s.last_think_tick, 1, "membership change forced the think");

        let new_lead = brain_of(&world, m1);
        assert_eq!(
            new_lead.behavior,
            Behavior::Waypoint,
            "m1 promoted to leader"
        );
        assert_eq!(new_lead.waypoint, Some(goal));
        assert_eq!(
            new_lead.leader, None,
            "the V-1 sweep cleared the dead leader"
        );
        assert_eq!(
            new_lead.throttle_cap,
            60.0 / 80.0,
            "re-paced to the new anchor"
        );

        let wing = brain_of(&world, m2);
        assert_eq!(wing.behavior, Behavior::FormationKeep);
        assert_eq!(wing.leader, Some(m1), "re-slotted onto the new leader");
        assert_eq!(
            wing.formation_slot,
            Some(formation.slots[1]),
            "slot 2 → slot 1"
        );

        let queue = world.resource::<RethinkQueue>();
        assert_eq!(queue.get(m1), Some(AiEvent::OrderChanged));
        assert_eq!(queue.get(m2), Some(AiEvent::OrderChanged));
    }

    /// Squad-of-1 degrade (Q6): the last member becomes an individual brain
    /// (order-derived goal, no leader/slot); the squad entity REMAINS.
    #[test]
    fn squad_of_one_degrades_to_individual_brain() {
        let (mut world, mut schedule) = squad_world();
        let goal = Vec2::new(150.0, 0.0);
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0));
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::wedge(2, 10.0),
            SquadOrder::MoveTo(goal),
        );
        step(&mut world, &mut schedule, 0);
        assert_eq!(brain_of(&world, m1).behavior, Behavior::FormationKeep);

        world.despawn(m0);
        step(&mut world, &mut schedule, 1);

        let b = brain_of(&world, m1);
        assert_eq!(
            b.behavior,
            Behavior::Waypoint,
            "MoveTo degrades to Waypoint"
        );
        assert_eq!(b.waypoint, Some(goal));
        assert_eq!((b.leader, b.formation_slot), (None, None));
        assert_eq!(b.throttle_cap, 1.0, "solo flies at its own pace");
        assert!(
            world.entities().contains(se),
            "the squad entity remains (inert, still tracked)"
        );
        assert_eq!(squad_of(&world, se).members, vec![m1]);
    }

    /// An EMPTY squad (last member died) despawns its squad entity.
    #[test]
    fn empty_squad_despawns_squad_entity() {
        let (mut world, mut schedule) = squad_world();
        let m0 = spawn_member(&mut world, Vec2::ZERO);
        let se = spawn_squad(
            &mut world,
            &[m0],
            FormationDef::wedge(1, 10.0),
            SquadOrder::Hold,
        );
        step(&mut world, &mut schedule, 0);
        assert!(world.entities().contains(se));

        world.despawn(m0);
        step(&mut world, &mut schedule, 1);
        assert!(
            !world.entities().contains(se),
            "len == 0 → the squad entity is despawned"
        );
    }

    /// The V-1 sweep prunes squad state the tick a referent despawns: dead
    /// members leave `members`, a dead anchor/wing clears, and a dangling
    /// `Engage` order degrades to `Hold`.
    #[test]
    fn despawn_sweep_prunes_squad_references() {
        let mut world = World::new();
        let live = world.spawn_empty().id();
        let dead_member = world.spawn_empty().id();
        let dead_anchor = world.spawn_empty().id();
        let dead_wing = world.spawn_empty().id();
        let dead_target = world.spawn_empty().id();
        let se = world
            .spawn(Squad {
                members: vec![live, dead_member],
                order: SquadOrder::Engage(dead_target),
                pace_anchor: Some(dead_anchor),
                wing: Some(dead_wing),
                formation: FormationDef::wedge(2, 10.0),
                anchor_speed: 0.0,
                last_think_tick: 0,
                phase_bucket: 0,
                last_member_count: 2,
            })
            .id();
        for e in [dead_member, dead_anchor, dead_wing, dead_target] {
            world.despawn(e);
        }

        let mut schedule = Schedule::default();
        schedule.add_systems(ai_despawn_sweep_system);
        schedule.run(&mut world);

        let s = squad_of(&world, se);
        assert_eq!(s.members, vec![live], "dead member pruned (V-1)");
        assert_eq!(s.pace_anchor, None, "dead anchor cleared");
        assert_eq!(s.wing, None, "dead wing cleared");
        assert_eq!(
            s.order,
            SquadOrder::Hold,
            "dangling Engage degrades to Hold"
        );
    }

    // --- T017: throttle cap in execution ------------------------------------

    /// `ai_execute_system` applies `throttle_cap` multiplicatively to forward
    /// intent: a capped leader burns at exactly `cap ×` the uncapped intent.
    #[test]
    fn throttle_cap_scales_forward_intent_in_execute() {
        let mut world = World::new();
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(ai_execute_system);

        let goal = Vec2::new(500.0, 0.0);
        let brain = AiBrain {
            behavior: Behavior::Waypoint,
            waypoint: Some(goal),
            ..AiBrain::default()
        };
        let uncapped = world
            .spawn((
                Position(Vec2::ZERO),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                ShipIntent::default(),
                brain,
            ))
            .id();
        let capped = world
            .spawn((
                Position(Vec2::ZERO),
                Velocity(Vec2::ZERO),
                Heading(0.0),
                ShipIntent::default(),
                AiBrain {
                    throttle_cap: 0.5,
                    ..brain
                },
            ))
            .id();

        schedule.run(&mut world);
        let full = world.get::<ShipIntent>(uncapped).unwrap().forward;
        let half = world.get::<ShipIntent>(capped).unwrap().forward;
        assert!(full > 0.9, "goal dead ahead → near-full burn (got {full})");
        assert_eq!(half, full * 0.5, "cap applied multiplicatively to forward");
    }
}
