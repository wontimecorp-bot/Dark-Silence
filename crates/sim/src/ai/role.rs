//! Scenario roles (T032, OBJ6, TR-015): scripted goals/postures layered OVER
//! the general [`AiBrain`] — the data-model §`ScenarioRole` component plus the
//! shared trigger pass ([`role_trigger_system`]).
//!
//! **Composition rule (OBJ6, data-model)**: the script DIRECTS (goal +
//! posture), the general brain fills TACTICS (utility selection runs WITHIN
//! the scripted goal; a threat interrupt returns to the goal after). The
//! mechanism is a `role_apply` step at THINK time — `ai_think_system` calls
//! [`role_apply`] before candidate scoring, so the role maintains
//! `brain.waypoint` / `brain.home` / target upkeep from its goal and the
//! normal utility selection then competes over that state:
//!
//! - [`RoleGoal::PatrolRoute`]: the role feeds `brain.waypoint` from
//!   `route[route_index]`; within the arrive radius the cursor advances
//!   (wrapping). A perceived threat (perception acquires `brain.target`,
//!   posture permitting) makes `Engage` win by ordinary bucket dominance; when
//!   the threat is GONE (despawn → V-1 sweep, or perception-lost → the
//!   staleness prune empties the contact entry and `role_apply` releases the
//!   target) the role re-asserts the route waypoint and the patrol resumes —
//!   the OBJ6-VC1 break-and-resume loop.
//! - [`RoleGoal::Ambush`]: assigned ships HOLD dark (no waypoint; any
//!   perception-acquired target is cleared every tick by the trigger pass)
//!   until a HOSTILE contact enters the trigger circle. The trigger is
//!   evaluated by ONE shared pass ([`role_trigger_system`]) so every assigned
//!   ship transitions the SAME tick (OBJ6-VC2). Once fired, the goal DEGRADES
//!   to [`RoleGoal::Defend`] anchored on the trigger center (the documented
//!   fired-marker: ambushers prosecute, then fall back to defending the spot).
//! - [`RoleGoal::Defend`]: `brain.home`/`brain.waypoint` = the anchor; with no
//!   threat the brain flies/arrives back at the anchor (`Waypoint`). The
//!   defend `radius` gates ACQUISITION (the role only self-acquires contacts
//!   whose last-seen position is inside the zone); prosecution RELEASE is v1
//!   despawn-driven (the V-1 sweep) — a live target that flees is chased, the
//!   staleness-release refinement is deferred (documented).
//! - [`RoleGoal::SweepRegion`] / [`RoleGoal::ScoutArea`] (T035, TR-021): both
//!   fly the deterministic [`sweep_route`] boustrophedon over their region —
//!   regenerated each apply (see the arm's regen-vs-cache note) and followed
//!   exactly like a patrol route (assert leg, advance + wrap on arrive,
//!   release unperceived targets). The DIFFERENCE is selection, wired in
//!   `ai_think_system`: a SweepRegion ship scores `Sweep` and ENGAGES once a
//!   target is perceived; a ScoutArea ship scores `Scout`, has `Engage`/`Ram`
//!   candidacy VETOED outright (flee-permitted — unlike `HoldFire`, survival
//!   behaviors stay live), and scores `Evade` against a SUPERIOR perceived
//!   threat. Contact REPORTING needs no code here: the scout's own
//!   [`ContactList`] feeds the `sensor_network_system` fusion, so everything
//!   it sees enters its faction's fused picture automatically (TR-021
//!   "report/maintain contacts into its faction picture").
//!
//! **Posture gates** (data-model: "fire-control gate layered over the brain"),
//! enforced at BOTH seams — `Engage`/`Ram` candidacy in `ai_think_system` and
//! the fire-decision overlay in `ai_execute_system`:
//! - [`Posture::HoldFire`]: NEVER fire, never select `Engage`/`Ram`.
//! - [`Posture::DefensiveOnly`]: engage/fire permitted only while fired-upon
//!   recently — [`ScenarioRole::fired_upon_until`] is armed to
//!   `now + `[`FIRED_UPON_WINDOW_TICKS`] when an [`AiEvent::DamageTaken`]
//!   re-think is pending for the ship (observed by the trigger pass, which
//!   runs before the think drains the queue), and the gate is
//!   `now < fired_upon_until`.
//! - [`Posture::FreeEngage`]: unrestricted (the default).
//!
//! **Determinism (V-3)**: the trigger pass iterates role carriers in
//! [`AiStableId`] order, groups ambushes in a `BTreeMap` keyed by faction +
//! trigger-geometry bits, and picks the firing target by (distance² to the
//! trigger center, entity bits) — no HashMap, no RNG. `Clone + Debug`, no
//! `Serialize` (V-9). No golden world spawns a `ScenarioRole`, so the gated
//! trigger system is a true no-op there and the goldens stay bit-identical.

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::brain::{
    AiBrain, AiEvent, CombatStance, MovementProfile, RethinkQueue, ARRIVE_RADIUS,
};
use crate::ai::ident::AiStableId;
use crate::ai::perception::{faction_key, ContactList};
use crate::broadphase::COARSE_CELL_SIZE;
use crate::clock::CurrentTick;
use crate::components::Faction;

