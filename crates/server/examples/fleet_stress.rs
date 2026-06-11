//! R56 — headless FLEET STRESS benchmark (+ T022/T023: the TR-017/TR-018 AI-cost bench).
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
//! # AI mode (00008-ship-ai T022/T023, TR-017/TR-018)
//!
//! `--ai` wires the REAL brain in: inserts `ScenarioActive` (mirroring `ServerApp::spawn_scenario`,
//! plus the `RefinedResources`/`MiningTuning` resources the gated scenario systems require), attaches
//! `AiBrain` + `AiStableId` + `AoiTier` to every fleet ship, groups each side into wedge squads
//! (`--squad-size`, default 8) with `Engage` orders against the opposing line, and spawns ONE
//! authoritative `PlayerShip`-marked, non-firing fitted ship at the engagement line (TR-018's
//! player-local protocol). Without `--ai` NONE of that happens — the no-AI path is byte-identical to
//! the original R57 bench, so the baseline cannot be contaminated (TR-018).
//!
//! **Bucket attribution (the documented T022 choice)**: per-bucket WALL-TIME inside one tick would
//! require instrumenting sim systems, which the bench must not do. Instead the AI-attributable TIME
//! is the PAIRED-RUN DELTA (`--gate`: ai mean − baseline mean; by TR-018's paired design that delta
//! is all-AI-attributable), and the per-bucket signal is reported as deterministic WORK COUNTERS read
//! from the world after each tick (outside the timer): brain thinks/tick, squad thinks/tick, per-tier
//! ship counts, gliding-squad count, rethink-queue depth, and the STF-001 off-screen promoted-battle
//! count (promoted squads farther than `aoi_radius_mid` from the player — reported separately; in v1
//! their cost still sits inside the paired delta, which makes the gate CONSERVATIVE, never lenient).
//! `ScenarioActive` also enables the scenario-gated non-AI systems (mining/turret/voxelize/
//! structure-ram); they are entity-empty no-ops here but live inside the delta (also conservative).
//!
//! **Pre-T025 combat-stub caveat (documented)**: v1 `ai_execute_system` emits ZERO intent (including
//! fire flags) for combat behaviors, so Active/Mid-tier brain ships near the player cease autocannon
//! fire under `--ai`. To keep the FIGHT paired with the baseline, the bench re-asserts
//! `fire_primary` each tick OUTSIDE the timer (a bench artifice like `--pin`'s re-anchor): that keeps
//! every Dormant-tier ship (the vast majority at N = 2000) firing exactly like the baseline —
//! `ai_execute_system` skips Dormant ships — while the ~`aoi_radius_mid` bubble around the player
//! stays silent until T025/T026 wire real combat intents. The residual divergence is reported in the
//! JSON `note` field.
//!
//! New flags: `--ai` (sweep with AI), `--gate` (paired baseline+AI run at one N, JSON report,
//! non-zero exit on a >30% mean-overhead breach or an AI p99 above baseline p99 + 33.3 ms — the T024 gate command),
//! `--calm` (all one faction, no firing → event-driven idle-savings case), `--squad-sweep`
//! (fixed N at squad sizes 4/8/16 — decision cost tracks squad count), `--squad-size <n>`,
//! `--report <path.json>` (write the machine-readable report; it is always also printed to stdout
//! on a line marked `REPORT_JSON`).
//!
//! Caveats it reports: ONE shard/core (sectoring multiplies the total); attrition during the timed
//! window (dying ships taper the load → the figure is "combatants engaged at start", see `alive_end`);
//! without `--ai`, ships skip every AI path, so that mode is combat-sim cost, not AI cost.

use std::collections::HashMap;
use std::time::Instant;

use bevy_ecs::prelude::{Entity, With};
use glam::Vec2;
use server::ServerApp;
use sim::ai::{
    spawn_squad, AiBrain, AiIdAllocator, AiTuning, AoiTier, FormationDef, GlideState, PlayerShip,
    RethinkQueue, Squad, SquadOrder, Tier,
};
use sim::components::{AngularVelocity, Faction, Heading, Position, Ship, Velocity};
use sim::fitting::{CellShape, FitLayout, GridCell, Hull, HullCatalog, HULL_FIGHTER};
use sim::{CurrentTick, MiningTuning, RefinedResources, ScenarioActive, ShipIntent};

const TICK_MS_30HZ: f64 = 1000.0 / 30.0; // 33.33 ms
const TICK_MS_60HZ: f64 = 1000.0 / 60.0; // 16.67 ms

