//! Player command override (R99 Phase A): the [`PlayerOrder`] component — a
//! user's DIRECT command to a specific AI ship, layered at the HIGHEST
//! precedence over the whole decision chain (player > squad > role > archetype
//! default).
//!
//! **Where it sits (the precedence keystone)**: `ai_think_system` (brain.rs)
//! applies a present `PlayerOrder` BEFORE the rest of the resolution and lets it
//! WIN every channel a user can command:
//!
//! - **Nav goal**: a `PlayerOrder` whose [`PlayerOrder::kind`] is `Some(_)` is
//!   applied via [`PlayerOrder::apply`] and OVERWRITES whatever `role_apply`
//!   wrote (the order runs after the role, so the player's waypoint/home/target
//!   are the ones the candidate scoring competes over). A `kind: None`
//!   ("settings only") `PlayerOrder` leaves the nav goal to the role/squad and
//!   only contributes its style/posture overrides.
//! - **Style** (`profile`/`stance`): folded into the resolved chain as the
//!   top link — `player.profile.or(squad).or(role).unwrap_or(archetype)`.
//! - **Posture**: `player.posture.or(role posture).unwrap_or(FreeEngage)` —
//!   the player can free a `HoldFire`-roled ship to engage, or pin a ship dark.
//!
//! **Squad-exempt + planner-skip**: a ship carrying a `PlayerOrder` is exempt
//! from squad goal assignment (`squad_think_system` skips it exactly as it skips
//! a roled member) and counts as "not squad-commandable" for the strategic /
//! wing planners (an all-commanded/roled squad is skipped). So a user command is
//! never stomped by the order layer between thinks.
//!
//! **Ephemeral (V-9)**: `Clone + Debug`, NO `Serialize` — like every other AI
//! component the order is reconstructable from the command stream, never
//! persisted. The absence of the component IS "no order" (there is no `None`
//! kind variant — `kind: None` means *settings only*, not *no order*).
//!
//! **Determinism**: pure component reads + the same `waypoint`/`home`/`target`
//! writes `role_apply` makes — no RNG, no HashMap, intent-only (V-6). No golden
//! world spawns a `PlayerOrder`, so the gated apply branch never runs there and
//! the goldens stay bit-identical.

use bevy_ecs::prelude::*;
use glam::Vec2;

use crate::ai::brain::{AiBrain, CombatStance, MovementProfile, ARRIVE_RADIUS};
use crate::ai::role::Posture;

/// The navigation goal of a [`PlayerOrder`] — the user's commanded movement
/// objective. `Clone + Debug + PartialEq`, no `Serialize` (ephemeral, V-9).
///
/// There is deliberately NO `None` variant: the ABSENCE of the whole
/// [`PlayerOrder`] component is "no order", and a `PlayerOrder { kind: None, .. }`
/// is "settings only" (style/posture without a nav command). So every variant
/// here is an active command.
#[derive(Clone, Debug, PartialEq)]
pub enum OrderKind {
    /// Fly to a point and hold there (clears the engage target so the ship
    /// actually goes there rather than chasing).
    MoveTo(Vec2),
    /// Hold position at `anchor`, defending within `radius` (Defend-style: home
    /// plus waypoint pinned to the anchor; in-range hostiles are handled by the
    /// ship's own perception/engage selection).
    HoldAt {
        /// The position to hold / defend.
        anchor: Vec2,
        /// The hold radius (the zone the ship guards around the anchor).
        radius: f32,
    },
    /// Attack a specific entity (the user-picked target; the brain's Engage
    /// selection + combat stances do the rest).
    Attack(Entity),
    /// Patrol a closed route, advancing the cursor on arrival (wrapping).
    Patrol {
        /// The route waypoints (the cursor wraps over them).
        points: Vec<Vec2>,
        /// The current cursor into `points` (advanced by [`PlayerOrder::apply`]).
        index: usize,
    },
}