/// How long (ticks) a [`Posture::DefensiveOnly`] ship stays weapons-free after
/// taking damage: 300 ticks = 10 s at the 30 Hz fixed step (documented v1
/// choice — long enough to finish a defensive exchange, short enough that a
/// single stray hit never converts a defensive squad into a standing patrol
/// of aggression).
pub const FIRED_UPON_WINDOW_TICKS: u64 = 300;

/// A scripted goal (data-model §`ScenarioRole.goal`).
#[derive(Clone, Debug, PartialEq)]
pub enum RoleGoal {
    /// Follow a closed waypoint route (the cursor wraps).
    PatrolRoute(Vec<Vec2>),
    /// Hold dark until a hostile contact enters the trigger circle, then
    /// spring TOGETHER (same tick for every ship sharing the trigger).
    Ambush {
        /// Center of the trigger circle.
        trigger_center: Vec2,
        /// Radius of the trigger circle.
        trigger_radius: f32,
    },
    /// Hold/return to `anchor`; self-acquire only contacts inside `radius`.
    Defend {
        /// The position to defend / return to.
        anchor: Vec2,
        /// Acquisition zone radius around the anchor.
        radius: f32,
    },
    /// Search-and-destroy sweep of an axis-aligned region (T035, TR-021): fly
    /// the [`sweep_route`] boustrophedon; the brain selects `Sweep` while no
    /// target is perceived and `Engage` once one is (the prosecute rule).
    SweepRegion {
        /// Lower-left corner of the assigned region.
        min: Vec2,
        /// Upper-right corner of the assigned region.
        max: Vec2,
    },
    /// Scouting coverage of an axis-aligned region (T035, TR-021): the same
    /// boustrophedon coverage, but combat candidacy is VETOED and a SUPERIOR
    /// perceived threat scores `Evade` (disengage-and-survive).
    ScoutArea {
        /// Lower-left corner of the assigned area.
        min: Vec2,
        /// Upper-right corner of the assigned area.
        max: Vec2,
    },
}

/// Coverage-lane spacing as a fraction of sensor range for [`sweep_route`]:
/// 1.5× keeps adjacent lanes (and the region edges ridden by the first/last
/// lanes) strictly inside the 2× geometric coverage limit, so a ship FLYING
/// the route brings every point of the region — and therefore every coarse
/// interest-tier cell it overlaps — within its sensor radius, with margin.
const SWEEP_LANE_FACTOR: f32 = 1.5;