/// TR-017 hard gate: AI overhead must stay ≤ 30.0% of the no-AI baseline mean tick.
const GATE_THRESHOLD_PCT: f64 = 30.0;
/// TR-018 pinned protocol N (used by --gate/--calm/--squad-sweep when --ships is absent).
const PINNED_N: u32 = 2000;

// Two facing lines at close range: Red at -GAP facing +X, Blue at +GAP facing -X, paired by row so
// each shot crosses into an enemy (maximal weapon-fire + collision + CARVE load → real attrition).
const GAP: f32 = 7.0;
const SPACING: f32 = 3.0;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let warmup = arg_val(&args, "--warmup").unwrap_or(30);
    let ticks = arg_val(&args, "--ticks").unwrap_or(120).max(1);
    let densify = arg_val(&args, "--densify").unwrap_or(1).max(1) as u16;
    // R57 — pin the formation (re-anchor each ship's pose every tick) so recoil/knockback/spin can't
    // scatter the head-on grind → a sustained, reproducible WORST-CASE engagement. `--no-pin` = free drift.
    let pin = !args.iter().any(|a| a == "--no-pin");
    // R59 — `--shaped` rewrites every STRUCTURAL cell to a triangle (`HalfNE`) so ~90% of the hull runs
    // the sub-shape polygon hitbox/mass path → isolates the polygon-vs-circle per-cell cost vs all-`Full`.
    let shaped = args.iter().any(|a| a == "--shaped");
    // T022/T023 — the AI bench modes.
    let ai_on = args.iter().any(|a| a == "--ai");
    let gate = args.iter().any(|a| a == "--gate");
    let calm = args.iter().any(|a| a == "--calm");
    let squad_sweep = args.iter().any(|a| a == "--squad-sweep");
    let squad_size = arg_val(&args, "--squad-size").unwrap_or(8).max(1);
    let report_path = arg_str(&args, "--report");

    let cfg = BenchCfg {
        densify,
        warmup,
        ticks,
        pin,
        shaped,
    };

    // The single-N modes use the TR-018 pinned N = 2000 unless --ships overrides it.
    let single_n = arg_list(&args, "--ships")
        .and_then(|v| v.first().copied())
        .unwrap_or(PINNED_N);

    if gate {
        run_gate(single_n, cfg, squad_size, report_path.as_deref());
        return; // (run_gate exits non-zero itself on a breach)
    }
    if squad_sweep {
        run_squad_sweep(single_n, cfg, report_path.as_deref());
        return;
    }
    if calm {
        run_calm(single_n, cfg, squad_size, report_path.as_deref());
        return;
    }

    // Sweep mode — the original R57 bench, optionally with the AI substrate wired in (--ai).
    let sweep = arg_list(&args, "--ships").unwrap_or_else(|| vec![25, 50, 100, 200, 400, 800]);
    let ai_mode = ai_on.then_some(AiMode {
        calm: false,
        squad_size,
    });

    println!("=== Dark Silence fleet stress (R57) ===");
    println!(
        "densify k={densify} (cells/ship ×{kk})  warmup={warmup} ticks  timed={ticks} ticks/run  pin={pin}  shaped={shaped}  ai={ai_on}",
        kk = densify as u32 * densify as u32
    );
    println!(
        "each ship: 2 autocannons, firing flat-out (no energy/heat throttle headless → worst case)"
    );
    println!("budget: 30 Hz = {TICK_MS_30HZ:.2} ms/tick   60 Hz = {TICK_MS_60HZ:.2} ms/tick");
    println!();
    if ai_on {
        println!(
            "{:>6} {:>9} {:>10} {:>8} {:>8} {:>8} {:>8} {:>8} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "ships",
            "alive_end",
            "carved",
            "mean_ms",
            "p50_ms",
            "p99_ms",
            "thnk/t",
            "sqth/t",
            "act",
            "mid",
            "dorm",
            "glide",
            "offscr"
        );
    } else {
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
    }

    let mut max_30 = 0u32;
    let mut max_60 = 0u32;
    let mut reports: Vec<String> = Vec::new();
    for &n in &sweep {
        let r = run_one(n, cfg, ai_mode);
        if r.mean_ms <= TICK_MS_30HZ {
            max_30 = max_30.max(n);
        }
        if r.mean_ms <= TICK_MS_60HZ {
            max_60 = max_60.max(n);
        }
        if let Some(b) = &r.buckets {
            println!(
                "{:>6} {:>9} {:>10} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>6.0} {:>6.0} {:>6.0} {:>6.1} {:>6.1}",
                n,
                r.alive_end,
                r.cells_carved,
                r.mean_ms,
                r.p50_ms,
                r.p99_ms,
                b.thinks_per_tick,
                b.squad_thinks_per_tick,
                b.tier_active,
                b.tier_mid,
                b.tier_dormant,
                b.gliding_squads,
                b.offscreen_battles
            );
            let json = report_json(n, cfg, ai_mode, None, Some(&r));
            println!("REPORT_JSON {json}");
            reports.push(json);
        } else {
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
    if ai_on {
        println!("\n{AI_NOTE}");
    }
    write_reports(report_path.as_deref(), &reports);
}

// ---------------------------------------------------------------------------
// T023 — the paired TR-017 gate, calm-fleet, and squad-size-sweep cases
// ---------------------------------------------------------------------------

/// `--gate` (the T024 gate command): baseline then `--ai` at the same N in ONE invocation, the
/// machine-readable report, and a NON-ZERO exit on a TR-017 breach (mean overhead > 30.0% of the
/// baseline, or AI p99 over the 33.3 ms budget).
fn run_gate(n: u32, cfg: BenchCfg, squad_size: u32, report_path: Option<&str>) {
    let ai_mode = AiMode {
        calm: false,
        squad_size,
    };
    println!("=== TR-017/TR-018 paired AI-cost gate (N={n}, pinned R57 config) ===");
    println!(
        "protocol: release, one shard, pin={}, warmup={} + measured={} ticks, squad_size={squad_size}, one PlayerShip at the line",
        cfg.pin, cfg.warmup, cfg.ticks
    );
    let base = run_one(n, cfg, None);
    println!(
        "baseline (no AI):  mean {:>8.3} ms   p50 {:>8.3} ms   p99 {:>8.3} ms   carved {}",
        base.mean_ms, base.p50_ms, base.p99_ms, base.cells_carved
    );
    let ai = run_one(n, cfg, Some(ai_mode));
    println!(
        "with --ai:         mean {:>8.3} ms   p50 {:>8.3} ms   p99 {:>8.3} ms   carved {}",
        ai.mean_ms, ai.p50_ms, ai.p99_ms, ai.cells_carved
    );

    let delta = ai.mean_ms - base.mean_ms;
    let overhead_pct = delta / base.mean_ms * 100.0;
    // TR-017 p99 rule (anti-burst intent): AI may add at most ONE tick budget of tail
    // latency over the paired baseline — `ai_p99 <= baseline_p99 + 33.3 ms`. With a clean
    // baseline this degrades to ≈ the absolute budget; with a spiky baseline (the
    // pre-existing mass-carve spikes, see R56/R57 — p99 ~70–90 ms at the pinned N=2000,
    // which made a literal absolute budget unsatisfiable by any AI implementation) it
    // grants exactly one tick of margin, which also absorbs the ±25% run-to-run p99
    // sampling noise of a 120-tick window. (Bench-measured spec amendment 2026-06-10,
    // additive form per playtest review; preserves "promotion/scan bursts can't hide in
    // the mean".)
    let p99_ok = ai.p99_ms <= base.p99_ms + TICK_MS_30HZ;
    let mean_ok = overhead_pct <= GATE_THRESHOLD_PCT;
    let pass = mean_ok && p99_ok;
    println!(
        "AI-attributable delta: {delta:+.3} ms/tick ({overhead_pct:+.2}% of baseline mean; threshold {GATE_THRESHOLD_PCT:.1}%)"
    );
    println!(
        "AI p99: {:.3} ms vs baseline p99 {:.3} ms + {TICK_MS_30HZ:.2} ms budget → {}",
        ai.p99_ms,
        base.p99_ms,
        if p99_ok { "OK" } else { "OVER" }
    );
    if let Some(b) = &ai.buckets {
        print_buckets(b);
    }
    println!("GATE: {}", if pass { "PASS" } else { "FAIL" });

    let json = report_json(n, cfg, Some(ai_mode), Some(&base), Some(&ai));
    println!("REPORT_JSON {json}");
    write_reports(report_path, std::slice::from_ref(&json));
    if !pass {
        std::process::exit(2);
    }
}

/// `--calm` (TR-018 calm-fleet case): the same spawn but ALL ONE FACTION and nobody firing —
/// no hostiles, no events → reports the near-zero think work (event-driven idle savings).
fn run_calm(n: u32, cfg: BenchCfg, squad_size: u32, report_path: Option<&str>) {
    let ai_mode = AiMode {
        calm: true,
        squad_size,
    };
    println!("=== TR-018 calm-fleet case (N={n}, all one faction, no firing) ===");
    let r = run_one(n, cfg, Some(ai_mode));
    let b = r.buckets.as_ref().expect("--calm always runs with AI");
    println!(
        "mean {:.3} ms   p50 {:.3} ms   p99 {:.3} ms   carved {} (expect 0)",
        r.mean_ms, r.p50_ms, r.p99_ms, r.cells_carved
    );
    print_buckets(b);
    println!(
        "calm thinks/tick = {:.2} for {} ships: no hostiles → zero EVENT thinks; the residue is the\n\
         phase-bucketed fallback cadence (Dormant ships re-think every ~{} ticks), the TR-018\n\
         event-driven idle-savings signal. Squads with no hostiles settle Dormant and cheap-glide\n\
         (gliding_squads above).",
        b.thinks_per_tick,
        n,
        90 // AiTuning::default().think_ticks_dormant — pinned default, printed for context
    );
    let json = report_json(n, cfg, Some(ai_mode), None, Some(&r));
    println!("REPORT_JSON {json}");
    write_reports(report_path, std::slice::from_ref(&json));
}

/// `--squad-sweep` (TR-018 fixed-N squad-size sweep): same N at squad sizes 4/8/16 — shows squad
/// DECISION cost tracking squad count (squad thinks/tick falls as squads grow) while per-member
/// execution stays ~constant (thinks/tick is member-count-driven, ~unchanged across rows).
fn run_squad_sweep(n: u32, cfg: BenchCfg, report_path: Option<&str>) {
    println!("=== TR-018 squad-size sweep (fixed N={n}) ===");
    println!(
        "{:>10} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "squad_size", "squads", "sqth/t", "thnk/t", "mean_ms", "p50_ms", "p99_ms"
    );
    let mut reports: Vec<String> = Vec::new();
    for size in [4u32, 8, 16] {
        let ai_mode = AiMode {
            calm: false,
            squad_size: size,
        };
        let r = run_one(n, cfg, Some(ai_mode));
        let b = r.buckets.as_ref().expect("squad sweep always runs with AI");
        println!(
            "{:>10} {:>8.0} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
            size,
            b.squad_count,
            b.squad_thinks_per_tick,
            b.thinks_per_tick,
            r.mean_ms,
            r.p50_ms,
            r.p99_ms
        );
        let json = report_json(n, cfg, Some(ai_mode), None, Some(&r));
        reports.push(json);
    }
    println!(
        "decision cost is O(squads): squad thinks/tick scales with squad COUNT (halving as size\n\
         doubles), while member thinks/tick stays ~constant — TR-009's cost shape, measured."
    );
    for json in &reports {
        println!("REPORT_JSON {json}");
    }
    write_reports(report_path, &reports);
}

// ---------------------------------------------------------------------------
// The measured run
// ---------------------------------------------------------------------------

/// Shared sweep-independent bench knobs (one struct so `run_one` stays within arity lints).
#[derive(Clone, Copy)]
struct BenchCfg {
    densify: u16,
    warmup: u32,
    ticks: u32,
    pin: bool,
    shaped: bool,
}

/// T022 AI-mode knobs. `calm` = all one faction + no firing (the TR-018 idle case).
#[derive(Clone, Copy)]
struct AiMode {
    calm: bool,
    squad_size: u32,
}

/// Per-bucket WORK COUNTERS (means over the measured window) — the T022 bucket report.
/// Wall-time attribution is the paired-run delta (see the module docs); these counters are the
/// deterministic per-bucket signal: think + squad-brain + LOD/tier + glide + scheduler depth.
struct Buckets {
    thinks_per_tick: f64,
    squad_thinks_per_tick: f64,
    tier_active: f64,
    tier_mid: f64,
    tier_dormant: f64,
    gliding_squads: f64,
    offscreen_battles: f64,
    rethink_queue_len: f64,
    squad_count: f64,
}

#[derive(Default)]
struct BucketAcc {
    ticks: u64,
    thinks: u64,
    squad_thinks: u64,
    tier_active: u64,
    tier_mid: u64,
    tier_dormant: u64,
    gliding: u64,
    offscreen: u64,
    queue_len: u64,
    squads: u64,
}

impl BucketAcc {
    fn finalize(&self) -> Buckets {
        let t = self.ticks.max(1) as f64;
        Buckets {
            thinks_per_tick: self.thinks as f64 / t,
            squad_thinks_per_tick: self.squad_thinks as f64 / t,
            tier_active: self.tier_active as f64 / t,
            tier_mid: self.tier_mid as f64 / t,
            tier_dormant: self.tier_dormant as f64 / t,
            gliding_squads: self.gliding as f64 / t,
            offscreen_battles: self.offscreen as f64 / t,
            rethink_queue_len: self.queue_len as f64 / t,
            squad_count: self.squads as f64 / t,
        }
    }
}

struct RunResult {
    cells_per_ship: usize,
    alive_end: usize,
    cells_carved: usize,
    mean_ms: f64,
    p50_ms: f64,
    p99_ms: f64,
    /// AI work counters — `Some` iff the run had `--ai` wired in.
    buckets: Option<Buckets>,
}

fn run_one(n: u32, cfg: BenchCfg, ai: Option<AiMode>) -> RunResult {
    let BenchCfg {
        densify,
        warmup,
        ticks,
        pin,
        shaped,
    } = cfg;
    let (mut server, _t) = ServerApp::loopback();

    // Optionally swap the fighter hull for a k×-densified and/or all-triangle (`--shaped`) copy before
    // spawning, to measure the finer-cell / polygon-hitbox cost. Default (k=1, !shaped) uses the seed hull.
    let orig = server
        .world()
        .resource::<HullCatalog>()
        .get(HULL_FIGHTER)
        .cloned()
        .expect("seed catalog has the fighter hull");
    let mut hull = if densify > 1 {
        densify_hull(&orig, densify)
    } else {
        orig.clone()
    };
    if shaped {
        shape_structural_cells(&mut hull);
    }
    let cells_per_ship = hull.cells.len();
    if densify > 1 || shaped {
        server
            .world_mut()
            .resource_mut::<HullCatalog>()
            .hulls
            .insert(HULL_FIGHTER, hull);
    }

    // Spawn the two facing lines (see GAP/SPACING). Record each ship's spawn pose so `--pin` can
    // re-anchor it every tick, and (for --ai) the per-line spawn order for squad grouping.
    // `--calm` spawns BOTH lines as Red (no hostiles) — positions/headings identical to combat runs.
    let calm = ai.is_some_and(|m| m.calm);
    let line_b_faction = if calm { Faction::Red } else { Faction::Blue };
    let per_side = n / 2;
    let mut poses: HashMap<Entity, (Vec2, f32)> = HashMap::new();
    let mut line_a: Vec<Entity> = Vec::with_capacity(per_side as usize);
    let mut line_b: Vec<Entity> = Vec::with_capacity(per_side as usize);
    for i in 0..per_side {
        let y = (i as f32 - per_side as f32 * 0.5) * SPACING;
        let (rp, bp) = (Vec2::new(-GAP, y), Vec2::new(GAP, y));
        let r = server.spawn_fitted_ship(rp, 0.0, Faction::Red);
        poses.insert(r, (rp, 0.0));
        line_a.push(r);
        let pi = std::f32::consts::PI;
        let b = server.spawn_fitted_ship(bp, pi, line_b_faction);
        poses.insert(b, (bp, pi));
        line_b.push(b);
    }
    // T022: ONLY under --ai — the no-AI path above is byte-identical to the original R57 bench
    // (no ScenarioActive, no AI components, no PlayerShip), so the baseline stays uncontaminated.
    if let Some(mode) = ai {
        setup_ai(&mut server, &line_a, &line_b, mode, &mut poses, per_side);
    }

    for _ in 0..warmup {
        server.tick();
        if pin {
            reanchor(&mut server, &poses);
        }
        if ai.is_some() && !calm {
            reassert_fire(&mut server, &line_a, &line_b);
        }
    }
    let start_cells = total_cells(&mut server);
    let mut times = Vec::with_capacity(ticks as usize);
    let mut acc = BucketAcc::default();
    let mut prev_thinks = ai.map(|_| total_thinks(&mut server));
    for _ in 0..ticks {
        let t0 = Instant::now();
        server.tick();
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        if pin {
            reanchor(&mut server, &poses); // outside the timer — a bench artifice, not server work
        }
        if let Some(prev) = prev_thinks.as_mut() {
            if !calm {
                reassert_fire(&mut server, &line_a, &line_b); // also outside the timer (module docs)
            }
            sample_ai_tick(&mut server, &mut acc, prev);
        }
    }
    let end_cells = total_cells(&mut server);

    // Alive combatants at the end (a carved-to-death ship loses its `Ship` marker → becomes a wreck).
    // Under --ai this count includes the one non-firing PlayerShip.
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
        buckets: ai.map(|_| acc.finalize()),
    }
}

