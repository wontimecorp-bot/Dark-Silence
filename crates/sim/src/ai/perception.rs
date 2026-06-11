//! AI perception (T029, TR-005/TR-013) + faction sensor networks (T030,
//! TR-014): per-ship [`ContactList`] tier-cadence signature-gated scans, and
//! the per-faction [`SensorNetworks`] fused picture built by a TX flood-fill
//! over datalink adjacency (the sever-logic connected-component pattern,
//! IP-007).
//!
//! **Scan candidate source (documented choice)**: the fine broadphase
//! `SpatialHash` is built per-tick LOCALLY inside the collision systems — it
//! is not a world Resource — so every scan tier queries the coarse interest
//! grid ([`CoarseIndex`], AD-002) and applies an EXACT distance filter over
//! the conservative candidate superset. Same result set as a fine-grid radius
//! query (the coarse `near` never false-negatives), just a slightly larger
//! candidate sweep — bounded at the default `base_sensor_range` 200 to a
//! ~4×4-coarse-cell neighborhood.
//!
//! **V-8 separation (TR-013)**: perception is signature-gated at EVERY tier
//! and never writes [`AoiTier`] — the sim-LOD tier scales only the scan
//! *cadence* (how often a ship looks), never the *detection decision* (what it
//! can see). This module has no write access to `AoiTier` at all.
//!
//! **Determinism (V-3)**: contact lists are `Vec`s kept sorted by target
//! `Entity` bits (binary-search upsert) — the live contact SET at any tick is
//! identical across identical runs (spawn order is deterministic), so the
//! bits ordering reproduces exactly; `AiStableId` is not used as the sort key
//! because contact targets (player ships, scenario targets) need not carry
//! one. Scanner iteration order is immaterial: each scanner mutates only its
//! own `ContactList`/`AiBrain` and pushes only its own [`RethinkQueue`] key
//! (a `BTreeMap`, coalescing). The network rebuild snapshots members, sorts
//! them by entity bits, flood-fills in that stable order, and stores results
//! in a `BTreeMap` keyed by faction discriminant — no `HashMap` anywhere.

use std::collections::{BTreeMap, VecDeque};

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::brain::{AiBrain, AiEvent, RethinkQueue};
use crate::ai::lod::{AoiTier, Tier};
use crate::ai::tuning::AiTuning;
use crate::broadphase::CoarseIndex;
use crate::clock::CurrentTick;
use crate::components::{hostile, CollisionRadius, Faction, Position};
use crate::fitting::ShipStats;

// ---------------------------------------------------------------------------
// T029 — Contact + ContactList
// ---------------------------------------------------------------------------

/// One sensed hostile (data-model §`ContactList`): where it was last seen,
/// when, and how detectable it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// The sensed entity. Pruned by `ai_despawn_sweep_system` the tick it
    /// despawns (V-1) and by the staleness window when perception-lost.
    pub target: Entity,
    /// World position at `last_seen_tick` (the last-known-position memory).
    pub last_pos: Vec2,
    /// Tick of the most recent detection (own scan or fused share).
    pub last_seen_tick: u64,
    /// Detectability scalar. **v1 = ship size**: the target's
    /// [`CollisionRadius`] (the spec's "size now; heat-signature feeds it
    /// later", IP-005). A body with no `CollisionRadius` has no detectable
    /// cross-section — signature `0.0`, visible only while `sig_threshold ≤ 0`.
    pub signature: f32,
}

/// Local perception memory of one AI ship (data-model §`ContactList`).
///
/// `contacts` is kept sorted by target `Entity` bits (see module docs on V-3);
/// all mutation goes through the sorted upsert/merge helpers.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ContactList {
    /// Known hostiles, sorted by target entity bits.
    pub contacts: Vec<Contact>,
    /// Last tick this ship's own scan ran (tier-cadence bookkeeping, Q4).
    pub last_scan_tick: u64,
}

// ---------------------------------------------------------------------------
// T030 — LinkState + SensorNetworks
// ---------------------------------------------------------------------------

/// Per-ship datalink seam flag (data-model V-2; the CAP-007 seam — set by
/// scenario/tests only in E011). EITHER flag `true` excludes the ship from the
/// sensor-network flood-fill entirely (TX **and** RX: its contacts don't enter
/// fusion and it receives no fused picture — local-only fallback, TR-014); the
/// ship's OWN local sensing is unaffected in v1. Absence of the component =
/// linked (the v1 baseline: every faction ship is TX+RX, Q3).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkState {
    /// Actively jammed: excluded from network connectivity.
    pub jammed: bool,
    /// Datalink physically severed: excluded from network connectivity.
    pub severed: bool,
}

/// One connected component of a faction's datalink network: its linked members
/// and their fused shared picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkComponent {
    /// Linked transmitting ships, sorted by entity bits (stable order).
    pub members: Vec<Entity>,
    /// The shared picture: union of member contacts, deduped newest-wins,
    /// sorted by target entity bits, capped at `max_fused_contacts`.
    pub fused: Vec<Contact>,
}

/// Per-faction fused sensor pictures (data-model §`SensorNetworks`). Rebuilt
/// at the mid scan cadence by [`sensor_network_system`]; components per
/// faction are ordered by their lowest-bits member (stable flood-fill seed
/// order).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct SensorNetworks {
    /// Faction discriminant ([`faction_key`]) → that faction's connected
    /// components. `BTreeMap` for stable iteration (V-3).
    pub by_faction: BTreeMap<u8, Vec<NetworkComponent>>,
}

