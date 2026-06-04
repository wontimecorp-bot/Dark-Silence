//! Phase F — **trapezoid HUD bars** rendered as camera-anchored 3D meshes (Bevy UI `Node`s are
//! rectangles, so the trapezoid/ramp shapes the user asked for need real geometry).
//!
//! The client is single-camera top-down 3D ([`crate::camera::MainCamera`] looks straight down the
//! world `-Z` with up `+Y`, so its rotation is identity). We parent each bar's trapezoid segments
//! as **children of that camera** at fixed local offsets: a child at local `(x, y, -depth)` keeps a
//! fixed screen position + size regardless of where the camera flies or how far it zooms (the
//! offset is constant, and the perspective only depends on the constant local depth). Each segment
//! is one [`scene::build_trapezoid_mesh`] unit-trapezoid scaled to its `(width, height)`; the ramp
//! "look" is the per-segment height, set ONCE at spawn — only the per-segment COLOUR changes each
//! frame (lit/unlit by `current/max`), via each segment's own `unlit` [`StandardMaterial`].
//!
//! The Phase-F bars on this infra:
//! - **Afterburner** — a single short→tall ramp (its own pool; drains on Shift, recharges idle).
//! - **Heat** — a **double-ramp** (two short→tall ramps with a reset at the midpoint, so the 50%
//!   mark is an obvious landmark). This SUPERSEDES the Phase-E segmented Heat VU bar.
//! - **Shield / Armor / Hull** (F4) — three **vertical** bars (left side), each 10 horizontal
//!   trapezoid segments stacked vertically, fixed per-layer hue (cyan / amber / red), depleting
//!   top-down by `current/max`.
//!
//! All pools are read straight from the embedded server world (like [`crate::hud::update_energy_hud`]
//! and the dev panel).

use bevy::prelude::*;
use sim::components::{Afterburner, ArmorHp, Heat};
use sim::damage::{HullStructure, Shields};

use crate::camera::MainCamera;
use crate::hud::{grade, scale_rgb, seg_dim};
use crate::net::{LoopbackHost, NetClientState};
use crate::scene::build_trapezoid_mesh;

/// Local depth (units in front of the camera, toward the gameplay plane) the HUD bars sit at. The
/// gameplay plane is `camera.height` (≈45) units away, so the bars float well in front of every
/// ship → they never get occluded. Screen size scales with this (closer = bigger).
const HUD_DEPTH: f32 = 12.0;
/// Each trapezoid segment's top/bottom width ratio — `<1` gives the tapered "battery cell" look.
const SEG_TAPER: f32 = 0.72;
/// Fraction of a segment's column width the trapezoid fills (the rest is the inter-segment gap).
const SEG_FILL: f32 = 0.8;

/// Which Phase-F trapezoid bar a segment belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrapBar {
    /// Boost pool — a single short→tall ramp; want it FULL (drains while boosting).
    Afterburner,
    /// Heat — a double-ramp; want it EMPTY (fills as you fire; full = overheated).
    Heat,
    /// Shield pool — a vertical stack; want it FULL (cyan).
    Shield,
    /// Armor-HP pool — a vertical stack; want it FULL (amber).
    Armor,
    /// Hull structure — a vertical stack; want it FULL (red).
    Hull,
}

/// One trapezoid segment of a camera-anchored ramp bar. Holds its own material handle so its
/// colour can be set independently each frame (cloned from a shared prototype at spawn).
#[derive(Component)]
pub struct TrapSegment {
    pub bar: TrapBar,
    pub index: usize,
    pub count: usize,
    pub material: Handle<StandardMaterial>,
}

/// The geometry of a bar's segments.
enum Shape {
    /// Horizontal row whose segment HEIGHTS ramp short→tall along `+x`. `double` = the heat
    /// double-ramp (resets to `min_h` at the midpoint, an obvious 50% landmark).
    Ramp {
        double: bool,
        min_h: f32,
        max_h: f32,
    },
    /// Vertical stack of uniform horizontal trapezoid segments (each `width` wide), growing `+y`.
    Stack { width: f32 },
}

/// Static layout of one trapezoid bar (all in camera-local units at [`HUD_DEPTH`]).
struct BarLayout {
    bar: TrapBar,
    count: usize,
    /// Centre of the bar on the cross axis (`x` for [`Shape::Ramp`], the column `x` for `Stack`).
    x_center: f32,
    /// The bar's start on its main axis: the bottom `y` (segments grow `+y`/`+x` from here). For a
    /// `Ramp` it is the row's `y`; for a `Stack` it is the lowest segment's `y`.
    y_base: f32,
    /// Total span along the bar's MAIN axis (`x` for `Ramp`, `y` for `Stack`).
    extent: f32,
    shape: Shape,
}