// ---------------------------------------------------------------------------
// T022 — AI wiring (only ever called under --ai)
// ---------------------------------------------------------------------------

/// Wire the AI substrate into the spawned fleet (T022):
/// 1. Insert `ScenarioActive` exactly as `ServerApp::spawn_scenario` does, plus the
///    `RefinedResources` + `MiningTuning` resources the `ScenarioActive`-gated scenario systems
///    require (the Sandbox pattern in `crates/server/src/scenario.rs` — they idle with no
///    transports here).
/// 2. Attach `AiBrain` (phase-bucketed) + `AiStableId` + default `AoiTier` to every fleet ship.
/// 3. Group each line into wedge squads of `squad_size` via `spawn_squad`; combat squads get
///    `Engage` orders against the facing ship of the opposing line (the OBJ4 engage source until
///    perception lands — execution is a v1 stub, see the module docs); calm squads `Hold`.
/// 4. Spawn ONE authoritative `PlayerShip`-marked, NON-firing fitted ship just past the end of the
///    engagement line (x = 0 — on the line, out of the row crossfire so it doesn't absorb shots and
///    skew the paired fight), pinned like everyone else. Its proximity drives the Active/Mid/Dormant
///    tier mix TR-018's player-local protocol requires.
fn setup_ai(
    server: &mut ServerApp,
    line_a: &[Entity],
    line_b: &[Entity],
    mode: AiMode,
    poses: &mut HashMap<Entity, (Vec2, f32)>,
    per_side: u32,
) {
    {
        let w = server.world_mut();
        w.insert_resource(ScenarioActive);
        w.insert_resource(RefinedResources::default());
        w.insert_resource(MiningTuning::default());

        let bucket_count = w.resource::<AiTuning>().fallback_bucket_count;
        for &e in line_a.iter().chain(line_b.iter()) {
            let id = w.resource_mut::<AiIdAllocator>().allocate();
            w.entity_mut(e)
                .insert((AiBrain::new(id, bucket_count), id, AoiTier::default()));
            if mode.calm {
                // Calm fleet: nobody fires (no hostiles, no combat load at all).
                if let Some(mut intent) = w.get_mut::<ShipIntent>(e) {
                    intent.fire_primary = false;
                }
            }
        }

        // Squads of `squad_size` over consecutive rows; each combat squad engages the opposing
        // ship facing its first member (same row → guaranteed hostile across the line).
        let k = mode.squad_size.max(1) as usize;
        for (lane, foes) in [(line_a, line_b), (line_b, line_a)] {
            for (i, chunk) in lane.chunks(k).enumerate() {
                let order = if mode.calm {
                    SquadOrder::Hold
                } else {
                    match foes.get(i * k).or_else(|| foes.first()) {
                        Some(&target) => SquadOrder::Engage(target),
                        None => SquadOrder::Hold,
                    }
                };
                spawn_squad(w, chunk, FormationDef::wedge(chunk.len(), SPACING), order);
            }
        }
    }

    // The one authoritative player ship at the engagement line (TR-018).
    let y_top = (per_side as f32 - 1.0 - per_side as f32 * 0.5) * SPACING;
    let player_pos = Vec2::new(0.0, y_top + 2.0 * SPACING);
    let player = server.spawn_fitted_ship(player_pos, 0.0, Faction::Red);
    let w = server.world_mut();
    w.entity_mut(player).insert(PlayerShip);
    if let Some(mut intent) = w.get_mut::<ShipIntent>(player) {
        intent.fire_primary = false; // The player observes; it must not add weapon load.
    }
    poses.insert(player, (player_pos, 0.0));
}