/// Stable discriminant of [`Faction`] for the [`SensorNetworks`] map key
/// (documented mapping — the enum has no explicit repr): `Red = 0`, `Blue = 1`.
pub fn faction_key(faction: Faction) -> u8 {
    match faction {
        Faction::Red => 0,
        Faction::Blue => 1,
    }
}

// ---------------------------------------------------------------------------
// Shared contact-merge helpers (scan upsert + network fusion use ONE rule)
// ---------------------------------------------------------------------------

/// Whether `new` supersedes `old` for the same target (the newest-wins dedupe
/// rule, data-model §`SensorNetworks`): strictly newer `last_seen_tick` wins;
/// an exact tick tie goes to the strictly higher `signature`; a full tie keeps
/// the incumbent (which, in fusion, is the contribution of the earlier member
/// in stable entity-bits order — the deterministic third level).
fn supersedes(new: &Contact, old: &Contact) -> bool {
    new.last_seen_tick > old.last_seen_tick
        || (new.last_seen_tick == old.last_seen_tick && new.signature > old.signature)
}

/// Outcome of merging one contact into a sorted list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergeOutcome {
    /// Target was not in the list — inserted (a NEW contact).
    New,
    /// Target was present and the entry superseded it — replaced.
    Updated,
    /// Target was present with an equal-or-better entry — list untouched.
    Kept,
}

/// Upsert `new` into `contacts` (sorted by target entity bits), applying the
/// [`supersedes`] newest-wins rule. Preserves the sort invariant.
fn merge_contact(contacts: &mut Vec<Contact>, new: Contact) -> MergeOutcome {
    match contacts.binary_search_by_key(&new.target.to_bits(), |c| c.target.to_bits()) {
        Ok(i) => {
            if supersedes(&new, &contacts[i]) {
                contacts[i] = new;
                MergeOutcome::Updated
            } else {
                MergeOutcome::Kept
            }
        }
        Err(i) => {
            contacts.insert(i, new);
            MergeOutcome::New
        }
    }
}

/// Whether merging `fused` into `contacts` would change anything — the
/// write-back guard, so an unchanged `ContactList` is never flagged mutated.
fn fused_would_change(contacts: &[Contact], fused: &[Contact]) -> bool {
    fused.iter().any(|c| {
        match contacts.binary_search_by_key(&c.target.to_bits(), |x| x.target.to_bits()) {
            Ok(i) => supersedes(c, &contacts[i]),
            Err(_) => true,
        }
    })
}

/// Cap a fused picture at `max` contacts with the deterministic cut
/// (data-model: "keep newest/highest-signature"). **Exact ordering
/// (documented)**: survivors are the first `max` under
/// `last_seen_tick DESC, signature DESC (total_cmp), target bits ASC` — newest
/// first, an exact tick tie kept by higher signature, a full tie by lower
/// target bits. The stored vector is then re-sorted by target bits (the list
/// invariant).
fn cap_fused(fused: &mut Vec<Contact>, max: usize) {
    if fused.len() <= max {
        return;
    }
    fused.sort_by(|a, b| {
        b.last_seen_tick
            .cmp(&a.last_seen_tick)
            .then(b.signature.total_cmp(&a.signature))
            .then(a.target.to_bits().cmp(&b.target.to_bits()))
    });
    fused.truncate(max);
    fused.sort_by_key(|c| c.target.to_bits());
}

// ---------------------------------------------------------------------------
// T029 — tier-cadence signature-gated scans
// ---------------------------------------------------------------------------

/// Contact-staleness window in scan PERIODS (documented choice): a contact not
/// re-seen (locally or via fusion) for `3 ×` the scanner's own current scan
/// cadence is dropped — perception-lost. At the pinned defaults that is ~1.5 s
/// for an Active scanner (3 × 15 ticks) and ~9 s for a Dormant one (3 × 90),
/// scaling memory with attention exactly like the cadence itself.
const STALE_SCAN_PERIODS: u64 = 3;

/// Scan cadence (ticks) for an AOI tier — the Q4 bands: near (Active) scans at
/// the Active think cadence (`think_ticks_active`, "≈ every think"), Mid at
/// `scan_ticks_mid` (~0.5 s), Dormant at `scan_ticks_far` (2–5 s coarse). A
/// degenerate `0` is clamped to `1`.
pub fn scan_cadence_for_tier(tier: Tier, tuning: &AiTuning) -> u64 {
    let ticks = match tier {
        Tier::Active => tuning.think_ticks_active,
        Tier::Mid => tuning.scan_ticks_mid,
        Tier::Dormant => tuning.scan_ticks_far,
    };
    u64::from(ticks.max(1))
}

/// Nearest contact to `from` (squared distance over `last_pos`), exact-tie
/// broken by lower target entity bits — the stable cross-entity tiebreak.
/// `crate`-visible since T035: the scout's superior-threat test picks ITS
/// threat reference with the exact same stable rule.
pub(crate) fn nearest_contact(contacts: &[Contact], from: Vec2) -> Option<Entity> {
    let mut best: Option<(f32, u64, Entity)> = None;
    for c in contacts {
        let d = (c.last_pos - from).length_squared();
        let bits = c.target.to_bits();
        let wins = match best {
            None => true,
            Some((bd, bb, _)) => d < bd || (d == bd && bits < bb),
        };
        if wins {
            best = Some((d, bits, c.target));
        }
    }
    best.map(|(_, _, e)| e)
}

