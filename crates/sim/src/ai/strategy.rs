//! Strategic objective / planner tier (R97 Phase 2 Stage E, TR-009): the SLOW,
//! additive HTN layer ABOVE the squad system. A squad that carries a
//! [`SquadObjective`] has its [`Objective`] decomposed — at a slow cadence — into
//! the next [`SquadOrder`] from world state; `squad_think_system` then translates
//! that order into member brain modes, and the Phase-1 per-ship channel-fusion
//! executes it. NOTHING below the order layer changes: this tier only WRITES
//! `squad.order` (compare-before-write, riding the existing `OrderChanged` path).
//!
//! **Layering (documented)**: `strategic_plan_system` runs BEFORE
//! `squad_think_system` so an objective sets the squad order the SAME tick the
//! squad think reads it. A squad WITHOUT a [`SquadObjective`] is untouched (the
//! planner's query skips it) — every existing squad test and the golden trio are
//! unaffected, and no golden world spawns an `Objective`, so the planner never
//! runs there.
//!
//! **v1 = authored objective + HTN adaptation (documented choice)**. The
//! objective is EXTERNALLY authored (scenario/tests), like a `SquadOrder` was in
//! Phase 1. The planner does NOT *select* among candidate objectives by utility
//! scoring; instead each objective AUTHORED onto a squad is decomposed by a
//! bounded, authored HTN ([`decompose`]) that adapts to the live picture
//! (engage → withdraw → regroup, patrol → engage → resume). This is the simplest
//! fully-deterministic v1 that still showcases adaptive strategy; multi-objective
//! UTILITY selection (score `value × feasibility × own_strength`, pick the
//! highest) is noted as a follow-up — the [`score_behavior`]-style machinery is
//! already in place to host it.
//!
//! [`score_behavior`]: crate::ai::brain::score_behavior
//!
//! **Determinism (V-3/V-4)**: planning iterates squads in [`AiStableId`] order;
//! the fused picture is read from the per-faction [`SensorNetworks`] `BTreeMap`
//! (no `HashMap`); the squad's faction is the FIRST alive member's faction in
//! stable member order; strength is an integer-count / `f32`-sum estimate with
//! no `rand` and no unbounded search; nearest-intruder/escort tiebreaks reuse
//! `perception::nearest_contact`'s exact bits rule. The cadence gate is the same
//! `(now + phase_bucket) % cadence` discipline the squad/think systems use, but
//! at the SLOW `strategic_plan_ticks` (~3 s) cadence.

use bevy_ecs::entity::Entities;
use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::command::PlayerOrder;
use crate::ai::ident::AiStableId;
use crate::ai::perception::{Contact, SensorNetworks};
use crate::ai::role::ScenarioRole;
use crate::ai::squad::{Squad, SquadOrder};
use crate::ai::tuning::AiTuning;
use crate::clock::CurrentTick;
use crate::components::{Faction, Health, Position};

/// R98 HOTFIX B1 / R99 Phase A — whether every ALIVE member of `members` is
/// squad-order-EXEMPT: it carries EITHER a [`ScenarioRole`] (script > squad) OR
/// a [`PlayerOrder`] (player > squad). Exempt members never receive a squad goal
/// at the brain level (`squad_think_system` skips them), so an order planned for
/// an all-exempt squad can reach NOTHING but the dormant cheap-glide, which
/// would then drag the members against their role/command (the full-speed
/// kinematic oscillation seen at the dormant boundary). BOTH planner tiers
/// ([`strategic_plan_system`] / [`wing_plan_system`]) skip such squads — planning
/// for them is incoherent. A squad with ZERO alive members returns `false`: the
/// existing empty-squad handling (skip/despawn via `squad_think_system`) owns
/// that case, unchanged.
fn all_members_roled(
    members: &[Entity],
    entities: &Entities,
    exempt: &Query<(Option<&ScenarioRole>, Has<PlayerOrder>)>,
) -> bool {
    let mut alive = 0usize;
    for &m in members {
        if !entities.contains(m) {
            continue; // Despawned — the V-1 sweep prunes it; not a member.
        }
        alive += 1;
        // A member is commandable unless it carries a role OR a player order.
        let member_exempt = matches!(exempt.get(m), Ok((Some(_), _)) | Ok((_, true)));
        if !member_exempt {
            return false; // At least one alive, commandable member.
        }
    }
    alive > 0
}

// ---------------------------------------------------------------------------
// Objective + SquadObjective (the STRATEGIC layer, over `Squad`)
// ---------------------------------------------------------------------------

/// A squad-level STRATEGIC goal (the planner's input) — ephemeral like every
/// other AI component (`Clone + Debug`, NO `Serialize`, V-9). Authored by the
/// scenario/tests onto a squad entity; [`decompose`] maps it to the next
/// [`SquadOrder`] each plan tick. The objective itself can FLIP under HTN
/// adaptation (e.g. `DestroyTarget` → `Withdraw` when outnumbered).
#[derive(Clone, Debug, PartialEq)]
pub enum Objective {
    /// Hold station — the planner sets [`SquadOrder::Hold`].
    Hold,
    /// Destroy `target`: clear perceived escorts/defenders first, then the
    /// target; flip to [`Objective::Withdraw`] when outnumbered (see
    /// [`decompose`]).
    DestroyTarget(Entity),
    /// Defend a zone: engage any hostile whose last-seen position is inside the
    /// `radius` ring around `anchor`; otherwise hold station at `anchor`.
    DefendZone {
        /// Centre of the defended ring (world units).
        anchor: Vec2,
        /// Defended radius (world units).
        radius: f32,
    },
    /// Patrol a closed route: advance `MoveTo(route[plan_index])`, wrapping on
    /// arrival; break to engage a perceived hostile, then resume.
    PatrolRoute(Vec<Vec2>),
    /// Withdraw to `pos`, then regroup there.
    Withdraw(Vec2),
    /// Re-form (`FormUp`) at `rally` until cohered + threat-clear, then v1 holds.
    Regroup {
        /// The rally point the squad re-forms on (world units).
        rally: Vec2,
    },
}

