// Refinement 25 — procedural infinite-depth starfield (Dark Silence).
//
// Drawn on a camera-child fullscreen quad. Each fragment is mapped to a WORLD point on the z=0 plane
// (the camera looks straight down), then several exponentially-spaced layers of Voronoi hard-point
// stars are accumulated. Far layers are nearly screen-locked (parallax ~0 => infinite distance),
// near layers drift with the camera. Stars are HARD points (a hard `step`, no smoothing / no texture
// sampling) at a fixed PIXEL radius (crisp at any zoom); the camera's Bloom supplies the glow.
// Colors follow the blackbody stellar sequence (M cool/red -> O hot/blue); brightness/size/twinkle
// vary per star, weighted so cool dim stars (M/K) are common and hot bright stars (O/B) are rare.

#import bevy_pbr::forward_io::VertexOutput

// One layer's parameters (Refinement 26/32/34). Order/types MUST match `StarLayer` in starfield.rs
// (12 floats = 48-byte uniform array stride). R34: the old trailing `pad0` is now `twinkle_speed`.
struct StarLayer {
    parallax: f32,
    frequency: f32,
    density: f32,
    brightness: f32,
    twinkle: f32,
    size: f32,
    temp_min: f32,
    temp_max: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    twinkle_speed: f32,
}

struct StarfieldParams {
    cam_pos: vec2<f32>,
    height: f32,
    fov: f32,
    resolution: vec2<f32>,
    time: f32,
    // R34: the old global star_brightness/star_density/twinkle_amount masters were removed (all
    // per-layer now) — kept as PADS so the byte layout still matches the Rust `StarfieldParams`.
    pad_a: f32,
    pad_b: f32,
    pad_c: f32,
    layer_count: u32,
    // Analytic-coverage edge softness in px (R30); reuses the 16-align slot (offset 44 → layers 48),
    // so the layout still matches the Rust `StarfieldParams`.
    edge_softness: f32,
    layers: array<StarLayer, 16>,
    // R34: the global twinkle_speed moved to per-layer; this trailing slot is now a PAD.
    pad_d: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: StarfieldParams;

const MAX_LAYERS: u32 = 16u;
const TAU: f32 = 6.2831853;

// --- hashing (deterministic, render-only) -----------------------------------------------------
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.xx + p3.yz) * p3.zy);
}

// Worley / cellular F1 noise: distance (≈0..1) to the nearest of one jittered feature point per
// cell. Used as the star DENSITY MAP — cellular clustering (clumps near features, voids between).
fn worley(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let f = fract(p);
    var d = 1.0;
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let g = vec2<f32>(f32(dx), f32(dy));
            let feat = g + hash22(cell + g);
            d = min(d, length(f - feat));
        }
    }
    return d;
}

// Blackbody temperature (Kelvin) → linear-ish RGB (Tanner-Helland piecewise, /255-normalized; valid
// ~1000–40000K). Gives the real stellar-class colors: ~3000K red (M) → ~30000K blue (O).
fn blackbody(kelvin: f32) -> vec3<f32> {
    let t = clamp(kelvin, 1000.0, 40000.0) / 100.0;
    var r: f32;
    var g: f32;
    var b: f32;
    if (t <= 66.0) {
        r = 1.0;
        g = clamp(0.39008157876902 * log(t) - 0.63184144378961, 0.0, 1.0);
    } else {
        r = clamp(1.29293618606274 * pow(t - 60.0, -0.1332047592), 0.0, 1.0);
        g = clamp(1.12989086089529 * pow(t - 60.0, -0.0755148492), 0.0, 1.0);
    }
    if (t >= 66.0) {
        b = 1.0;
    } else if (t <= 19.0) {
        b = 0.0;
    } else {
        b = clamp(0.54320678911019 * log(t - 10.0) - 1.19625408914, 0.0, 1.0);
    }
    return vec3<f32>(r, g, b);
}

