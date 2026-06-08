// R48/R52 — cinematic hull extension shader: a faction-tinted fresnel RIM light on top of the full
// StandardMaterial PBR forward path (mirrors bevy_pbr::render/pbr.wgsl). R52 dropped the R50/R51
// panel/grime modulation — the hull's surface detail is now REAL per-cell plate GEOMETRY
// (`build_hull_mesh_detailed`), which catches the key light at any rotation (no faked-relief shimmer),
// so the shader only adds the rim on top. (`params.x/y/z` are now inert; `faction_color` + `params.w`
// drive the rim.)

#import bevy_pbr::forward_io::{VertexOutput, FragmentOutput}
#import bevy_pbr::pbr_fragment::pbr_input_from_standard_material
#import bevy_pbr::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing, alpha_discard}
#import bevy_pbr::mesh_view_bindings::view

struct HullSettings {
    faction_color: vec4<f32>,   // rgb rim tint, a rim strength
    params: vec4<f32>,          // w rim power (x/y/z inert since R52)
};
// R49 — the forward-pass MATERIAL bind group is index 3 in Bevy 0.18 (group 2 is the prepass); the
// `#{MATERIAL_BIND_GROUP}` preprocessor placeholder substitutes the correct index. Hardcoding `2` was
// the R48 crash ("binding 100 not available in the pipeline layout").
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> hull: HullSettings;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // Build the StandardMaterial PBR input. The per-cell raised plate GEOMETRY is the surface detail
    // now (real relief), so the shader does no albedo/roughness modulation — just lighting + the rim.
    var pbr_input = pbr_input_from_standard_material(in, is_front);
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