/// The STRATEGIC objective on a WING entity — the THIN tier ABOVE
/// [`SquadObjective`] (R97 Phase 2 Stage F). A wing groups role-coherent squads
/// (`Squad.wing == Some(this_wing)`) under one brain; [`wing_plan_system`]
/// decomposes this wing-level [`Objective`] into each member squad's
/// [`SquadObjective`] at a SLOW cadence. Ephemeral like every other AI component
/// (`Clone + Debug`, no `Serialize`, V-9). A wing WITHOUT this component is
/// skipped by the planner; a squad WITHOUT a wing is driven by its own
/// `SquadObjective` directly (Stage E) and is unaffected.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct WingObjective {
    /// The wing-level strategic goal, decomposed onto member squads by
    /// [`wing_plan_system`] (see its `decompose_wing` doc for the v1 split).
    pub goal: Objective,
    /// Last tick [`wing_plan_system`] re-decomposed this wing objective
    /// (slow-cadence bookkeeping; mirrors [`SquadObjective::last_plan_tick`]).
    pub last_plan_tick: u64,
}

impl WingObjective {
    /// A fresh wing objective at `goal`, never-planned.
    pub fn new(goal: Objective) -> Self {
        Self {
            goal,
            last_plan_tick: 0,
        }
    }
}

/// The STRATEGIC planner state on a SQUAD entity (layered over [`Squad`]).
/// `Clone + Debug`, no `Serialize` (V-9). A squad without this component is
/// invisible to [`strategic_plan_system`].
#[derive(Component, Clone, Debug, PartialEq)]
pub struct SquadObjective {
    /// The current strategic goal (can flip under HTN adaptation).
    pub goal: Objective,
    /// Cursor into a [`Objective::PatrolRoute`] (the next waypoint index);
    /// unused by the other objectives. Wraps on arrival.
    pub plan_index: usize,
    /// Last tick the planner re-decomposed this objective (slow-cadence
    /// bookkeeping; the test asserts it advances on the plan cadence, not every
    /// tick).
    pub last_plan_tick: u64,
}

