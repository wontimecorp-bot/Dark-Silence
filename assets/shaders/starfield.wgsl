// Refinement 25/35 — procedural galaxy starfield (Dark Silence).
//
// Drawn on a camera-child fullscreen quad. Each fragment maps to a WORLD point on the z=0 plane (the
// camera looks straight down). A faint galactic background (milky haze + dark dust lanes + a warm
// core bulge) is laid down first, then several parallax DEPTH layers of stars are accumulated.
//
// In SPECTRAL mode (R35, `spectral_enabled > 0.5`) every star is classified into a Morgan–Keenan
// class (M…O) by the editable population CDF, then takes that class's blackbody temp/color, size,
// HDR brightness, scintillation, edge profile and within-class magnitude spread; hot classes
// (O/B/A) are clustered along the galactic band; the brightest get diffraction-spike glare. In
// LEGACY mode the older R34 per-layer model runs (per-layer temp/tint/twinkle).
//
// Motion stability (R31): per-star fields (density map, band clustering) are sampled in each layer's
// PARALLAX frame `s`, so they're invariant under camera translation (no twinkle while flying). The
// galactic background is sampled in the screen frame (the galaxy is effectively at infinity).

#import bevy_pbr::forward_io::VertexOutput

// One depth layer's parameters. Order/types MUST match `StarLayer` in starfield.rs (8 f32 = 32 B).
// R36: depth only (parallax/frequency/density + brightness/size depth multipliers) + the optional
// per-layer tint overlay (tint_r/g/b, packed as the effective lerp(white,tint,strength)).
struct StarLayer {
    parallax: f32,
    frequency: f32,
    density: f32,
    brightness: f32,
    size: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
}

// One spectral class's parameters (R35). Order/types MUST match `SpectralClass` in starfield.rs
// (16 f32 = 64 B; 13 real + 3 pad).
struct SpectralClass {
    cdf: f32,
    temp_min: f32,
    temp_max: f32,
    brightness: f32,
    size: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    clustering: f32,
    twinkle: f32,
    twinkle_speed: f32,
    softness: f32,
    mag_spread: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
}

