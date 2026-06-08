//! R48 — the cinematic "used-future" hull material.
//!
//! An [`ExtendedMaterial`] that layers a **faction-tinted fresnel RIM light**, **procedural panel-line
//! grooves**, and **grime/wear** on top of full [`StandardMaterial`] PBR (the extension fragment shader
//! runs after the standard lighting). One baked handle per faction (neutral / red / blue) — NOT per
//! cell-set, so it doesn't churn when a ship is carved. Used for the normal combat hull look only
//! (the module-colour inspection view + contour + wrecks keep plain `StandardMaterial`). Entirely
//! client render → determinism-neutral. The WGSL lives at `assets/shaders/hull_extension.wgsl`.

use bevy::pbr::{ExtendedMaterial, MaterialExtension, StandardMaterial};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// The extended hull material used for the combat ship look.
pub type HullMaterial = ExtendedMaterial<StandardMaterial, HullExtension>;

/// The hull-shader extension on top of [`StandardMaterial`]. The two `vec4` uniforms share binding
/// `100` (slots 0..99 are the base StandardMaterial) — the flat multi-field-same-binding pattern from
/// Bevy's `extended_material` example (packed into one uniform buffer matching the WGSL `HullSettings`
/// struct). The WGSL keys panels off WORLD position + adds a faction fresnel rim.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct HullExtension {
    /// Rim-light tint in `.rgb`, rim strength in `.a` — set per faction.
    #[uniform(100)]
    pub faction_color: Vec4,
    /// `x` = panel spacing (world units), `y` = panel-line width, `z` = grime strength, `w` = rim power.
    #[uniform(100)]
    pub params: Vec4,
}

impl MaterialExtension for HullExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/hull_extension.wgsl".into()
    }
    // The hull is opaque + the top-down ships don't need shadow/depth-prepass detail; skipping the
    // prepass avoids a prepass-pipeline layout that doesn't carry the extension's binding 100.
    fn enable_prepass() -> bool {
        false
    }
}

/// Build the extension for a faction rim tint (`a` = strength) at the shared panel/grime/rim tuning:
/// panel spacing 0.45 u (~1.4 cells), line width 0.045, grime 1.0, rim power 2.6.
pub fn hull_extension(faction_color: Vec4) -> HullExtension {
    HullExtension {
        faction_color,
        params: Vec4::new(0.45, 0.045, 1.0, 2.6),
    }
}
