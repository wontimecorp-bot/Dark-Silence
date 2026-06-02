//! Bit-packed quantization of gameplay floats to fixed-width integers (TR-013,
//! TR-045).
//!
//! Snapshots carry positions, velocities, and headings. Sending them as `f32`
//! is wasteful and non-deterministic in size; instead we map each value onto a
//! fixed-width integer over a **build-pinned range**, so the encoded size of a
//! given snapshot shape is deterministic per build (Principle VI — bandwidth is
//! the budget). The ranges and bit widths below are `pub const`s so downstream
//! phases and tests can reason about exact wire sizes.
//!
//! Position/velocity are quantized to **sector-relative** bounds (Principle III
//! / `sim` keeps coordinates sector-relative so `f32` precision holds), never to
//! large absolute world coordinates. Heading is an angle on `(-π, π]` mapped to
//! a full unsigned range so it wraps cleanly.
//!
//! Quantization is lossy. Each `Q*` documents its worst-case round-trip
//! tolerance; `dequantize(quantize(x))` lands within that tolerance for any `x`
//! inside the pinned range (values outside the range are clamped to it).

use bitcode::{Decode, Encode};
use glam::Vec2;
use serde::{Deserialize, Serialize};

// --- Build-pinned ranges and bit widths --------------------------------------
//
// These constants pin the wire format. Changing any of them changes the encoded
// size of every snapshot and is a wire-breaking change. They are deliberately
// `pub const` so tests (T017) and downstream phases can assert exact sizes and
// tolerances against them.

/// Half-extent of the position range, in sim units. Positions are quantized
/// over `[-POS_RANGE, +POS_RANGE]` on each axis — a sector-relative bound, not
/// an absolute world coordinate (Principle III). A sector is far smaller than
/// this; the headroom keeps projectiles and fast targets inside the range.
pub const POS_RANGE: f32 = 4096.0;

/// Bit width of each quantized position axis. 16 bits over the position range
/// gives a resolution of `2 * POS_RANGE / (2^16 - 1)` sim units per step.
pub const POS_BITS: u32 = 16;

/// Half-extent of the velocity range, in sim units per second. Velocities are
/// quantized over `[-VEL_RANGE, +VEL_RANGE]` on each axis.
pub const VEL_RANGE: f32 = 1024.0;

/// Bit width of each quantized velocity axis.
pub const VEL_BITS: u32 = 16;

/// Bit width of the quantized heading. The full `(-π, π]` circle maps onto
/// `0..=2^ANGLE_BITS - 1`, so heading wraps with no discontinuity at ±π.
pub const ANGLE_BITS: u32 = 16;

/// Number of distinct position codes per axis (`2^POS_BITS`).
const POS_CODES: u32 = 1 << POS_BITS;
/// Number of distinct velocity codes per axis (`2^VEL_BITS`).
const VEL_CODES: u32 = 1 << VEL_BITS;
/// Number of distinct heading codes (`2^ANGLE_BITS`).
const ANGLE_CODES: u32 = 1 << ANGLE_BITS;

/// Worst-case round-trip error for a single position axis, in sim units. The
/// ideal-arithmetic bound is half a quantization step
/// (`POS_RANGE / (POS_CODES - 1)`); this is widened to a full step to also cover
/// `f32` rounding noise in the (de)quantization arithmetic.
/// `dequantize(quantize(p))` is within this of `p` for any `p` in
/// `[-POS_RANGE, POS_RANGE]`.
pub const POS_TOLERANCE: f32 = 2.0 * POS_RANGE / (POS_CODES - 1) as f32;

/// Worst-case round-trip error for a single velocity axis, in sim units/s
/// (one quantization step; see [`POS_TOLERANCE`]).
pub const VEL_TOLERANCE: f32 = 2.0 * VEL_RANGE / (VEL_CODES - 1) as f32;

/// Worst-case round-trip error for a heading, in radians: one quantization step
/// over the full `2π` circle (see [`POS_TOLERANCE`]).
pub const ANGLE_TOLERANCE: f32 = core::f32::consts::TAU / (ANGLE_CODES - 1) as f32;

// --- Scalar quantization helpers ---------------------------------------------

