//! R51 — procedurally baked hull-plating textures (no asset files): a tiling NORMAL map (beveled
//! brick-laid plates + corner rivets + faint metal noise) and a packed ORM map (AO / roughness /
//! metallic). Applied to the hull material's `StandardMaterial` base so the relief catches the key
//! light → real plated metal (replacing the flat "+ sign" shader panels). Client render only.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Baked tile resolution (the texture repeats across the hull via the Repeat sampler).
const TEX: u32 = 256;
/// Plates per tile edge — EVEN so the half-plate brick offset tiles seamlessly.
const PLATES: f32 = 4.0;
/// Bevel ramp width into a seam (fraction of a plate).
const BEVEL: f32 = 0.12;
/// Seam groove depth (height units).
const DEPTH: f32 = 1.0;
/// Rivet radius + height + inset from the plate corner.
const RIVET_R: f32 = 0.05;
const RIVET_H: f32 = 0.55;
const RIVET_INSET: f32 = 0.13;
/// Faint metal microvariation amplitude.
const NOISE_AMP: f32 = 0.12;
/// Noise lattice period across the tile (integer → periodic → tiles).
const NOISE_P: i32 = 8;
/// Normal-map slope strength (how much the bevels/rivets tilt the normal).
const NORMAL_STRENGTH: f32 = 2.4;

fn hash21(x: i32, y: i32) -> f32 {
    let mut h = (x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))) as u32;
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0x00FF_FFFF) as f32 / 0x00FF_FFFF as f32
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Periodic value noise (lattice wraps at `NOISE_P`) so the baked tile is seamless.
fn vnoise(x: f32, y: f32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let (xf, yf) = (x - xi, y - yi);
    let (ix, iy) = (xi as i32, yi as i32);
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let w = |i: i32, j: i32| hash21(i.rem_euclid(NOISE_P), j.rem_euclid(NOISE_P));
    let a = w(ix, iy);
    let b = w(ix + 1, iy);
    let c = w(ix, iy + 1);
    let d = w(ix + 1, iy + 1);
    let ab = a + (b - a) * u;
    let cd = c + (d - c) * u;
    ab + (cd - ab) * v
}

/// Hull-plate height field at tile coords `(u, v)` in `0..1` (periodic). 0 on a plate face, dipping to
/// `-DEPTH` in the recessed seams, with `+` rivet bumps inset from each plate corner + faint noise.
fn height(u: f32, v: f32) -> f32 {
    let pw = 1.0 / PLATES;
    let row = (v / pw).floor();
    let off = if (row as i32).rem_euclid(2) == 1 {
        0.5
    } else {
        0.0
    };
    let px = (u / pw + off).fract();
    let py = (v / pw).fract();
    // Beveled seam valley: ramp from -DEPTH at the seam up to 0 in the plate interior.
    let edge = px.min(1.0 - px).min(py.min(1.0 - py));
    let h_plate = (smoothstep(0.0, BEVEL, edge) - 1.0) * DEPTH;
    // Rivets near the four inset plate corners.
    let mut rmin = 9.0_f32;
    for (cx, cy) in [
        (RIVET_INSET, RIVET_INSET),
        (1.0 - RIVET_INSET, RIVET_INSET),
        (RIVET_INSET, 1.0 - RIVET_INSET),
        (1.0 - RIVET_INSET, 1.0 - RIVET_INSET),
    ] {
        rmin = rmin.min(((px - cx).powi(2) + (py - cy).powi(2)).sqrt());
    }
    let rivet = RIVET_H * (-(rmin / RIVET_R).powi(2)).exp();
    let noise = (vnoise(u * NOISE_P as f32, v * NOISE_P as f32) - 0.5) * NOISE_AMP;
    h_plate + rivet + noise
}

fn make_image(fill: impl Fn(u32, u32) -> [u8; 4]) -> Image {
    let mut data = vec![0u8; (TEX * TEX * 4) as usize];
    for y in 0..TEX {
        for x in 0..TEX {
            let i = ((y * TEX + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&fill(x, y));
        }
    }
    let mut img = Image::new(
        Extent3d {
            width: TEX,
            height: TEX,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        // Linear (NOT sRGB) — normal + ORM data are linear, not colour.
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    let mut d = ImageSamplerDescriptor::linear();
    d.address_mode_u = ImageAddressMode::Repeat;
    d.address_mode_v = ImageAddressMode::Repeat;
    img.sampler = ImageSampler::Descriptor(d);
    img
}

/// Bake the tangent-space NORMAL map from the plate height field (its gradient).
pub fn generate_hull_normal_map() -> Image {
    let eps = 1.0 / TEX as f32;
    let wrap = |t: f32| t.rem_euclid(1.0);
    make_image(|x, y| {
        let u = (x as f32 + 0.5) / TEX as f32;
        let v = (y as f32 + 0.5) / TEX as f32;
        let hl = height(wrap(u - eps), v);
        let hr = height(wrap(u + eps), v);
        let hd = height(u, wrap(v - eps));
        let hu = height(u, wrap(v + eps));
        let n = Vec3::new(
            (hl - hr) * NORMAL_STRENGTH,
            (hd - hu) * NORMAL_STRENGTH,
            1.0,
        )
        .normalize();
        [
            ((n.x * 0.5 + 0.5) * 255.0) as u8,
            ((n.y * 0.5 + 0.5) * 255.0) as u8,
            ((n.z * 0.5 + 0.5) * 255.0) as u8,
            255,
        ]
    })
}

/// Bake the packed ORM map: R = ambient occlusion (darker in seams), G = roughness (rougher in seams +
/// noise), B = metallic (~0.85). Used as both `occlusion_texture` (R) + `metallic_roughness_texture`.
pub fn generate_hull_orm_map() -> Image {
    make_image(|x, y| {
        let u = (x as f32 + 0.5) / TEX as f32;
        let v = (y as f32 + 0.5) / TEX as f32;
        let h = height(u, v);
        let in_plate = smoothstep(-DEPTH * 0.9, 0.0, h.min(0.0)); // 0 in seam → 1 on plate
        let ao = 0.45 + 0.55 * in_plate;
        let rough = (0.32 + 0.28 * (1.0 - in_plate) + (vnoise(u * 16.0, v * 16.0) - 0.5) * 0.12)
            .clamp(0.05, 1.0);
        [
            (ao * 255.0) as u8,
            (rough * 255.0) as u8,
            (0.85 * 255.0) as u8,
            255,
        ]
    })
}
