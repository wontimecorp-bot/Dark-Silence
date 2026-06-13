//! AI sim-LOD (T005 + T019, TR-007/TR-008/TR-013): `AoiTier` Active/Mid/Dormant
//! classification from authoritative player proximity (promotion-asymmetric
//! boundary hysteresis), plus the cheap-glide dormant aggregates with
//! deterministic expand/collapse and the far hostile-scan promotion trigger
//! (Clarification Q1).
//!
//! **Authoritative proximity, never a camera** (data-model §AoiTier): tiers
//! derive from distance to the nearest *authoritative player ship* —
//! [`PlayerShip`]-marked entities in the server world — so every client
//! observes the same tiers and the determinism doctrine holds.
//!
//! **Hysteresis asymmetry** (the documented design): a *promotion* (toward
//! `Active`) applies **immediately** — combat responsiveness; a ship the player
//! flies up to must wake this tick, not a second later. A *demotion* (toward
//! `Dormant`) only commits after the entity has dwelt in its current tier for
//! at least [`AiTuning::tier_hysteresis_ticks`] — so an entity oscillating
//! across an AOI boundary thrashes at most once per hysteresis window
//! (data-model thrash note; the TR-020 rate counters key off this bound).
//!
//! **No players present → everything targets `Dormant`** (documented choice):
//! an empty server idles its AI rather than holding stale attention; normal
//! demotion hysteresis still applies on the way down.

use bevy_ecs::entity::Entities;
use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::brain::{AiBrain, AiEvent, RethinkQueue};
use crate::ai::ident::AiStableId;
use crate::ai::role::ScenarioRole;
use crate::ai::squad::{Squad, SquadOrder};
use crate::ai::tuning::AiTuning;
use crate::broadphase::{CoarseIndex, COARSE_CELL_SIZE};
use crate::clock::{CurrentTick, FixedDt};
use crate::components::{hostile, CollisionRadius, Faction, Position, Velocity};
use crate::intent::ShipIntent;

/// Attention tier of an AI-relevant entity (ADR-0015 behavior-LOD).
///
/// Declaration order is the *attention* order — `Active < Mid < Dormant` (the
/// derived `Ord`) — so "promotion" is exactly `target < current`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Inside `aoi_radius_active` of a player: full per-ship AI.
    Active,
    /// Inside `aoi_radius_mid`: squad-driven AI, reduced steering (AD-004).
    Mid,
    /// Beyond both radii (or no players at all): skipped / cheap-glide (AD-001).
    Dormant,
}

/// Per-entity AOI sim-LOD state (data-model §AoiTier). Carried by AI ships AND
/// (from T016/T019) squad/aggregate entities; later tasks attach it where
/// needed — T005 only defines + classifies it.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AoiTier {
    /// The current attention tier.
    pub tier: Tier,
    /// Tick the current tier was entered — the hysteresis dwell anchor.
    pub since_tick: u64,
}

impl Default for AoiTier {
    /// Entities start `Dormant` at tick 0: cheap until a player proves
    /// otherwise, and the immediate-promotion rule wakes them the very tick a
    /// player is near (no hysteresis penalty on the way up).
    fn default() -> Self {
        Self {
            tier: Tier::Dormant,
            since_tick: 0,
        }
    }
}

/// Marker: an authoritative, human-controlled player ship — the proximity
/// source AOI tiers derive from (TR-007).
///
/// The sim has no player concept of its own (control is per-entity
/// `ShipIntent`, attached for AI and humans alike), so this marker is the seam:
/// the server's spawn path attaches it to client-owned ships in a later task
/// (T033 scenario authoring); until then tests attach it directly. Purely
/// additive — nothing else reads it.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PlayerShip;

/// Classify every `AoiTier` carrier Active/Mid/Dormant from its distance to
/// the **nearest** [`PlayerShip`] (T005, TR-007; HINT-001).
///
/// Registered in the `ScenarioActive`-gated AI set right after
/// `build_coarse_index_system` and before every tier consumer
/// (scheduler/perception/squad/brain — later tasks). Additionally gated on
/// [`AiTuning`] + [`CurrentTick`] existing (the `recompute_ship_stats_system`
/// graceful-degradation pattern), so a scenario world without the AI resources
/// skips it instead of panicking. It writes ONLY `AoiTier` — additive state
/// nothing else reads yet — so the scenario goldens (`demo_enemies_smoke`,
/// which DOES carry `ScenarioActive`) stay bit-identical.
///
/// **Determinism**: player positions are collected into a `Vec` sorted by
/// entity bits (stable order, independent of archetype iteration); distances
/// compare squared (strict f32, no sqrt); tier changes are pure
/// per-entity state transitions keyed off the shared [`CurrentTick`].
///
/// **Hostile-contact hold (T019, Q1)**: a carrier with an ACTIVE
/// [`HostileContact`] (`now < until_tick`) is never demoted to `Dormant` —
/// player proximity alone would re-demote a hostile-scan-promoted squad the
/// instant its dwell elapsed (no player is near an off-screen battle), so the
/// hold pins it awake while [`far_hostile_scan_system`] keeps refreshing the
/// contact; once hostiles are gone the refresh stops, the hold expires by tick
/// comparison, and normal demotion (with hysteresis) resumes. An expired
/// component is inert (compared, never read otherwise) and is left in place —
/// the next scan-hit simply overwrites it.
pub fn classify_aoi_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    players: Query<(Entity, &Position), With<PlayerShip>>,
    mut subjects: Query<(&Position, &mut AoiTier, Option<&HostileContact>)>,
) {
    let now = tick.0;
    let hysteresis = u64::from(tuning.tier_hysteresis_ticks);
    let active2 = tuning.aoi_radius_active * tuning.aoi_radius_active;
    // R102 Part A — FLOOR the Dormant/cheap-glide cutoff at `glide_min_radius`,
    // DECOUPLED from the tunable Mid radius: a ship within `glide_min_radius`
    // of a player is never Dormant (so never glides), no matter how small
    // `aoi_radius_mid` is set. `aoi_radius_mid` still drives the Active/Mid
    // *think-cadence* split below the floor — but the Dormant boundary (the only
    // one that gates the no-physics glide) is the larger of the two. This is the
    // fix for the dormant-GLIDE LOD leak; the floor is ≥ the max camera view, so
    // no visible ship ever runs the kinematic glide.
    let dormant_cut = tuning.aoi_radius_mid.max(tuning.glide_min_radius);
    let dormant_cut2 = dormant_cut * dormant_cut;

    // Stable-order player snapshot: sorted by entity bits so any tie-sensitive
    // consumer of this list is iteration-order independent (the nearest-distance
    // min below is order-independent already; the sort is the doctrine).
    let mut player_pos: Vec<(u64, Vec2)> =
        players.iter().map(|(e, p)| (e.to_bits(), p.0)).collect();
    player_pos.sort_unstable_by_key(|&(bits, _)| bits);

    for (pos, mut aoi, hold) in &mut subjects {
        // Target tier from nearest-player distance; no players → Dormant
        // (documented module-level choice — empty servers idle their AI).
        let target = if player_pos.is_empty() {
            Tier::Dormant
        } else {
            let mut best = f32::INFINITY;
            for &(_, pp) in &player_pos {
                let d2 = (pp - pos.0).length_squared();
                if d2 < best {
                    best = d2;
                }
            }
            if best <= active2 {
                Tier::Active
            } else if best <= dormant_cut2 {
                // Within the FLOORED Dormant cutoff → Mid (full physics, never
                // glides). The `mid2` band is the natural Mid think-cadence
                // region; the `mid2 < best <= dormant_cut2` shell (present only
                // when `aoi_radius_mid < glide_min_radius`) is held in Mid purely
                // to keep the ship out of the no-physics glide while visible.
                Tier::Mid
            } else {
                Tier::Dormant
            }
        };

        // T019/Q1: an active hostile-contact hold blocks demotion-to-Dormant
        // (see the system docs); every other transition proceeds normally.
        if target == Tier::Dormant && hold.is_some_and(|h| now < h.until_tick) {
            continue;
        }
        if target == aoi.tier {
            continue; // No transition → no write (no change-detection churn).
        }
        // Promotions (toward Active) apply immediately; demotions wait out the
        // dwell (`since_tick + hysteresis ≤ now`) — see the module-level
        // asymmetry rationale.
        let promotion = target < aoi.tier;
        let dwelled = aoi.since_tick.saturating_add(hysteresis) <= now;
        if promotion || dwelled {
            *aoi = AoiTier {
                tier: target,
                since_tick: now,
            };
        }
    }
}