/// T035 (TR-021) — deterministic boustrophedon ("lawnmower") coverage route
/// over the axis-aligned region `[min, max]`.
///
/// Horizontal lanes start ON `min.y`, step by [`SWEEP_LANE_FACTOR`]` ×
/// sensor_range`, and the last lane is clamped ONTO `max.y` (edges are flown,
/// never approximated); each lane contributes its two endpoints and the
/// fly-direction alternates per lane (the boustrophedon turn). Pure `f32`
/// arithmetic over the inputs — identical inputs yield the identical `Vec` on
/// every call (the [`role_apply`] regen contract). Degenerate inputs are safe:
/// corners are normalized (`min`/`max` swapped per axis as needed), a zero-area
/// region yields a single 2-point lane, and a non-positive `sensor_range`
/// falls back to one coarse cell ([`COARSE_CELL_SIZE`]) of lane spacing.
pub fn sweep_route(min: Vec2, max: Vec2, sensor_range: f32) -> Vec<Vec2> {
    let lo = min.min(max);
    let hi = min.max(max);
    let spacing = if sensor_range > 0.0 {
        sensor_range * SWEEP_LANE_FACTOR
    } else {
        COARSE_CELL_SIZE
    };
    let mut route = Vec::new();
    let mut y = lo.y;
    let mut rightward = true;
    loop {
        let (from_x, to_x) = if rightward {
            (lo.x, hi.x)
        } else {
            (hi.x, lo.x)
        };
        route.push(Vec2::new(from_x, y));
        route.push(Vec2::new(to_x, y));
        if y >= hi.y {
            break; // The clamped top lane was just emitted.
        }
        y = (y + spacing).min(hi.y);
        rightward = !rightward;
    }
    route
}

/// Fire-control posture layered over the brain (data-model
/// §`ScenarioRole.posture`). See the module docs for the gate semantics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Posture {
    /// Unrestricted engagement (the default).
    #[default]
    FreeEngage,
    /// Engage/fire only while fired-upon recently (`fired_upon_until`).
    DefensiveOnly,
    /// Never fire, never select `Engage`/`Ram`.
    HoldFire,
}

/// The scenario-script overlay component (optional; scripted ships only) —
/// layered OVER [`AiBrain`], never replacing it (data-model §`ScenarioRole`).
#[derive(Component, Clone, Debug, PartialEq)]
pub struct ScenarioRole {
    /// The scripted goal the brain fills tactics within.
    pub goal: RoleGoal,
    /// The fire-control posture gate.
    pub posture: Posture,
    /// Current patrol waypoint cursor (wraps).
    pub route_index: usize,
    /// `DefensiveOnly` weapons-free deadline: engage/fire allowed while
    /// `now < fired_upon_until`. Armed by the trigger pass on a pending
    /// [`AiEvent::DamageTaken`].
    pub fired_upon_until: u64,
    /// R96 precedence — the role's optional [`MovementProfile`] OVERRIDE (the
    /// MIDDLE link of the resolved chain `squad ← role ← archetype default`).
    /// `Some(...)` makes a roled ship pace this way regardless of its archetype;
    /// `None` (the default) defers to the archetype default. Read by
    /// `ai_think_system` after `role_apply` and folded into the brain's resolved
    /// `movement_profile` (a squad override still wins — but roled members are
    /// squad-exempt, so for them the role override is the highest live link).
    /// Set via [`ScenarioRole::with_style`].
    pub movement_profile: Option<MovementProfile>,
    /// R96 precedence — the role's optional [`CombatStance`] OVERRIDE (the
    /// [`ScenarioRole::movement_profile`] twin). `Some(...)` overrides the
    /// archetype default combat style; `None` defers. Set via
    /// [`ScenarioRole::with_style`].
    pub combat_stance: Option<CombatStance>,
}

impl ScenarioRole {
    /// A fresh role: cursor at the route start, never fired upon, no style
    /// override (both `Option` styles `None` → the brain resolves to the
    /// archetype default unless a squad imposes one). Signature UNCHANGED so
    /// every existing call site compiles; use [`ScenarioRole::with_style`] to
    /// add an R96 style override.
    pub fn new(goal: RoleGoal, posture: Posture) -> Self {
        Self {
            goal,
            posture,
            route_index: 0,
            fired_upon_until: 0,
            movement_profile: None,
            combat_stance: None,
        }
    }

