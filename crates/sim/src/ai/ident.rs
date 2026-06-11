//! Sim-stable identity for AI entities (T003, TR-004/TR-005).
//!
//! Deterministic phase buckets and cross-entity tiebreaks must NOT derive from
//! `Entity` bits: Bevy reuses entity indices after despawn, so an index-keyed
//! bucket would differ between two runs whose spawn/despawn interleaving
//! differs only transiently (data-model V-4). Instead every AI-driven entity
//! gets an [`AiStableId`] from the monotonic [`AiIdAllocator`] at spawn time —
//! never reused, identical across re-runs because the spawn path itself is
//! deterministic. [`phase_bucket`] then spreads those ids across scheduler
//! buckets via the same SplitMix64 mix the turret jitter uses (no RNG crate).
//!
//! This module also owns [`ai_despawn_sweep_system`] — the V-1 prune seam that
//! runs FIRST in the AI system set so no later AI system (perception, squad,
//! brain) ever reads a dangling `Entity` the tick its referent despawned.

use bevy_ecs::entity::Entities;
use bevy_ecs::prelude::*;

use crate::ai::brain::AiBrain;
use crate::ai::command::{OrderKind, PlayerOrder};
use crate::ai::perception::{ContactList, SensorNetworks};
use crate::ai::squad::{Squad, SquadOrder};
use crate::turret::splitmix64;

/// Monotonic spawn-order identity for an AI-driven entity (data-model V-4).
///
/// Assigned once at spawn from [`AiIdAllocator::allocate`]; never reused. All
/// deterministic cross-entity orderings (phase buckets, sorted collections,
/// utility tiebreaks) key off this — never `Entity` index/generation bits.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AiStableId(pub u64);

/// World resource handing out [`AiStableId`]s in spawn order (0, 1, 2, …).
///
/// Inserted at world construction (`ServerApp::new`) so it exists in both the
/// headless and windowed authoritative worlds; inert until a spawn path
/// allocates from it, so non-AI worlds are byte-identical to before.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AiIdAllocator {
    /// The next id to hand out. Strictly increasing; never rolled back.
    pub next: u64,
}

impl AiIdAllocator {
    /// Hand out the next spawn-order id and advance the counter.
    pub fn allocate(&mut self) -> AiStableId {
        let id = AiStableId(self.next);
        self.next += 1;
        id
    }
}

/// Deterministic scheduler bucket for a stable id: `splitmix64(id) % buckets`.
///
/// Well-mixed (consecutive spawn ids land in spread-out buckets, so a wave of
/// ships spawned together doesn't all re-think on the same tick) yet fully
/// deterministic — same id + bucket count always yields the same bucket.
/// `bucket_count == 0` is degenerate (no cadence) and maps to bucket 0 rather
/// than panicking on a modulo-by-zero.
pub fn phase_bucket(id: AiStableId, bucket_count: u32) -> u16 {
    if bucket_count == 0 {
        return 0;
    }
    (splitmix64(id.0) % bucket_count as u64) as u16
}