// ---------------------------------------------------------------------------
// T019 — cheap-glide dormant aggregates (TR-008/TR-013, AD-001, Q1)
// ---------------------------------------------------------------------------

/// Marker on a collapsed squad member: this LIVE ship is currently carried by
/// its squad's cheap glide (AD-001 — members are never despawned; their
/// fit/health/identity components stay intact). Systems SKIP gliding ships:
/// `ship_motion_system` excludes them via `Without<Gliding>` (the true
/// O-savings — a gliding member costs one Position/Velocity write per tick in
/// [`glide_motion_system`], not a full flight-model step). Inserted by
/// [`glide_collapse_system`], removed at expansion by [`glide_motion_system`].
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Gliding;

/// Glide state on the SQUAD entity while its members are collapsed (T019).
///
/// The squad entity is the only thing that "moves" during a glide: its
/// `Position` integrates `pos += vel · dt` each tick and every member is
/// re-projected to `squad_pos + offset` — the same op order every tick, so the
/// per-tick member positions ARE the deterministic glide extrapolation TR-008's
/// no-pop definition compares against. `Clone + Debug`, no `Serialize` (V-9).
#[derive(Component, Clone, Debug, PartialEq)]
pub struct GlideState {
    /// Aggregate glide velocity (world units/s) — seeded at collapse to the
    /// members' mean velocity, re-aimed at the squad order's goal at the
    /// anchor speed by [`glide_motion_system`] (on order change only).
    pub vel: Vec2,
    /// Member → offset from the squad centroid at collapse, in the squad's
    /// stable (spawner-authored, never reordered) member order. Dead members
    /// are pruned in place (V-1 spirit); offsets are never recomputed.
    pub member_offsets: Vec<(Entity, Vec2)>,
    /// Tick the collapse happened (TR-020 lifecycle bookkeeping).
    pub collapsed_at: u64,
    /// The EFFECTIVE goal `vel` was last derived from (R98 HOTFIX B2): the
    /// squad order's `MoveTo`/`Withdraw` target while ≥1 alive member is
    /// non-roled, the LEAD member's `brain.waypoint` when every alive member
    /// carries a `ScenarioRole`, `None` when goal-less. Comparing this against
    /// the CURRENT effective goal each tick implements "recompute the aim only
    /// on goal change" — the per-tick glide math stays exactly
    /// `pos += vel · dt` (no per-tick normalize), keeping the extrapolation
    /// cheap AND bit-stable.
    pub goal: Option<Vec2>,
}

/// Hostile-contact promotion hold on a squad entity (T019, Q1): while
/// `now < until_tick`, [`classify_aoi_system`] refuses to demote the carrier
/// to `Dormant`, so a hostile-scan promotion STICKS while hostiles remain.
/// Inserted/refreshed by [`far_hostile_scan_system`] on every scan hit; never
/// removed — an expired hold is inert (pure tick comparison) and the next hit
/// overwrites it.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostileContact {
    /// Demotion-to-Dormant is blocked while `CurrentTick < until_tick`.
    pub until_tick: u64,
}

/// Glide arrive radius (world units): a gliding squad within this range of its
/// order goal stops (`vel = ZERO`) instead of overshooting forever. Mirrors the
/// brain's waypoint arrive radius; per-tick cost is one squared-distance
/// compare.
const GLIDE_ARRIVE_RADIUS: f32 = 10.0;

/// De-penetration step count for the TR-008 validity nudge: the reverse-glide
/// displacement is searched in `promote_nudge_max / NUDGE_STEPS` increments
/// (deterministic fixed ladder, no solver).
const NUDGE_STEPS: u32 = 8;

/// R102 Part A — the glide-visibility guard MARGIN (world units) added to
/// [`AiTuning::glide_min_radius`] when deciding whether a gliding/collapsing
/// squad is "near a player". The classifier floors the Dormant *tier* exactly
/// at `glide_min_radius`, but a squad's MEMBER can sit inside the play space
/// while the centroid is outside it, and a MOVING player closes the gap a few
/// world units per tick (player ~70 u/s + glide anchor speed, each × the 1/30 s
/// step ≈ ≤ 5 u/tick combined). This margin makes the per-member glide guard
/// wake a squad a tick or two BEFORE any member actually enters
/// `glide_min_radius`, so a ship is never observed gliding at or inside the
/// play-space radius (the test/camera boundary). 20 u is generous headroom over
/// one tick of mutual closing yet far below the radius itself.
const GLIDE_FLOOR_MARGIN: f32 = 20.0;