/// Bench artifice (outside the timer, like `--pin`'s re-anchor): re-assert `fire_primary` on every
/// fleet ship so the --ai fight stays paired with the baseline. Effective for Dormant-tier ships
/// (`ai_execute_system` skips them, so the intent survives into the next tick's weapon fire) and for
/// ships whose intent was zeroed by a transient glide collapse; Active/Mid brain ships are
/// re-overwritten in-tick by the v1 combat-stub execute and stay silent (see the module docs).
fn reassert_fire(server: &mut ServerApp, line_a: &[Entity], line_b: &[Entity]) {
    let w = server.world_mut();
    for &e in line_a.iter().chain(line_b.iter()) {
        if let Some(mut intent) = w.get_mut::<ShipIntent>(e) {
            if !intent.fire_primary {
                intent.fire_primary = true;
            }
        }
    }
}

/// Sum of every brain's lifetime completed-think counter (the T021 counter the think bucket reads).
fn total_thinks(server: &mut ServerApp) -> u64 {
    let w = server.world_mut();
    let mut q = w.query::<&AiBrain>();
    q.iter(w).map(|b| b.thinks_total).sum()
}

/// Sample the per-tick AI work counters AFTER a tick (outside the timer — cheap world reads only):
/// think delta, squad thinks (squads whose `last_think_tick` is this tick), per-tier ship counts,
/// gliding squads, rethink-queue depth, and the STF-001 off-screen promoted-battle count (squads
/// promoted out of Dormant while farther than `aoi_radius_mid` from the player).
fn sample_ai_tick(server: &mut ServerApp, acc: &mut BucketAcc, prev_thinks: &mut u64) {
    let now = server.world().resource::<CurrentTick>().0;
    let mid_radius = server.world().resource::<AiTuning>().aoi_radius_mid;
    let queue_len = server.world().resource::<RethinkQueue>().len() as u64;
    let w = server.world_mut();

    let player = {
        let mut q = w.query_filtered::<&Position, With<PlayerShip>>();
        q.iter(w).next().map(|p| p.0)
    };
    let thinks_now: u64 = {
        let mut q = w.query::<&AiBrain>();
        q.iter(w).map(|b| b.thinks_total).sum()
    };
    acc.thinks += thinks_now.saturating_sub(*prev_thinks);
    *prev_thinks = thinks_now;

    {
        let mut q = w.query::<(&Squad, &AoiTier, &Position, Option<&GlideState>)>();
        for (squad, aoi, pos, glide) in q.iter(w) {
            acc.squads += 1;
            if squad.last_think_tick == now {
                acc.squad_thinks += 1;
            }
            if glide.is_some() {
                acc.gliding += 1;
            }
            if aoi.tier != Tier::Dormant
                && player.is_some_and(|pp| (pos.0 - pp).length() > mid_radius)
            {
                acc.offscreen += 1;
            }
        }
    }
    {
        let mut q = w.query_filtered::<&AoiTier, With<Ship>>();
        for aoi in q.iter(w) {
            match aoi.tier {
                Tier::Active => acc.tier_active += 1,
                Tier::Mid => acc.tier_mid += 1,
                Tier::Dormant => acc.tier_dormant += 1,
            }
        }
    }
    acc.queue_len += queue_len;
    acc.ticks += 1;
}