impl SquadObjective {
    /// A fresh objective at `goal`, cursor 0, never-planned.
    pub fn new(goal: Objective) -> Self {
        Self {
            goal,
            plan_index: 0,
            last_plan_tick: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Strength estimate (the outnumbered test) — deterministic + cheap
// ---------------------------------------------------------------------------

/// A squad's OWN combat strength: Σ over alive members of their hull fraction
/// (a member with [`Health`] contributes `health / FLAT_HULL_BASELINE` clamped
/// to `[0, 1]`; a member with no `Health` contributes a full `1.0`). A simple
/// "member count × health-ish" sum — deterministic (summed in stable member
/// order), cheap (one query hit per member), and documented as the outnumbered
/// numerator. Despawned members (pruned by the V-1 sweep before this runs) never
/// appear in `members`.
fn own_strength(members: &[Entity], healths: &Query<Option<&Health>>) -> f32 {
    let mut strength = 0.0f32;
    for &m in members {
        let Ok(health) = healths.get(m) else {
            continue; // Not a member we can read — skip (no contribution).
        };
        // `health / baseline` clamped, or a full unit when health is absent —
        // the same "assume healthy with no info" spirit as `hull_fraction`.
        strength += health.map_or(1.0, |h| (h.0 / FLAT_HULL_BASELINE).clamp(0.0, 1.0));
    }
    strength
}

/// Flat-health hull baseline for the [`own_strength`] estimate — mirrors
/// `brain::FLAT_HULL_BASELINE` (kept local so strategy doesn't reach into the
/// brain module's private constant; a member at this much health counts as one
/// full unit of strength).
const FLAT_HULL_BASELINE: f32 = 100.0;

/// Perceived ENEMY strength near `anchor` within `radius`: the COUNT of fused
/// hostile contacts whose last-seen position is inside the ring (each body =
/// one unit). Kept on the SAME scale as [`own_strength`] (each member ≈ one
/// unit of hull) so the [`AiTuning::outnumbered_ratio`] test compares like with
/// like — "enemy bodies vs own members×health". Deterministic: the fused
/// picture is bits-sorted, the ring test is exact. `radius <= 0` means "anywhere"
/// (the whole fused picture counts — used by the DestroyTarget outnumbered test,
/// which weighs the WHOLE perceived force). Signature is intentionally NOT
/// weighted here (it would put enemy strength on a different scale than own); it
/// is available on each [`Contact`] for a future strength-by-class refinement.
fn enemy_strength(fused: &[Contact], anchor: Vec2, radius: f32) -> f32 {
    let r_sq = radius * radius;
    let mut count = 0.0f32;
    for c in fused {
        if radius > 0.0 && (c.last_pos - anchor).length_squared() > r_sq {
            continue;
        }
        count += 1.0;
    }
    count
}

/// The nearest fused contact to `from` (squared distance over `last_pos`),
/// exact-tie broken by lower target entity bits — the SAME stable rule
/// [`crate::ai::perception::nearest_contact`] uses, re-implemented over the
/// strategy view so it can additionally gate on the ring membership the
/// objectives need. `None` when no contact qualifies.
fn nearest_in_ring(fused: &[Contact], from: Vec2, anchor: Vec2, radius: f32) -> Option<Entity> {
    let r_sq = radius * radius;
    let mut best: Option<(f32, u64, Entity)> = None;
    for c in fused {
        if radius > 0.0 && (c.last_pos - anchor).length_squared() > r_sq {
            continue;
        }
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

// ---------------------------------------------------------------------------
// HTN decomposition — Objective → (next SquadOrder, possibly a new Objective)
// ---------------------------------------------------------------------------

/// The world the HTN decomposition reads: the squad's fused contact picture, its
/// centroid, its strength, and the entity table (target-alive test). All cheap,
/// all deterministic.
struct PlanContext<'a> {
    /// The squad faction's fused hostile contacts (union of its network
    /// components), bits-sorted.
    fused: &'a [Contact],
    /// The squad centroid (its `Position`).
    centroid: Vec2,
    /// The squad's own combat strength ([`own_strength`]).
    own: f32,
    /// Outnumbered ratio knob ([`AiTuning::outnumbered_ratio`]).
    outnumbered_ratio: f32,
    /// Arrive radius for DefendZone hold / Withdraw / Patrol arrival tests.
    arrive_radius: f32,
    /// R98 HOTFIX D — DefendZone engage-release hysteresis factor
    /// ([`AiTuning::defend_release_factor`]): an already-engaged intruder is
    /// kept until its last-seen position leaves `radius × this` of the anchor.
    defend_release_factor: f32,
}

/// Outcome of one HTN decomposition step: the next squad order, plus an optional
/// objective MUTATION (the adaptive flips — engage→withdraw→regroup, withdraw→
/// regroup, patrol cursor advance). The caller applies both compare-before-write.
struct PlanResult {
    order: SquadOrder,
    /// `Some(new_goal)` replaces `SquadObjective::goal`; `None` keeps it.
    new_goal: Option<Objective>,
    /// `Some(idx)` sets `SquadObjective::plan_index` (patrol cursor); `None`
    /// keeps it.
    new_plan_index: Option<usize>,
}

impl PlanResult {
    /// An order with no objective/cursor change.
    fn order(order: SquadOrder) -> Self {
        Self {
            order,
            new_goal: None,
            new_plan_index: None,
        }
    }
}

/// Whether `pos` lies inside the `from`-centred ring of `radius` (inclusive).
fn within(pos: Vec2, from: Vec2, radius: f32) -> bool {
    (pos - from).length_squared() <= radius * radius
}

/// The authored HTN decomposition (the heart of Stage E): map `goal` +
/// `plan_index` + the live `ctx` to the next [`SquadOrder`] and any objective
/// flip. Bounded and authored — no search — so it is deterministic and cheap.
///
/// - **`Hold`** → [`SquadOrder::Hold`].
/// - **`DestroyTarget(t)`**:
///   - `t` despawned (not in the fused picture as a live target is irrelevant;
///     the caller passes `target_alive`) → flip to `Regroup{rally: centroid}`.
///   - OUTNUMBERED (perceived enemy strength over the WHOLE fused picture ≥
///     `outnumbered_ratio × own`) → flip to `Withdraw(centroid)` (the squad then
///     regroups on arrival).
///   - else if escorts/defenders are perceived NEAR the target (any fused
///     contact other than `t` itself within `arrive_radius`-scaled escort range
///     of the target's last-seen position) → `Engage(nearest escort)`.
///   - else → `Engage(t)`.
/// - **`DefendZone{anchor, radius}`**: R98 HOTFIX D — engage with RELEASE
///   hysteresis. If the squad's CURRENT order is already `Engage(t)`, keep
///   engaging `t` while it is still PERCEIVED (present in the fused picture)
///   AND its last-seen position is within `radius × defend_release_factor` of
///   the anchor (the wider release ring); release — fall through to the normal
///   acquisition below — only when `t` despawned/unperceived or left the
///   release ring. Acquisition is unchanged: a hostile whose last-seen pos is
///   inside the ring → `Engage(nearest intruder in ring)`; else
///   `MoveTo(anchor)`. The asymmetric acquire-at-`radius` / release-at-`radius
///   × factor` rings kill the Engage↔MoveTo flap for an intruder hovering on
///   the acquisition edge.
/// - **`PatrolRoute(route)`**: a perceived hostile (anywhere in the picture) →
///   `Engage(nearest)` WITHOUT advancing the cursor (resume on threat-gone);
///   else `MoveTo(route[plan_index])`, advancing the cursor (wrapping) when the
///   centroid has arrived.
/// - **`Withdraw(pos)`**: `Withdraw(pos)`; on arrival (centroid within
///   `arrive_radius`) flip to `Regroup{rally: pos}`.
/// - **`Regroup{rally}`**: `FormUp`; when cohered (every member within
///   `cohesion_radius` of the centroid — tested by the caller) AND no threat is
///   perceived, v1 flips to `Hold` (documented; restoring the PRIOR objective is
///   the noted follow-up).
fn decompose(
    goal: &Objective,
    plan_index: usize,
    current_order: SquadOrder,
    target_alive: bool,
    cohered: bool,
    ctx: &PlanContext,
) -> PlanResult {
    match goal {
        Objective::Hold => PlanResult::order(SquadOrder::Hold),

        Objective::DestroyTarget(t) => {
            // Target gone → there is nothing to destroy; regroup where we stand.
            if !target_alive {
                return PlanResult {
                    order: SquadOrder::FormUp,
                    new_goal: Some(Objective::Regroup {
                        rally: ctx.centroid,
                    }),
                    new_plan_index: None,
                };
            }
            // Outnumbered → withdraw (regroup follows on arrival). The whole
            // perceived force is weighed (radius 0 = "anywhere"): the squad
            // can't out-attrition a force `outnumbered_ratio×` its own.
            let enemy = enemy_strength(ctx.fused, ctx.centroid, 0.0);
            if enemy >= ctx.outnumbered_ratio * ctx.own {
                return PlanResult {
                    order: SquadOrder::Withdraw(ctx.centroid),
                    new_goal: Some(Objective::Withdraw(ctx.centroid)),
                    new_plan_index: None,
                };
            }
            // Escorts/defenders perceived near the target → clear them first.
            // The escort ring is the target's last-seen position; any OTHER
            // fused contact within `escort range` of it is a defender. The
            // target's own last-seen position anchors the ring.
            if let Some(target_pos) = fused_pos(ctx.fused, *t) {
                let escort_range = ctx.arrive_radius;
                if let Some(escort) = nearest_escort(ctx.fused, *t, target_pos, escort_range) {
                    return PlanResult::order(SquadOrder::Engage(escort));
                }
            }
            // Clear (or unseen escorts) → engage the target itself.
            PlanResult::order(SquadOrder::Engage(*t))
        }

        Objective::DefendZone { anchor, radius } => {
            // R98 HOTFIX D — engage-release hysteresis: an ALREADY-engaged
            // intruder is kept while still perceived AND inside the wider
            // release ring (`radius × defend_release_factor`); only a
            // despawned/unperceived/escaped intruder falls through to the
            // normal acquisition (see the function docs — kills the flap).
            if let SquadOrder::Engage(t) = current_order {
                if let Some(seen) = fused_pos(ctx.fused, t) {
                    if within(seen, *anchor, *radius * ctx.defend_release_factor) {
                        return PlanResult::order(SquadOrder::Engage(t));
                    }
                }
            }
            if let Some(intruder) = nearest_in_ring(ctx.fused, ctx.centroid, *anchor, *radius) {
                PlanResult::order(SquadOrder::Engage(intruder))
            } else {
                PlanResult::order(SquadOrder::MoveTo(*anchor))
            }
        }

        Objective::PatrolRoute(route) => {
            if route.is_empty() {
                return PlanResult::order(SquadOrder::Hold);
            }
            // A perceived hostile breaks the patrol to an engage; the cursor is
            // LEFT in place so the route resumes once the threat is gone.
            if let Some(threat) = nearest_in_ring(ctx.fused, ctx.centroid, Vec2::ZERO, 0.0) {
                return PlanResult::order(SquadOrder::Engage(threat));
            }
            let idx = plan_index % route.len();
            let waypoint = route[idx];
            // Advance (wrap) on arrival; otherwise keep flying to this waypoint.
            if within(ctx.centroid, waypoint, ctx.arrive_radius) {
                let next = (idx + 1) % route.len();
                PlanResult {
                    order: SquadOrder::MoveTo(route[next]),
                    new_goal: None,
                    new_plan_index: Some(next),
                }
            } else {
                PlanResult {
                    order: SquadOrder::MoveTo(waypoint),
                    new_goal: None,
                    new_plan_index: Some(idx),
                }
            }
        }

        Objective::Withdraw(pos) => {
            if within(ctx.centroid, *pos, ctx.arrive_radius) {
                PlanResult {
                    order: SquadOrder::FormUp,
                    new_goal: Some(Objective::Regroup { rally: *pos }),
                    new_plan_index: None,
                }
            } else {
                PlanResult::order(SquadOrder::Withdraw(*pos))
            }
        }

        Objective::Regroup { rally: _ } => {
            // Cohered + threat-clear → v1 holds (documented; restoring the prior
            // objective is the noted follow-up). Until then keep forming up.
            // (Cohesion is computed by the caller and passed in as `cohered`.)
            let threat_clear = nearest_in_ring(ctx.fused, ctx.centroid, Vec2::ZERO, 0.0).is_none();
            if cohered && threat_clear {
                PlanResult {
                    order: SquadOrder::Hold,
                    new_goal: Some(Objective::Hold),
                    new_plan_index: None,
                }
            } else {
                PlanResult::order(SquadOrder::FormUp)
            }
        }
    }
}

/// Last-seen position of `target` in the fused picture, if present.
fn fused_pos(fused: &[Contact], target: Entity) -> Option<Vec2> {
    fused
        .iter()
        .find(|c| c.target == target)
        .map(|c| c.last_pos)
}

/// The nearest fused contact OTHER than `target` within `range` of
/// `target_pos` — an escort/defender screening the target. Same stable
/// nearest/bits rule as [`nearest_in_ring`], excluding the target itself.
fn nearest_escort(
    fused: &[Contact],
    target: Entity,
    target_pos: Vec2,
    range: f32,
) -> Option<Entity> {
    let r_sq = range * range;
    let mut best: Option<(f32, u64, Entity)> = None;
    for c in fused {
        if c.target == target {
            continue;
        }
        let d = (c.last_pos - target_pos).length_squared();
        if d > r_sq {
            continue;
        }
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

// ---------------------------------------------------------------------------
// strategic_plan_system — the SLOW planner pass
// ---------------------------------------------------------------------------

/// The squad's faction = the FIRST alive member's faction in stable member
/// order (deterministic; squads carry no `Faction` of their own). `None` when no
/// member is factioned — such a squad sees an empty fused picture (no enemies).
fn squad_faction(members: &[Entity], factions: &Query<Option<&Faction>>) -> Option<Faction> {
    for &m in members {
        if let Ok(Some(f)) = factions.get(m) {
            return Some(*f);
        }
    }
    None
}

/// The union of the squad faction's fused contacts across all its network
/// components (bits-sorted, deduped newest-wins). Empty when the faction has no
/// network entry (no contacts perceived). Read from the [`SensorNetworks`]
/// `BTreeMap` (no `HashMap`); the union is built in component order then merged
/// to a single deduped list.
fn fused_for_faction(networks: &SensorNetworks, faction: Faction) -> Vec<Contact> {
    use crate::ai::perception::faction_key;
    let mut out: Vec<Contact> = Vec::new();
    if let Some(comps) = networks.by_faction.get(&faction_key(faction)) {
        for nc in comps {
            for &c in &nc.fused {
                // Insert keeping bits-sorted + newest-wins (the same dedupe the
                // network fusion uses, applied across components here).
                match out.binary_search_by_key(&c.target.to_bits(), |x| x.target.to_bits()) {
                    Ok(i) => {
                        if c.last_seen_tick > out[i].last_seen_tick
                            || (c.last_seen_tick == out[i].last_seen_tick
                                && c.signature > out[i].signature)
                        {
                            out[i] = c;
                        }
                    }
                    Err(i) => out.insert(i, c),
                }
            }
        }
    }
    out
}

/// R97 Phase 2 Stage E — the STRATEGIC planner (TR-009): per squad that carries a
/// [`SquadObjective`], in [`AiStableId`] order, at the SLOW
/// [`AiTuning::strategic_plan_ticks`] cadence (offset by the squad's
/// `phase_bucket`, the same discipline `squad_think_system` uses), decompose the
/// [`Objective`] into the next [`SquadOrder`] and WRITE it onto `squad.order`
/// ONLY when it changed (compare-before-write — no `OrderChanged` storm), plus
/// any adaptive objective/cursor flip.
///
/// Reads the squad faction's fused contact picture ([`SensorNetworks`]), the
/// squad's own strength ([`own_strength`] over member [`Health`]), and the
/// squad centroid/cohesion ([`Position`]). Writes ONLY `squad.order`,
/// `objective.goal`, `objective.plan_index`, and `objective.last_plan_tick`.
///
/// **Registration**: in the `ScenarioActive`-gated AI set, BEFORE
/// `squad_think_system` (the objective sets the order → the squad think
/// translates it to members → channel-fusion executes — all the same tick). A
/// squad WITHOUT a `SquadObjective` is skipped by the query, so existing squad
/// tests + the golden trio are unaffected; no golden world spawns an
/// `Objective`, so this never runs there.
///
/// **R98 HOTFIX B1**: a squad whose ALIVE members ALL carry a [`ScenarioRole`]
/// is SKIPPED entirely — its members are squad-order-exempt at the brain level,
/// so a planned order could reach nothing but the dormant cheap-glide and drag
/// the squad against its roles (see [`all_members_roled`]).
#[allow(clippy::too_many_arguments)] // One param per seam: resources + the disjoint queries.
pub fn strategic_plan_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    networks: Option<Res<SensorNetworks>>,
    entities: &Entities,
    mut squads: Query<(
        Entity,
        &AiStableId,
        &mut Squad,
        &mut SquadObjective,
        &Position,
    )>,
    factions: Query<Option<&Faction>>,
    healths: Query<Option<&Health>>,
    member_pos: Query<&Position>,
    exempt: Query<(Option<&ScenarioRole>, Has<PlayerOrder>)>,
) {
    let now = tick.0;
    let cadence = u64::from(tuning.strategic_plan_ticks.max(1));

    // V-3 stable order — plan squads in AiStableId order (ids are unique).
    let mut order_pass: Vec<(AiStableId, Entity)> =
        squads.iter().map(|(e, id, ..)| (*id, e)).collect();
    order_pass.sort_unstable();

    for (_, squad_entity) in order_pass {
        let Ok((_, _, mut squad, mut objective, squad_pos)) = squads.get_mut(squad_entity) else {
            continue;
        };

        // SLOW cadence gate (same `(now + bucket) % cadence` discipline as the
        // squad/think systems, but at the strategic plan cadence). Off-cadence:
        // zero planning work — the standing order/objective carry forward.
        if !(now + u64::from(squad.phase_bucket)).is_multiple_of(cadence) {
            continue;
        }

        // R98 HOTFIX B1: skip uncommandable squads — every alive member is
        // role-exempt, so the order layer can't reach them (see
        // `all_members_roled`; zero planning work, no `last_plan_tick` stamp).
        if all_members_roled(&squad.members, entities, &exempt) {
            continue;
        }
        objective.last_plan_tick = now;

        // The squad faction's fused picture (empty if factionless / no contacts).
        let fused = match squad_faction(&squad.members, &factions) {
            Some(f) => networks
                .as_ref()
                .map(|n| fused_for_faction(n, f))
                .unwrap_or_default(),
            None => Vec::new(),
        };

        // Own strength + cohesion over the live members (stable order).
        let own = own_strength(&squad.members, &healths);
        let centroid = squad_pos.0;
        let cohesion_radius = tuning.regroup_cohesion_radius;
        let cohered = squad.members.iter().all(|&m| {
            member_pos
                .get(m)
                .is_ok_and(|p| within(p.0, centroid, cohesion_radius))
        });

        // Target-alive test for DestroyTarget (the V-1 sweep already pruned a
        // dead Engage target from the squad order; here we drive the objective).
        let target_alive = match &objective.goal {
            Objective::DestroyTarget(t) => entities.contains(*t),
            _ => true,
        };

        let ctx = PlanContext {
            fused: &fused,
            centroid,
            own,
            outnumbered_ratio: tuning.outnumbered_ratio,
            arrive_radius: tuning.defend_arrive_radius,
            defend_release_factor: tuning.defend_release_factor,
        };

        let result = decompose(
            &objective.goal,
            objective.plan_index,
            squad.order, // R98 HOTFIX D: the DefendZone release hysteresis reads it.
            target_alive,
            cohered,
            &ctx,
        );

        // Compare-before-write: only a REAL order change touches `squad.order`
        // (and so only a real change rides `squad_think`'s OrderChanged the next
        // tick) — no order spam (the `apply_assignment` discipline, one tier up).
        if squad.order != result.order {
            squad.order = result.order;
        }
        if let Some(goal) = result.new_goal {
            if objective.goal != goal {
                objective.goal = goal;
            }
        }
        if let Some(idx) = result.new_plan_index {
            if objective.plan_index != idx {
                objective.plan_index = idx;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// wing_plan_system — the THIN wing tier (R97 Phase 2 Stage F)
// ---------------------------------------------------------------------------

/// The v1 THIN wing decomposition: map a [`WingObjective`] goal to the
/// [`Objective`] each member squad should carry, given the squad's RANK among
/// the wing's members (`rank == 0` is the LEAD squad — the lowest
/// [`AiStableId`] in the wing). Bounded + authored — no search — so it is
/// deterministic and cheap, and the per-squad HTN ([`decompose`], Stage E)
/// then ADAPTS each squad's objective to the live picture.
///
/// **v1 split (documented choice — keep it MINIMAL):**
/// - **`DefendZone { anchor, radius }`** → the LEAD squad (`rank 0`) defends the
///   anchor directly (`DefendZone` of the same ring); every OTHER member squad
///   patrols a closed ring AROUND the anchor (`PatrolRoute` of four points at
///   `radius` from the anchor). This is the visible "coordinated defense": one
///   squad anchors the zone while the rest screen its perimeter, and each
///   squad's Stage-E HTN still breaks off to engage an intruder and resumes.
/// - **`DestroyTarget(t)`** → EVERY member squad gets `DestroyTarget(t)` (the
///   simplest v1 — the whole wing converges on the target; a lead+support split
///   by squad strength is the noted follow-up). Each squad's HTN then clears
///   escorts / withdraws-when-outnumbered independently (Stage E).
/// - **all other goals** (`Hold`, `PatrolRoute`, `Withdraw`, `Regroup`) → a
///   pure PASS-THROUGH: every member squad gets a clone of the wing goal. The
///   wing tier adds no decomposition for these in v1 (documented); the squad
///   HTN drives them as before.
///
/// A wing-frame patrol ring around `anchor` at `radius` (the perimeter the
/// non-lead `DefendZone` squads screen): four cardinal points, deterministic.
fn defend_ring(anchor: Vec2, radius: f32) -> Vec<Vec2> {
    vec![
        anchor + Vec2::new(radius, 0.0),
        anchor + Vec2::new(0.0, radius),
        anchor + Vec2::new(-radius, 0.0),
        anchor + Vec2::new(0.0, -radius),
    ]
}

/// Decompose a wing `goal` into the member-squad objective for the squad at
/// `rank` (0 = lead). See [`defend_ring`]'s sibling doc for the v1 split rules.
fn decompose_wing(goal: &Objective, rank: usize) -> Objective {
    match goal {
        Objective::DefendZone { anchor, radius } => {
            if rank == 0 {
                // The LEAD squad anchors the zone directly.
                Objective::DefendZone {
                    anchor: *anchor,
                    radius: *radius,
                }
            } else {
                // Every other squad screens the perimeter (a ring patrol).
                Objective::PatrolRoute(defend_ring(*anchor, *radius))
            }
        }
        // The whole wing converges on the target (v1: simplest split).
        Objective::DestroyTarget(t) => Objective::DestroyTarget(*t),
        // Pass-through for the remaining goals (no wing-tier decomposition v1).
        other => other.clone(),
    }
}

/// R97 Phase 2 Stage F — the THIN WING planner: per WING entity that carries a
/// [`WingObjective`], in [`AiStableId`] order, at the SLOW
/// [`AiTuning::strategic_plan_ticks`] cadence, decompose the wing
/// [`Objective`] into each member squad's [`SquadObjective`] (compare-before-
/// write — no spam) and stamp `last_plan_tick`.
///
/// Member squads are the squads whose `wing == Some(this_wing)`, taken in
/// stable [`AiStableId`] order (V-3); the LOWEST-id member is the LEAD squad
/// (`rank 0`) — the deterministic "strongest/anchor" pick for the v1 split (a
/// strength-ranked pick is the noted follow-up). A member squad that already
/// carries a `SquadObjective` has its `goal` UPDATED in place
/// (compare-before-write, preserving `plan_index`/`last_plan_tick`); a squad
/// without one is left untouched here — the wing only re-targets squads that
/// already participate in the strategic tier, so authoring a `SquadObjective`
/// on each member squad (the scenario does) is what enrolls it.
///
/// **Registration**: in the `ScenarioActive`-gated AI set, BEFORE
/// `strategic_plan_system` (the wing sets each member squad's objective →
/// `strategic_plan_system` decomposes that objective into the squad order →
/// `squad_think` translates it to members — all the same tick). A wing WITHOUT
/// a `WingObjective` is skipped by the query; a squad with `wing == None` is
/// never matched, so it keeps its own `SquadObjective` (Stage E) untouched. No
/// golden world spawns a `WingObjective`, so the golden trio is unaffected.
///
/// **R98 HOTFIX B1**: a member squad whose ALIVE members ALL carry a
/// [`ScenarioRole`] is EXCLUDED from the wing's rank list (not just skipped at
/// the write): its members are squad-order-exempt, so a wing-assigned objective
/// could reach nothing but the dormant cheap-glide (see [`all_members_roled`]).
/// Excluding it keeps the lead/perimeter ranks assigned only over squads the
/// order layer can actually command.
pub fn wing_plan_system(
    tuning: Res<AiTuning>,
    tick: Res<CurrentTick>,
    entities: &Entities,
    mut wings: Query<(Entity, &AiStableId, &mut WingObjective)>,
    mut squads: Query<(Entity, &AiStableId, &Squad, &mut SquadObjective)>,
    exempt: Query<(Option<&ScenarioRole>, Has<PlayerOrder>)>,
) {
    let now = tick.0;
    let cadence = u64::from(tuning.strategic_plan_ticks.max(1));

    // V-3 stable order — plan wings in AiStableId order (ids are unique).
    let mut order_pass: Vec<(AiStableId, Entity)> =
        wings.iter().map(|(e, id, _)| (*id, e)).collect();
    order_pass.sort_unstable();

    for (_, wing_entity) in order_pass {
        let Ok((_, _, mut wing_obj)) = wings.get_mut(wing_entity) else {
            continue;
        };

        // SLOW cadence gate — the wing re-plans at the strategic cadence (the
        // wing carries no phase bucket of its own, so it re-plans on the bare
        // `now % cadence` multiple; off-cadence is zero work). The member
        // squads' own phase-bucketed `strategic_plan_system` then re-decomposes
        // the new objective into orders.
        if !now.is_multiple_of(cadence) {
            continue;
        }
        wing_obj.last_plan_tick = now;

        // Gather this wing's member squads in stable AiStableId order — the
        // lowest id is the LEAD squad (rank 0). Collect `(id, entity)` so the
        // mutate pass below can address each squad by entity (the immutable
        // gather can't overlap the `get_mut`).
        let mut members: Vec<(AiStableId, Entity)> = squads
            .iter()
            .filter(|(_, _, squad, _)| {
                squad.wing == Some(wing_entity)
                    // R98 HOTFIX B1 / R99: an all-roled/all-commanded squad is
                    // uncommandable — the wing plans only over squads its orders
                    // can reach.
                    && !all_members_roled(&squad.members, entities, &exempt)
            })
            .map(|(e, id, ..)| (*id, e))
            .collect();
        members.sort_unstable();

        for (rank, (_, squad_entity)) in members.iter().enumerate() {
            let Ok((_, _, _, mut objective)) = squads.get_mut(*squad_entity) else {
                continue;
            };
            // Compare-before-write: only a real goal change touches the member
            // objective (and so only a real change resets its cursor). The
            // member's own `strategic_plan_system` then re-decomposes it.
            let new_goal = decompose_wing(&wing_obj.goal, rank);
            if objective.goal != new_goal {
                objective.goal = new_goal;
                // A goal CHANGE resets the patrol cursor so a freshly assigned
                // PatrolRoute starts from waypoint 0 (deterministic).
                objective.plan_index = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(target: Entity, pos: Vec2, sig: f32) -> Contact {
        Contact {
            target,
            last_pos: pos,
            last_seen_tick: 0,
            signature: sig,
        }
    }

    /// `own_strength` sums member hull fractions: a `Health`-carrying member
    /// contributes `health/baseline` clamped, a `Health`-less member a full 1.0.
    #[test]
    fn own_strength_sums_member_hull_fractions() {
        let mut world = World::new();
        let full = world.spawn(Health(100.0)).id();
        let half = world.spawn(Health(50.0)).id();
        let bare = world.spawn_empty().id();
        let mut state = world.query::<Option<&Health>>();
        let q = state.query(&world);
        let strength = own_strength(&[full, half, bare], &q);
        assert_eq!(strength, 1.0 + 0.5 + 1.0, "1.0 + 0.5 + 1.0 = 2.5");
    }

    /// `enemy_strength` COUNTS contacts in the ring (one unit per body, same
    /// scale as own strength); radius 0 counts the whole picture; a far contact
    /// is excluded by the ring.
    #[test]
    fn enemy_strength_counts_bodies_in_ring() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn_empty().id();
        let c = world.spawn_empty().id();
        let fused = {
            let mut v = vec![
                contact(a, Vec2::new(0.0, 0.0), 3.0),
                contact(b, Vec2::new(10.0, 0.0), 0.0),
                contact(c, Vec2::new(500.0, 0.0), 5.0),
            ];
            v.sort_by_key(|x| x.target.to_bits());
            v
        };
        // Whole picture (radius 0): three bodies → 3.0.
        assert_eq!(enemy_strength(&fused, Vec2::ZERO, 0.0), 3.0);
        // Ring of 50 around origin: a + b only → 2.0.
        assert_eq!(enemy_strength(&fused, Vec2::ZERO, 50.0), 2.0);
    }

    /// `nearest_in_ring` is the stable nearest (bits tiebreak) and honors the
    /// ring; `nearest_escort` excludes the target itself.
    #[test]
    fn nearest_helpers_are_stable_and_ring_gated() {
        let mut world = World::new();
        let t = world.spawn_empty().id();
        let near = world.spawn_empty().id();
        let far = world.spawn_empty().id();
        let mut fused = vec![
            contact(t, Vec2::new(100.0, 0.0), 2.0),
            contact(near, Vec2::new(110.0, 0.0), 2.0),
            contact(far, Vec2::new(900.0, 0.0), 2.0),
        ];
        fused.sort_by_key(|x| x.target.to_bits());

        // Nearest to the target position, within a 50-ring of the target,
        // excluding the target → the escort `near`.
        assert_eq!(
            nearest_escort(&fused, t, Vec2::new(100.0, 0.0), 50.0),
            Some(near)
        );
        // Far escort is out of the ring → None when only it remains.
        assert_eq!(
            nearest_escort(
                &[
                    contact(t, Vec2::new(100.0, 0.0), 2.0),
                    contact(far, Vec2::new(900.0, 0.0), 2.0),
                ],
                t,
                Vec2::new(100.0, 0.0),
                50.0
            ),
            None
        );
    }

    /// HTN: `Hold` → `Hold`; `Withdraw` → `Withdraw` then flips to `Regroup` on
    /// arrival; `Regroup` cohered+clear → `Hold`/`Hold`.
    #[test]
    fn decompose_withdraw_then_regroup_then_hold() {
        let ctx = |centroid: Vec2| PlanContext {
            fused: &[],
            centroid,
            own: 3.0,
            outnumbered_ratio: 1.5,
            arrive_radius: 30.0,
            defend_release_factor: 1.25,
        };
        // Withdraw far from the point → still withdrawing.
        let far = decompose(
            &Objective::Withdraw(Vec2::new(0.0, 0.0)),
            0,
            SquadOrder::Hold,
            true,
            false,
            &ctx(Vec2::new(500.0, 0.0)),
        );
        assert_eq!(far.order, SquadOrder::Withdraw(Vec2::ZERO));
        assert!(far.new_goal.is_none());
        // Arrived → FormUp + flip to Regroup.
        let arrived = decompose(
            &Objective::Withdraw(Vec2::ZERO),
            0,
            SquadOrder::Hold,
            true,
            false,
            &ctx(Vec2::new(5.0, 0.0)),
        );
        assert_eq!(arrived.order, SquadOrder::FormUp);
        assert_eq!(
            arrived.new_goal,
            Some(Objective::Regroup { rally: Vec2::ZERO })
        );
        // Regroup, not cohered → FormUp.
        let forming = decompose(
            &Objective::Regroup { rally: Vec2::ZERO },
            0,
            SquadOrder::Hold,
            true,
            false,
            &ctx(Vec2::ZERO),
        );
        assert_eq!(forming.order, SquadOrder::FormUp);
        // Regroup, cohered + clear → Hold/Hold (v1).
        let done = decompose(
            &Objective::Regroup { rally: Vec2::ZERO },
            0,
            SquadOrder::Hold,
            true,
            true,
            &ctx(Vec2::ZERO),
        );
        assert_eq!(done.order, SquadOrder::Hold);
        assert_eq!(done.new_goal, Some(Objective::Hold));
    }

    /// R98 HOTFIX D — the DefendZone engage-release hysteresis: acquisition at
    /// the ring is unchanged; an ALREADY-engaged intruder is kept while
    /// perceived inside `radius × defend_release_factor`; despawned/unperceived
    /// or beyond the release ring falls back to acquisition (MoveTo here).
    #[test]
    fn defend_zone_engage_release_hysteresis() {
        let anchor = Vec2::ZERO;
        let radius = 100.0;
        let goal = Objective::DefendZone { anchor, radius };
        let mut world = World::new();
        let intruder = world.spawn_empty().id();
        let with_intruder_at = |x: f32| vec![contact(intruder, Vec2::new(x, 0.0), 3.0)];

        let case = |fused: &[Contact], current: SquadOrder| {
            let ctx = PlanContext {
                fused,
                centroid: anchor,
                own: 2.0,
                outnumbered_ratio: 1.5,
                arrive_radius: 50.0,
                defend_release_factor: 1.25,
            };
            decompose(&goal, 0, current, true, false, &ctx).order
        };

        // Acquisition unchanged: an intruder INSIDE the ring is engaged.
        let inside = with_intruder_at(90.0);
        assert_eq!(
            case(&inside, SquadOrder::Hold),
            SquadOrder::Engage(intruder),
            "intruder inside the acquisition ring → Engage"
        );
        // Hysteresis: already engaging + intruder hovering BETWEEN the
        // acquisition ring (100) and the release ring (125) → KEEP engaging
        // (this exact geometry used to flap Engage↔MoveTo every plan tick).
        let hovering = with_intruder_at(110.0);
        assert_eq!(
            case(&hovering, SquadOrder::Engage(intruder)),
            SquadOrder::Engage(intruder),
            "engaged intruder inside the release ring is kept (no flap)"
        );
        // …but the SAME hovering intruder is NOT acquired fresh (asymmetry).
        assert_eq!(
            case(&hovering, SquadOrder::Hold),
            SquadOrder::MoveTo(anchor),
            "an un-engaged hoverer outside the acquisition ring is not acquired"
        );
        // Release: beyond the release ring → fall through to acquisition.
        let escaped = with_intruder_at(130.0);
        assert_eq!(
            case(&escaped, SquadOrder::Engage(intruder)),
            SquadOrder::MoveTo(anchor),
            "intruder beyond radius × release factor → released"
        );
        // Release: unperceived (gone from the fused picture) → released.
        assert_eq!(
            case(&[], SquadOrder::Engage(intruder)),
            SquadOrder::MoveTo(anchor),
            "unperceived/despawned intruder → released"
        );
    }

    /// Stage F wing decomposition (v1 thin split): `DefendZone` → the LEAD squad
    /// (rank 0) anchors the zone, every other squad patrols the perimeter ring;
    /// `DestroyTarget` → every squad converges on the target; other goals pass
    /// through unchanged.
    #[test]
    fn decompose_wing_splits_defend_and_passes_through() {
        let anchor = Vec2::new(100.0, -50.0);
        let radius = 400.0;
        let defend = Objective::DefendZone { anchor, radius };

        // Lead squad anchors the zone directly.
        assert_eq!(
            decompose_wing(&defend, 0),
            Objective::DefendZone { anchor, radius }
        );
        // Non-lead squads screen the perimeter (a four-point ring at `radius`).
        assert_eq!(
            decompose_wing(&defend, 1),
            Objective::PatrolRoute(defend_ring(anchor, radius))
        );
        assert_eq!(decompose_wing(&defend, 2), decompose_wing(&defend, 1));

        // DestroyTarget converges the whole wing.
        let mut w = World::new();
        let t = w.spawn_empty().id();
        let destroy = Objective::DestroyTarget(t);
        assert_eq!(decompose_wing(&destroy, 0), Objective::DestroyTarget(t));
        assert_eq!(decompose_wing(&destroy, 3), Objective::DestroyTarget(t));

        // Pass-through goals are cloned verbatim for every rank.
        for goal in [
            Objective::Hold,
            Objective::Withdraw(anchor),
            Objective::Regroup { rally: anchor },
        ] {
            assert_eq!(decompose_wing(&goal, 0), goal);
            assert_eq!(decompose_wing(&goal, 5), goal);
        }
    }
}
