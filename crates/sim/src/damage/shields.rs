//! Shield absorption + powered regen/decay (US1, Phase 3, FR-010).
//!
//! The outermost [`DefenseLayer::Shields`](crate::damage::DefenseLayer) step of the
//! traversal, plus the over-time regen/decay system. Both follow the substrate's
//! **pure-core + thin-system-wrapper** discipline: [`shield_absorb`] and
//! [`regen_shield`] are pure functions of their inputs (trivially testable), and
//! [`shield_regen_system`] is a thin `bevy_ecs` wrapper that reads the per-entity
//! `powered` state and calls the pure core.
//!
//! - [`shield_absorb`] (FR-010): absorbs first, mitigated by the `(Shields,
//!   channel)` matrix cell. A depleted/unpowered shield (`current <= 0`) passes the
//!   event through **untouched** — armor is exposed. `ThermalEnergy` has LOW shield
//!   mitigation, so it melts through; `Kinetic` is HIGH, so it is heavily absorbed.
//! - [`regen_shield`] / [`shield_regen_system`] (FR-010, INV-D14): a `power_linked`
//!   shield regenerates toward `max` only while powered, and decays at
//!   [`ShieldConfig::unpowered_decay`] while `power_linked && !powered` (exposing
//!   Armor at `0`). A non-`power_linked` unpowered shield holds (no regen, no decay).

use bevy_ecs::prelude::*;

use super::content::ShieldConfig;
use super::event::DamageEvent;
use super::layers::Shields;
use super::resist::{layer_resist, DefenseLayer, ResistanceMatrix};
use crate::clock::FixedDt;
use crate::fitting::ShipStats;

/// Absorb a [`DamageEvent`] at the shield layer (FR-010), returning the
/// `(surviving_magnitude, now_depleted)` pair the traversal carries on to Armor.
///
/// A depleted/unpowered shield (`current <= 0`) passes the event through
/// **untouched** — the armor is exposed immediately (edge case). Otherwise the
/// shield mitigates by the `(Shields, channel)` matrix cell and absorbs up to its
/// remaining `current`:
///
/// - `effective = magnitude * (1 - layer_resist(Shields, channel))`;
/// - `absorbed = min(effective, current)`; `current -= absorbed` clamped `>= 0`
///   (INV-D01);
/// - `surviving = max(effective - absorbed, 0)` flows to Armor.
///
/// Because `ThermalEnergy` has LOW shield mitigation (little removed), most of it
/// survives → it "melts through"; `Kinetic` is HIGH → heavily absorbed. Pure; never
/// panics.
pub fn shield_absorb(
    shields: &mut Shields,
    ev: &DamageEvent,
    matrix: &ResistanceMatrix,
) -> (f32, bool) {
    // Depleted/unpowered (residual `current <= 0`): pass through untouched — the
    // armor is exposed (edge case). The shield neither mitigates nor absorbs.
    if shields.current <= 0.0 {
        return (ev.magnitude, true);
    }

    let r = layer_resist(matrix, DefenseLayer::Shields, ev.channel);
    let effective = ev.magnitude * (1.0 - r);
    let absorbed = effective.min(shields.current);
    // INV-D01: shield HP clamped `>= 0`.
    shields.current = (shields.current - absorbed).max(0.0);
    let surviving = (effective - absorbed).max(0.0);
    (surviving, shields.current <= 0.0)
}

/// Regenerate or decay a shield pool over `dt` (FR-010, INV-D14) — the pure core of
/// [`shield_regen_system`], factored out for direct testability.
///
/// - while `powered`: `current = (current + regen_rate * dt).min(max)` — regen
///   toward `max` (a fitted shield's own `regen_rate` is used, seeded from
///   [`ShieldConfig::shield_regen_default`] at authoring time);
/// - while `power_linked && !powered`: `current = (current - unpowered_decay *
///   dt).max(0)` — the reactor is lost, so the pool drains and exposes Armor at `0`
///   (INV-D14);
/// - a non-`power_linked` unpowered shield **holds** (no regen, no decay).
///
/// Health is clamped `0..=max` (INV-D01). Pure; never panics.
pub fn regen_shield(shields: &mut Shields, powered: bool, dt: f32, cfg: &ShieldConfig) {
    if powered {
        // Regen toward max at the shield's own rate (INV-D14).
        shields.current = (shields.current + shields.regen_rate * dt).min(shields.max);
    } else if shields.power_linked {
        // Power lost on a power-linked shield: decay toward 0, exposing Armor.
        shields.current = (shields.current - cfg.unpowered_decay * dt).max(0.0);
    }
    // A non-power-linked unpowered shield holds: no regen, no decay.
}