// Order/types MUST match `StarfieldParams` in starfield.rs (one explicit `pad_layers` before the
// `layers` array so both arrays are 16-byte aligned; a headless Rust test validates the layout).
struct StarfieldParams {
    cam_pos: vec2<f32>,
    height: f32,
    fov: f32,
    resolution: vec2<f32>,
    time: f32,
    layer_count: u32,
    band_angle: f32,
    band_width: f32,
    band_offset: f32,
    band_strength: f32,
    band_clumpiness: f32,
    haze_brightness: f32,
    haze_r: f32,
    haze_g: f32,
    haze_b: f32,
    dust_depth: f32,
    dust_scale: f32,
    dust_contrast: f32,
    core_along: f32,
    core_size: f32,
    core_brightness: f32,
    core_r: f32,
    core_g: f32,
    core_b: f32,
    core_density_boost: f32,
    glare_threshold: f32,
    glare_halo_size: f32,
    glare_halo_intensity: f32,
    glare_spike_len: f32,
    glare_spike_count: f32,
    glare_spike_intensity: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
    layers: array<StarLayer, 16>,
    classes: array<SpectralClass, 8>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: StarfieldParams;

const MAX_LAYERS: u32 = 16u;
const NUM_CLASSES: u32 = 7u;
const TAU: f32 = 6.2831853;
// World units the galactic band/haze/core coordinates are normalised by (sets the galaxy's scale).
const GALAXY_SCALE: f32 = 400.0;

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

// Worley / cellular F1 noise: distance (≈0..1) to the nearest jittered feature point. The per-layer
// star DENSITY MAP (cellular clumping — clumps near features, voids between).
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

// Smooth value noise (R35) — for the dust lanes + along-band clumpiness.
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i + vec2<f32>(0.0, 0.0));
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Blackbody temperature (Kelvin) → linear-ish RGB (Tanner-Helland piecewise, /255-normalized; valid
// ~1000–40000K): ~3000K red (M) → ~30000K+ blue-white (O). (True violet is off the locus — use the
// per-class tint to nudge O toward violet.)
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

// Galactic band intensity (0..1) at a galaxy-frame point `g` — a Gaussian lane at `band_angle`,
// thickness `band_width`, offset `band_offset`, modulated along its length by `band_clumpiness`.
fn band_value(g: vec2<f32>) -> f32 {
    let a = params.band_angle;
    let dir = vec2<f32>(cos(a), sin(a));
    let nrm = vec2<f32>(-sin(a), cos(a));
    let perp = dot(g, nrm) - params.band_offset;
    let w = max(params.band_width, 0.01);
    let core = exp(-(perp * perp) / (w * w));
    let along = dot(g, dir);
    let clump = mix(1.0, 0.35 + 0.65 * vnoise(vec2<f32>(along * 1.3 + 17.0, 4.0)), clamp(params.band_clumpiness, 0.0, 1.0));
    return core * clump;
}

// Galactic-core bulge weight (0..1) at galaxy-frame `g` — a Gaussian blob on the band axis.
fn core_weight(g: vec2<f32>) -> f32 {
    let a = params.band_angle;
    let dir = vec2<f32>(cos(a), sin(a));
    let center = dir * params.core_along;
    let d = length(g - center);
    let cs = max(params.core_size, 0.01);
    return exp(-(d * d) / (cs * cs));
}

// One depth layer's accumulated star color.
fn star_layer(
    world: vec2<f32>,
    cam: vec2<f32>,
    layer: StarLayer,
    px_per_world: f32,
    seed: f32,
) -> vec3<f32> {
    let pf = layer.parallax;
    let freq = layer.frequency;
    // Parallax sample coordinate: far layers (pf~0) screen-locked; near layers world-anchored.
    let s = world - cam * (1.0 - pf);
    let q = s * freq + vec2<f32>(seed, seed * 1.7);
    let cell = floor(q);
    // R31: density map + band sampled in THIS layer's parallax frame `s` → motion-stable.
    let dmap = 0.30 + 0.70 * (1.0 - worley(s * 0.03 + vec2<f32>(seed, seed)));
    let g_layer = s / GALAXY_SCALE;
    let band = band_value(g_layer);
    let core_w = core_weight(g_layer);
    // R36: OPTIONAL per-layer tint overlay (effective tint packed CPU-side; white = no-op).
    let layer_tint = vec3<f32>(layer.tint_r, layer.tint_g, layer.tint_b);
    let dens = layer.density * dmap * (1.0 + params.core_density_boost * core_w); // denser near the core

    var col = vec3<f32>(0.0);
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let cid = cell + vec2<f32>(f32(dx), f32(dy));
            let present = hash21(cid * 1.37 + seed);
            if (present > dens) {
                continue;
            }
            let jitter = hash22(cid * 2.71 + seed);
            let star_q = cid + jitter;
            let off = q - star_q;
            let off_px = off / freq * px_per_world; // pixel-space offset (for spikes)
            let px = length(off_px); // distance to the star in PIXELS
            let gate = 1.0 - smoothstep(dens * 0.5, dens, present);

            // Pick the spectral class from the population CDF.
            let uc = hash21(cid * 8.9 + seed + 2.3);
            var ci = NUM_CLASSES - 1u;
            for (var k: u32 = 0u; k < NUM_CLASSES; k = k + 1u) {
                if (uc < params.classes[k].cdf) {
                    ci = k;
                    break;
                }
            }
            let cls = params.classes[ci];
            // Clustering: confine high-clustering classes (O/B/A) to the galactic band.
            let hb = hash21(cid * 7.3 + seed + 3.1);
            let confine = clamp(cls.clustering * params.band_strength, 0.0, 1.0) * (1.0 - band);
            if (hb < confine) {
                continue;
            }
            let tn = hash21(cid * 4.13 + seed + 9.1);
            let temp_k = mix(cls.temp_min, cls.temp_max, tn);
            // Class blackbody color × the class tint × the optional per-layer tint overlay.
            let color = blackbody(temp_k) * vec3<f32>(cls.tint_r, cls.tint_g, cls.tint_b) * layer_tint;
            // Magnitude spread: a few bright, many faint (power curve; 0 spread = uniform).
            let mh = hash21(cid * 11.1 + seed + 5.7);
            let mag = pow(mh, mix(1.0, 4.0, clamp(cls.mag_spread, 0.0, 1.0)));
            let bright = cls.brightness * mag * layer.brightness;
            let r = cls.size * layer.size;
            let aa = max(cls.softness, 0.001);
            let phase = hash21(cid * 5.7 + seed) * TAU;
            let t = params.time;
            let sp = cls.twinkle_speed;
            let raw = 0.5 * sin(t * 1.3 * sp + phase)
                + 0.3 * sin(t * 2.1 * sp + phase * 1.7)
                + 0.2 * sin(t * 3.7 * sp + phase * 2.3);
            let tw = clamp(1.0 + 0.4 * raw * cls.twinkle, 0.0, 1.6);
            let glare_eligible = (cls.brightness * layer.brightness) > params.glare_threshold;

            // Bright-star glare (R35): a halo + diffraction spikes that reach BEYOND the core radius.
            if (glare_eligible) {
                let ax = abs(off_px.x);
                let ay = abs(off_px.y);
                let slen = max(params.glare_spike_len, 1.0);
                let halo = params.glare_halo_intensity * exp(-px / max(params.glare_halo_size, 0.5));
                var spikes = exp(-ay / 1.5) * exp(-ax / slen) + exp(-ax / 1.5) * exp(-ay / slen);
                if (params.glare_spike_count > 5.0) {
                    let d1 = abs(off_px.x + off_px.y) * 0.70710678;
                    let d2 = abs(off_px.x - off_px.y) * 0.70710678;
                    spikes = spikes + exp(-d2 / 1.5) * exp(-d1 / slen) + exp(-d1 / 1.5) * exp(-d2 / slen);
                }
                let glow = halo + spikes * params.glare_spike_intensity;
                col = col + color * bright * tw * glow * gate;
            }

            // Core star: analytic coverage (R30) + energy-conserving fill.
            if (px <= r + aa) {
                let cov = 1.0 - smoothstep(r - aa, r + aa, px);
                let fill = clamp(r, 0.0, 1.0);
                col = col + color * bright * tw * cov * fill * gate;
            }
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

    var col = vec3<f32>(0.0);

    // R35 galactic background: milky haze + dark dust lanes + warm core bulge, in the SCREEN frame
    // (`world - cam` = screen offset → the galaxy is fixed on screen, at infinity). Turn it off via
    // the haze/core brightness knobs (the "Plain stars" preset zeroes them).
    {
        let g = (world - params.cam_pos) / GALAXY_SCALE;
        let band = band_value(g);
        let dust = pow(clamp(vnoise(g / max(params.dust_scale, 0.001)), 0.0, 1.0), max(params.dust_contrast, 0.1));
        let haze = max(params.haze_brightness * band * (1.0 - params.dust_depth * dust), 0.0);
        let core = core_weight(g) * params.core_brightness;
        col = vec3<f32>(params.haze_r, params.haze_g, params.haze_b) * haze
            + vec3<f32>(params.core_r, params.core_g, params.core_b) * core;
    }

    let n = clamp(params.layer_count, 1u, MAX_LAYERS);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let seed = f32(i) * 13.7;
        let layer = params.layers[i];
        col = col + star_layer(world, params.cam_pos, layer, px_per_world, seed);
    }

    return vec4<f32>(col, 1.0);
}