/// The Phase-F bars. First-pass placement — TUNABLE; refine after playtest. The afterburner + heat
/// ramps sit centred near the bottom; the Shield/Armor/Hull stacks run up the left side (kept within
/// `x ≈ ±6` so they stay on-screen down to a 4:3 window).
const BARS: &[BarLayout] = &[
    BarLayout {
        bar: TrapBar::Afterburner,
        count: 16,
        x_center: 0.0,
        y_base: -3.2,
        extent: 5.5,
        shape: Shape::Ramp {
            double: false,
            min_h: 0.35,
            max_h: 1.0,
        },
    },
    BarLayout {
        bar: TrapBar::Heat,
        count: 24,
        x_center: 0.0,
        y_base: -4.4,
        extent: 6.0,
        shape: Shape::Ramp {
            double: true,
            min_h: 0.3,
            max_h: 0.95,
        },
    },
    BarLayout {
        bar: TrapBar::Shield,
        count: 10,
        x_center: -5.6,
        y_base: -2.5,
        extent: 5.0,
        shape: Shape::Stack { width: 0.5 },
    },
    BarLayout {
        bar: TrapBar::Armor,
        count: 10,
        x_center: -5.0,
        y_base: -2.5,
        extent: 5.0,
        shape: Shape::Stack { width: 0.5 },
    },
    BarLayout {
        bar: TrapBar::Hull,
        count: 10,
        x_center: -4.4,
        y_base: -2.5,
        extent: 5.0,
        shape: Shape::Stack { width: 0.5 },
    },
];

/// Lit-segment count for a fill fraction over `count` segments (rounded, clamped).
fn lit_count(frac: f32, count: usize) -> usize {
    ((frac.clamp(0.0, 1.0) * count as f32).round() as usize).min(count)
}

/// Height of segment `i` in a single short→tall ramp of `count` segments.
fn ramp_height(i: usize, count: usize, min_h: f32, max_h: f32) -> f32 {
    if count <= 1 {
        return max_h;
    }
    let t = i as f32 / (count - 1) as f32;
    min_h + (max_h - min_h) * t
}

/// Height of segment `i` in a **double** ramp: `count` split into two halves, each ramping
/// short→tall, so the second half RESETS to `min_h` at the midpoint (an obvious 50% landmark). An
/// odd `count` puts the extra segment in the first half.
fn double_ramp_height(i: usize, count: usize, min_h: f32, max_h: f32) -> f32 {
    let half = count / 2;
    if i < half {
        ramp_height(i, half, min_h, max_h)
    } else {
        ramp_height(i - half, count - half, min_h, max_h)
    }
}

/// The local `(x, y, width, height)` of segment `i` in a bar (before the `-HUD_DEPTH` z-offset). A
/// `Ramp` lays segments along `+x` with ramping heights; a `Stack` lays uniform segments along `+y`.
fn seg_placement(layout: &BarLayout, i: usize) -> (f32, f32, f32, f32) {
    let spacing = layout.extent / layout.count as f32;
    match layout.shape {
        Shape::Ramp {
            double,
            min_h,
            max_h,
        } => {
            let x_left = layout.x_center - layout.extent * 0.5;
            let x = x_left + (i as f32 + 0.5) * spacing;
            let h = if double {
                double_ramp_height(i, layout.count, min_h, max_h)
            } else {
                ramp_height(i, layout.count, min_h, max_h)
            };
            (x, layout.y_base, spacing * SEG_FILL, h)
        }
        Shape::Stack { width } => {
            let y = layout.y_base + i as f32 * spacing;
            (layout.x_center, y, width, spacing * SEG_FILL)
        }
    }
}

