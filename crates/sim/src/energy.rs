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
use crate::components::{Energy, Heat};
use crate::fitting::ShipStats;
use crate::tuning::SimTuning;

/// Recharge Energy + cool Heat each fixed step, re-deriving the caps from the live [`ShipStats`].
/// `SimTuning` is `Option` so a minimal world degrades to the const defaults rather than panicking.
/// Pure per-entity integration (f32, fixed order) — deterministic.
pub fn energy_system(
    dt: Res<FixedDt>,
    sim: Option<Res<SimTuning>>,
    mut q: Query<(&mut Energy, &mut Heat, &ShipStats)>,
) {
    let dt = dt.0;
    let sim = sim.map(|s| *s).unwrap_or_default();
    for (mut energy, mut heat, stats) in &mut q {
        // Energy: capacity + regen track the live reactor output; recharge toward max.
        energy.max = (stats.power_supply * sim.energy_capacity_secs).max(0.0);
        energy.regen = stats.power_supply.max(0.0);
        energy.current = (energy.current + energy.regen * dt).clamp(0.0, energy.max);
        // Heat: cool toward 0, clamped into [0, max].
        heat.max = sim.heat_capacity.max(f32::MIN_POSITIVE);
        heat.dissipation = sim.heat_dissipation.max(0.0);
        heat.current = (heat.current - heat.dissipation * dt).clamp(0.0, heat.max);
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
}
