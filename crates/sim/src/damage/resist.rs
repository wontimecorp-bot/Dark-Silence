//! The ordered defense-layer stack + the (layer × channel) mitigation table
//! (FR-003/004/022).
//!
//! A [`DamageEvent`](crate::damage::DamageEvent) traverses the ordered absorber
//! stack **Shields → Armor → Hull/Structure → Systems** ([`DefenseLayer`]); at each
//! layer the [`ResistanceMatrix`] removes a flat fraction of the surviving
//! magnitude for that `(layer, channel)` pair via [`layer_resist`].
//!
//! The matrix is **content, not code** (FR-022): the table is a `Resource`
//! const-seeded from [`default_resistance_matrix`](crate::damage::default_resistance_matrix)
//! and tunable without touching logic. Every cell is bounded `∈ [0.0, 1.0)`
//! (INV-D02): `1.0` (total immunity) and `< 0` (amplification) are both forbidden,
//! so a layer always lets *some* damage through and never amplifies. The
//! non-degenerate property (every channel beats a layer, every layer resists a
//! channel — INV-D11) is a **test-guarded** constraint on the seeded content, not
//! enforced by this lookup.
//!
//! Derive discipline matches the rest of the domain: serde as a replication
//! (E003) / persistence (E004) seam — present, not exercised this epic; value
//! semantics; `Resource` so it lives as a `bevy_ecs` singleton read by the damage
//! systems. The matrix is immutable at runtime (content reload only).

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use super::event::Channel;

/// The maximum mitigation any cell may hold, strictly `< 1.0` (INV-D02): no
/// `(layer, channel)` pair is a free pass / total immunity. The seed content and
/// any tuned override must keep every cell `∈ [0.0, MAX_MITIGATION]`.
pub const MAX_MITIGATION: f32 = 0.85;

/// The ordered absorber stack a [`DamageEvent`](crate::damage::DamageEvent)
/// traverses, outer → inner (FR-003, data-model.md). Each layer is a **row** of
/// [`ResistanceMatrix::table`]: `DefenseLayer as usize` indexes it.
///
/// The order is the in-scope subset of ADR-0008's full stack (the outer
/// Avoidance/PD/ECM layer is E010; Crew is later). `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DefenseLayer {
    /// Regenerating power-linked pool; absorbs first. Strong vs `ThermalEnergy`.
    Shields,
    /// Angle-based penetration gate (per hull section). Strong vs `Kinetic`.
    Armor,
    /// Structural HP backstop. Strong vs `Blast`.
    HullStructure,
    /// The module struck (the device behind). Strong-resisted by `Em`/`Radiation`.
    Systems,
}

impl DefenseLayer {
    /// The number of layers — the [`ResistanceMatrix::table`] row count. Keep in
    /// lock-step with the variant list.
    pub const COUNT: usize = 4;

    /// All layers in traversal (and matrix-row) order, for exhaustive iteration.
    pub const ALL: [DefenseLayer; Self::COUNT] = [
        DefenseLayer::Shields,
        DefenseLayer::Armor,
        DefenseLayer::HullStructure,
        DefenseLayer::Systems,
    ];

    /// This layer's row index into [`ResistanceMatrix::table`]. Stable and equal
    /// to `self as usize`; a named accessor so call sites do not cast directly.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// The (layer × channel) flat-% mitigation lookup table (FR-004, data-model.md).
///
/// `table[layer][channel]` is the fraction of magnitude the layer removes for that
/// channel: `surviving = magnitude * (1.0 - mitigation)`. Strong-vs pairings are
/// **low** mitigation (the channel gets through); resisted pairings are **high**.
/// Every cell is `∈ [0.0, MAX_MITIGATION < 1.0]` (INV-D02). A `Resource`
/// const-seeded from content (FR-022); immutable at runtime.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResistanceMatrix {
    /// Row-major `[DefenseLayer::COUNT][Channel::COUNT]`; each cell `∈ [0, <1)`.
    pub table: [[f32; Channel::COUNT]; DefenseLayer::COUNT],
}

impl ResistanceMatrix {
    /// The mitigation fraction for one `(layer, channel)` cell — the data-driven
    /// lookup (FR-004). Equivalent to the free [`layer_resist`] function; provided
    /// as a method for ergonomic call sites. Always `∈ [0, 1)` for valid content.
    pub fn mitigation(&self, layer: DefenseLayer, channel: Channel) -> f32 {
        self.table[layer.index()][channel.index()]
    }

    /// INV-D02 bounds check: every cell `∈ [0.0, MAX_MITIGATION]` (so `< 1.0`,
    /// finite, non-negative). The matrix-load validation + the content test assert
    /// this; no cell may be a free pass or amplify.
    pub fn is_bounded(&self) -> bool {
        self.table
            .iter()
            .flatten()
            .all(|&m| m.is_finite() && (0.0..=MAX_MITIGATION).contains(&m))
    }
}

/// The data-driven mitigation fraction `∈ [0, 1)` for one `(layer, channel)` cell
/// (FR-004/022, contracts/damage-api.md §1 `layer_resist`).
///
/// `surviving = magnitude * (1.0 - layer_resist(..))`. Total over the matrix (every
/// cell is defined); the non-degeneracy guard (FR-023, INV-D11) is a **test** over
/// this table, not a runtime branch. Pure; never panics for an in-bounds matrix.
pub fn layer_resist(matrix: &ResistanceMatrix, layer: DefenseLayer, channel: Channel) -> f32 {
    matrix.mitigation(layer, channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::damage::content::default_resistance_matrix;

    #[test]
    fn layer_and_channel_indices_match_variant_order() {
        assert_eq!(DefenseLayer::Shields.index(), 0);
        assert_eq!(DefenseLayer::Armor.index(), 1);
        assert_eq!(DefenseLayer::HullStructure.index(), 2);
        assert_eq!(DefenseLayer::Systems.index(), 3);
        assert_eq!(DefenseLayer::COUNT, DefenseLayer::ALL.len());
    }

    #[test]
    fn layer_resist_reads_the_seeded_table() {
        let matrix = default_resistance_matrix();
        // The free function and the method agree, and both index the same cell.
        for layer in DefenseLayer::ALL {
            for channel in Channel::ALL {
                let viafn = layer_resist(&matrix, layer, channel);
                let viamethod = matrix.mitigation(layer, channel);
                assert_eq!(viafn, viamethod);
                assert!((0.0..1.0).contains(&viafn));
            }
        }
    }

    #[test]
    fn seeded_matrix_is_bounded() {
        assert!(default_resistance_matrix().is_bounded());
    }
}