/// COLLAPSE (T019, AD-001): a squad that has settled into `Dormant` becomes a
/// cheap-glide aggregate.
///
/// **Trigger**: squad `AoiTier::tier == Dormant` AND the tier has dwelt at
/// least `tier_hysteresis_ticks` (`since_tick + hysteresis ≤ now`) — the same
/// dwell convention the classifier uses, so a squad that JUST demoted (and
/// might bounce straight back) is not collapsed mid-thrash — AND no
/// [`GlideState`] yet.
///
/// **The collapse** (all in stable member order):
/// 1. record each alive member's offset from the squad centroid (the squad
///    `Position`, freshly maintained by `squad_think_system` this tick),
/// 2. seed the representative glide velocity = mean of member velocities,
/// 3. insert [`GlideState`] on the squad + [`Gliding`] on every member and
///    zero each member's `ShipIntent` (members stay LIVE, components intact —
///    AD-001: re-enable at expansion, never reconstruct).
///
/// Iterates squads in [`AiStableId`] order (V-3 doctrine; each squad touches
/// only its own members, so the order is belt-and-suspenders). Registered
/// gated after `squad_think_system`; no golden world spawns a `Squad`, so the
/// goldens stay bit-identical.
pub fn glide_collapse_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    mut commands: Commands,
    players: Query<&Position, With<PlayerShip>>,
    squads: Query<(Entity, &AiStableId, &Squad, &AoiTier, &Position), Without<GlideState>>,
    mut members: Query<(&Position, &Velocity, Option<&mut ShipIntent>), Without<Squad>>,
) {
    let now = tick.0;
    let hysteresis = u64::from(tuning.tier_hysteresis_ticks);
    // R102 Part A — the glide-visibility FLOOR (see `classify_aoi_system`): a
    // squad is NEVER collapsed to the no-physics glide while ANY of its member
    // ships is within `glide_min_radius` of ANY player, even if the squad
    // CENTROID (the AOI-classified subject) sits just beyond the floor — a
    // formation's spread can place a member inside the play space while the
    // centroid is outside it. This per-MEMBER guard closes that gap exactly (no
    // formation-radius margin guesswork), so a ship the player can see never
    // begins gliding regardless of how the centroid classifies. A small margin
    // (`GLIDE_FLOOR_MARGIN`) keeps a squad from collapsing just OUTSIDE the floor
    // only to be re-woken next tick as a moving player closes in.
    let guard = tuning.glide_min_radius + GLIDE_FLOOR_MARGIN;
    let glide_floor2 = guard * guard;
    let player_pos: Vec<Vec2> = players.iter().map(|p| p.0).collect();
    let member_inside_floor =
        |members: &Query<(&Position, &Velocity, Option<&mut ShipIntent>), Without<Squad>>,
         squad: &Squad|
         -> bool {
            for &m in &squad.members {
                let Ok((pos, _, _)) = members.get(m) else {
                    continue;
                };
                for &pp in &player_pos {
                    if (pp - pos.0).length_squared() <= glide_floor2 {
                        return true;
                    }
                }
            }
            false
        };

    let mut order: Vec<(AiStableId, Entity)> = squads.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();

    for (_, squad_entity) in order {
        let Ok((_, _, squad, aoi, squad_pos)) = squads.get(squad_entity) else {
            continue;
        };
        if aoi.tier != Tier::Dormant || aoi.since_tick.saturating_add(hysteresis) > now {
            continue; // Not dormant, or not hysteresis-settled yet.
        }
        if member_inside_floor(&members, squad) {
            continue; // A visible member — never collapse (R102 floor).
        }

        // Offsets + mean velocity, in stable member order.
        let mut offsets: Vec<(Entity, Vec2)> = Vec::with_capacity(squad.members.len());
        let mut vel_sum = Vec2::ZERO;
        for &m in &squad.members {
            if let Ok((pos, vel, _)) = members.get(m) {
                offsets.push((m, pos.0 - squad_pos.0));
                vel_sum += vel.0;
            }
        }
        if offsets.is_empty() {
            continue; // Nothing to glide (empty squads despawn via squad_think).
        }
        let vel = vel_sum / offsets.len() as f32;

        for &(m, _) in &offsets {
            if let Ok((_, _, Some(mut intent))) = members.get_mut(m) {
                intent.set_if_neq(ShipIntent::default());
            }
            commands.entity(m).insert(Gliding);
        }
        commands.entity(squad_entity).insert(GlideState {
            vel,
            member_offsets: offsets,
            collapsed_at: now,
            goal: None, // First glide tick re-aims if the order has a target.
        });
    }
}

/// Far hostile scan (T019, TR-013/Q1): the dormant-band promotion trigger.
///
/// At the far cadence (`(now + phase_bucket) % scan_ticks_far == 0`), each
/// relevant squad queries the [`CoarseIndex`] around its position with
/// `base_sensor_range`. Relevant = `Dormant` tier, OR currently gliding, OR
/// already holding a [`HostileContact`] (so the hold keeps REFRESHING while
/// hostiles remain — without that the hold would expire, the squad would
/// re-collapse, re-scan, re-promote: oscillation at hysteresis rate).
///
/// A candidate is hostile iff it carries a [`Faction`] and
/// [`hostile`]`(mine, theirs, false)` — the squad's own faction is its first
/// member carrying one (stable member order; none → neutral, which `hostile()`
/// treats as hostile to every factioned entity, matching combat semantics).
/// Own members and the squad entity itself are excluded; faction-less bodies
/// (asteroids, players) never trigger (players promote via proximity already).
///
/// On a hit: promote the squad to `Mid` (`since_tick = now` — promotion
/// semantics, immediate) and insert/refresh
/// `HostileContact { until_tick: now + scan_ticks_far + tier_hysteresis_ticks }`
/// (one full scan period + the hysteresis margin, so the hold always outlives
/// the gap to the next refreshing scan). **Mutuality emerges** (Q1): the other
/// squad's own scan finds THIS squad's member ships the same way — same tick
/// or within its own cadence phase — so both groups promote and the battle
/// runs at full physics; no combat ever occurs while dormant.
pub fn far_hostile_scan_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    coarse: Res<CoarseIndex>,
    mut commands: Commands,
    mut squads: Query<(
        Entity,
        &AiStableId,
        &Squad,
        &mut AoiTier,
        Option<&GlideState>,
        Option<&HostileContact>,
        &Position,
    )>,
    factions: Query<&Faction>,
    bodies: Query<&Position, Without<Squad>>,
) {
    let now = tick.0;
    let cadence = u64::from(tuning.scan_ticks_far.max(1));
    let range = tuning.base_sensor_range;
    let range2 = range * range;

    let mut order: Vec<(AiStableId, Entity)> = squads.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();

    for (_, squad_entity) in order {
        let Ok((e, _, squad, mut aoi, glide, hold, pos)) = squads.get_mut(squad_entity) else {
            continue;
        };
        let relevant = aoi.tier == Tier::Dormant || glide.is_some() || hold.is_some();
        if !relevant || !(now + u64::from(squad.phase_bucket)).is_multiple_of(cadence) {
            continue;
        }

        // Own faction: the first member carrying one (stable member order).
        let mine = squad
            .members
            .iter()
            .find_map(|&m| factions.get(m).ok().copied());

        // Coarse-neighborhood candidates are sorted + de-duplicated; the
        // any-hostile-in-range answer is order-independent regardless.
        let mut found = false;
        for c in coarse.0.near(pos.0, range) {
            if c == e || squad.members.contains(&c) {
                continue;
            }
            let Ok(theirs) = factions.get(c) else {
                continue; // Faction-less bodies never trigger the far scan.
            };
            if !hostile(mine, Some(*theirs), false) {
                continue;
            }
            let Ok(cpos) = bodies.get(c) else {
                continue;
            };
            if (cpos.0 - pos.0).length_squared() <= range2 {
                found = true;
                break;
            }
        }
        if !found {
            continue;
        }

        if aoi.tier == Tier::Dormant {
            *aoi = AoiTier {
                tier: Tier::Mid,
                since_tick: now,
            };
        }
        commands.entity(e).insert(HostileContact {
            until_tick: now + cadence + u64::from(tuning.tier_hysteresis_ticks),
        });
    }
}