/// Startup (after [`crate::camera::setup_camera`]): spawn the Phase-F trapezoid bars as children of
/// the camera so they stay fixed on screen. One shared unit-trapezoid mesh, scaled per segment to
/// its `(width, height)`; each segment gets its OWN `unlit` material so `update_trapezoid_bars` can
/// colour it independently.
pub fn setup_trapezoid_bars(
    mut commands: Commands,
    cam_q: Query<Entity, With<MainCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(cam) = cam_q.single() else {
        return;
    };
    // ONE unit trapezoid (bottom width 1, height 1, tapered top): each segment's Transform scales
    // it to its column width and ramp height (the taper ratio is preserved under x-scale).
    let mesh = meshes.add(build_trapezoid_mesh(SEG_TAPER, 1.0, 1.0));

    commands.entity(cam).with_children(|parent| {
        for layout in BARS {
            for i in 0..layout.count {
                let (x, y, w, h) = seg_placement(layout, i);
                // Each segment starts dim (unlit); the update system lights it by fill.
                let material = materials.add(StandardMaterial {
                    base_color: seg_dim(),
                    unlit: true,
                    ..default()
                });
                parent.spawn((
                    TrapSegment {
                        bar: layout.bar,
                        index: i,
                        count: layout.count,
                        material: material.clone(),
                    },
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material),
                    Transform::from_xyz(x, y, -HUD_DEPTH).with_scale(Vec3::new(w, h, 1.0)),
                ));
            }
        }
    });
}

/// Fixed per-layer hue for the Shield/Armor/Hull vertical bars (lit segments). Distinct colours so a
/// glance reads WHICH layer; the FILL (lit count) reads how much remains. Cyan shield, amber armor,
/// red hull.
const SHIELD_HUE: Color = Color::srgb(0.25, 0.70, 1.0);
const ARMOR_HUE: Color = Color::srgb(0.95, 0.72, 0.20);
const HULL_HUE: Color = Color::srgb(0.95, 0.42, 0.28);

/// One frame's snapshot of the five pool fractions (`None` until the ship exists / is fitted).
#[derive(Default)]
struct PoolFracs {
    afterburner: Option<f32>,
    heat: Option<f32>,
    shield: Option<f32>,
    armor: Option<f32>,
    hull: Option<f32>,
}

/// Update each frame: read the local ship's five pools from the embedded server world and colour
/// each segment lit/unlit by `current/max`. The ramp bars grade green→amber→red (Afterburner reddens
/// as it EMPTIES, Heat as it FILLS); the Shield/Armor/Hull stacks use their fixed per-layer hue and
/// pulse only when critically low.
pub fn update_trapezoid_bars(
    host: Option<NonSend<LoopbackHost>>,
    net: Option<NonSend<NetClientState>>,
    time: Res<Time>,
    segs: Query<&TrapSegment>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Resolve the local ship's pools (all None until it exists / is fitted).
    let pools = match (host.as_ref(), net.as_ref()) {
        (Some(host), Some(net)) => match host.server.ship_entity_for(net.local_id) {
            Some(e) => {
                let w = host.server.world();
                PoolFracs {
                    afterburner: w.get::<Afterburner>(e).map(|p| frac(p.current, p.max)),
                    heat: w.get::<Heat>(e).map(|p| frac(p.current, p.max)),
                    shield: w.get::<Shields>(e).map(|p| frac(p.current, p.max)),
                    armor: w.get::<ArmorHp>(e).map(|p| frac(p.current, p.max)),
                    hull: w.get::<HullStructure>(e).map(|p| frac(p.current, p.max)),
                }
            }
            None => PoolFracs::default(),
        },
        _ => PoolFracs::default(),
    };
    // Critical-pulse brightness oscillation (0.55..=1.0), applied only in a bar's danger band.
    let pulse = 0.55 + 0.45 * (time.elapsed_secs() * 9.0).sin().abs();

    for seg in &segs {
        let f = match seg.bar {
            TrapBar::Afterburner => pools.afterburner,
            TrapBar::Heat => pools.heat,
            TrapBar::Shield => pools.shield,
            TrapBar::Armor => pools.armor,
            TrapBar::Hull => pools.hull,
        };
        let color = match f {
            // No pool yet → dim everything.
            None => seg_dim(),
            Some(frac) if seg.index >= lit_count(frac, seg.count) => seg_dim(),
            Some(frac) => {
                let (mut c, critical) = match seg.bar {
                    // Ramp bars: graded green→amber→red. Afterburner is "want full" (bad as it
                    // empties); Heat is "want empty" (bad as it fills).
                    TrapBar::Afterburner => (grade(1.0 - frac), frac < 0.2),
                    TrapBar::Heat => (grade(frac), frac > 0.85),
                    // Defense stacks: a FIXED per-layer hue (identity), pulsing only when low.
                    TrapBar::Shield => (SHIELD_HUE, frac < 0.2),
                    TrapBar::Armor => (ARMOR_HUE, frac < 0.2),
                    TrapBar::Hull => (HULL_HUE, frac < 0.2),
                };
                if critical {
                    c = scale_rgb(c, pulse);
                }
                c
            }
        };
        if let Some(m) = materials.get_mut(&seg.material) {
            m.base_color = color;
        }
    }
}