/// Map a value in `[-range, range]` onto `0..=codes-1`. Values outside the
/// range are clamped (fail-safe: an out-of-bounds entity quantizes to the
/// nearest representable edge rather than wrapping to the far side).
fn quantize_signed(value: f32, range: f32, codes: u32) -> u32 {
    let max_code = (codes - 1) as f32;
    // Normalize to 0..=1 then scale to the code range, rounding to nearest.
    let normalized = ((value / range) + 1.0) * 0.5;
    let scaled = normalized * max_code;
    // `clamp` guards NaN-free inputs; the cast saturates separately for safety.
    scaled.round().clamp(0.0, max_code) as u32
}

/// Inverse of [`quantize_signed`].
fn dequantize_signed(code: u32, range: f32, codes: u32) -> f32 {
    let max_code = (codes - 1) as f32;
    let normalized = code as f32 / max_code;
    (normalized * 2.0 - 1.0) * range
}

// --- QVec2 --------------------------------------------------------------------

/// A 2D vector quantized to a fixed-width integer pair over a build-pinned
/// range. Used for both position (over [`POS_RANGE`]) and velocity (over
/// [`VEL_RANGE`]); the range is chosen by the constructor. Stored as `u16` per
/// axis so the encoded width is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct QVec2 {
    /// Quantized x axis code.
    pub x: u16,
    /// Quantized y axis code.
    pub y: u16,
}

impl QVec2 {
    /// Quantize a world-space **position** (sector-relative) over [`POS_RANGE`].
    pub fn quantize_pos(v: Vec2) -> Self {
        Self {
            x: quantize_signed(v.x, POS_RANGE, POS_CODES) as u16,
            y: quantize_signed(v.y, POS_RANGE, POS_CODES) as u16,
        }
    }

    /// Dequantize a value produced by [`QVec2::quantize_pos`] back to sim units.
    pub fn dequantize_pos(self) -> Vec2 {
        Vec2::new(
            dequantize_signed(self.x as u32, POS_RANGE, POS_CODES),
            dequantize_signed(self.y as u32, POS_RANGE, POS_CODES),
        )
    }

    /// Quantize a **velocity** over [`VEL_RANGE`].
    pub fn quantize_vel(v: Vec2) -> Self {
        Self {
            x: quantize_signed(v.x, VEL_RANGE, VEL_CODES) as u16,
            y: quantize_signed(v.y, VEL_RANGE, VEL_CODES) as u16,
        }
    }

    /// Dequantize a value produced by [`QVec2::quantize_vel`] back to sim units/s.
    pub fn dequantize_vel(self) -> Vec2 {
        Vec2::new(
            dequantize_signed(self.x as u32, VEL_RANGE, VEL_CODES),
            dequantize_signed(self.y as u32, VEL_RANGE, VEL_CODES),
        )
    }
}

// --- QAngle -------------------------------------------------------------------

/// A heading angle quantized to [`ANGLE_BITS`] bits over the full circle.
/// Wraps cleanly: any radian value is first reduced to `(-π, π]`, so headings
/// near ±π round-trip without a seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Encode, Decode)]
pub struct QAngle {
    /// Quantized angle code over `0..=2^ANGLE_BITS - 1`.
    pub code: u16,
}

impl QAngle {
    /// Quantize a heading in radians (any magnitude) to a fixed-bit code.
    pub fn quantize(radians: f32) -> Self {
        use core::f32::consts::TAU;
        // Reduce to [0, TAU) so the mapping is monotone and seam-free.
        let mut a = radians % TAU;
        if a < 0.0 {
            a += TAU;
        }
        // Map [0, TAU) onto [0, ANGLE_CODES); modulo guards the TAU edge.
        let code = ((a / TAU) * ANGLE_CODES as f32).round() as u32 % ANGLE_CODES;
        Self { code: code as u16 }
    }

    /// Dequantize back to radians in `(-π, π]`.
    pub fn dequantize(self) -> f32 {
        use core::f32::consts::{PI, TAU};
        let a = (self.code as f32 / ANGLE_CODES as f32) * TAU;
        // Fold [0, TAU) into (-π, π] so the value matches a typical heading.
        if a > PI {
            a - TAU
        } else {
            a
        }
    }
}
