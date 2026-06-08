// R48 — cinematic "used-future" hull extension shader.
//
// Layers a faction-tinted fresnel RIM light + procedural panel grooves + grime on top of the full
// StandardMaterial PBR forward path (mirrors bevy_pbr::render/pbr.wgsl). Detail is done via albedo +
// roughness modulation (the hull mesh has no tangents, so no normal-map path) + an added emissive-like
// rim. Panel lines key off WORLD XY (the hull UVs are per-cell 0..1, unusable for continuous panels).

#import bevy_pbr::forward_io::{VertexOutput, FragmentOutput}
#import bevy_pbr::pbr_fragment::pbr_input_from_standard_material
#import bevy_pbr::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing, alpha_discard}
#import bevy_pbr::mesh_view_bindings::view

struct HullSettings {
    faction_color: vec4<f32>,   // rgb rim tint, a rim strength
    params: vec4<f32>,          // x panel spacing, y line width, z grime, w rim power
};
// R49 — the forward-pass MATERIAL bind group is index 3 in Bevy 0.18 (group 2 is the prepass); the
// `#{MATERIAL_BIND_GROUP}` preprocessor placeholder substitutes the correct index. Hardcoding `2` was
// the R48 crash ("binding 100 not available in the pipeline layout").
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> hull: HullSettings;

fn hash21(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453);
}

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

// R50 — IRREGULAR panel seams (not a uniform grid): brick-offset alternate rows + a per-plate hash
// jitter on the seam position so plate sizes vary → reads as real hull plating, not graph paper.
// `uv` is hull-LOCAL (world units); `scale` = plate spacing; `width` = seam half-width (in plate units).
fn panel_seam(uv: vec2<f32>, scale: f32, width: f32) -> f32 {
    let row = floor(uv.y / scale);
    // Stagger vertical seams per row (half-plate brick offset + a per-row hash wobble).
    let shift = (fract(row * 0.5) * 0.5 + (hash21(vec2<f32>(row, 7.3)) - 0.5) * 0.4) * scale;
    let p = vec2<f32>(uv.x + shift, uv.y) / scale;
    let cell = floor(p);
    // Jitter the seam centre per plate so plate widths/heights differ.
    let jx = (hash21(cell) - 0.5) * 0.35;
    let jy = (hash21(cell + vec2<f32>(3.1, 1.7)) - 0.5) * 0.35;
    let g = abs(fract(p) - vec2<f32>(0.5) + vec2<f32>(jx, jy));
    return 1.0 - smoothstep(0.0, width, min(g.x, g.y));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // Build the StandardMaterial PBR input (albedo / metallic / roughness from the base material).
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // --- used-future surface: panel grooves + grime, BEFORE lighting ---
    // R50: key off `in.uv` = the hull-LOCAL position (baked into the mesh UVs) so the pattern is fixed
    // to the hull and moves WITH the ship — NOT `world_position` (which made it swim).
    let wp = in.uv;
    // Two plate scales (coarse + fine) of irregular seams so the paneling doesn't read as one grid.
    let line = max(
        panel_seam(wp, hull.params.x, hull.params.y),
        panel_seam(wp, hull.params.x * 2.3, hull.params.y * 1.3) * 0.7,
    );
    let grime = vnoise(wp * (1.4 / hull.params.x)) * hull.params.z;  // low-freq splotchy wear
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * (1.0 - 0.45 * line) * (0.80 + 0.30 * grime),
        pbr_input.material.base_color.a,
    );
    pbr_input.material.perceptual_roughness = clamp(
        pbr_input.material.perceptual_roughness + 0.30 * line + 0.18 * grime, 0.05, 1.0);

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    // --- full PBR lighting (key + cool fill + ambient + emissive accents) ---
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);

    // --- faction fresnel RIM, added after lighting (blooms) ---
    let N = normalize(in.world_normal);
    let V = normalize(view.world_position - in.world_position.xyz);
    let fres = pow(clamp(1.0 - dot(N, V), 0.0, 1.0), hull.params.w);
    out.color = vec4<f32>(
        out.color.rgb + hull.faction_color.rgb * (fres * hull.faction_color.a),
        out.color.a,
    );

    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