fn print_buckets(b: &Buckets) {
    println!(
        "AI work/tick (means): thinks {:.2}   squad thinks {:.2} (of {:.0} squads)   rethink-queue {:.1}\n\
         ship tiers A/M/D: {:.0}/{:.0}/{:.0}   gliding squads {:.1}   off-screen promoted battles {:.1} (STF-001, reported separately)",
        b.thinks_per_tick,
        b.squad_thinks_per_tick,
        b.squad_count,
        b.rethink_queue_len,
        b.tier_active,
        b.tier_mid,
        b.tier_dormant,
        b.gliding_squads,
        b.offscreen_battles
    );
}

// ---------------------------------------------------------------------------
// T023 — machine-readable report (hand-rolled JSON; server has no serde_json dep)
// ---------------------------------------------------------------------------

/// The documented bucket-attribution + combat-stub note, embedded in every report (no quotes —
/// it must stay a valid hand-rolled JSON string).
const AI_NOTE: &str = "bucket attribution: AI-attributable TIME is the paired-run delta \
(ai mean - baseline mean; all-AI-attributable per TR-018) because per-bucket wall-time would \
require instrumenting sim systems; per-bucket signal = deterministic work counters sampled \
post-tick outside the timer. ScenarioActive also enables entity-empty scenario systems inside the \
delta (conservative). Off-screen promoted battles are reported separately but remain inside the \
delta in v1 (also conservative). Pre-T025 combat stub: Active/Mid brain ships emit zero intent \
(incl. fire flags), so ships within aoi_radius_mid of the player cease fire under --ai; the bench \
re-asserts fire_primary post-tick so Dormant-tier ships keep the baseline weapon load.";

