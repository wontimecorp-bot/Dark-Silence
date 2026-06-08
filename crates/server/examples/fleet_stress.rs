//! R56 — headless FLEET STRESS benchmark.
//!
//! Spawns N actively-fighting ships (half Red, half Blue, in two facing lines firing autocannons — the
//! REAL weapon → projectile → collision → carve hot path), steps the authoritative `ServerApp` tick, and
//! times each tick to find the ship count that holds the real-time budget (30 Hz = 33.3 ms, 60 Hz =
//! 16.7 ms per tick). `--densify k` subdivides each hull cell k× (k² cells per ship) to measure the
//! SIM-level finer-cell cost (render-only slope/triangle tiles cost ZERO here — the sim is unchanged).
//!
//! RUN RELEASE (debug is ~10× slower and misleading):
//!   cargo run --release -p server --example fleet_stress
//!   cargo run --release -p server --example fleet_stress -- --ships 50,100,200,400 --densify 2
//!
//! Caveats it reports: ONE shard/core (sectoring multiplies the total); attrition during the timed
//! window (dying ships taper the load → the figure is "combatants engaged at start", see `alive_end`);
//! Ships skip the `Target` AI path, so this is combat-sim cost, not AI cost.

use std::collections::HashMap;
use std::time::Instant;

use bevy_ecs::prelude::{Entity, With};
use glam::Vec2;
use server::ServerApp;
use sim::components::{AngularVelocity, Faction, Heading, Position, Ship, Velocity};
use sim::fitting::{FitLayout, GridCell, Hull, HullCatalog, HULL_FIGHTER};

const TICK_MS_30HZ: f64 = 1000.0 / 30.0; // 33.33 ms
const TICK_MS_60HZ: f64 = 1000.0 / 60.0; // 16.67 ms

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sweep = arg_list(&args, "--ships").unwrap_or_else(|| vec![25, 50, 100, 200, 400, 800]);
    let warmup = arg_val(&args, "--warmup").unwrap_or(30);
    let ticks = arg_val(&args, "--ticks").unwrap_or(120).max(1);
    let densify = arg_val(&args, "--densify").unwrap_or(1).max(1) as u16;
    // R57 — pin the formation (re-anchor each ship's pose every tick) so recoil/knockback/spin can't
    // scatter the head-on grind → a sustained, reproducible WORST-CASE engagement. `--no-pin` = free drift.
    let pin = !args.iter().any(|a| a == "--no-pin");

    println!("=== Dark Silence fleet stress (R57) ===");
    println!(
        "densify k={densify} (cells/ship ×{kk})  warmup={warmup} ticks  timed={ticks} ticks/run  pin={pin}",
        kk = densify as u32 * densify as u32
    );
    println!(
        "each ship: 2 autocannons, firing flat-out (no energy/heat throttle headless → worst case)"
    );
    println!("budget: 30 Hz = {TICK_MS_30HZ:.2} ms/tick   60 Hz = {TICK_MS_60HZ:.2} ms/tick");
    println!();
    println!(
        "{:>6} {:>10} {:>9} {:>10} {:>8} {:>8} {:>8}  {:>12} {:>12}",
        "ships",
        "cells/shp",
        "alive_end",
        "carved",
        "mean_ms",
        "p50_ms",
        "p99_ms",
        "vs 30Hz",
        "vs 60Hz"
    );

    let mut max_30 = 0u32;
    let mut max_60 = 0u32;
    for &n in &sweep {
        let r = run_one(n, densify, warmup, ticks, pin);
        if r.mean_ms <= TICK_MS_30HZ {
            max_30 = max_30.max(n);
        }
        if r.mean_ms <= TICK_MS_60HZ {
            max_60 = max_60.max(n);
        }
        println!(
            "{:>6} {:>10} {:>9} {:>10} {:>8.2} {:>8.2} {:>8.2}  {:>12} {:>12}",
            n,
            r.cells_per_ship,
            r.alive_end,
            r.cells_carved,
            r.mean_ms,
            r.p50_ms,
            r.p99_ms,
            budget_str(r.mean_ms, TICK_MS_30HZ),
            budget_str(r.mean_ms, TICK_MS_60HZ),
        );
    }
    println!();
    println!(
        "Largest N holding the MEAN-tick budget:  30 Hz → {max_30} ships   60 Hz → {max_60} ships"
    );
    println!(
        "`carved` = hull cells actually removed over the window (nonzero ⇒ a real, sustained carving fight).\n\
         Caveats: ONE shard/core (sectoring multiplies the total fleet); attrition during the window (see\n\
         alive_end) → 'combatants engaged at START'; pinned + flat-out 2-gun fire = a WORST-CASE combat\n\
         load (real ships throttle/spread/miss → sim handles more on the combat side; AI/network are the\n\
         real cap). Render-only slope/triangle tiles add ZERO sim cost → the k=1 row IS the with-triangles\n\
         ceiling; --densify shows the SIM-level finer-cell cost; --no-pin = free-drift comparison."
    );
}

struct RunResult {
    cells_per_ship: usize,
    alive_end: usize,
    cells_carved: usize,
    mean_ms: f64,
    p50_ms: f64,
    p99_ms: f64,
}