/// Glide motion + EXPANSION (T019, TR-008): per gliding squad, either expand
/// (the squad's tier left `Dormant` — player proximity via
/// [`classify_aoi_system`], or a far hostile scan hit) or advance the glide
/// one tick.
///
/// **The glide step** (squad still `Dormant`):
/// 1. *Re-aim on effective-goal change only* (R98 HOTFIX B2 — the glide aims by
///    WHO OWNS the members): when at least one alive member is NON-roled, the
///    effective goal is the squad ORDER's target (`MoveTo`/`Withdraw` — today's
///    behavior; the order layer commands those members). When EVERY alive
///    member carries a [`ScenarioRole`] the squad order can't reach them
///    (roled members are squad-order-exempt — steering the glide by the order
///    would drag them against their roles, the playtest oscillation bug), so
///    the effective goal is the LEAD (first alive) member's `brain.waypoint`
///    (role_apply keeps it pointed along the scripted route at think cadence)
///    — a dormant roled patrol glides its route; with no lead waypoint the
///    glide HOLDS (zero velocity, members coast in place). A CHANGE of the
///    effective goal is treated exactly like the pre-fix order-change re-aim:
///    compare against [`GlideState::goal`], re-aim `vel` at the squad's ANCHOR
///    speed ONCE on change (never per tick); a change to a goal-less state
///    stops the glide. Goal-less from collapse on the ORDER path (e.g. `Hold`)
///    keeps the collapse-time mean drift velocity (documented: a coasting hold
///    drifts — cheap glide has no drag).
/// 2. *Arrive*: within [`GLIDE_ARRIVE_RADIUS`] of the goal → `vel = ZERO`
///    (hold at goal, no overshoot).
/// 3. *Integrate*: `squad_pos += vel · dt` (`dt` = the same [`FixedDt`] the
///    flight model consumes), then every member `Position = squad_pos +
///    offset` and `Velocity = vel` (sensors/render see sane values) — the
///    same op order every tick: this IS the deterministic glide extrapolation.
///
/// **Expansion** (tier ≠ `Dormant`): remove [`GlideState`] + every member's
/// [`Gliding`] marker and push a re-think event per member (`NewContact` when
/// a [`HostileContact`] drove the promotion, `OrderChanged` otherwise).
/// Member positions are NOT recomputed — they are exactly what the last glide
/// tick wrote, so the promote-tick position equals the glide-extrapolated
/// position bit-exactly (TR-008's no-pop definition). The ONLY permitted
/// deviation is the **validity nudge**: a member whose position penetrates a
/// nearby collidable (coarse-neighborhood candidates with a
/// [`CollisionRadius`], overlap = `dist < r_member + r_other`) is de-penetrated
/// along the REVERSE of the glide direction in a fixed ladder of
/// [`NUDGE_STEPS`] increments, total displacement ≤
/// `AiTuning::promote_nudge_max` — deterministic; still-penetrating at the
/// bound keeps the bounded best-effort position. A zero glide velocity has no
/// reverse direction → no nudge (a stationary glide cannot have swept into
/// geometry). Collidable candidates exclude gliding members (`Without<Gliding>`
/// — co-members of THIS squad keep their rigid collapse offsets, and members
/// of other still-gliding/yet-to-apply squads are mid-glide phantoms; the
/// marker removal is deferred Commands, so a same-tick mutual expansion also
/// excludes both sides — accepted v1 simplification) and squad entities.
///
/// Registered gated after [`far_hostile_scan_system`] (a scan promotion
/// expands the SAME tick) and before `ai_think_system`/`ship_motion_system`
/// (an expanded member steers + flies full physics this very tick — the nudge
/// lands "before the first full-physics tick" as TR-008 requires). While a
/// squad glides, `squad_think_system` skips its centroid upkeep — this system
/// owns the squad `Position` (data-model V-6: dormant gliding is the only path
/// mutating `Position`/`Velocity` outside the motion systems).
#[allow(clippy::too_many_arguments)] // One param per seam: resources + three disjoint queries.
pub fn glide_motion_system(
    tuning: Res<AiTuning>,
    dt: Res<FixedDt>,
    coarse: Res<CoarseIndex>,
    entities: &Entities,
    mut queue: ResMut<RethinkQueue>,
    mut commands: Commands,
    // R103 (was R102 Part A) — players for the glide-visibility floor: a gliding
    // squad whose centroid still classifies Dormant but ANY of whose members has
    // drifted within `glide_min_radius` of a player must EXPAND this tick. The
    // centroid-only classifier can lag a member crossing the floor (formation
    // spread + the player MOVING toward the glide), so this is the in-flight twin
    // of the per-member collapse guard.
    //
    // R103 Task 2(a) — the `Without<Squad>` + `Without<Gliding>` filters are
    // SOUNDNESS-REQUIRED, not behavioral: they make this `&Position` view
    // ECS-access-disjoint from the `squads`/`members` queries below (which BORROW
    // `Position` mutably — `members` is `&mut Position` + `With<Gliding>`, so
    // without `Without<Gliding>` Bevy 0.18 flags a conflicting `Position` access
    // and PANICS at schedule build; `colliders` carries the same pair for the
    // same reason). Crucially NEITHER filter can ever exclude a REAL player: a
    // `PlayerShip` is never a squad-member offset carrier (`Squad`) and is never
    // `Gliding` (that marker is inserted ONLY on collapsed squad MEMBERS by
    // `glide_collapse_system`, never on a player). So this set equals
    // `classify_aoi_system`'s `With<PlayerShip>` set restricted to
    // not-squad/not-gliding entities — which for players is the SAME set. The
    // filters therefore CANNOT "silently empty" the set of a present player (the
    // R103 footgun review): a faction-swap that teleports the player onto a
    // gliding neighborhood still expands those squads THIS tick via the
    // floor-breach below, because the marker (and so this set's membership) is
    // never dropped — proven by `server`'s `faction_swap_midflight_does_not_mass_glide`.
    // The remaining transient-empty-player concern (a momentarily missing player)
    // cannot mass-collapse squads either: `classify_aoi_system` demotes to
    // `Dormant` only after the full `tier_hysteresis_ticks` dwell, so a few-tick
    // blip never reaches the Dormant state collapse requires.
    players: Query<&Position, (With<PlayerShip>, Without<Squad>, Without<Gliding>)>,
    mut squads: Query<(
        Entity,
        &AiStableId,
        &Squad,
        &AoiTier,
        Option<&HostileContact>,
        &mut GlideState,
        &mut Position,
    )>,
    mut members: Query<
        (&mut Position, &mut Velocity, Option<&CollisionRadius>),
        (With<Gliding>, Without<Squad>),
    >,
    colliders: Query<(&Position, &CollisionRadius), (Without<Gliding>, Without<Squad>)>,
    // R98 HOTFIX B2 — read-only member OWNERSHIP view (brain waypoint + role
    // presence) for the glide-aim rule. Access-disjoint from `members` (which
    // mutates only Position/Velocity), so the queries coexist.
    member_minds: Query<(Option<&AiBrain>, Option<&ScenarioRole>), Without<Squad>>,
) {
    let dt = dt.0;
    let nudge_max = tuning.promote_nudge_max;
    // R102 Part A — the glide-visibility guard radius: the floor plus a one-tick
    // closing margin (see `GLIDE_FLOOR_MARGIN`), so a gliding squad whose member
    // is about to enter the play space (centroid still Dormant, player moving in)
    // expands BEFORE the member is observed inside `glide_min_radius`.
    let guard = tuning.glide_min_radius + GLIDE_FLOOR_MARGIN;
    let glide_floor2 = guard * guard;
    let player_pos: Vec<Vec2> = players.iter().map(|p| p.0).collect();

    // Overlap test against the coarse neighborhood (conservative margin: one
    // coarse cell covers any neighbor with radius ≤ COARSE_CELL_SIZE, since
    // bodies are point-inserted into the coarse grid).
    let penetrates = |p: Vec2, my_r: f32, me: Entity| -> bool {
        for c in coarse.0.near(p, my_r + COARSE_CELL_SIZE) {
            if c == me {
                continue;
            }
            if let Ok((cpos, cr)) = colliders.get(c) {
                let r = my_r + cr.0;
                if (cpos.0 - p).length_squared() < r * r {
                    return true;
                }
            }
        }
        false
    };

    let mut order: Vec<(AiStableId, Entity)> = squads.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();

    for (_, squad_entity) in order {
        let Ok((_, _, squad, aoi, hold, mut gs, mut squad_pos)) = squads.get_mut(squad_entity)
        else {
            continue;
        };

        // Prune dead members from the offset table (V-1 spirit; the canonical
        // sweep doesn't know GlideState). Order preserved, never recomputed.
        if gs
            .member_offsets
            .iter()
            .any(|(m, _)| !entities.contains(*m))
        {
            gs.member_offsets.retain(|(m, _)| entities.contains(*m));
        }

        // R102 Part A — the glide-visibility FLOOR breach: ANY member within
        // `glide_min_radius` of ANY player forces expansion this tick, even
        // while the centroid still classifies Dormant. Tested against the
        // member's position AFTER this tick's glide integration
        // (`squad_pos + vel·dt + offset`) so a member the glide is about to push
        // INTO the play space wakes the SAME tick it crosses (not a tick late) —
        // the current position is `squad_pos + offset`, and `vel·dt` is the step
        // this system is about to take; covering the post-step position also
        // covers the current one whenever the glide closes on the player. The
        // squad's `Position` is owned by this system, so this is the authoritative
        // pre-integration value.
        let predicted = squad_pos.0 + gs.vel * dt;
        let floor_breach = !player_pos.is_empty()
            && gs.member_offsets.iter().any(|&(_, off)| {
                let mpos = predicted + off;
                let cur = squad_pos.0 + off;
                player_pos.iter().any(|&pp| {
                    (pp - mpos).length_squared() <= glide_floor2
                        || (pp - cur).length_squared() <= glide_floor2
                })
            });

        // ------------------------------------------------------------------
        // EXPAND: the tier left Dormant (player proximity or hostile scan), OR
        // a member crossed the glide-visibility floor (R102 Part A).
        // ------------------------------------------------------------------
        if aoi.tier != Tier::Dormant || floor_breach {
            let glide_vel = gs.vel;
            let speed2 = glide_vel.length_squared();
            let event = if hold.is_some() {
                AiEvent::NewContact // Hostile-scan promotion: a contact woke us.
            } else {
                AiEvent::OrderChanged // Player-proximity promotion: re-think.
            };
            for &(m, _) in &gs.member_offsets {
                let Ok((mut mpos, _, mr)) = members.get_mut(m) else {
                    continue;
                };
                // TR-008 validity nudge (see system docs): reverse-glide
                // de-penetration ladder, ≤ promote_nudge_max total.
                let my_r = mr.map_or(0.0, |r| r.0);
                if speed2 > 0.0 && nudge_max > 0.0 && penetrates(mpos.0, my_r, m) {
                    let reverse = -(glide_vel / speed2.sqrt());
                    let step = nudge_max / NUDGE_STEPS as f32;
                    let base = mpos.0;
                    for k in 1..=NUDGE_STEPS {
                        mpos.0 = base + reverse * (step * k as f32);
                        if !penetrates(mpos.0, my_r, m) {
                            break; // Clear — and ≤ nudge_max by construction.
                        }
                    }
                }
                commands.entity(m).remove::<Gliding>();
                queue.push(m, event);
            }
            commands.entity(squad_entity).remove::<GlideState>();
            continue;
        }

        // ------------------------------------------------------------------
        // GLIDE: one cheap extrapolation tick.
        // ------------------------------------------------------------------
        // 1. Re-aim on EFFECTIVE-GOAL change ONLY (per-tick math stays
        //    pos += vel·dt). R98 HOTFIX B2 — the effective goal is owned by
        //    whoever commands the members (see the system docs): ≥1 alive
        //    NON-roled member → the squad ORDER's target (today's behavior);
        //    ALL alive members roled → the LEAD (first alive) member's brain
        //    waypoint (the scripted route), else HOLD (zero glide velocity).
        let mut alive_seen = false;
        let mut all_roled = true;
        let mut lead_waypoint: Option<Vec2> = None;
        for &m in &squad.members {
            if !entities.contains(m) {
                continue;
            }
            let Ok((brain, role)) = member_minds.get(m) else {
                continue;
            };
            if !alive_seen {
                alive_seen = true;
                lead_waypoint = brain.and_then(|b| b.waypoint);
            }
            if role.is_none() {
                all_roled = false;
                break; // The order layer commands this member → order path.
            }
        }
        let role_owned = alive_seen && all_roled;
        let goal = if role_owned {
            lead_waypoint
        } else {
            match squad.order {
                SquadOrder::MoveTo(g) | SquadOrder::Withdraw(g) => Some(g),
                SquadOrder::Hold | SquadOrder::FormUp | SquadOrder::Engage(_) => None,
            }
        };
        if goal != gs.goal {
            gs.goal = goal;
            gs.vel = match goal {
                Some(g) => {
                    let to = g - squad_pos.0;
                    let len = to.length();
                    if len > f32::EPSILON {
                        to / len * squad.anchor_speed
                    } else {
                        Vec2::ZERO
                    }
                }
                None => Vec2::ZERO, // The effective goal vanished mid-glide: stop.
            };
        } else if role_owned && goal.is_none() && gs.vel != Vec2::ZERO {
            // R98 HOTFIX B2 — a role-owned squad with NO lead waypoint HOLDS:
            // unlike the order path's documented coasting Hold (mean-drift),
            // there is no order to drift on — zero the glide so the roled
            // members coast in place (compare-before-write keeps this quiet).
            gs.vel = Vec2::ZERO;
        }
        // 2. Arrive: hold at the goal instead of overshooting forever.
        if let Some(g) = gs.goal {
            if gs.vel != Vec2::ZERO
                && (g - squad_pos.0).length_squared() <= GLIDE_ARRIVE_RADIUS * GLIDE_ARRIVE_RADIUS
            {
                gs.vel = Vec2::ZERO;
            }
        }
        // 3. Integrate the squad, project the members (bit-stable op order).
        let new_pos = squad_pos.0 + gs.vel * dt;
        if squad_pos.0 != new_pos {
            squad_pos.0 = new_pos;
        }
        for &(m, offset) in &gs.member_offsets {
            if let Ok((mut mpos, mut mvel, _)) = members.get_mut(m) {
                let p = new_pos + offset;
                if mpos.0 != p {
                    mpos.0 = p;
                }
                if mvel.0 != gs.vel {
                    mvel.0 = gs.vel;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::Schedule;
    use bevy_ecs::world::World;

    /// A minimal world with the two resources the classifier reads, at tick 0.
    fn lod_world() -> World {
        let mut world = World::new();
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick(0));
        world
    }

    fn run_classifier(world: &mut World) {
        let mut schedule = Schedule::default();
        schedule.add_systems(classify_aoi_system);
        schedule.run(world);
    }

    fn set_tick(world: &mut World, tick: u64) {
        world.resource_mut::<CurrentTick>().0 = tick;
    }

    fn tier_of(world: &World, e: Entity) -> AoiTier {
        *world.get::<AoiTier>(e).expect("subject carries AoiTier")
    }

    /// (a) Promotion is immediate: a fresh (Dormant-default) entity near a
    /// player goes Active on the very first classify — and one in the mid band
    /// goes Mid — with zero hysteresis wait.
    #[test]
    fn near_player_promotes_immediately() {
        let mut world = lod_world();
        world.spawn((PlayerShip, Position(Vec2::ZERO)));
        // Defaults (R98 HOTFIX B3): aoi_radius_active 120, aoi_radius_mid 520
        // — same band geometry as before, positions re-pinned to the new radii.
        let near = world
            .spawn((Position(Vec2::new(10.0, 0.0)), AoiTier::default()))
            .id();
        let mid = world
            .spawn((Position(Vec2::new(300.0, 0.0)), AoiTier::default()))
            .id();
        let far = world
            .spawn((Position(Vec2::new(1000.0, 0.0)), AoiTier::default()))
            .id();

        run_classifier(&mut world);

        assert_eq!(
            tier_of(&world, near).tier,
            Tier::Active,
            "promoted same tick"
        );
        assert_eq!(tier_of(&world, near).since_tick, 0);
        assert_eq!(
            tier_of(&world, mid).tier,
            Tier::Mid,
            "mid band promotes too"
        );
        assert_eq!(
            tier_of(&world, far).tier,
            Tier::Dormant,
            "out of range stays"
        );
    }

    /// (b) Demotion waits out the hysteresis: an Active entity whose player
    /// moved away keeps its tier until `since_tick + hysteresis ≤ now`, then
    /// demotes (and re-stamps `since_tick`).
    #[test]
    fn demotion_waits_for_hysteresis_then_commits() {
        let mut world = lod_world();
        world.spawn((PlayerShip, Position(Vec2::new(1000.0, 0.0)))); // far away
        let e = world
            .spawn((
                Position(Vec2::ZERO),
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 100,
                },
            ))
            .id();

        // Default tier_hysteresis_ticks = 30 → dwell holds until tick 130.
        set_tick(&mut world, 110);
        run_classifier(&mut world);
        assert_eq!(
            tier_of(&world, e),
            AoiTier {
                tier: Tier::Active,
                since_tick: 100
            },
            "demotion deferred inside the hysteresis window"
        );

        set_tick(&mut world, 130);
        run_classifier(&mut world);
        assert_eq!(
            tier_of(&world, e),
            AoiTier {
                tier: Tier::Dormant,
                since_tick: 130
            },
            "demotion commits once the dwell elapses, re-stamping since_tick"
        );
    }

    /// (c) No players at all → everything targets Dormant (the documented
    /// empty-server choice); the demotion still honors hysteresis on the way.
    #[test]
    fn no_players_everything_goes_dormant() {
        let mut world = lod_world();
        let active = world
            .spawn((
                Position(Vec2::ZERO),
                AoiTier {
                    tier: Tier::Active,
                    since_tick: 0,
                },
            ))
            .id();
        let fresh = world.spawn((Position(Vec2::ZERO), AoiTier::default())).id();

        set_tick(&mut world, 10); // inside the 30-tick dwell
        run_classifier(&mut world);
        assert_eq!(tier_of(&world, active).tier, Tier::Active, "dwell holds");
        assert_eq!(tier_of(&world, fresh).tier, Tier::Dormant, "default stays");

        set_tick(&mut world, 30); // dwell elapsed (0 + 30 ≤ 30)
        run_classifier(&mut world);
        assert_eq!(tier_of(&world, active).tier, Tier::Dormant);
        assert_eq!(tier_of(&world, active).since_tick, 30);
    }

    /// (d) The promotion asymmetry: a Dormant entity that JUST changed tier
    /// (mid-hysteresis) still promotes to Active the instant a player is near.
    #[test]
    fn promotion_is_immediate_even_mid_hysteresis() {
        let mut world = lod_world();
        world.spawn((PlayerShip, Position(Vec2::ZERO)));
        let e = world
            .spawn((
                Position(Vec2::new(10.0, 0.0)),
                AoiTier {
                    tier: Tier::Dormant,
                    since_tick: 95, // only 5 ticks dwelt at tick 100 (< 30)
                },
            ))
            .id();

        set_tick(&mut world, 100);
        run_classifier(&mut world);
        assert_eq!(
            tier_of(&world, e),
            AoiTier {
                tier: Tier::Active,
                since_tick: 100
            },
            "promotion bypasses the hysteresis dwell"
        );
    }

    /// The attention ordering the promotion test keys off: Active < Mid < Dormant.
    #[test]
    fn tier_ordering_is_attention_order() {
        assert!(Tier::Active < Tier::Mid);
        assert!(Tier::Mid < Tier::Dormant);
    }

    // --- T019: cheap-glide aggregates --------------------------------------

    use crate::ai::brain::AiBrain;
    use crate::ai::squad::{spawn_squad, squad_think_system, FormationDef};
    use crate::broadphase::build_coarse_index_system;
    use crate::components::{Heading, Ship};
    use crate::tuning::Tuning;

    /// A world + schedule mirroring the real registration order of every
    /// system the glide seam interacts with (coarse build → classify →
    /// squad think → collapse → far scan → glide/expand), at tick 0.
    fn glide_world() -> (World, Schedule) {
        let mut world = World::new();
        world.insert_resource(Tuning::default());
        world.insert_resource(AiTuning::default());
        world.insert_resource(CurrentTick(0));
        world.insert_resource(FixedDt::default());
        world.insert_resource(RethinkQueue::default());
        world.insert_resource(CoarseIndex::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                build_coarse_index_system,
                classify_aoi_system,
                squad_think_system,
                glide_collapse_system,
                far_hostile_scan_system,
                glide_motion_system,
            )
                .chain(),
        );
        (world, schedule)
    }

    fn step(world: &mut World, schedule: &mut Schedule, tick: u64) {
        world.resource_mut::<CurrentTick>().0 = tick;
        schedule.run(world);
    }

    fn spawn_member(world: &mut World, pos: Vec2, vel: Vec2) -> Entity {
        world
            .spawn((
                Ship,
                Position(pos),
                Velocity(vel),
                Heading(0.0),
                ShipIntent::default(),
                AiBrain::default(),
            ))
            .id()
    }

    fn pos_of(world: &World, e: Entity) -> Vec2 {
        world.get::<Position>(e).expect("entity has Position").0
    }

    /// Default hysteresis is 30 ticks: a fresh (since_tick 0) Dormant squad
    /// collapses exactly at tick 30. Run ticks `0..=29` (no collapse yet).
    fn run_to_collapse_eve(world: &mut World, schedule: &mut Schedule) {
        for t in 0..30 {
            step(world, schedule, t);
        }
    }

    /// (1) Collapse records stable-order centroid offsets + the mean member
    /// velocity, marks members `Gliding`, and zeroes their intent — and only
    /// fires once the Dormant dwell elapses.
    #[test]
    fn collapse_records_offsets_and_marks_members() {
        let (mut world, mut schedule) = glide_world();
        let m0 = spawn_member(&mut world, Vec2::new(-10.0, 0.0), Vec2::new(3.0, 0.0));
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0), Vec2::new(1.0, 0.0));
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::line_abreast(2, 10.0),
            SquadOrder::Hold, // Goal-less: the collapse drift velocity persists.
        );
        world.get_mut::<ShipIntent>(m0).unwrap().forward = 1.0;

        run_to_collapse_eve(&mut world, &mut schedule);
        assert!(
            world.get::<GlideState>(se).is_none(),
            "no collapse inside the Dormant dwell (hysteresis-settled rule)"
        );

        step(&mut world, &mut schedule, 30);
        let gs = world.get::<GlideState>(se).expect("collapsed at tick 30");
        assert_eq!(gs.collapsed_at, 30);
        assert_eq!(
            gs.member_offsets,
            vec![(m0, Vec2::new(-10.0, 0.0)), (m1, Vec2::new(10.0, 0.0))],
            "centroid offsets in stable member order"
        );
        assert_eq!(
            gs.vel,
            Vec2::new(2.0, 0.0),
            "glide velocity = mean of member velocities"
        );
        for m in [m0, m1] {
            assert!(world.get::<Gliding>(m).is_some(), "member marked Gliding");
            assert_eq!(
                *world.get::<ShipIntent>(m).unwrap(),
                ShipIntent::default(),
                "member intent zeroed at collapse"
            );
        }
    }

    /// (2) The glide is bit-consistent: every tick, member position equals
    /// `squad_pos + offset` EXACTLY, member velocity equals the glide
    /// velocity, and the squad path replays the documented arithmetic
    /// (`re-aim once, then pos += vel·dt`) bit-for-bit.
    #[test]
    fn glide_moves_squad_and_members_bit_consistently() {
        let (mut world, mut schedule) = glide_world();
        let goal = Vec2::new(400.0, 0.0);
        let m0 = spawn_member(&mut world, Vec2::new(-10.0, 0.0), Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0), Vec2::ZERO);
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::line_abreast(2, 10.0),
            SquadOrder::MoveTo(goal),
        );

        run_to_collapse_eve(&mut world, &mut schedule);

        // Replay the system's exact arithmetic alongside it: the re-aim at the
        // collapse tick (centroid (0,0)), then pos += vel·dt every tick.
        let dt = FixedDt::default().0;
        let anchor = Tuning::default().top_speed();
        let mut expected = Vec2::ZERO;
        let to = goal - expected;
        let vel = to / to.length() * anchor;
        for t in 30..=90 {
            step(&mut world, &mut schedule, t);
            expected += vel * dt;
            assert_eq!(pos_of(&world, se), expected, "squad glide path (tick {t})");
            let gs = world.get::<GlideState>(se).unwrap();
            assert_eq!(gs.vel, vel, "re-aimed once at the anchor speed");
            for (m, off) in [(m0, Vec2::new(-10.0, 0.0)), (m1, Vec2::new(10.0, 0.0))] {
                assert_eq!(
                    pos_of(&world, m),
                    expected + off,
                    "member = squad + offset, bit-exact (tick {t})"
                );
                assert_eq!(
                    world.get::<Velocity>(m).unwrap().0,
                    vel,
                    "member velocity mirrors the glide velocity"
                );
            }
        }
    }

    /// (3) TR-008 no-pop: promotion (player proximity) at tick N leaves every
    /// member EXACTLY at its glide-extrapolated position — bit-compared — with
    /// markers/state removed and a re-think event pushed (no penetration → no
    /// nudge; `OrderChanged` since no hostile contact drove it).
    #[test]
    fn expansion_positions_equal_glide_positions_bit_exactly() {
        let (mut world, mut schedule) = glide_world();
        let m0 = spawn_member(&mut world, Vec2::new(-10.0, 0.0), Vec2::ZERO);
        let m1 = spawn_member(&mut world, Vec2::new(10.0, 0.0), Vec2::ZERO);
        let se = spawn_squad(
            &mut world,
            &[m0, m1],
            FormationDef::line_abreast(2, 10.0),
            SquadOrder::MoveTo(Vec2::new(400.0, 0.0)),
        );

        for t in 0..=60 {
            step(&mut world, &mut schedule, t);
        }
        assert!(world.get::<GlideState>(se).is_some(), "gliding by tick 60");
        let glide_pos = [pos_of(&world, m0), pos_of(&world, m1)];

        // A player appears on top of the squad → classify promotes the squad
        // this very tick (promotion is immediate) → same-tick expansion.
        world.spawn((PlayerShip, Position(pos_of(&world, se))));
        world.resource_mut::<RethinkQueue>().clear();
        step(&mut world, &mut schedule, 61);

        assert!(world.get::<GlideState>(se).is_none(), "GlideState removed");
        for (i, m) in [m0, m1].into_iter().enumerate() {
            assert!(world.get::<Gliding>(m).is_none(), "Gliding removed");
            assert_eq!(
                pos_of(&world, m),
                glide_pos[i],
                "promote-tick position == glide-extrapolated position (bit-exact)"
            );
            assert_eq!(
                world.resource::<RethinkQueue>().get(m),
                Some(AiEvent::OrderChanged),
                "player-proximity expansion pushes OrderChanged"
            );
        }
    }

    /// (4) The TR-008 validity nudge: a member expanding INSIDE a collidable is
    /// de-penetrated along the REVERSE of the glide direction, by the smallest
    /// ladder step that clears, bounded by `promote_nudge_max`.
    #[test]
    fn forced_penetration_expansion_nudges_along_reverse_glide() {
        let (mut world, mut schedule) = glide_world();
        world.resource_mut::<AiTuning>().promote_nudge_max = 8.0;
        let m0 = spawn_member(&mut world, Vec2::ZERO, Vec2::ZERO);
        let se = spawn_squad(
            &mut world,
            &[m0],
            FormationDef::wedge(1, 10.0),
            SquadOrder::MoveTo(Vec2::new(400.0, 0.0)), // Glide direction = +X.
        );

        for t in 0..=60 {
            step(&mut world, &mut schedule, t);
        }
        assert!(world.get::<GlideState>(se).is_some(), "gliding by tick 60");
        let glide_pos = pos_of(&world, m0);

        // A collidable 2 units AHEAD with radius 2.5 → the member (radius 0)
        // penetrates (dist ≈ 2 < 2.5). Reverse-glide is -X; the 8-step ladder
        // over nudge_max 8 steps 1.0 at a time: k = 1 gives dist ≈ 3 ≥ 2.5
        // (clear, with float headroom on both sides of the inequality).
        world.spawn((
            Ship,
            Position(glide_pos + Vec2::new(2.0, 0.0)),
            CollisionRadius(2.5),
        ));
        world.spawn((PlayerShip, Position(glide_pos)));
        step(&mut world, &mut schedule, 61);

        assert!(world.get::<Gliding>(m0).is_none(), "expanded");
        let promoted = pos_of(&world, m0);
        assert_eq!(
            promoted,
            glide_pos - Vec2::new(1.0, 0.0),
            "nudged one ladder step along the REVERSE of the glide direction"
        );
        assert!(
            (promoted - glide_pos).length() <= 8.0,
            "total displacement within promote_nudge_max"
        );
    }

    /// (5) Q1 mutual hostile promotion: a gliding squad whose far scan finds a
    /// hostile-factioned ship promotes (Gliding removed, `Mid` + hold,
    /// `NewContact` pushed) — and the other squad promotes via ITS own scan.
    #[test]
    fn far_hostile_within_sensor_range_promotes_the_squad() {
        let (mut world, mut schedule) = glide_world();
        let ma = spawn_member(&mut world, Vec2::ZERO, Vec2::ZERO);
        world.entity_mut(ma).insert(Faction::Red);
        let sa = spawn_squad(
            &mut world,
            &[ma],
            FormationDef::wedge(1, 10.0),
            SquadOrder::Hold,
        );

        // Let squad A settle into its glide alone first (collapse ≈ tick 30;
        // 130 also clears any scan-due tick A had while nothing was around).
        for t in 0..=130 {
            step(&mut world, &mut schedule, t);
        }
        assert!(
            world.get::<GlideState>(sa).is_some(),
            "A gliding while alone"
        );

        // A hostile squad appears within base_sensor_range (200).
        let mb = spawn_member(&mut world, Vec2::new(150.0, 0.0), Vec2::ZERO);
        world.entity_mut(mb).insert(Faction::Blue);
        let sb = spawn_squad(
            &mut world,
            &[mb],
            FormationDef::wedge(1, 10.0),
            SquadOrder::Hold,
        );

        // A's next due far scan (≤ one 90-tick cadence away) must promote it.
        let mut expanded_at = None;
        for t in 131..=320 {
            world.resource_mut::<RethinkQueue>().clear();
            step(&mut world, &mut schedule, t);
            if world.get::<GlideState>(sa).is_none() {
                expanded_at = Some(t);
                break;
            }
        }
        let t = expanded_at.expect("A promotes within one far-scan cadence");
        assert!(world.get::<Gliding>(ma).is_none(), "Gliding removed");
        let aoi = *world.get::<AoiTier>(sa).unwrap();
        assert_eq!(aoi.tier, Tier::Mid, "hostile promotion targets Mid");
        assert_eq!(aoi.since_tick, t, "promotion stamped at the scan tick");
        assert!(
            world
                .get::<HostileContact>(sa)
                .is_some_and(|h| h.until_tick > t),
            "hostile-contact hold inserted"
        );
        assert_eq!(
            world.resource::<RethinkQueue>().get(ma),
            Some(AiEvent::NewContact),
            "hostile expansion pushes NewContact"
        );

        // Mutuality (Q1): B promotes via its OWN scan within its cadence, and
        // the holds keep BOTH awake (no re-collapse) while in contact.
        for t in (t + 1)..=(t + 200) {
            step(&mut world, &mut schedule, t);
        }
        assert_eq!(world.get::<AoiTier>(sb).unwrap().tier, Tier::Mid);
        assert!(world.get::<HostileContact>(sb).is_some(), "B holds contact");
        for s in [sa, sb] {
            assert!(
                world.get::<GlideState>(s).is_none(),
                "promoted squads stay expanded while hostiles remain (no dormant combat)"
            );
        }
    }

    /// (7) R98 HOTFIX B2 — the glide aims by member OWNERSHIP: a squad whose
    /// alive members ALL carry a `ScenarioRole` glides toward the LEAD member's
    /// `brain.waypoint` (the scripted route) — NOT the squad order's target —
    /// and HOLDS (zero velocity) when the lead has no waypoint; a MIXED squad
    /// (≥1 non-roled member) keeps aiming by the squad order (today's path).
    #[test]
    fn role_owned_glide_follows_lead_waypoint_not_order() {
        use crate::ai::role::{Posture, RoleGoal, ScenarioRole};
        let (mut world, mut schedule) = glide_world();
        let order_goal = Vec2::new(400.0, 0.0); // The order pulls +X …
        let route_goal = Vec2::new(0.0, 400.0); // … the role's route pulls +Y.

        // (a) ALL-ROLED squad: glide follows the lead waypoint, not the order.
        let roled = spawn_member(&mut world, Vec2::ZERO, Vec2::ZERO);
        world.entity_mut(roled).insert(ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![route_goal]),
            Posture::HoldFire,
        ));
        world.get_mut::<AiBrain>(roled).unwrap().waypoint = Some(route_goal);
        let roled_sq = spawn_squad(
            &mut world,
            &[roled],
            FormationDef::wedge(1, 10.0),
            SquadOrder::MoveTo(order_goal),
        );

        // (b) ALL-ROLED squad with NO lead waypoint: the glide HOLDS (zero
        // velocity) — seeded drift is zeroed, members coast in place.
        let parked = spawn_member(&mut world, Vec2::new(2_000.0, 0.0), Vec2::new(3.0, 0.0));
        world.entity_mut(parked).insert(ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![]),
            Posture::HoldFire,
        ));
        let parked_sq = spawn_squad(
            &mut world,
            &[parked],
            FormationDef::wedge(1, 10.0),
            SquadOrder::MoveTo(order_goal),
        );

        // (c) MIXED squad (roled lead + plain wingman): the ORDER still owns
        // the glide (≥1 commandable member — today's behavior, unchanged).
        let mixed_roled = spawn_member(&mut world, Vec2::new(4_000.0, 0.0), Vec2::ZERO);
        world.entity_mut(mixed_roled).insert(ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![route_goal]),
            Posture::HoldFire,
        ));
        world.get_mut::<AiBrain>(mixed_roled).unwrap().waypoint = Some(route_goal);
        let mixed_plain = spawn_member(&mut world, Vec2::new(4_010.0, 0.0), Vec2::ZERO);
        let mixed_sq = spawn_squad(
            &mut world,
            &[mixed_roled, mixed_plain],
            FormationDef::line_abreast(2, 10.0),
            SquadOrder::MoveTo(Vec2::new(4_005.0, 800.0)),
        );

        // No players → everything settles Dormant; collapse lands at tick 30
        // and the SAME tick's glide step performs the first re-aim.
        for t in 0..=30 {
            step(&mut world, &mut schedule, t);
        }

        let gs = world
            .get::<GlideState>(roled_sq)
            .expect("roled squad glides");
        assert_eq!(
            gs.goal,
            Some(route_goal),
            "all-roled squad: effective goal = the LEAD member's waypoint"
        );
        assert!(
            gs.vel.y > 0.0 && gs.vel.x == 0.0,
            "the glide aims along the scripted route (+Y), not the order (+X): {:?}",
            gs.vel
        );

        let gs = world
            .get::<GlideState>(parked_sq)
            .expect("parked squad glides");
        assert_eq!(gs.goal, None, "no lead waypoint → no effective goal");
        assert_eq!(
            gs.vel,
            Vec2::ZERO,
            "role-owned squad with no waypoint HOLDS (seeded drift zeroed)"
        );
        let parked_pos = pos_of(&world, parked);
        step(&mut world, &mut schedule, 31);
        assert_eq!(
            pos_of(&world, parked),
            parked_pos,
            "held members coast in place (no kinematic drag toward the order)"
        );

        let gs = world
            .get::<GlideState>(mixed_sq)
            .expect("mixed squad glides");
        assert_eq!(
            gs.goal,
            Some(Vec2::new(4_005.0, 800.0)),
            "a mixed squad still glides by the squad ORDER target (unchanged)"
        );
    }

    /// (6) No hostile → the squad keeps gliding: same-faction neighbors and a
    /// squad's OWN members never trigger the far scan, and nothing demotes the
    /// glide state across many cadences.
    #[test]
    fn no_hostile_stays_gliding() {
        let (mut world, mut schedule) = glide_world();
        let ma = spawn_member(&mut world, Vec2::ZERO, Vec2::ZERO);
        world.entity_mut(ma).insert(Faction::Red);
        let sa = spawn_squad(
            &mut world,
            &[ma],
            FormationDef::wedge(1, 10.0),
            SquadOrder::Hold,
        );
        // A FRIENDLY squad well inside sensor range: must not promote anyone.
        let mb = spawn_member(&mut world, Vec2::new(100.0, 0.0), Vec2::ZERO);
        world.entity_mut(mb).insert(Faction::Red);
        let sb = spawn_squad(
            &mut world,
            &[mb],
            FormationDef::wedge(1, 10.0),
            SquadOrder::Hold,
        );

        for t in 0..=300 {
            step(&mut world, &mut schedule, t);
        }
        for (s, m) in [(sa, ma), (sb, mb)] {
            assert!(world.get::<GlideState>(s).is_some(), "still gliding");
            assert!(world.get::<Gliding>(m).is_some(), "member still marked");
            assert_eq!(world.get::<AoiTier>(s).unwrap().tier, Tier::Dormant);
            assert!(
                world.get::<HostileContact>(s).is_none(),
                "friendlies/own members never register as hostile contact"
            );
        }
    }
}