fn stats_json(r: &RunResult) -> String {
    format!(
        "{{\"mean_ms\":{:.4},\"p50_ms\":{:.4},\"p99_ms\":{:.4}}}",
        r.mean_ms, r.p50_ms, r.p99_ms
    )
}

fn buckets_json(b: &Buckets) -> String {
    format!(
        "{{\"thinks_per_tick\":{:.2},\"squad_thinks_per_tick\":{:.2},\
         \"tier_counts\":{{\"active\":{:.1},\"mid\":{:.1},\"dormant\":{:.1}}},\
         \"gliding_squads\":{:.1},\"offscreen_battles\":{:.1},\"rethink_queue_len\":{:.1},\
         \"squad_count\":{:.1}}}",
        b.thinks_per_tick,
        b.squad_thinks_per_tick,
        b.tier_active,
        b.tier_mid,
        b.tier_dormant,
        b.gliding_squads,
        b.offscreen_battles,
        b.rethink_queue_len,
        b.squad_count
    )
}

/// One report object (T023). `baseline` + `ai_run` both present = a paired `--gate` run with the
/// overhead/gate fields populated; otherwise the missing side and the derived fields are `null`.
fn report_json(
    n: u32,
    cfg: BenchCfg,
    ai_mode: Option<AiMode>,
    baseline: Option<&RunResult>,
    ai_run: Option<&RunResult>,
) -> String {
    let config = format!(
        "{{\"n\":{n},\"ticks\":{},\"warmup\":{},\"pinned\":{},\"densify\":{},\"shaped\":{},\
         \"ai\":{},\"calm\":{},\"squad_size\":{}}}",
        cfg.ticks,
        cfg.warmup,
        cfg.pin,
        cfg.densify,
        cfg.shaped,
        ai_mode.is_some(),
        ai_mode.is_some_and(|m| m.calm),
        ai_mode.map_or(0, |m| m.squad_size),
    );
    let base_s = baseline.map_or("null".to_string(), stats_json);
    let ai_s = ai_run.map_or("null".to_string(), stats_json);
    let (delta_s, overhead_s, pass_s) = match (baseline, ai_run) {
        (Some(b), Some(a)) => {
            let delta = a.mean_ms - b.mean_ms;
            let overhead = delta / b.mean_ms * 100.0;
            // p99 rule: AI may add at most one tick budget of tail latency over the baseline.
            let pass = overhead <= GATE_THRESHOLD_PCT && a.p99_ms <= b.p99_ms + TICK_MS_30HZ;
            (
                format!("{delta:.4}"),
                format!("{overhead:.2}"),
                format!("{pass}"),
            )
        }
        _ => ("null".into(), "null".into(), "null".into()),
    };
    let p99_s = ai_run.map_or("null".to_string(), |a| format!("{:.4}", a.p99_ms));
    let buckets_s = ai_run
        .and_then(|a| a.buckets.as_ref())
        .map_or("null".to_string(), buckets_json);
    let carved = ai_run.or(baseline).map_or(0, |r| r.cells_carved);
    format!(
        "{{\"config\":{config},\"baseline\":{base_s},\"ai\":{ai_s},\"delta_ms\":{delta_s},\
         \"overhead_pct\":{overhead_s},\"p99_ms\":{p99_s},\
         \"gate\":{{\"threshold_pct\":{GATE_THRESHOLD_PCT:.1},\"p99_budget_ms\":{TICK_MS_30HZ:.2},\"p99_rule\":\"ai_p99 <= baseline_p99 + 33.3ms\",\"pass\":{pass_s}}},\
         \"buckets\":{buckets_s},\"carved\":{carved},\"note\":\"{AI_NOTE}\"}}"
    )
}