/// A user's DIRECT command override on a specific AI ship (R99 Phase A) —
/// HIGHEST precedence, applied in `ai_think_system` before role/squad
/// resolution. `Clone + Debug`, no `Serialize` (ephemeral, V-9).
///
/// **`kind: Option<OrderKind>` (documented decision)**: `Some(_)` is an active
/// nav command that overrides the role/squad goal; `None` is "settings only" —
/// the ship keeps its role/squad nav goal but adopts the player's style/posture
/// overrides. This lets a UI set a ship's pacing/stance/posture WITHOUT
/// disturbing its current movement objective (e.g. "hold fire on that patroller"
/// or "kite from now on") and lets it issue a pure movement command, or both.
///
/// **Style/posture channels** (`profile`/`stance`/`posture`): each `Some(...)`
/// is the TOP link in the resolved precedence chain (`player > squad > role >
/// archetype default`); `None` defers to the next link. Set via the builder
/// setters ([`PlayerOrder::with_profile`] / [`PlayerOrder::with_stance`] /
/// [`PlayerOrder::with_posture`]).
#[derive(Component, Clone, Debug)]
pub struct PlayerOrder {
    /// The commanded nav goal, or `None` for a settings-only order (see the
    /// type docs).
    pub kind: Option<OrderKind>,
    /// Top-precedence [`MovementProfile`] override (`None` = defer to squad/
    /// role/archetype).
    pub profile: Option<MovementProfile>,
    /// Top-precedence [`CombatStance`] override (`None` = defer).
    pub stance: Option<CombatStance>,
    /// Top-precedence [`Posture`] override (`None` = defer to the role posture,
    /// then `FreeEngage`).
    pub posture: Option<Posture>,
}

impl PlayerOrder {
    /// A bare order with the given `kind` and no style/posture overrides — the
    /// shared constructor body the command helpers spread over.
    fn bare(kind: Option<OrderKind>) -> Self {
        Self {
            kind,
            profile: None,
            stance: None,
            posture: None,
        }
    }

    /// Command the ship to fly to `p` and hold there.
    pub fn move_to(p: Vec2) -> Self {
        Self::bare(Some(OrderKind::MoveTo(p)))
    }

    /// Command the ship to hold/defend at `anchor` within `radius`.
    pub fn hold_at(anchor: Vec2, radius: f32) -> Self {
        Self::bare(Some(OrderKind::HoldAt { anchor, radius }))
    }

    /// Command the ship to attack `target`.
    pub fn attack(target: Entity) -> Self {
        Self::bare(Some(OrderKind::Attack(target)))
    }

    /// Command the ship to patrol the closed route `points` (cursor at 0).
    pub fn patrol(points: Vec<Vec2>) -> Self {
        Self::bare(Some(OrderKind::Patrol { points, index: 0 }))
    }

    /// A SETTINGS-ONLY order: no nav command (`kind == None`), only the
    /// style/posture overrides applied via the builder setters. Leaves the
    /// ship's role/squad nav goal untouched.
    pub fn settings_only() -> Self {
        Self::bare(None)
    }