/// V-1 prune seam: drops dangling `Entity` references held by AI state the
/// same tick the referent despawns, BEFORE any other AI system runs.
///
/// Ordering contract (data-model V-1): this system is registered FIRST in the
/// `ScenarioActive`-gated AI set in `add_fixed_step_systems`, ahead of
/// LOD/perception/squad/brain/steering — so no AI system ever reads a stale
/// `Entity` within the tick it died.
///
/// Prune passes land here per owning task; implemented so far:
/// - **T010/T011**: clear [`AiBrain::target`] / [`AiBrain::leader`] when the
///   referenced entity no longer exists (a leaderless `FormationKeep`/`Follow`
///   brain then degrades at its next think — its candidate vanishes).
/// - **T016/T018**: prune dead entries from [`Squad::members`] (order
///   preserved, never reordered — Q6) and clear a dead
///   [`Squad::pace_anchor`] / [`Squad::wing`]; a dangling
///   [`SquadOrder::Engage`] target degrades the standing order to
///   [`SquadOrder::Hold`] so the squad brain never re-asserts a despawned
///   `Entity` into member brains (V-1). The squad-of-1 degrade / empty-squad
///   despawn REACTIONS live in `squad_think_system`, which a membership
///   change forces to run the same tick.
/// - **T029/T030**: prune dead targets from every [`ContactList`] and dead
///   members/fused targets from the [`SensorNetworks`] pictures (V-1: no AI
///   system reads a dangling contact the tick its referent despawns). The
///   resource is `Option`al (graceful degradation in minimal test worlds) and
///   only written when something dangling was actually found.
/// - **R99 Phase A**: a [`PlayerOrder`] whose `kind` is
///   [`OrderKind::Attack`]`(t)` for a despawned `t` has its `kind` cleared to
///   `None` (settings-only) — the user's STYLE overrides survive, but the ship
///   stops chasing the ghost target. The whole component is kept (the user's
///   pacing/stance/posture stand) — only the dangling attack command is dropped.
///
/// Golden safety: in a world with no `AiBrain`/`Squad`/`ContactList` entities
/// (every golden world, including the `demo_enemies_smoke` Sandbox where
/// `ScenarioActive` IS present) all queries are empty, the `SensorNetworks`
/// map stays empty, and this remains a true no-op. Field READS go through
/// `Deref` (no change-detection flag); state is only marked changed when a
/// dangling reference is actually cleared.
pub fn ai_despawn_sweep_system(
    entities: &Entities,
    mut brains: Query<&mut AiBrain>,
    mut squads: Query<&mut Squad>,
    mut contact_lists: Query<&mut ContactList>,
    mut player_orders: Query<&mut PlayerOrder>,
    networks: Option<ResMut<SensorNetworks>>,
) {
    for mut brain in &mut brains {
        if brain.target.is_some_and(|t| !entities.contains(t)) {
            brain.target = None;
        }
        if brain.leader.is_some_and(|l| !entities.contains(l)) {
            brain.leader = None;
        }
    }
    // R99 Phase A (V-1): a dangling `PlayerOrder::Attack(t)` clears to
    // settings-only (kind = None) so the ship stops engaging the ghost while the
    // user's style overrides survive. Same dangling-Entity check as the brain.
    for mut order in &mut player_orders {
        if matches!(order.kind, Some(OrderKind::Attack(t)) if !entities.contains(t)) {
            order.kind = None;
        }
    }
    for mut squad in &mut squads {
        if squad.members.iter().any(|m| !entities.contains(*m)) {
            squad.members.retain(|m| entities.contains(*m));
        }
        if squad.pace_anchor.is_some_and(|a| !entities.contains(a)) {
            squad.pace_anchor = None;
        }
        if squad.wing.is_some_and(|w| !entities.contains(w)) {
            squad.wing = None;
        }
        if let SquadOrder::Engage(t) = squad.order {
            if !entities.contains(t) {
                squad.order = SquadOrder::Hold;
            }
        }
    }
    // T029 (V-1): drop contacts whose target despawned this tick.
    for mut list in &mut contact_lists {
        if list.contacts.iter().any(|c| !entities.contains(c.target)) {
            list.contacts.retain(|c| entities.contains(c.target));
        }
    }
    // T030 (V-1): prune dead members + dangling fused targets from the
    // per-faction network pictures (guarded: untouched when nothing dangles).
    if let Some(mut nets) = networks {
        let dangling = nets.by_faction.values().flatten().any(|nc| {
            nc.members.iter().any(|m| !entities.contains(*m))
                || nc.fused.iter().any(|c| !entities.contains(c.target))
        });
        if dangling {
            for comps in nets.by_faction.values_mut() {
                for nc in comps {
                    nc.members.retain(|m| entities.contains(*m));
                    nc.fused.retain(|c| entities.contains(c.target));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_is_monotonic_from_zero() {
        let mut alloc = AiIdAllocator::default();
        assert_eq!(alloc.allocate(), AiStableId(0));
        assert_eq!(alloc.allocate(), AiStableId(1));
        assert_eq!(alloc.allocate(), AiStableId(2));
        assert_eq!(alloc.next, 3, "counter advances past every handout");
    }

    #[test]
    fn phase_bucket_is_deterministic_and_in_range() {
        for raw in [0u64, 1, 7, 42, u64::MAX] {
            let id = AiStableId(raw);
            let b = phase_bucket(id, 16);
            assert_eq!(b, phase_bucket(id, 16), "same id → same bucket (id {raw})");
            assert!(b < 16, "bucket within range (id {raw} → {b})");
        }
    }

    #[test]
    fn phase_bucket_spreads_ids_across_buckets() {
        // 64 consecutive spawn ids over 16 buckets: SplitMix64 should mix them
        // well past any trivial clustering. ≥8 distinct buckets is the bar
        // (every-bucket-nonempty would over-constrain the hash).
        let mut seen = std::collections::BTreeSet::new();
        for raw in 0u64..64 {
            seen.insert(phase_bucket(AiStableId(raw), 16));
        }
        assert!(
            seen.len() >= 8,
            "expected ≥8 distinct buckets, got {} ({seen:?})",
            seen.len()
        );
    }

    #[test]
    fn phase_bucket_zero_count_is_zero_not_panic() {
        assert_eq!(phase_bucket(AiStableId(0), 0), 0);
        assert_eq!(phase_bucket(AiStableId(u64::MAX), 0), 0);
    }

    /// V-1: the sweep clears `AiBrain.target`/`leader` the tick their referent
    /// despawns — and leaves live references untouched.
    #[test]
    fn despawn_sweep_prunes_dangling_brain_refs() {
        let mut world = World::new();
        let doomed_target = world.spawn_empty().id();
        let doomed_leader = world.spawn_empty().id();
        let live_ref = world.spawn_empty().id();
        let dangling = world
            .spawn(AiBrain {
                target: Some(doomed_target),
                leader: Some(doomed_leader),
                ..AiBrain::default()
            })
            .id();
        let healthy = world
            .spawn(AiBrain {
                target: Some(live_ref),
                leader: Some(live_ref),
                ..AiBrain::default()
            })
            .id();
        world.despawn(doomed_target);
        world.despawn(doomed_leader);

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(ai_despawn_sweep_system);
        schedule.run(&mut world);

        let pruned = world.get::<AiBrain>(dangling).unwrap();
        assert_eq!(pruned.target, None, "dangling target cleared (V-1)");
        assert_eq!(pruned.leader, None, "dangling leader cleared (V-1)");
        let kept = world.get::<AiBrain>(healthy).unwrap();
        assert_eq!(kept.target, Some(live_ref), "live target kept");
        assert_eq!(kept.leader, Some(live_ref), "live leader kept");
    }

    #[test]
    fn despawn_sweep_runs_as_a_real_system() {
        // The sweep must be schedulable + a true no-op: run it in a world and
        // assert nothing changed (entity count, allocator state).
        let mut world = World::new();
        world.insert_resource(AiIdAllocator::default());
        world.spawn(AiStableId(0));
        let before = world.entities().len();

        let mut schedule = bevy_ecs::schedule::Schedule::default();
        schedule.add_systems(ai_despawn_sweep_system);
        schedule.run(&mut world);

        assert_eq!(world.entities().len(), before, "no-op: no entity mutation");
        assert_eq!(
            *world.resource::<AiIdAllocator>(),
            AiIdAllocator::default(),
            "no-op: no resource mutation"
        );
    }
}