// One layer's accumulated star color. Per-layer parameters come from `layer` (Refinement 26).
fn star_layer(
    world: vec2<f32>,
    cam: vec2<f32>,
    layer: StarLayer,
    px_per_world: f32,
    seed: f32,
) -> vec3<f32> {
    let pf = layer.parallax;
    let freq = layer.frequency;
    // Parallax sample coordinate: far layers (pf~0) are screen-locked; near layers world-anchored.
    let s = world - cam * (1.0 - pf);
    let q = s * freq + vec2<f32>(seed, seed * 1.7);
    let cell = floor(q);
    // R31: density map sampled in THIS layer's parallax frame `s` (NOT raw world) — so the star grid
    // and the density field move together → each star's density value is invariant under camera
    // translation → no twinkle as you fly (at parallax 0, `s` = screen offset → density screen-locked).
    let dmap = 0.30 + 0.70 * (1.0 - worley(s * 0.03 + vec2<f32>(seed, seed)));
    // Fraction of candidate cells that host a star, modulated by the density map (R34: per-layer
    // density only — the global `star_density` master was removed).
    let dens = layer.density * dmap;

    var col = vec3<f32>(0.0);
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let cid = cell + vec2<f32>(f32(dx), f32(dy));
            let present = hash21(cid * 1.37 + seed);
            if (present > dens) {
                continue;
            }
            // Star feature point (Voronoi) + per-star randoms.
            let jitter = hash22(cid * 2.71 + seed);
            let star_q = cid + jitter;
            let d_cells = length(q - star_q);
            let px = d_cells / freq * px_per_world; // distance to the star in PIXELS

            // Stellar class, weighted toward cool (`tn` cool-biased via pow(.,3)): the layer's own
            // R32 temperature RANGE — `temp_min` (common cool end) → `temp_max` (rare hot end) — so
            // each layer reads as a distinct stellar class, with real per-star blackbody variation.
            let th = hash21(cid * 4.13 + seed + 9.1);
            let tn = pow(th, 3.0);
            let temp_k = mix(layer.temp_min, layer.temp_max, tn);
            // R30: analytic sub-pixel COVERAGE instead of a hard step → temporally stable (kills the
            // motion shimmer) + twinkle-controllable. No 1px floor; energy-conserving so sub-pixel
            // stars get fainter (by radius) rather than clamping to a full-bright 1px dot.
            let r = mix(1.0, 2.2, tn) * layer.size; // pixel radius (may be < 1)
            let aa = max(params.edge_softness, 0.001); // 0 (tiny) ⇒ effectively a hard step
            if (px > r + aa) {
                continue; // beyond the AA'd edge — contributes nothing
            }
            let cov = 1.0 - smoothstep(r - aa, r + aa, px); // soft coverage at this fragment
            let fill = clamp(r, 0.0, 1.0); // energy-conserving: sub-pixel stars dimmer

            // Desynchronised twinkle: per-star phase + summed incommensurate sines. R34: the sine
            // frequencies scale by THIS LAYER's twinkle SPEED (`layer.twinkle_speed`) — the pulse RATE,
            // distinct from `layer.twinkle` (the DEPTH). Both are per-layer now.
            let phase = hash21(cid * 5.7 + seed) * TAU;
            let t = params.time;
            let sp = layer.twinkle_speed;
            let raw = 0.5 * sin(t * 1.3 * sp + phase)
                + 0.3 * sin(t * 2.1 * sp + phase * 1.7)
                + 0.2 * sin(t * 3.7 * sp + phase * 2.3);
            let amp = mix(1.0, 0.4, tn); // cool/dim stars scintillate more, hot/bright steadier
            // R32: base 1.0 (not 0.6) so twinkle=0 ⇒ tw=1.0 (steady, FULL brightness); twinkle>0
            // dips + flares around full. R34: per-layer twinkle depth only (global master removed).
            let tw = clamp(1.0 + 0.4 * raw * layer.twinkle * amp, 0.0, 1.6);

            let bright = mix(0.5, 2.2, tn) * layer.brightness; // hot brighter; >1 => HDR bloom
            // R30b: SOFT density gate — fade stars in/out near the threshold instead of a hard pop, so
            // they don't twinkle as the world-space density map (dmap) sweeps past while you fly. Only
            // the marginal (upper-half-of-threshold) stars fade; core stars (present < 0.5·dens) stay
            // fully on.
            let gate = 1.0 - smoothstep(dens * 0.5, dens, present);
            // R32: per-layer flat color TINT multiplier on top of the blackbody color (white = no
            // change).
            let tint = vec3<f32>(layer.tint_r, layer.tint_g, layer.tint_b);
            col = col + blackbody(temp_k) * tint * bright * tw * cov * fill * gate;
        }
    }
    return col;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let res = params.resolution;
    if (res.y < 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // `in.position` is the framebuffer coordinate (pixels) in the fragment stage.
    let uv = in.position.xy / res; // 0..1, y downwards
    let half_h = params.height * tan(params.fov * 0.5);
    let half_w = half_h * (res.x / res.y);
    let ndc = uv * 2.0 - vec2<f32>(1.0, 1.0);
    // World point under this fragment on the z=0 plane (flip y: framebuffer y is down, world y up).
    let world = params.cam_pos + vec2<f32>(ndc.x * half_w, -ndc.y * half_h);
    let px_per_world = res.y / (2.0 * half_h);

    let n = clamp(params.layer_count, 1u, MAX_LAYERS);
    var col = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let seed = f32(i) * 13.7;
        let layer = params.layers[i]; // per-layer depth/spacing/density/brightness/twinkle/size
        col = col + star_layer(world, params.cam_pos, layer, px_per_world, seed);
    }

    // R34: no global brightness master — each layer's `brightness` is applied inside `star_layer`.
    return vec4<f32>(col, 1.0);
}