    /// Builder: pin the [`MovementProfile`] override (the top precedence link).
    pub fn with_profile(mut self, profile: MovementProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// R100 — the movement profile a commanded MOVE uses when the user hasn't
    /// pinned one: POSITIONAL kinds (`MoveTo`/`HoldAt`/`Patrol`) default to
    /// [`MovementProfile::Rush`] so they ACTIVELY BRAKE and PARK (`arrive_braked`)
    /// instead of inheriting the archetype default (which is `Cruise` — a
    /// drag-braked COAST that overshoots and limit-cycles). A user-pinned
    /// `profile` always wins (`self.profile.or(..)`). `Attack` and settings-only
    /// orders DEFER to `self.profile` (their pace is the role/squad/archetype
    /// chain — `engage_motion` drives the approach, not a parking waypoint).
    ///
    /// This is the L1 seam consumed by `ai_think_system`'s style resolution:
    /// `player_profile = order.resolved_move_profile()` (instead of the bare
    /// `order.profile`), so a bare `move_to(p)` parks without the user having to
    /// `.with_profile(Rush)`.
    pub fn resolved_move_profile(&self) -> Option<MovementProfile> {
        match self.kind {
            Some(OrderKind::MoveTo(_))
            | Some(OrderKind::HoldAt { .. })
            | Some(OrderKind::Patrol { .. }) => self.profile.or(Some(MovementProfile::Rush)),
            Some(OrderKind::Attack(_)) | None => self.profile,
        }
    }

    /// Builder: pin the [`CombatStance`] override (the top precedence link).
    pub fn with_stance(mut self, stance: CombatStance) -> Self {
        self.stance = Some(stance);
        self
    }

    /// Builder: pin the [`Posture`] override (the top precedence link).
    pub fn with_posture(mut self, posture: Posture) -> Self {
        self.posture = Some(posture);
        self
    }

    /// Apply the commanded nav goal onto `brain` — the HIGHEST-precedence
    /// nav write, mirroring `role_apply`'s shapes (waypoint/home/target). Called
    /// by `ai_think_system` AFTER `role_apply` so the player's writes overwrite
    /// the role's. A settings-only order (`kind == None`) writes NOTHING here
    /// (the role/squad goal stands); the style/posture overrides are resolved by
    /// the caller separately.
    ///
    /// - **`MoveTo(p)`**: `brain.waypoint = Some(p)` and CLEAR `brain.target`
    ///   (so the ship flies to the point instead of chasing a leftover target;
    ///   the role/squad never re-acquire because the ship is command-exempt).
    /// - **`HoldAt { anchor, radius }`**: Defend-style — `brain.home =
    ///   Some(anchor)`, `brain.waypoint = Some(anchor)`. The ship returns to /
    ///   holds the anchor; an in-range hostile is acquired and engaged by the
    ///   ship's own perception/engage path (no special acquisition here, the
    ///   documented v1 — the hold is the command, prosecution is emergent).
    /// - **`Attack(target)`**: `brain.target = Some(target)` (Engage selection +
    ///   the combat stances close and fire). The nav waypoint is left as-is —
    ///   `engage_motion` drives the approach from the target, not a waypoint.
    /// - **`Patrol { points, index }`**: assert `points[index]` as the waypoint;
    ///   within [`ARRIVE_RADIUS`] of it, advance the cursor (wrapping). The
    ///   index lives in the component, so this mutates `self` — hence the `&mut
    ///   self` query in `ai_think_system`.
    pub fn apply(&mut self, brain: &mut AiBrain, pos: Vec2, _now: u64) {
        let Some(kind) = self.kind.as_mut() else {
            return; // Settings-only: leave the nav goal to role/squad.
        };
        match kind {
            OrderKind::MoveTo(p) => {
                let p = *p;
                if brain.waypoint != Some(p) {
                    brain.waypoint = Some(p);
                }
                if brain.target.is_some() {
                    brain.target = None; // Fly to the point, don't chase.
                }
            }
            OrderKind::HoldAt { anchor, radius: _ } => {
                let anchor = *anchor;
                if brain.home != Some(anchor) {
                    brain.home = Some(anchor);
                }
                if brain.waypoint != Some(anchor) {
                    brain.waypoint = Some(anchor);
                }
                // In-range hostiles are acquired/engaged by the ship's own
                // perception + Engage selection (the documented v1 hold rule).
            }
            OrderKind::Attack(target) => {
                let target = *target;
                if brain.target != Some(target) {
                    brain.target = Some(target);
                }
            }
            OrderKind::Patrol { points, index } => {
                if points.is_empty() {
                    return; // Degenerate route: nothing to direct.
                }
                if *index >= points.len() {
                    *index = 0;
                }
                if (points[*index] - pos).length() <= ARRIVE_RADIUS {
                    *index = (*index + 1) % points.len();
                }
                let goal = points[*index];
                if brain.waypoint != Some(goal) {
                    brain.waypoint = Some(goal);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `move_to` sets the waypoint and clears a leftover target (fly to the
    /// point, don't chase); a settings-only order touches no nav field.
    #[test]
    fn move_to_sets_waypoint_and_clears_target() {
        let mut world = bevy_ecs::world::World::new();
        let ghost = world.spawn_empty().id();
        let mut brain = AiBrain {
            target: Some(ghost),
            waypoint: Some(Vec2::new(1.0, 1.0)),
            ..AiBrain::default()
        };
        let mut order = PlayerOrder::move_to(Vec2::new(50.0, 0.0));
        order.apply(&mut brain, Vec2::ZERO, 0);
        assert_eq!(brain.waypoint, Some(Vec2::new(50.0, 0.0)));
        assert_eq!(brain.target, None, "MoveTo clears the leftover target");
    }

    /// `hold_at` anchors home + waypoint (Defend-style).
    #[test]
    fn hold_at_anchors_home_and_waypoint() {
        let anchor = Vec2::new(10.0, -5.0);
        let mut brain = AiBrain::default();
        let mut order = PlayerOrder::hold_at(anchor, 100.0);
        order.apply(&mut brain, Vec2::ZERO, 0);
        assert_eq!(brain.home, Some(anchor));
        assert_eq!(brain.waypoint, Some(anchor));
    }

    /// `attack` sets the brain target (Engage selection does the rest).
    #[test]
    fn attack_sets_target() {
        let mut world = bevy_ecs::world::World::new();
        let foe = world.spawn_empty().id();
        let mut brain = AiBrain::default();
        let mut order = PlayerOrder::attack(foe);
        order.apply(&mut brain, Vec2::ZERO, 0);
        assert_eq!(brain.target, Some(foe));
    }

    /// `patrol` asserts the current leg and advances + wraps the cursor on
    /// arrival (the [`ARRIVE_RADIUS`] rule, mirroring `role_apply`'s patrol).
    #[test]
    fn patrol_advances_and_wraps() {
        let (p0, p1) = (Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0));
        let mut brain = AiBrain::default();
        let mut order = PlayerOrder::patrol(vec![p0, p1]);

        // Mid-leg: asserts the current point, cursor unchanged.
        order.apply(&mut brain, Vec2::new(50.0, 0.0), 0);
        assert_eq!(brain.waypoint, Some(p0));
        let OrderKind::Patrol { index, .. } = order.kind.as_ref().unwrap() else {
            panic!("patrol kind");
        };
        assert_eq!(*index, 0);

        // On p0: advance to p1; on the LAST point p1: wrap to 0.
        order.apply(&mut brain, p0, 1);
        assert_eq!(brain.waypoint, Some(p1));
        order.apply(&mut brain, p1, 2);
        assert_eq!(brain.waypoint, Some(p0), "cursor wraps");
    }

    /// R100 — `resolved_move_profile`: bare POSITIONAL kinds park (default
    /// `Rush`); a user-pinned `profile` wins over the default; `Attack` and
    /// settings-only defer to `self.profile` (no park default).
    #[test]
    fn resolved_move_profile() {
        let foe = bevy_ecs::world::World::new().spawn_empty().id();
        // Bare positional kinds default to Rush (park onto the goal).
        assert_eq!(
            PlayerOrder::move_to(Vec2::new(50.0, 0.0)).resolved_move_profile(),
            Some(MovementProfile::Rush)
        );
        assert_eq!(
            PlayerOrder::hold_at(Vec2::ZERO, 100.0).resolved_move_profile(),
            Some(MovementProfile::Rush)
        );
        assert_eq!(
            PlayerOrder::patrol(vec![Vec2::ZERO, Vec2::new(10.0, 0.0)]).resolved_move_profile(),
            Some(MovementProfile::Rush)
        );
        // A user-pinned profile WINS the default.
        assert_eq!(
            PlayerOrder::move_to(Vec2::new(50.0, 0.0))
                .with_profile(MovementProfile::Leisurely)
                .resolved_move_profile(),
            Some(MovementProfile::Leisurely)
        );
        // Attack defers to self.profile (no park default — `engage_motion` paces).
        assert_eq!(PlayerOrder::attack(foe).resolved_move_profile(), None);
        assert_eq!(
            PlayerOrder::attack(foe)
                .with_profile(MovementProfile::Rush)
                .resolved_move_profile(),
            Some(MovementProfile::Rush)
        );
        // Settings-only defers to self.profile.
        assert_eq!(PlayerOrder::settings_only().resolved_move_profile(), None);
        assert_eq!(
            PlayerOrder::settings_only()
                .with_profile(MovementProfile::Cruise)
                .resolved_move_profile(),
            Some(MovementProfile::Cruise)
        );
    }

    /// A settings-only order (`kind == None`) writes no nav field; the style
    /// builders carry the overrides instead.
    #[test]
    fn settings_only_leaves_nav_untouched() {
        let mut brain = AiBrain {
            waypoint: Some(Vec2::new(7.0, 7.0)),
            home: Some(Vec2::new(1.0, 1.0)),
            ..AiBrain::default()
        };
        let before = brain;
        let mut order = PlayerOrder::settings_only()
            .with_profile(MovementProfile::Rush)
            .with_stance(CombatStance::Kite)
            .with_posture(Posture::HoldFire);
        order.apply(&mut brain, Vec2::ZERO, 0);
        assert_eq!(brain, before, "settings-only touches no nav field");
        assert_eq!(order.profile, Some(MovementProfile::Rush));
        assert_eq!(order.stance, Some(CombatStance::Kite));
        assert_eq!(order.posture, Some(Posture::HoldFire));
    }
}
