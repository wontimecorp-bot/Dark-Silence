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

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // Build the StandardMaterial PBR input. R51: the BASE material now carries the baked normal/ORM
    // plating textures (beveled panel seams + rivets), so `pbr_input` already has the plate relief +
    // roughness/AO — the panel detail is REAL surface relief that catches light (not flat shader lines).
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // --- faint used-metal GRIME on top of the baked plating (anchored to the hull via in.uv) ---
    let grime = vnoise(in.uv * 1.5) * hull.params.z;
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * (0.82 + 0.28 * grime),
        pbr_input.material.base_color.a,
    );

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
