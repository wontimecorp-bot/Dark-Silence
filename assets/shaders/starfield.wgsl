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

struct StarfieldParams {
    cam_pos: vec2<f32>,
    height: f32,
    fov: f32,
    resolution: vec2<f32>,
    time: f32,
    star_brightness: f32,
    star_density: f32,
    twinkle_amount: f32,
    layer_count: u32,
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

// Smooth value noise — the low-frequency density map (galactic bands / voids).
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Blackbody-ish color across the stellar sequence: t=0 -> M (cool red), t=1 -> O (hot blue).
fn star_color(t: f32) -> vec3<f32> {
    let cool = vec3<f32>(1.0, 0.55, 0.35); // M / K  (red-orange)
    let mid = vec3<f32>(1.0, 0.96, 0.88);  // G / F  (white)
    let hot = vec3<f32>(0.70, 0.80, 1.0);  // B / O  (blue)
    if (t < 0.5) {
        return mix(cool, mid, t * 2.0);
    }
    return mix(mid, hot, (t - 0.5) * 2.0);
}

// One layer's accumulated star color.
fn star_layer(
    world: vec2<f32>,
    cam: vec2<f32>,
    pf: f32,
    freq: f32,
    px_per_world: f32,
    fi: f32,
    layer_bright: f32,
    seed: f32,
    dmap: f32,
) -> vec3<f32> {
    // Parallax sample coordinate: far layers (pf~0) are screen-locked; near layers world-anchored.
    let s = world - cam * (1.0 - pf);
    let q = s * freq + vec2<f32>(seed, seed * 1.7);
    let cell = floor(q);
    // Fraction of candidate cells that host a star (far layers denser), modulated by the density map.
    let dens = mix(0.40, 0.22, fi) * params.star_density * dmap;

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

            // Temperature weighted toward cool (M most common, O rarest).
            let th = hash21(cid * 4.13 + seed + 9.1);
            let temp = pow(th, 3.0);
            let radius = max(mix(1.0, 2.2, temp) * mix(0.9, 1.3, fi), 1.0); // pixels
            // HARD point: lit iff within radius_px (no smoothstep / no AA).
            if (step(px, radius) < 0.5) {
                continue;
            }

            // Desynchronised twinkle: per-star phase + summed incommensurate sines.
            let phase = hash21(cid * 5.7 + seed) * TAU;
            let t = params.time;
            let raw = 0.5 * sin(t * 1.3 + phase)
                + 0.3 * sin(t * 2.1 + phase * 1.7)
                + 0.2 * sin(t * 3.7 + phase * 2.3);
            let amp = mix(1.0, 0.4, temp); // cool/dim stars scintillate more, hot/bright steadier
            let tw = clamp(0.6 + 0.4 * raw * params.twinkle_amount * amp, 0.0, 1.6);

            let bright = mix(0.5, 2.2, temp) * layer_bright; // hot brighter; >1 => HDR bloom
            col = col + star_color(temp) * bright * tw;
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

    // Low-frequency clustering field (never fully empty).
    let dmap = 0.35 + 0.65 * vnoise(world * 0.02);

    let n = clamp(params.layer_count, 1u, MAX_LAYERS);
    let denom = f32(max(n - 1u, 1u));
    var col = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let fi = f32(i) / denom;            // 0 (farthest) .. 1 (nearest)
        let pf = mix(0.015, 0.45, fi);      // parallax factor (far ~ screen-locked)
        let freq = mix(2.5, 0.35, fi);      // cells/world (far denser/smaller spacing)
        let lb = mix(0.6, 1.0, fi);         // near layers a touch brighter
        let seed = f32(i) * 13.7;
        col = col + star_layer(world, params.cam_pos, pf, freq, px_per_world, fi, lb, seed, dmap);
    }

    col = col * params.star_brightness;
    return vec4<f32>(col, 1.0);
}