/// `--report <path>`: persist the printed REPORT_JSON content — a single object for one report,
/// a JSON array for multi-run modes. No-op without the flag or with nothing to write.
fn write_reports(path: Option<&str>, reports: &[String]) {
    let Some(path) = path else { return };
    if reports.is_empty() {
        return;
    }
    let content = if reports.len() == 1 {
        format!("{}\n", reports[0])
    } else {
        format!("[{}]\n", reports.join(","))
    };
    match std::fs::write(path, content) {
        Ok(()) => println!("report written to {path}"),
        Err(e) => eprintln!("failed to write report to {path}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Shared fixture helpers (unchanged from R57)
// ---------------------------------------------------------------------------

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

/// R59 — rewrite every STRUCTURAL `Full` cell to a triangle (`HalfNE`) so ~90% of the hull runs the
/// sub-shape polygon hitbox/mass path. Module cells stay `Full` (they carry the slots). Isolates the
/// polygon-vs-circle per-cell cost when compared against the default all-`Full` run.
fn shape_structural_cells(hull: &mut Hull) {
    for c in &mut hull.cells {
        if c.structural && c.shape == CellShape::Full {
            c.shape = CellShape::HalfNE;
        }
    }
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

fn arg_str(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn arg_list(args: &[String], flag: &str) -> Option<Vec<u32>> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
}