/// `current/max` clamped to `[0,1]` (0 when `max <= 0`).
fn frac(current: f32, max: f32) -> f32 {
    if max > 0.0 {
        (current / max).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lit_count_rounds_and_clamps() {
        assert_eq!(lit_count(0.0, 16), 0);
        assert_eq!(lit_count(1.0, 16), 16);
        assert_eq!(lit_count(0.5, 16), 8);
        assert_eq!(lit_count(1.5, 16), 16, "clamps over-full to count");
        assert_eq!(lit_count(-1.0, 16), 0, "clamps negative to 0");
    }

    #[test]
    fn single_ramp_is_monotonic_short_to_tall() {
        let n = 16;
        let mut prev = -1.0;
        for i in 0..n {
            let h = ramp_height(i, n, 0.3, 1.0);
            assert!(h > prev, "ramp height strictly increases (i={i})");
            prev = h;
        }
        assert!(
            (ramp_height(0, n, 0.3, 1.0) - 0.3).abs() < 1e-6,
            "starts at min"
        );
        assert!(
            (ramp_height(n - 1, n, 0.3, 1.0) - 1.0).abs() < 1e-6,
            "ends at max"
        );
    }

    #[test]
    fn double_ramp_resets_at_the_midpoint() {
        let n = 24;
        let (min_h, max_h) = (0.3, 0.95);
        // First half ramps to max at i = 11; the midpoint i = 12 RESETS to min (the 50% landmark).
        assert!((double_ramp_height(0, n, min_h, max_h) - min_h).abs() < 1e-6);
        assert!((double_ramp_height(11, n, min_h, max_h) - max_h).abs() < 1e-6);
        assert!(
            (double_ramp_height(12, n, min_h, max_h) - min_h).abs() < 1e-6,
            "second ramp restarts at min — the obvious 50% landmark"
        );
        assert!((double_ramp_height(23, n, min_h, max_h) - max_h).abs() < 1e-6);
        // The drop at the midpoint is the defining feature.
        assert!(
            double_ramp_height(12, n, min_h, max_h) < double_ramp_height(11, n, min_h, max_h),
            "height drops across the midpoint"
        );
    }

    #[test]
    fn frac_handles_zero_max() {
        assert_eq!(frac(5.0, 0.0), 0.0);
        assert_eq!(frac(50.0, 100.0), 0.5);
        assert_eq!(frac(200.0, 100.0), 1.0, "clamps over-full");
    }

    #[test]
    fn ramp_lays_segments_along_x_at_fixed_y() {
        let layout = BarLayout {
            bar: TrapBar::Afterburner,
            count: 4,
            x_center: 0.0,
            y_base: -3.0,
            extent: 4.0,
            shape: Shape::Ramp {
                double: false,
                min_h: 0.3,
                max_h: 1.0,
            },
        };
        let (x0, y0, _w0, h0) = seg_placement(&layout, 0);
        let (x3, y3, _w3, h3) = seg_placement(&layout, 3);
        assert!(x3 > x0, "ramp segments advance along +x");
        assert!(
            (y0 - y3).abs() < 1e-6 && (y0 + 3.0).abs() < 1e-6,
            "fixed y at y_base"
        );
        assert!(h3 > h0, "heights ramp short→tall");
    }

    #[test]
    fn stack_lays_uniform_segments_up_y_at_fixed_x() {
        let layout = BarLayout {
            bar: TrapBar::Shield,
            count: 10,
            x_center: -5.6,
            y_base: -2.5,
            extent: 5.0,
            shape: Shape::Stack { width: 0.5 },
        };
        let (x0, y0, w0, h0) = seg_placement(&layout, 0);
        let (x9, y9, w9, h9) = seg_placement(&layout, 9);
        assert!(
            (x0 - x9).abs() < 1e-6 && (x0 + 5.6).abs() < 1e-6,
            "fixed column x"
        );
        assert!(
            y9 > y0 && (y0 + 2.5).abs() < 1e-6,
            "segments stack up +y from y_base"
        );
        assert!(
            (w0 - 0.5).abs() < 1e-6 && (w9 - 0.5).abs() < 1e-6,
            "uniform width"
        );
        assert!((h0 - h9).abs() < 1e-6, "uniform height (no ramp)");
    }
}