    /// R96 — set this role's optional style OVERRIDES (the MIDDLE link of the
    /// resolved precedence chain). A builder over [`ScenarioRole::new`]: pass
    /// `Some(...)` to pin a [`MovementProfile`] / [`CombatStance`] the roled
    /// ship adopts regardless of its archetype, `None` to defer to the archetype
    /// default. Roled members are squad-exempt, so a role override is the
    /// highest live precedence link for them.
    pub fn with_style(
        mut self,
        profile: Option<MovementProfile>,
        stance: Option<CombatStance>,
    ) -> Self {
        self.movement_profile = profile;
        self.combat_stance = stance;
        self
    }

    /// Whether the posture permits selecting `Engage`/`Ram` and pulling the
    /// trigger at `now` (the documented gate table — see module docs).
    pub fn allows_engage(&self, now: u64) -> bool {
        match self.posture {
            Posture::FreeEngage => true,
            Posture::DefensiveOnly => now < self.fired_upon_until,
            Posture::HoldFire => false,
        }
    }
}

/// Nearest contact whose last-seen position lies within `radius` of `center`
/// (squared distance; exact tie → lower target entity bits — the stable
/// cross-entity tiebreak). `None` when nothing is in the zone.
fn nearest_contact_in_zone(
    contacts: Option<&ContactList>,
    center: Vec2,
    radius: f32,
) -> Option<Entity> {
    let list = contacts?;
    let radius_sq = radius * radius;
    let mut best: Option<(f32, u64, Entity)> = None;
    for c in &list.contacts {
        let d = (c.last_pos - center).length_squared();
        if d > radius_sq {
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

/// Shared route-following upkeep for the route-shaped goals (`PatrolRoute` /
/// `SweepRegion` / `ScoutArea`) — one "arrived" definition, one resume rule:
///
/// - Defensive cursor clamp (a live-edited shrunken route never panics).
/// - Within [`ARRIVE_RADIUS`] of the current point → advance + wrap.
/// - Threat-gone release (the OBJ6-VC1 / TR-021 resume): a target the ship no
///   longer PERCEIVES (no contact entry — despawn is pruned by the V-1 sweep,
///   out-of-range ages out via the staleness window) is dropped, so the route
///   waypoint wins the next selection.
/// - The script re-asserts the current leg every think — squad/other writers
///   never permanently divert the route.
fn follow_route(
    route: &[Vec2],
    route_index: &mut usize,
    brain: &mut AiBrain,
    pos: Option<Vec2>,
    contacts: Option<&ContactList>,
) {
    if route.is_empty() {
        return; // Degenerate script: nothing to direct.
    }
    if *route_index >= route.len() {
        *route_index = 0;
    }
    if let Some(p) = pos {
        if (route[*route_index] - p).length() <= ARRIVE_RADIUS {
            *route_index = (*route_index + 1) % route.len();
        }
    }
    if let Some(t) = brain.target {
        let perceived = contacts.is_some_and(|c| c.contacts.iter().any(|x| x.target == t));
        if !perceived {
            brain.target = None;
        }
    }
    let goal = route[*route_index];
    if brain.waypoint != Some(goal) {
        brain.waypoint = Some(goal);
    }
}

/// The think-time composition step (see module docs): maintain the brain's
/// goal fields (`waypoint`/`home`/target upkeep) from the scripted goal.
/// Called by `ai_think_system` BEFORE candidate scoring, so this tick's
/// utility selection competes over the role-directed state. `sensor_range`
/// (the caller's `AiTuning::base_sensor_range`) sizes the recon goals'
/// coverage lanes.
pub(crate) fn role_apply(
    role: &mut ScenarioRole,
    brain: &mut AiBrain,
    pos: Option<Vec2>,
    contacts: Option<&ContactList>,
    sensor_range: f32,
    now: u64,
) {
    match &role.goal {
        RoleGoal::PatrolRoute(route) => {
            follow_route(route, &mut role.route_index, brain, pos, contacts);
        }
        RoleGoal::SweepRegion { min, max } | RoleGoal::ScoutArea { min, max } => {
            // T035 — REGENERATED each apply (documented regen-vs-cache
            // choice): region + sensor range are fixed scenario inputs, so
            // [`sweep_route`] returns the IDENTICAL `Vec` every time —
            // deterministic by construction. Caching it in the component
            // would only add live-tuning-edit staleness; thinks are sparse
            // (event/cadence-driven) and generation is O(lanes).
            let route = sweep_route(*min, *max, sensor_range);
            follow_route(&route, &mut role.route_index, brain, pos, contacts);
        }
        RoleGoal::Ambush { .. } => {
            // Hold dark: no goal, no self-acquired target (the trigger pass
            // also clears every tick; this is think-time defense in depth).
            if brain.waypoint.is_some() {
                brain.waypoint = None;
            }
            if brain.target.is_some() {
                brain.target = None;
            }
        }
        RoleGoal::Defend { anchor, radius } => {
            if brain.home != Some(*anchor) {
                brain.home = Some(*anchor);
            }
            // Zone-gated ACQUISITION (documented v1 rule): the role only
            // self-acquires contacts inside the zone; an already-live target
            // is prosecuted until despawn (the V-1 sweep clears it).
            if brain.target.is_none() && role.allows_engage(now) {
                brain.target = nearest_contact_in_zone(contacts, *anchor, *radius);
            }
            // No threat → arrive back at the anchor (`Waypoint` candidate;
            // with a target, `Engage` outranks it by bucket).
            if brain.waypoint != Some(*anchor) {
                brain.waypoint = Some(*anchor);
            }
        }
    }
}

/// T032 — the shared scenario-trigger pass (TR-015), registered in the gated
/// AI set AFTER the perception scan (triggers see THIS tick's contacts) and
/// BEFORE `ai_think_system` (fired ships think + transition the same tick).
///
/// Per tick, over every role-carrying ship in [`AiStableId`] order (V-3):
///
/// 1. **`DefensiveOnly` bookkeeping**: a pending [`AiEvent::DamageTaken`]
///    re-think (queued by damage producers/tests, observed here before the
///    think drains the queue) arms `fired_upon_until = now +`
///    [`FIRED_UPON_WINDOW_TICKS`].
/// 2. **Ambush hold**: un-fired ambush ships have any perception-acquired
///    `brain.target` cleared (they hold dark until the trigger).
/// 3. **Ambush triggers — ONE shared evaluation per group** (OBJ6-VC2):
///    ambush roles group by (faction, trigger-center bits, trigger-radius
///    bits); a group fires when ANY member perceives a hostile contact whose
///    last-seen position is inside the circle (contact lists only ever hold
///    hostiles — perception gates by faction opposition). On fire, EVERY
///    member that tick:
///    - gets `brain.target` = the group's firing target (the contact nearest
///      the trigger center; exact tie → lower entity bits — one coordinated
///      target),
///    - has its commit window CLEARED (`commit_until_tick = now`) — the
///      trigger is an order-level interrupt, so the soft `OrderChanged`
///      re-think below is never deferred by HINT-004 hysteresis (this is the
///      same-tick guarantee),
///    - gets [`AiEvent::OrderChanged`] pushed (the same-tick event think),
///    - and its goal DEGRADES to [`RoleGoal::Defend`] anchored on the trigger
///      center (the fired marker — the ambush never re-triggers).
///
/// Golden safety: no golden world spawns a `ScenarioRole`, so the query is
/// empty there and the goldens stay bit-identical.
pub fn role_trigger_system(
    tick: Res<CurrentTick>,
    mut queue: ResMut<RethinkQueue>,
    mut ships: Query<(
        Entity,
        &AiStableId,
        &mut ScenarioRole,
        &mut AiBrain,
        Option<&ContactList>,
        Option<&Faction>,
    )>,
) {
    let now = tick.0;

    // V-3 stable order (stable ids are unique → the sort is total).
    let mut order: Vec<(AiStableId, Entity)> = ships.iter().map(|(e, id, ..)| (*id, e)).collect();
    order.sort_unstable();

    // Ambush group key: faction discriminant (`u8::MAX` = factionless) +
    // trigger geometry bits — ships scripted onto the SAME trigger fire as one.
    type GroupKey = (u8, u32, u32, u32);
    let mut group_members: BTreeMap<GroupKey, Vec<Entity>> = BTreeMap::new();
    let mut group_candidates: BTreeMap<GroupKey, Vec<(Entity, Vec2)>> = BTreeMap::new();

    // Pass 1: per-ship bookkeeping + ambush group collection (stable order).
    for &(_, entity) in &order {
        let Ok((_, _, mut role, mut brain, contacts, faction)) = ships.get_mut(entity) else {
            continue;
        };
        // (1) DefensiveOnly: arm the weapons-free window on a pending hit.
        if queue.get(entity) == Some(AiEvent::DamageTaken) {
            role.fired_upon_until = now + FIRED_UPON_WINDOW_TICKS;
        }
        let RoleGoal::Ambush {
            trigger_center,
            trigger_radius,
        } = role.goal
        else {
            continue;
        };
        // (2) Hold dark: perception target acquisition runs earlier in the
        // tick — an un-fired ambusher releases it every tick.
        if brain.target.is_some() {
            brain.target = None;
        }
        let key: GroupKey = (
            faction.map_or(u8::MAX, |f| faction_key(*f)),
            trigger_center.x.to_bits(),
            trigger_center.y.to_bits(),
            trigger_radius.to_bits(),
        );
        group_members.entry(key).or_default().push(entity);
        // In-circle hostile contacts this member perceives (fresh: the scan +
        // network fusion ran earlier this tick).
        if let Some(list) = contacts {
            let radius_sq = trigger_radius * trigger_radius;
            for c in &list.contacts {
                if (c.last_pos - trigger_center).length_squared() <= radius_sq {
                    group_candidates
                        .entry(key)
                        .or_default()
                        .push((c.target, c.last_pos));
                }
            }
        }
    }

    // Pass 2: fire each tripped group — every member transitions THIS tick.
    for (key, members) in &group_members {
        let Some(candidates) = group_candidates.get(key) else {
            continue; // No member perceives an in-circle hostile: stay dark.
        };
        let center = Vec2::new(f32::from_bits(key.1), f32::from_bits(key.2));
        let radius = f32::from_bits(key.3);
        // The coordinated firing target: nearest to the trigger center
        // (squared distance), exact tie → lower entity bits.
        let mut best: Option<(f32, u64, Entity)> = None;
        for &(target, pos) in candidates {
            let d = (pos - center).length_squared();
            let bits = target.to_bits();
            let wins = match best {
                None => true,
                Some((bd, bb, _)) => d < bd || (d == bd && bits < bb),
            };
            if wins {
                best = Some((d, bits, target));
            }
        }
        let Some((_, _, target)) = best else { continue };
        for &member in members {
            let Ok((_, _, mut role, mut brain, _, _)) = ships.get_mut(member) else {
                continue;
            };
            brain.target = Some(target);
            // Order-level interrupt: clear the commitment so the OrderChanged
            // think below happens THIS tick for every member (OBJ6-VC2).
            brain.commit_until_tick = now;
            // Fired marker: degrade to defending the sprung trap's center —
            // FreeEngage-style prosecution, never re-triggers.
            role.goal = RoleGoal::Defend {
                anchor: center,
                radius,
            };
            queue.push(member, AiEvent::OrderChanged);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented posture gate table, including the DefensiveOnly window
    /// edge (strict `<`: the deadline tick itself is already weapons-tight).
    #[test]
    fn posture_gate_table() {
        let free = ScenarioRole::new(RoleGoal::PatrolRoute(vec![Vec2::ZERO]), Posture::FreeEngage);
        assert!(free.allows_engage(0));
        assert!(free.allows_engage(u64::MAX));

        let hold = ScenarioRole::new(RoleGoal::PatrolRoute(vec![Vec2::ZERO]), Posture::HoldFire);
        assert!(!hold.allows_engage(0));
        assert!(!hold.allows_engage(u64::MAX));

        let mut def = ScenarioRole::new(
            RoleGoal::PatrolRoute(vec![Vec2::ZERO]),
            Posture::DefensiveOnly,
        );
        assert!(!def.allows_engage(0), "never fired upon → no engage");
        def.fired_upon_until = 100;
        assert!(def.allows_engage(99), "inside the window");
        assert!(!def.allows_engage(100), "deadline tick is already closed");
    }

    /// `role_apply` patrol: asserts the current leg, advances + wraps the
    /// cursor on arrive, and releases an unperceived target (the VC1 resume).
    #[test]
    fn role_apply_patrol_advances_wraps_and_releases_lost_target() {
        let (p0, p1) = (Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0));
        let mut role = ScenarioRole::new(RoleGoal::PatrolRoute(vec![p0, p1]), Posture::FreeEngage);
        let mut brain = AiBrain::default();

        // Mid-leg: asserts the current point, cursor unchanged.
        role_apply(
            &mut role,
            &mut brain,
            Some(Vec2::new(50.0, 0.0)),
            None,
            200.0,
            0,
        );
        assert_eq!(role.route_index, 0);
        assert_eq!(brain.waypoint, Some(p0));

        // On the point: advance; on the LAST point: wrap to 0.
        role_apply(&mut role, &mut brain, Some(p0), None, 200.0, 1);
        assert_eq!(role.route_index, 1);
        assert_eq!(brain.waypoint, Some(p1));
        role_apply(&mut role, &mut brain, Some(p1), None, 200.0, 2);
        assert_eq!(role.route_index, 0, "cursor wraps");
        assert_eq!(brain.waypoint, Some(p0));

        // A target with NO contact entry is released (perception-lost).
        let mut world = bevy_ecs::world::World::new();
        let ghost = world.spawn_empty().id();
        brain.target = Some(ghost);
        let empty = ContactList::default();
        role_apply(
            &mut role,
            &mut brain,
            Some(Vec2::new(50.0, 0.0)),
            Some(&empty),
            200.0,
            3,
        );
        assert_eq!(brain.target, None, "unperceived target released → resume");
    }

    /// `role_apply` defend: anchors home/waypoint and zone-gates acquisition
    /// (in-zone contact acquired; out-of-zone ignored; HoldFire never).
    #[test]
    fn role_apply_defend_zone_gates_acquisition() {
        let mut world = bevy_ecs::world::World::new();
        let inside = world.spawn_empty().id();
        let outside = world.spawn_empty().id();
        let anchor = Vec2::new(10.0, 0.0);
        let contact = |target: Entity, pos: Vec2| crate::ai::perception::Contact {
            target,
            last_pos: pos,
            last_seen_tick: 0,
            signature: 1.0,
        };
        let mut contacts = ContactList::default();
        let mut entries = vec![
            contact(outside, anchor + Vec2::new(500.0, 0.0)),
            contact(inside, anchor + Vec2::new(30.0, 0.0)),
        ];
        entries.sort_by_key(|c| c.target.to_bits()); // list invariant
        contacts.contacts = entries;

        let goal = RoleGoal::Defend {
            anchor,
            radius: 100.0,
        };
        let mut role = ScenarioRole::new(goal.clone(), Posture::FreeEngage);
        let mut brain = AiBrain::default();
        role_apply(
            &mut role,
            &mut brain,
            Some(anchor),
            Some(&contacts),
            200.0,
            0,
        );
        assert_eq!(brain.home, Some(anchor));
        assert_eq!(brain.waypoint, Some(anchor));
        assert_eq!(brain.target, Some(inside), "only the in-zone contact");

        // HoldFire: the zone never acquires.
        let mut hold_role = ScenarioRole::new(goal, Posture::HoldFire);
        let mut hold_brain = AiBrain::default();
        role_apply(
            &mut hold_role,
            &mut hold_brain,
            Some(anchor),
            Some(&contacts),
            200.0,
            0,
        );
        assert_eq!(hold_brain.target, None, "HoldFire never self-acquires");
    }

    /// T035 `sweep_route`: deterministic boustrophedon — lanes ride min.y and
    /// max.y, spacing ≤ 1.5×sensor_range, fly-direction alternates, identical
    /// inputs reproduce the identical Vec (the regen contract), and the
    /// degenerate cases (swapped corners, zero area, zero sensor) are safe.
    #[test]
    fn sweep_route_is_deterministic_boustrophedon_coverage() {
        let (min, max) = (Vec2::new(0.0, 0.0), Vec2::new(160.0, 160.0));
        let route = sweep_route(min, max, 80.0); // spacing 120 → lanes 0/120/160
        assert_eq!(
            route,
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(160.0, 0.0), // lane 1: rightward
                Vec2::new(160.0, 120.0),
                Vec2::new(0.0, 120.0), // lane 2: leftward (the turn)
                Vec2::new(0.0, 160.0),
                Vec2::new(160.0, 160.0), // top lane clamped ONTO max.y
            ]
        );
        // Regen contract: bit-identical Vec on every call.
        assert_eq!(route, sweep_route(min, max, 80.0));
        // Lane spacing never exceeds 1.5 × sensor_range (coverage bound).
        for pair in route.chunks(2).collect::<Vec<_>>().windows(2) {
            assert!((pair[1][0].y - pair[0][0].y) <= 1.5 * 80.0 + 1e-3);
        }
        // Swapped corners normalize to the same route.
        assert_eq!(sweep_route(max, min, 80.0), route);
        // Zero-area region: one 2-point lane (still followable).
        let p = Vec2::new(5.0, 5.0);
        assert_eq!(sweep_route(p, p, 80.0), vec![p, p]);
        // Non-positive sensor range: coarse-cell fallback spacing, finite.
        let fallback = sweep_route(min, max, 0.0);
        assert!(!fallback.is_empty() && fallback.len() < 100);
    }

    /// T035 `role_apply` on the recon goals: the regenerated route is asserted
    /// as the brain waypoint, the cursor advances + wraps on arrive, and an
    /// unperceived target is released (the resume rule, shared with patrol).
    #[test]
    fn role_apply_recon_goals_follow_regenerated_route() {
        let (min, max) = (Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0));
        let route = sweep_route(min, max, 200.0); // one lane: [min, max]
        assert_eq!(route, vec![min, max]);
        for goal in [
            RoleGoal::SweepRegion { min, max },
            RoleGoal::ScoutArea { min, max },
        ] {
            let mut role = ScenarioRole::new(goal, Posture::FreeEngage);
            let mut brain = AiBrain::default();
            // Mid-leg: asserts the first point.
            role_apply(
                &mut role,
                &mut brain,
                Some(Vec2::new(50.0, 50.0)),
                None,
                200.0,
                0,
            );
            assert_eq!(brain.waypoint, Some(min));
            // On the point: advance; on the last: wrap.
            role_apply(&mut role, &mut brain, Some(min), None, 200.0, 1);
            assert_eq!((role.route_index, brain.waypoint), (1, Some(max)));
            role_apply(&mut role, &mut brain, Some(max), None, 200.0, 2);
            assert_eq!((role.route_index, brain.waypoint), (0, Some(min)));
            // Unperceived target released (the TR-021 resume rule).
            let mut world = bevy_ecs::world::World::new();
            let ghost = world.spawn_empty().id();
            brain.target = Some(ghost);
            let empty = ContactList::default();
            role_apply(
                &mut role,
                &mut brain,
                Some(Vec2::ZERO),
                Some(&empty),
                200.0,
                3,
            );
            assert_eq!(brain.target, None);
        }
    }
}