fn run_one(n: u32, densify: u16, warmup: u32, ticks: u32, pin: bool) -> RunResult {
    let (mut server, _t) = ServerApp::loopback();

    // Optionally swap the fighter hull for a k×-densified copy (k² cells) before spawning.
    let orig = server
        .world()
        .resource::<HullCatalog>()
        .get(HULL_FIGHTER)
        .cloned()
        .expect("seed catalog has the fighter hull");
    let cells_per_ship = if densify > 1 {
        let dense = densify_hull(&orig, densify);
        let len = dense.cells.len();
        server
            .world_mut()
            .resource_mut::<HullCatalog>()
            .hulls
            .insert(HULL_FIGHTER, dense);
        len
    } else {
        orig.cells.len()
    };

    // Two facing lines at close range: Red at -GAP facing +X, Blue at +GAP facing -X, paired by row so
    // each shot crosses into an enemy (maximal weapon-fire + collision + CARVE load → real attrition).
    // Record each ship's spawn pose so `--pin` can re-anchor it every tick.
    const GAP: f32 = 7.0;
    const SPACING: f32 = 3.0;
    let per_side = n / 2;
    let mut poses: HashMap<Entity, (Vec2, f32)> = HashMap::new();
    for i in 0..per_side {
        let y = (i as f32 - per_side as f32 * 0.5) * SPACING;
        let (rp, bp) = (Vec2::new(-GAP, y), Vec2::new(GAP, y));
        poses.insert(server.spawn_fitted_ship(rp, 0.0, Faction::Red), (rp, 0.0));
        let pi = std::f32::consts::PI;
        poses.insert(server.spawn_fitted_ship(bp, pi, Faction::Blue), (bp, pi));
    }

    for _ in 0..warmup {
        server.tick();
        if pin {
            reanchor(&mut server, &poses);
        }
    }
    let start_cells = total_cells(&mut server);
    let mut times = Vec::with_capacity(ticks as usize);
    for _ in 0..ticks {
        let t0 = Instant::now();
        server.tick();
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        if pin {
            reanchor(&mut server, &poses); // outside the timer — a bench artifice, not server work
        }
    }
    let end_cells = total_cells(&mut server);

    // Alive combatants at the end (a carved-to-death ship loses its `Ship` marker → becomes a wreck).
    let alive_end = {
        let w = server.world_mut();
        let mut q = w.query_filtered::<Entity, With<Ship>>();
        q.iter(w).count()
    };

    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let pct = |frac: f64| times[((times.len() as f64 - 1.0) * frac).round() as usize];
    RunResult {
        cells_per_ship,
        alive_end,
        cells_carved: start_cells.saturating_sub(end_cells),
        mean_ms: mean,
        p50_ms: pct(0.50),
        p99_ms: pct(0.99),
    }
}

/// Re-anchor every alive `Ship` to its spawn pose (position + heading) and zero its velocity + spin, so
/// firing recoil + projectile knockback + off-centre-hit spin can't scatter or rotate the formation.
fn reanchor(server: &mut ServerApp, poses: &HashMap<Entity, (Vec2, f32)>) {
    let w = server.world_mut();
    let mut q = w.query_filtered::<(
        Entity,
        &mut Position,
        &mut Heading,
        &mut Velocity,
        &mut AngularVelocity,
    ), With<Ship>>();
    for (e, mut p, mut h, mut v, mut a) in q.iter_mut(w) {
        if let Some(&(pos, head)) = poses.get(&e) {
            p.0 = pos;
            h.0 = head;
            v.0 = Vec2::ZERO;
            a.0 = 0.0;
        }
    }
}

/// Total live hull cells across every fitted body (ships + wrecks) — sampled before/after the timed
/// window so `start − end` PROVES how many cells were actually carved off (a real, sustained fight).
fn total_cells(server: &mut ServerApp) -> usize {
    let w = server.world_mut();
    let mut q = w.query::<&FitLayout>();
    q.iter(w).map(|fl| fl.cells.len()).sum()
}

/// Subdivide every `GridCell` into `k×k` sub-cells (grid_dims ×k, slot coords ×k) → `k²×` the cells. The
/// rest of the hull (budgets, sections, structural flags) is preserved so the fit still validates.
fn densify_hull(orig: &Hull, k: u16) -> Hull {
    let mut dense = orig.clone();
    let (cols, rows) = orig.grid_dims;
    dense.grid_dims = (cols * k, rows * k);
    dense.cells = orig
        .cells
        .iter()
        .flat_map(|c| {
            let (cc, rr) = c.coord;
            let cell = *c; // copy preserves section/structural/shape; only coord changes
            (0..k).flat_map(move |dr| {
                (0..k).map(move |dc| GridCell {
                    coord: (cc * k + dc, rr * k + dr),
                    ..cell
                })
            })
        })
        .collect();
    dense.slots = orig
        .slots
        .iter()
        .map(|s| {
            let mut s = *s;
            s.coord = (s.coord.0 * k, s.coord.1 * k);
            s
        })
        .collect();
    dense
}

fn budget_str(mean: f64, budget: f64) -> String {
    let r = mean / budget;
    if mean <= budget {
        format!("OK {r:.2}x")
    } else {
        format!("OVER {r:.2}x")
    }
}

fn arg_val(args: &[String], flag: &str) -> Option<u32> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn arg_list(args: &[String], flag: &str) -> Option<Vec<u32>> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
}