/// The fixed-step shield regen/decay system (FR-010, INV-D14) — the thin wrapper
/// over the pure [`regen_shield`] core.
///
/// For each `(Shields, ShipStats)` ship, derives `powered = power_supply >=
/// power_draw` (a destroyed reactor collapses `power_supply`, un-powering the
/// shield, FR-013) and applies the per-entity regen/decay. Mirrors the
/// `feedback_decay_system` fixed-step shape; server-authoritative (INV-D16).
pub fn shield_regen_system(
    dt: Res<FixedDt>,
    cfg: Res<ShieldConfig>,
    mut q: Query<(&mut Shields, &ShipStats)>,
) {
    let dt = dt.0;
    for (mut shields, stats) in &mut q {
        let powered = stats.power_supply >= stats.power_draw;
        regen_shield(&mut shields, powered, dt, &cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::content::default_resistance_matrix;
    use crate::damage::event::Channel;
    use glam::Vec2;

    fn event(channel: Channel, magnitude: f32) -> DamageEvent {
        DamageEvent {
            channel,
            magnitude,
            penetration: 0.0,
            pen_size: 0.0,
            point: Vec2::ZERO,
            dir: Vec2::new(1.0, 0.0),
            source: None,
        }
    }

    #[test]
    fn depleted_shield_passes_through_untouched() {
        let matrix = default_resistance_matrix();
        let mut shields = Shields::depleted(100.0, 5.0, true);
        let ev = event(Channel::Kinetic, 50.0);
        let (surviving, depleted) = shield_absorb(&mut shields, &ev, &matrix);
        assert_eq!(surviving, 50.0, "a depleted shield exposes armor untouched");
        assert!(depleted);
    }

    #[test]
    fn thermal_melts_through_more_than_kinetic_is_absorbed() {
        let matrix = default_resistance_matrix();
        // Same magnitude, full shield: ThermalEnergy (LOW shield mitigation) leaves
        // more surviving than Kinetic (HIGH shield mitigation).
        let mut sh_thermal = Shields::full(10.0, 5.0, true);
        let mut sh_kinetic = Shields::full(10.0, 5.0, true);
        let (surv_thermal, _) = shield_absorb(
            &mut sh_thermal,
            &event(Channel::ThermalEnergy, 100.0),
            &matrix,
        );
        let (surv_kinetic, _) =
            shield_absorb(&mut sh_kinetic, &event(Channel::Kinetic, 100.0), &matrix);
        assert!(
            surv_thermal > surv_kinetic,
            "thermal ({surv_thermal}) should melt through more than kinetic ({surv_kinetic})"
        );
    }

    #[test]
    fn regen_climbs_to_max_while_powered() {
        let cfg = ShieldConfig::default();
        // max 100, regen 5/s → ~20s to fill; 2000 ticks at 60 Hz (~33s) over-fills,
        // proving the `.min(max)` clamp holds.
        let mut shields = Shields::depleted(100.0, 5.0, true);
        for _ in 0..2000 {
            regen_shield(&mut shields, true, 1.0 / 60.0, &cfg);
        }
        assert_eq!(shields.current, 100.0, "regen clamps at max");
    }

    #[test]
    fn power_linked_unpowered_decays_to_zero_unlinked_holds() {
        let cfg = ShieldConfig::default();
        let mut linked = Shields::full(100.0, 5.0, true);
        let mut unlinked = Shields::full(100.0, 5.0, false);
        // unpowered_decay 10/s on a 100 pool → ~10s; 2000 ticks (~33s) over-drains,
        // proving the `.max(0)` clamp holds.
        for _ in 0..2000 {
            regen_shield(&mut linked, false, 1.0 / 60.0, &cfg);
            regen_shield(&mut unlinked, false, 1.0 / 60.0, &cfg);
        }
        assert_eq!(linked.current, 0.0, "power-linked unpowered shield decays");
        assert_eq!(
            unlinked.current, 100.0,
            "non-power-linked unpowered shield holds"
        );
    }
}