/// T029 (TR-005/TR-013, [COMPLETES TR-005]) — the per-ship perception scan.
///
/// For each AI ship (`AiBrain` + `ContactList` + `Faction` + `Position`), at
/// its TIER-scaled cadence ([`scan_cadence_for_tier`], offset by the brain's
/// `phase_bucket` exactly like the think fallback — `(now + bucket) % cadence`;
/// a ship with no [`AoiTier`] component is treated as Active, matching the
/// think/execute convention):
///
/// 1. **Query** coarse-grid candidates within `base_sensor_range` (see module
///    docs: `CoarseIndex` + exact distance filter — no fine-grid Resource
///    exists) and gate each on:
///    - not self;
///    - **hostile-factioned** (documented v1 rule): the candidate CARRIES a
///      [`Faction`] and [`hostile`] says the two differ. Faction-less bodies
///      (asteroids, neutral targets — which `hostile` treats as fair game for
///      DAMAGE) are not tracked as contacts, and the `CombatRules`
///      friendly-fire toggle is ignored here — perception is pure faction
///      opposition in v1;
///    - **signature gate (V-8)**: `signature >= sig_threshold` at every tier
///      (signature = target `CollisionRadius`, see [`Contact::signature`]).
/// 2. **Upsert** each detection (newest `last_seen_tick` wins via
///    [`merge_contact`]); a genuinely NEW target pushes ONE
///    [`AiEvent::NewContact`] into the [`RethinkQueue`] (coalesced; re-seeing
///    a known contact never re-fires it). The scan runs BEFORE
///    `squad_think_system`/`ai_think_system` in the AI set, so this tick's
///    thinks see this tick's contacts.
/// 3. **Prune** contacts older than the staleness window
///    ([`STALE_SCAN_PERIODS`] × cadence); despawned targets are pruned by
///    `ai_despawn_sweep_system` first in the set (V-1).
/// 4. **Target acquisition (documented v1 rule)**: if `brain.target` is
///    `None` and the ship is combat-capable (Engage-eligible = ARMED:
///    `ShipStats::can_fire`; unarmed/unfitted ships never auto-acquire), set
///    `brain.target` to the NEAREST contact (exact-tie → lower entity bits).
///    Squad `Engage` orders still override: `squad_think_system` runs after
///    this scan and re-asserts its own target into member brains the same
///    tick.
///
/// Never writes `AoiTier` (V-8: perception is separate from sim-LOD). Golden
/// safety: no golden world spawns a `ContactList`, so the scanner query is
/// empty there and the scenario goldens stay bit-identical.
pub fn perception_scan_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    index: Res<CoarseIndex>,
    mut queue: ResMut<RethinkQueue>,
    mut scanners: Query<(
        Entity,
        &mut AiBrain,
        &mut ContactList,
        &Faction,
        &Position,
        Option<&AoiTier>,
        Option<&ShipStats>,
    )>,
    // Candidate view (read-only, access-disjoint from the scanners' mutable
    // components `AiBrain`/`ContactList`) — so scanners can sense each other.
    candidates: Query<(&Position, Option<&Faction>, Option<&CollisionRadius>)>,
) {
    let now = tick.0;
    let range_sq = tuning.base_sensor_range * tuning.base_sensor_range;
    for (entity, mut brain, mut list, faction, pos, aoi, stats) in &mut scanners {
        // Reads below go through `Deref` (no change-detection flag).
        let tier = aoi.map_or(Tier::Active, |a| a.tier);
        let cadence = scan_cadence_for_tier(tier, &tuning);
        if !(now + u64::from(brain.phase_bucket)).is_multiple_of(cadence) {
            continue; // Off-cadence: zero scan work this tick.
        }

        let mut new_contact = false;
        for cand in index.0.near(pos.0, tuning.base_sensor_range) {
            if cand == entity {
                continue;
            }
            let Ok((cpos, cfaction, cradius)) = candidates.get(cand) else {
                continue; // Indexed body without the candidate components.
            };
            if (cpos.0 - pos.0).length_squared() > range_sq {
                continue; // Coarse superset → exact range filter.
            }
            // v1 hostility gate: factioned + opposed (see system docs).
            if !cfaction.is_some_and(|cf| hostile(Some(*faction), Some(*cf), false)) {
                continue;
            }
            // V-8 signature gate, identical at every tier.
            let signature = cradius.map_or(0.0, |r| r.0);
            if signature < tuning.sig_threshold {
                continue;
            }
            let outcome = merge_contact(
                &mut list.contacts,
                Contact {
                    target: cand,
                    last_pos: cpos.0,
                    last_seen_tick: now,
                    signature,
                },
            );
            if outcome == MergeOutcome::New {
                new_contact = true;
            }
        }

        // Staleness prune (documented window: 3 × this scanner's cadence).
        let window = cadence.saturating_mul(STALE_SCAN_PERIODS);
        if list
            .contacts
            .iter()
            .any(|c| now.saturating_sub(c.last_seen_tick) > window)
        {
            list.contacts
                .retain(|c| now.saturating_sub(c.last_seen_tick) <= window);
        }
        list.last_scan_tick = now;

        if new_contact {
            queue.push(entity, AiEvent::NewContact);
        }

        // v1 target acquisition (see system docs): idle + armed → nearest.
        if brain.target.is_none() && stats.is_some_and(|s| s.can_fire) {
            if let Some(best) = nearest_contact(&list.contacts, pos.0) {
                brain.target = Some(best);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T030 — sensor-network flood-fill + fusion
// ---------------------------------------------------------------------------

/// T030 (TR-014) — rebuild the per-faction [`SensorNetworks`] fused pictures.
///
/// **Cadence (documented v1 choice)**: every `scan_ticks_mid` ticks at GLOBAL
/// phase 0 (`now % cadence == 0`) — the data-model's mid rebuild cadence; the
/// "also rebuild on `LinkState`/membership change events" refinement is
/// deliberately deferred (v1 keeps the trigger simple; a flipped flag takes
/// effect at the next rebuild, ≤ 0.5 s later). Runs AFTER
/// [`perception_scan_system`] in the AI set so fusion sees this tick's
/// detections, and before `squad_think_system`/`ai_think_system` so thinks see
/// the fused picture.
///
/// Per faction (key = [`faction_key`]):
/// 1. **Members**: every faction ship with a `ContactList` that is NOT
///    jammed/severed (either [`LinkState`] flag excludes — TX **and** RX;
///    absence of the component = linked, V-2; baseline every faction ship is
///    TX+RX, Q3). Snapshot, sorted by entity bits (stable member order).
/// 2. **Flood-fill** connected components over pairwise `datalink_radius`
///    adjacency, seeds visited in stable member order, BFS frontier in stable
///    order (the sever-logic pattern, IP-007) — component members re-sorted by
///    entity bits.
/// 3. **Fuse** per component: concat member contacts in stable member order →
///    dedupe by target ([`supersedes`]: newest `last_seen_tick` wins, exact
///    tie → higher signature, full tie → the earlier member's entry) → the
///    list is maintained sorted by target entity bits → cap at
///    `max_fused_contacts` ([`cap_fused`]'s documented deterministic cut).
///    O(C log C) per rebuild.
/// 4. **Write-back (documented v1 fusion-consumption mechanism)**: MERGE the
///    fused picture into each member's own `ContactList` with the same
///    newest-wins dedupe (never replace — a member's fresher local detail
///    survives). Members therefore "see" the shared picture through their
///    ordinary contact list; an excluded (jammed/severed) or isolated ship is
///    in no component, gets no write-back, and keeps only its OWN local
///    picture going forward — TR-014's local-only fallback. Shared contacts
///    age out of a member's list by ITS staleness window once the share stops.
///
/// The Resource write is guarded (`!=` compare) so a world where nothing
/// changed — in particular every golden world, where no ship carries a
/// `ContactList` and the map stays empty — never flags the resource mutated.
pub fn sensor_network_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    mut networks: ResMut<SensorNetworks>,
    mut ships: Query<(
        Entity,
        &Faction,
        &Position,
        &mut ContactList,
        Option<&LinkState>,
    )>,
) {
    let cadence = u64::from(tuning.scan_ticks_mid.max(1));
    if !tick.0.is_multiple_of(cadence) {
        return; // Mid-cadence rebuild only (global phase 0).
    }

    // 1. Snapshot transmitting members per faction, in stable bits order.
    //    (Members: entity bits, entity, position, local contacts.)
    type Member = (u64, Entity, Vec2, Vec<Contact>);
    let mut members_by_faction: BTreeMap<u8, Vec<Member>> = BTreeMap::new();
    for (entity, faction, pos, list, link) in ships.iter() {
        if link.is_some_and(|l| l.jammed || l.severed) {
            continue; // EITHER flag excludes (fusion exclusion, TR-014).
        }
        members_by_faction
            .entry(faction_key(*faction))
            .or_default()
            .push((entity.to_bits(), entity, pos.0, list.contacts.clone()));
    }
    for members in members_by_faction.values_mut() {
        members.sort_unstable_by_key(|m| m.0);
    }

    // 2 + 3. Flood-fill components in stable member order, fuse each.
    let link_sq = tuning.datalink_radius * tuning.datalink_radius;
    let max_fused = tuning.max_fused_contacts as usize;
    let mut next: BTreeMap<u8, Vec<NetworkComponent>> = BTreeMap::new();
    for (key, members) in &members_by_faction {
        let comps = next.entry(*key).or_default();
        let mut visited = vec![false; members.len()];
        for seed in 0..members.len() {
            if visited[seed] {
                continue;
            }
            visited[seed] = true;
            let mut comp = vec![seed];
            let mut frontier: VecDeque<usize> = VecDeque::from([seed]);
            while let Some(i) = frontier.pop_front() {
                for (j, seen) in visited.iter_mut().enumerate() {
                    if !*seen && (members[i].2 - members[j].2).length_squared() <= link_sq {
                        *seen = true;
                        comp.push(j);
                        frontier.push_back(j);
                    }
                }
            }
            comp.sort_unstable(); // Index order == entity-bits order.
            let mut fused: Vec<Contact> = Vec::new();
            for &i in &comp {
                for &contact in &members[i].3 {
                    merge_contact(&mut fused, contact);
                }
            }
            cap_fused(&mut fused, max_fused);
            comps.push(NetworkComponent {
                members: comp.iter().map(|&i| members[i].1).collect(),
                fused,
            });
        }
    }

    // 4. Write the fused picture back into each member's ContactList (merge,
    //    newest-wins; guarded so an unchanged list is never flagged mutated).
    for comps in next.values() {
        for nc in comps {
            for &member in &nc.members {
                let Ok((_, _, _, mut list, _)) = ships.get_mut(member) else {
                    continue;
                };
                if !fused_would_change(&list.contacts, &nc.fused) {
                    continue;
                }
                let contacts = &mut list.contacts;
                for &contact in &nc.fused {
                    merge_contact(contacts, contact);
                }
            }
        }
    }

    // Store (guarded: golden worlds keep an empty map, never flagged mutated).
    if networks.by_faction != next {
        networks.by_faction = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ident::ai_despawn_sweep_system;
    use crate::broadphase::CoarseGrid;

    // --- shared fixtures ----------------------------------------------------

    /// A real derived fighter fit (reactor + thruster + autocannon) — an ARMED
    /// `ShipStats` (`can_fire == true`) for the target-acquisition rule.
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

    fn scan_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick::default());
        world.insert_resource(CoarseIndex::default());
        world.insert_resource(RethinkQueue::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(perception_scan_system);
        (world, schedule)
    }

    /// Rebuild the coarse index from every positioned entity, set the tick,
    /// and run the scan schedule (the production index build is its own
    /// system; tests pin its contents explicitly).
    fn run_scan_at(world: &mut World, schedule: &mut Schedule, tick: u64) {
        let items: Vec<(Entity, Vec2)> = world
            .query::<(Entity, &Position)>()
            .iter(world)
            .map(|(e, p)| (e, p.0))
            .collect();
        world.resource_mut::<CoarseIndex>().0 = CoarseGrid::build(items.into_iter());
        world.resource_mut::<CurrentTick>().0 = tick;
        schedule.run(world);
    }

    fn spawn_scanner(world: &mut World, faction: Faction, pos: Vec2) -> Entity {
        world
            .spawn((
                AiBrain::default(), // phase_bucket 0: scans on cadence ticks.
                ContactList::default(),
                faction,
                Position(pos),
            ))
            .id()
    }

    fn spawn_body(world: &mut World, faction: Faction, pos: Vec2, radius: f32) -> Entity {
        world
            .spawn((faction, Position(pos), CollisionRadius(radius)))
            .id()
    }

    fn contacts_of(world: &World, e: Entity) -> Vec<Contact> {
        world.get::<ContactList>(e).unwrap().contacts.clone()
    }

    // --- T029: scans ---------------------------------------------------------

    /// The scan tracks a hostile within `base_sensor_range`, ignores one
    /// beyond it and a friendly inside it; the contact records the seen
    /// position, tick, and signature (= CollisionRadius).
    #[test]
    fn scan_detects_hostile_in_range_ignores_far_and_friendly() {
        let (mut world, mut schedule) = scan_world();
        let scanner = spawn_scanner(&mut world, Faction::Red, Vec2::ZERO);
        let near_hostile = spawn_body(&mut world, Faction::Blue, Vec2::new(100.0, 0.0), 3.0);
        spawn_body(&mut world, Faction::Blue, Vec2::new(1000.0, 0.0), 3.0); // out of range
        spawn_body(&mut world, Faction::Red, Vec2::new(50.0, 0.0), 3.0); // friendly

        run_scan_at(&mut world, &mut schedule, 0);

        let contacts = contacts_of(&world, scanner);
        assert_eq!(contacts.len(), 1, "exactly the near hostile is tracked");
        let c = contacts[0];
        assert_eq!(c.target, near_hostile);
        assert_eq!(c.last_pos, Vec2::new(100.0, 0.0));
        assert_eq!(c.last_seen_tick, 0);
        assert_eq!(c.signature, 3.0, "signature = CollisionRadius (v1 size)");
        let list = world.get::<ContactList>(scanner).unwrap();
        assert_eq!(list.last_scan_tick, 0);
    }

    /// V-8 signature gate: below `sig_threshold` → not detected (including a
    /// body with NO CollisionRadius — signature 0); at/above → detected.
    #[test]
    fn signature_gate_blocks_below_threshold() {
        let (mut world, mut schedule) = scan_world();
        world.resource_mut::<AiTuning>().sig_threshold = 5.0;
        let scanner = spawn_scanner(&mut world, Faction::Red, Vec2::ZERO);
        spawn_body(&mut world, Faction::Blue, Vec2::new(80.0, 0.0), 1.0); // stealthy
        world.spawn((Faction::Blue, Position(Vec2::new(90.0, 0.0)))); // no radius → sig 0
        let loud = spawn_body(&mut world, Faction::Blue, Vec2::new(120.0, 0.0), 8.0);

        run_scan_at(&mut world, &mut schedule, 0);

        let contacts = contacts_of(&world, scanner);
        assert_eq!(contacts.len(), 1, "only the above-threshold signature");
        assert_eq!(contacts[0].target, loud);
    }

    /// A NEW contact pushes `AiEvent::NewContact` exactly once; re-seeing the
    /// same target on a later scan refreshes it without re-firing the event.
    #[test]
    fn new_contact_event_fires_once_not_on_resee() {
        let (mut world, mut schedule) = scan_world();
        let scanner = spawn_scanner(&mut world, Faction::Red, Vec2::ZERO);
        let hostile_e = spawn_body(&mut world, Faction::Blue, Vec2::new(100.0, 0.0), 3.0);

        run_scan_at(&mut world, &mut schedule, 0);
        assert_eq!(
            world.resource::<RethinkQueue>().get(scanner),
            Some(AiEvent::NewContact),
            "first sighting queues a re-think"
        );

        world.resource_mut::<RethinkQueue>().clear();
        world.get_mut::<Position>(hostile_e).unwrap().0 = Vec2::new(110.0, 0.0);
        run_scan_at(&mut world, &mut schedule, 15); // Active cadence re-scan.

        assert!(
            world.resource::<RethinkQueue>().is_empty(),
            "re-seeing a known contact never re-fires NewContact"
        );
        let c = contacts_of(&world, scanner)[0];
        assert_eq!(c.last_seen_tick, 15, "newest detection wins");
        assert_eq!(c.last_pos, Vec2::new(110.0, 0.0), "position refreshed");
    }

    /// v1 target acquisition: an ARMED idle brain acquires the NEAREST
    /// contact; an unarmed scanner never does; an existing target is kept.
    #[test]
    fn armed_idle_brain_acquires_nearest_contact() {
        let (mut world, mut schedule) = scan_world();
        let armed = world
            .spawn((
                AiBrain::default(),
                ContactList::default(),
                Faction::Red,
                Position(Vec2::ZERO),
                fighter_stats(), // can_fire == true
            ))
            .id();
        let unarmed = spawn_scanner(&mut world, Faction::Red, Vec2::new(0.0, 10.0));
        spawn_body(&mut world, Faction::Blue, Vec2::new(60.0, 0.0), 3.0);
        let nearer = spawn_body(&mut world, Faction::Blue, Vec2::new(40.0, 0.0), 3.0);

        run_scan_at(&mut world, &mut schedule, 0);

        assert_eq!(
            world.get::<AiBrain>(armed).unwrap().target,
            Some(nearer),
            "armed + idle → nearest hostile contact"
        );
        assert_eq!(
            world.get::<AiBrain>(unarmed).unwrap().target,
            None,
            "no ShipStats → not Engage-eligible → never auto-acquires"
        );

        // An existing target is never overwritten by acquisition.
        let pinned = world.spawn_empty().id();
        world.get_mut::<AiBrain>(armed).unwrap().target = Some(pinned);
        run_scan_at(&mut world, &mut schedule, 15);
        assert_eq!(
            world.get::<AiBrain>(armed).unwrap().target,
            Some(pinned),
            "acquisition only fills an empty target slot"
        );
    }

    /// Tier scaling: a Dormant scanner only scans at the far cadence
    /// (`scan_ticks_far`), not at the Active/mid cadence ticks.
    #[test]
    fn dormant_scanner_uses_far_cadence() {
        let (mut world, mut schedule) = scan_world();
        let scanner = spawn_scanner(&mut world, Faction::Red, Vec2::ZERO);
        world.entity_mut(scanner).insert(AoiTier {
            tier: Tier::Dormant,
            since_tick: 0,
        });
        spawn_body(&mut world, Faction::Blue, Vec2::new(100.0, 0.0), 3.0);

        run_scan_at(&mut world, &mut schedule, 15); // mid/active tick: skipped
        assert!(
            contacts_of(&world, scanner).is_empty(),
            "off the far cadence: no scan"
        );
        run_scan_at(&mut world, &mut schedule, 90); // scan_ticks_far
        assert_eq!(contacts_of(&world, scanner).len(), 1, "far-cadence scan");
    }

    /// Staleness: a contact not re-seen within 3× the scanner's cadence is
    /// dropped (perception-lost); within the window it is remembered.
    #[test]
    fn stale_contacts_pruned_after_three_scan_periods() {
        let (mut world, mut schedule) = scan_world();
        let scanner = spawn_scanner(&mut world, Faction::Red, Vec2::ZERO);
        let hostile_e = spawn_body(&mut world, Faction::Blue, Vec2::new(100.0, 0.0), 3.0);

        run_scan_at(&mut world, &mut schedule, 0);
        assert_eq!(contacts_of(&world, scanner).len(), 1);

        // Target slips out of sensor range: memory persists inside the window…
        world.get_mut::<Position>(hostile_e).unwrap().0 = Vec2::new(1000.0, 0.0);
        run_scan_at(&mut world, &mut schedule, 45); // age 45 == 3 × 15: kept
        assert_eq!(
            contacts_of(&world, scanner).len(),
            1,
            "remembered at exactly the staleness window edge"
        );
        // …and is forgotten past it.
        run_scan_at(&mut world, &mut schedule, 60); // age 60 > 45: pruned
        assert!(
            contacts_of(&world, scanner).is_empty(),
            "perception-lost past 3 scan periods"
        );
    }

    /// V-1 wiring: the despawn sweep prunes ContactList entries and
    /// SensorNetworks members/fused entries whose referent despawned.
    #[test]
    fn despawn_sweep_prunes_contacts_and_network_pictures() {
        let mut world = World::new();
        let live = world.spawn_empty().id();
        let doomed = world.spawn_empty().id();
        let contact = |target: Entity| Contact {
            target,
            last_pos: Vec2::ZERO,
            last_seen_tick: 0,
            signature: 1.0,
        };
        let mut contacts = vec![contact(live), contact(doomed)];
        contacts.sort_by_key(|c| c.target.to_bits());
        let ship = world
            .spawn(ContactList {
                contacts: contacts.clone(),
                last_scan_tick: 0,
            })
            .id();
        let mut networks = SensorNetworks::default();
        networks.by_faction.insert(
            0,
            vec![NetworkComponent {
                members: vec![ship, doomed],
                fused: contacts,
            }],
        );
        world.insert_resource(networks);
        world.despawn(doomed);

        let mut schedule = Schedule::default();
        schedule.add_systems(ai_despawn_sweep_system);
        schedule.run(&mut world);

        let list = world.get::<ContactList>(ship).unwrap();
        assert_eq!(list.contacts.len(), 1, "dangling contact pruned (V-1)");
        assert_eq!(list.contacts[0].target, live);
        let nets = world.resource::<SensorNetworks>();
        let nc = &nets.by_faction[&0][0];
        assert_eq!(nc.members, vec![ship], "dead member pruned from network");
        assert_eq!(nc.fused.len(), 1, "dangling fused contact pruned");
        assert_eq!(nc.fused[0].target, live);
    }

    // --- T030: networks ------------------------------------------------------

    fn network_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick::default());
        world.insert_resource(SensorNetworks::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(sensor_network_system);
        (world, schedule)
    }

    fn spawn_node(
        world: &mut World,
        faction: Faction,
        pos: Vec2,
        mut contacts: Vec<Contact>,
    ) -> Entity {
        contacts.sort_by_key(|c| c.target.to_bits()); // list invariant
        world
            .spawn((
                faction,
                Position(pos),
                ContactList {
                    contacts,
                    last_scan_tick: 0,
                },
            ))
            .id()
    }

    fn contact_at(target: Entity, pos: Vec2, tick: u64, sig: f32) -> Contact {
        Contact {
            target,
            last_pos: pos,
            last_seen_tick: tick,
            signature: sig,
        }
    }

    /// Flood-fill: three ships chained within `datalink_radius` (ends NOT
    /// directly adjacent) form ONE component; a far fourth forms its own; a
    /// different faction never joins.
    #[test]
    fn flood_fill_chains_one_component_and_splits_far_ship() {
        let (mut world, mut schedule) = network_world();
        // datalink_radius 300: 0↔250 ✓, 250↔500 ✓, 0↔500 ✗ (transitive link).
        let a = spawn_node(&mut world, Faction::Red, Vec2::new(0.0, 0.0), vec![]);
        let b = spawn_node(&mut world, Faction::Red, Vec2::new(250.0, 0.0), vec![]);
        let c = spawn_node(&mut world, Faction::Red, Vec2::new(500.0, 0.0), vec![]);
        let far = spawn_node(&mut world, Faction::Red, Vec2::new(5000.0, 0.0), vec![]);
        let blue = spawn_node(&mut world, Faction::Blue, Vec2::new(100.0, 0.0), vec![]);

        schedule.run(&mut world); // tick 0: on the mid cadence.

        let nets = world.resource::<SensorNetworks>();
        let red = &nets.by_faction[&faction_key(Faction::Red)];
        assert_eq!(red.len(), 2, "one chained component + one isolated");
        // Members are stored in entity-BITS order, which is deterministic but
        // not spawn order (Bevy's EntityRow stores the index inverted).
        let mut chain = vec![a, b, c];
        chain.sort_by_key(|e| e.to_bits());
        assert!(
            red.iter().any(|nc| nc.members == chain),
            "chain fused transitively into one component: {red:?}"
        );
        assert!(
            red.iter().any(|nc| nc.members == vec![far]),
            "far ship is its own component: {red:?}"
        );
        let blue_net = &nets.by_faction[&faction_key(Faction::Blue)];
        assert_eq!(blue_net.len(), 1);
        assert_eq!(blue_net[0].members, vec![blue], "factions never cross-link");
    }

    /// Fusion dedupe (the pinned unit case): two members disagree about one
    /// target — the newest `last_seen_tick` wins (pos included), an exact tick
    /// tie goes to the higher signature; write-back shares the winner.
    #[test]
    fn fusion_dedupes_newest_wins_and_writes_back() {
        let (mut world, mut schedule) = network_world();
        let enemy = world.spawn_empty().id();
        let other = world.spawn_empty().id();
        let stale = contact_at(enemy, Vec2::new(1.0, 0.0), 5, 2.0);
        let fresh = contact_at(enemy, Vec2::new(9.0, 9.0), 9, 2.0);
        let weak = contact_at(other, Vec2::new(2.0, 0.0), 7, 1.0);
        let strong = contact_at(other, Vec2::new(3.0, 0.0), 7, 4.0);
        let a = spawn_node(&mut world, Faction::Red, Vec2::ZERO, vec![stale, weak]);
        let b = spawn_node(
            &mut world,
            Faction::Red,
            Vec2::new(100.0, 0.0),
            vec![fresh, strong],
        );

        schedule.run(&mut world);

        let nets = world.resource::<SensorNetworks>();
        let nc = &nets.by_faction[&faction_key(Faction::Red)][0];
        let mut ab = vec![a, b];
        ab.sort_by_key(|e| e.to_bits()); // bits order, not spawn order
        assert_eq!(nc.members, ab);
        assert_eq!(nc.fused.len(), 2, "deduped by target");
        let fused_enemy = nc.fused.iter().find(|c| c.target == enemy).unwrap();
        assert_eq!(*fused_enemy, fresh, "newest last_seen_tick wins");
        let fused_other = nc.fused.iter().find(|c| c.target == other).unwrap();
        assert_eq!(*fused_other, strong, "tick tie → higher signature wins");
        // Write-back: the stale member now carries the fused picture.
        let a_contacts = contacts_of(&world, a);
        assert!(a_contacts.contains(&fresh), "member A upgraded via fusion");
        assert!(a_contacts.contains(&strong));
        assert!(
            a_contacts
                .windows(2)
                .all(|w| w[0].target.to_bits() < w[1].target.to_bits()),
            "list invariant: sorted by target bits"
        );
    }

    /// A jammed member is excluded from connectivity (TX and RX): it joins no
    /// component, contributes nothing to fusion, receives no fused picture,
    /// and keeps only its own local contacts (TR-014 fallback).
    #[test]
    fn jammed_member_excluded_keeps_local_only_picture() {
        let (mut world, mut schedule) = network_world();
        let e1 = world.spawn_empty().id();
        let e2 = world.spawn_empty().id();
        let clear_c = contact_at(e1, Vec2::new(5.0, 0.0), 3, 1.0);
        let jammed_c = contact_at(e2, Vec2::new(6.0, 0.0), 4, 1.0);
        let clear = spawn_node(&mut world, Faction::Red, Vec2::ZERO, vec![clear_c]);
        let jammed = spawn_node(
            &mut world,
            Faction::Red,
            Vec2::new(100.0, 0.0),
            vec![jammed_c],
        );
        world.entity_mut(jammed).insert(LinkState {
            jammed: true,
            severed: false,
        });

        schedule.run(&mut world);

        let nets = world.resource::<SensorNetworks>();
        let red = &nets.by_faction[&faction_key(Faction::Red)];
        assert_eq!(red.len(), 1, "jammed ship forms no component at all");
        assert_eq!(red[0].members, vec![clear]);
        assert_eq!(red[0].fused, vec![clear_c], "jammed contact never fused");
        assert_eq!(
            contacts_of(&world, jammed),
            vec![jammed_c],
            "jammed ship keeps its OWN local picture only"
        );
        assert_eq!(
            contacts_of(&world, clear),
            vec![clear_c],
            "clear ship never receives the jammed ship's contact"
        );
    }

    /// The severed flag excludes exactly like jammed (EITHER flag, tested
    /// independently per the data-model).
    #[test]
    fn severed_member_also_excluded() {
        let (mut world, mut schedule) = network_world();
        let clear = spawn_node(&mut world, Faction::Red, Vec2::ZERO, vec![]);
        let severed = spawn_node(&mut world, Faction::Red, Vec2::new(100.0, 0.0), vec![]);
        world.entity_mut(severed).insert(LinkState {
            jammed: false,
            severed: true,
        });

        schedule.run(&mut world);

        let nets = world.resource::<SensorNetworks>();
        let red = &nets.by_faction[&faction_key(Faction::Red)];
        assert_eq!(red.len(), 1, "severed ship excluded from connectivity");
        assert_eq!(red[0].members, vec![clear]);
    }

    /// The `max_fused_contacts` cap keeps the newest (then highest-signature)
    /// contacts — a deterministic cut — and stores the survivors sorted by
    /// target bits.
    #[test]
    fn max_fused_contacts_cap_is_deterministic() {
        let (mut world, mut schedule) = network_world();
        world.resource_mut::<AiTuning>().max_fused_contacts = 2;
        let t1 = world.spawn_empty().id();
        let t2 = world.spawn_empty().id();
        let t3 = world.spawn_empty().id();
        let t4 = world.spawn_empty().id();
        let oldest = contact_at(t1, Vec2::ZERO, 1, 9.0);
        let newest = contact_at(t2, Vec2::ZERO, 8, 1.0);
        let tied_weak = contact_at(t3, Vec2::ZERO, 5, 1.0);
        let tied_strong = contact_at(t4, Vec2::ZERO, 5, 3.0);
        let node = spawn_node(
            &mut world,
            Faction::Red,
            Vec2::ZERO,
            vec![oldest, newest, tied_weak, tied_strong],
        );

        schedule.run(&mut world);

        let nets = world.resource::<SensorNetworks>();
        let fused = &nets.by_faction[&faction_key(Faction::Red)][0].fused;
        assert_eq!(fused.len(), 2, "capped at max_fused_contacts");
        // Survivors: newest (tick 8), then the tick-5 tie broken by higher
        // signature → tied_strong; oldest + tied_weak cut. Stored bits-sorted.
        let mut expected = vec![newest, tied_strong];
        expected.sort_by_key(|c| c.target.to_bits());
        assert_eq!(*fused, expected, "deterministic newest/highest-sig cut");
        // The member's OWN list is uncapped (the cap is per fused picture).
        assert_eq!(contacts_of(&world, node).len(), 4);
    }

    /// Off the mid cadence the rebuild does nothing (the v1 trigger is
    /// `now % scan_ticks_mid == 0`, global phase 0).
    #[test]
    fn rebuild_only_on_mid_cadence() {
        let (mut world, mut schedule) = network_world();
        spawn_node(&mut world, Faction::Red, Vec2::ZERO, vec![]);
        world.resource_mut::<CurrentTick>().0 = 7; // not a multiple of 15
        schedule.run(&mut world);
        assert!(
            world.resource::<SensorNetworks>().by_faction.is_empty(),
            "off-cadence: no rebuild"
        );
        world.resource_mut::<CurrentTick>().0 = 15;
        schedule.run(&mut world);
        assert_eq!(
            world.resource::<SensorNetworks>().by_faction.len(),
            1,
            "on-cadence: rebuilt"
        );
    }

    // --- helpers -------------------------------------------------------------

    #[test]
    fn faction_key_is_the_documented_mapping() {
        assert_eq!(faction_key(Faction::Red), 0);
        assert_eq!(faction_key(Faction::Blue), 1);
    }

    #[test]
    fn scan_cadence_matches_q4_bands() {
        let t = AiTuning::default();
        assert_eq!(
            scan_cadence_for_tier(Tier::Active, &t),
            u64::from(t.think_ticks_active),
            "near ≈ every think"
        );
        assert_eq!(
            scan_cadence_for_tier(Tier::Mid, &t),
            u64::from(t.scan_ticks_mid)
        );
        assert_eq!(
            scan_cadence_for_tier(Tier::Dormant, &t),
            u64::from(t.scan_ticks_far)
        );
        let degenerate = AiTuning {
            scan_ticks_mid: 0,
            ..t
        };
        assert_eq!(
            scan_cadence_for_tier(Tier::Mid, &degenerate),
            1,
            "0 cadence clamps to every-tick, never a modulo-by-zero"
        );
    }
}
