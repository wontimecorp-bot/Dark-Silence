//! Phase E — the dynamic **Energy capacitor** + **Heat** pools (the combat power loop).
//!
//! [`energy_system`] maintains both pools each fixed step from the ship's live [`ShipStats`]: the
//! Energy capacitor recharges from the reactor toward `power_supply · energy_capacity_secs`, and
//! Heat cools toward `0`. Firing (in [`weapon_fire_system`](crate::weapon::weapon_fire_system)) is
//! what DRAINS energy + ADDS heat and is gated on both; this system only does the per-tick
//! recharge/cooldown and (re)derives the caps from the live stats (so reactor damage shrinks the
//! Energy pool, emergently).
//!
//! Only LIVE-spawned fitted ships carry [`Energy`]/[`Heat`]; the headless determinism/botkit worlds
//! do not, so this system is a no-op there (and the weapon gate stays `Option`-skipped) → the
//! determinism + botkit comparisons remain bit-identical.

use bevy_ecs::prelude::*;

use crate::clock::FixedDt;
use crate::components::{Afterburner, Energy, Heat};
use crate::fitting::ShipStats;
use crate::intent::ShipIntent;
use crate::tuning::SimTuning;

/// Recharge/drain Energy + cool Heat each fixed step, re-deriving the caps from the live
/// [`ShipStats`]. **Phase F:** the capacitor's net steady rate is `power_supply − continuous_draw −
/// thrust_drain` (so shields-on lowers it and thrusting drains it); `Energy.rate` carries that net
/// for the HUD. `ShipIntent` is `Option` (the thrust drain needs the pilot input; absent ⇒ 0).
/// `SimTuning` is `Option` so a minimal world degrades to the defaults. Pure, deterministic.
pub fn energy_system(
    dt: Res<FixedDt>,
    sim: Option<Res<SimTuning>>,
    mut q: Query<(&mut Energy, &mut Heat, &ShipStats, Option<&ShipIntent>)>,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();
    for (mut energy, mut heat, stats, intent) in &mut q {
        // Energy capacity + gross regen track the live reactor output. R92 — fitted energy STORES
        // (capacitors/batteries) add flat capacity: with a dead reactor `max` = the stores and
        // `regen` = 0, so the pool persists and drains as used — you fight on the stored charge.
        energy.max = (stats.power_supply * sim.energy_capacity_secs + stats.energy_store).max(0.0);
        energy.regen = stats.power_supply.max(0.0);
        // Active thrust drain (proportional to pilot input, scaled by the turn-power share so a hard
        // turn that already saps translational thrust also costs less energy).
        let thrust_drain = intent.map_or(0.0, |i| {
            sim.thrust_energy_per_input
                * (i.forward.abs() + i.strafe.abs() + stats.turn_power_share * i.turn.abs())
        });
        // Net steady rate: + charging, − draining. Weapon fire is a separate per-shot impulse.
        let net = energy.regen - stats.continuous_draw - thrust_drain;
        energy.rate = net;
        energy.current = (energy.current + net * dt).clamp(0.0, energy.max);
        // Heat: cool toward 0, clamped into [0, max].
        heat.max = sim.heat_capacity.max(f32::MIN_POSITIVE);
        heat.dissipation = sim.heat_dissipation.max(0.0);
        heat.current = (heat.current - heat.dissipation * dt).clamp(0.0, heat.max);
    }
}

/// Phase F — drain/recharge the **afterburner** pool each fixed step: boosting
/// (`intent.afterburner && current > 0`) drains it; otherwise it recharges toward `max`. The thrust
/// BOOST itself is applied in [`ship_motion_system`](crate::flight::ship_motion_system) (which reads
/// this pool + the intent). A self-contained pool (does NOT touch [`Energy`]). Only LIVE ships carry
/// [`Afterburner`] → a no-op in the headless/determinism/botkit worlds. Pure, deterministic.
pub fn afterburner_system(
    dt: Res<FixedDt>,
    sim: Option<Res<SimTuning>>,
    mut q: Query<(&mut Afterburner, &ShipIntent)>,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();
    for (mut ab, intent) in &mut q {
        ab.max = sim.afterburner_capacity.max(f32::MIN_POSITIVE);
        ab.regen = sim.afterburner_regen_rate.max(0.0);
        ab.drain = sim.afterburner_drain_rate.max(0.0);
        let boosting = intent.afterburner && ab.current > 0.0;
        let delta = if boosting { -ab.drain } else { ab.regen };
        ab.current = (ab.current + delta * dt).clamp(0.0, ab.max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fitting::{derive_ship_stats, seed_catalogs, Fit, HULL_FIGHTER};

    /// A fighter `ShipStats` (reactor + thruster + autocannon) for the energy derivation.
    fn fighter_stats() -> ShipStats {
        use crate::fitting::content::{
            MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_THRUSTER_BASIC,
        };
        use crate::fitting::{build_layout, SlotId};
        let (modules, hulls) = seed_catalogs();
        let hull = hulls.get(HULL_FIGHTER).unwrap();
        let mut fit = Fit::new(HULL_FIGHTER);
        fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
        fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
        fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
        let layout = build_layout(hull, &fit, &modules);
        derive_ship_stats(hull, &fit, &modules, &layout)
    }

    fn world_with(energy: Energy, heat: Heat, stats: ShipStats) -> (World, Entity) {
        let mut w = World::new();
        w.insert_resource(FixedDt(0.1));
        w.insert_resource(SimTuning::default());
        let e = w.spawn((energy, heat, stats)).id();
        (w, e)
    }

    #[test]
    fn energy_recharges_toward_max_and_heat_cools_toward_zero() {
        let stats = fighter_stats();
        let sim = SimTuning::default();
        let (mut w, e) = world_with(
            Energy {
                current: 0.0,
                max: 0.0,
                regen: 0.0,
                rate: 0.0,
            },
            Heat {
                current: 30.0,
                max: 45.0,
                dissipation: 6.0,
            },
            stats,
        );
        let mut sched = Schedule::default();
        sched.add_systems(energy_system);
        sched.run(&mut w);
        let energy = w.get::<Energy>(e).unwrap();
        let heat = w.get::<Heat>(e).unwrap();
        // Cap re-derived from power_supply; current recharged by regen·dt.
        assert!((energy.max - stats.power_supply * sim.energy_capacity_secs).abs() < 1e-4);
        assert!(energy.current > 0.0 && energy.current <= energy.max);
        assert!((energy.current - stats.power_supply * 0.1).abs() < 1e-3);
        // Heat cooled by dissipation·dt.
        assert!((heat.current - (30.0 - 6.0 * 0.1)).abs() < 1e-3);
    }

    #[test]
    fn energy_clamps_at_max_and_heat_never_negative() {
        let stats = fighter_stats();
        let (mut w, e) = world_with(
            Energy {
                current: 9999.0,
                max: 0.0,
                regen: 0.0,
                rate: 0.0,
            },
            Heat {
                current: 0.0,
                max: 45.0,
                dissipation: 6.0,
            },
            stats,
        );
        let mut sched = Schedule::default();
        sched.add_systems(energy_system);
        sched.run(&mut w);
        let energy = w.get::<Energy>(e).unwrap();
        let heat = w.get::<Heat>(e).unwrap();
        assert_eq!(energy.current, energy.max, "energy clamps to max");
        assert_eq!(heat.current, 0.0, "heat never goes negative");
    }

    /// Phase F: full-forward thrust drains the capacitor (net rate < 0) when the thrust cost exceeds
    /// the reactor surplus; idle (no intent) charges.
    #[test]
    fn thrusting_drains_energy_and_sets_negative_rate() {
        let stats = fighter_stats(); // power_supply 30, continuous_draw 0
        let mut w = World::new();
        w.insert_resource(FixedDt(0.1));
        w.insert_resource(SimTuning::default()); // thrust_energy_per_input 35 → net = 30 − 35 = −5
        let e = w
            .spawn((
                Energy {
                    current: 100.0,
                    max: 200.0,
                    regen: 0.0,
                    rate: 0.0,
                },
                Heat {
                    current: 0.0,
                    max: 45.0,
                    dissipation: 0.0,
                },
                stats,
                ShipIntent {
                    forward: 1.0,
                    ..Default::default()
                },
            ))
            .id();
        let mut sched = Schedule::default();
        sched.add_systems(energy_system);
        sched.run(&mut w);
        let energy = w.get::<Energy>(e).unwrap();
        assert!(
            energy.rate < 0.0,
            "full-forward thrust drains (rate < 0), got {}",
            energy.rate
        );
        assert!(energy.current < 100.0, "energy dropped while thrusting");
    }

    /// Phase F: the afterburner pool drains while boosting and recharges when released.
    #[test]
    fn afterburner_drains_while_boosting_and_recharges_idle() {
        use crate::components::Afterburner;
        let sim = SimTuning::default();
        let mut w = World::new();
        w.insert_resource(FixedDt(0.1));
        w.insert_resource(sim);
        let e = w
            .spawn((
                Afterburner::seed(),
                ShipIntent {
                    afterburner: true,
                    ..Default::default()
                },
            ))
            .id();
        let mut sched = Schedule::default();
        sched.add_systems(afterburner_system);
        // Boosting drains by drain_rate·dt.
        sched.run(&mut w);
        let after = w.get::<Afterburner>(e).unwrap().current;
        assert!(after < sim.afterburner_capacity, "boosting drains the pool");
        assert!(
            (after - (sim.afterburner_capacity - sim.afterburner_drain_rate * 0.1)).abs() < 1e-3
        );
        // Release → recharges.
        *w.get_mut::<ShipIntent>(e).unwrap() = ShipIntent::default();
        let before = w.get::<Afterburner>(e).unwrap().current;
        sched.run(&mut w);
        assert!(
            w.get::<Afterburner>(e).unwrap().current > before,
            "idle recharges the pool"
        );
    }
}
