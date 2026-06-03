//! The typed damage packet + the damage-type axis (FR-001).
//!
//! A [`DamageEvent`] is the unit that flows the ordered [`DefenseLayer`] stack
//! (Shields → Armor → Hull → Systems): built from a projectile hit + its
//! `WeaponProfile` channel/pen, it is mitigated at each layer it traverses by the
//! [`ResistanceMatrix`], and its [`Channel`] selects which matrix row applies.
//!
//! [`Channel`] is the 5-variant damage-type axis (data-model.md): each channel is
//! strong against (gets through) one preferred [`DefenseLayer`] and resisted by the
//! others — the non-degenerate property (FR-023, INV-D11) is a test-guarded
//! constraint on the *content* matrix, not on this enum.
//!
//! Derive discipline matches `crate::components` + the E006 fitting domain: serde
//! as a replication (E003) / persistence (E004) seam — present, not exercised this
//! epic; value semantics for round-trip equality. `Channel` is `Copy`. `source`
//! wraps a runtime-local `Entity` (the firing ship), so [`DamageEvent`] is **not**
//! `Serialize`/`Deserialize` — mirroring `ProjectileOwner`'s deliberate opt-out.
//!
//! [`ResistanceMatrix`]: crate::damage::ResistanceMatrix
//! [`DefenseLayer`]: crate::damage::DefenseLayer

use bevy_ecs::entity::Entity;
use glam::Vec2;
use serde::{Deserialize, Serialize};

/// The damage type — the 5-channel matrix axis (FR-001, data-model.md).
///
/// Each channel is **strong against** one preferred defense layer (low mitigation
/// there, it gets through) and resisted by the others, so no channel is globally
/// dominant (INV-D11; a test-guarded property of the content matrix). The variant
/// order is the column order of [`ResistanceMatrix::table`](crate::damage::ResistanceMatrix):
/// `Channel as usize` indexes the matrix.
///
/// `Copy` (the packet carries it by value through every layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// Slugs / penetrators; carries the penetration value + size (overmatch).
    /// Strong against **Armor**.
    Kinetic,
    /// Energy / laser; melts shields, plinks off armor. Strong against **Shields**.
    ThermalEnergy,
    /// Explosive concussion; chews structural HP. Strong against **Hull/Structure**.
    Blast,
    /// Ignores plating to disrupt the device behind. Strong against **Systems**.
    Em,
    /// Like `Em`; degrades the module/electronics behind cover. Strong against
    /// **Systems**.
    Radiation,
}

impl Channel {
    /// The number of channels — the [`ResistanceMatrix`](crate::damage::ResistanceMatrix)
    /// column count. Keep in lock-step with the variant list.
    pub const COUNT: usize = 5;

    /// All channels in matrix-column order, for exhaustive iteration (the
    /// non-degeneracy guard + content tests walk this).
    pub const ALL: [Channel; Self::COUNT] = [
        Channel::Kinetic,
        Channel::ThermalEnergy,
        Channel::Blast,
        Channel::Em,
        Channel::Radiation,
    ];

    /// This channel's column index into [`ResistanceMatrix::table`](crate::damage::ResistanceMatrix).
    /// Stable and equal to `self as usize`; provided as a named accessor so call
    /// sites do not cast the enum directly.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// The typed damage packet — the unit that flows the layers (FR-001).
///
/// Built from a projectile hit + its `WeaponProfile` channel/pen. `magnitude` is
/// the base damage **before** any layer mitigation; `penetration`/`pen_size` drive
/// the armor gate ([`resolve_penetration`](crate::damage::resolve_penetration)),
/// where `pen_size` vs plate thickness is the overmatch test (INV-D04).
/// `point`/`dir` are the entry geometry (hull-local) the armor angle is read from.
///
/// `source` is the firing ship (for single-resolution wreck claiming); damage
/// applies regardless of source (friendly fire). Because it wraps a runtime-local
/// `Entity`, the whole packet opts out of serde (like `ProjectileOwner`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageEvent {
    /// Damage type; selects the matrix row (FR-001).
    pub channel: Channel,
    /// Base damage before any layer mitigation (`>= 0`, FR-001).
    pub magnitude: f32,
    /// Penetration value vs effective armor (`>= 0`, FR-005/008).
    pub penetration: f32,
    /// Penetrator size for the overmatch test vs plate thickness (`>= 0`, FR-007).
    pub pen_size: f32,
    /// Where it struck (hull-local; the `resolve_hit` entry geometry). Finite.
    pub point: Vec2,
    /// Incoming direction; with the surface normal gives the impact angle
    /// (FR-005). Finite, ~unit.
    pub dir: Vec2,
    /// The firing ship (`ProjectileOwner`); runtime-local, not serialized. `None`
    /// for a sourceless hit (e.g. environmental). Damage applies regardless.
    pub source: Option<Entity>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_index_matches_variant_order() {
        assert_eq!(Channel::Kinetic.index(), 0);
        assert_eq!(Channel::ThermalEnergy.index(), 1);
        assert_eq!(Channel::Blast.index(), 2);
        assert_eq!(Channel::Em.index(), 3);
        assert_eq!(Channel::Radiation.index(), 4);
        assert_eq!(Channel::COUNT, Channel::ALL.len());
    }

    #[test]
    fn channel_round_trips_through_serde() {
        for ch in Channel::ALL {
            let json = serde_json::to_string(&ch).unwrap();
            let back: Channel = serde_json::from_str(&json).unwrap();
            assert_eq!(ch, back);
        }
    }

    #[test]
    fn damage_event_constructs_and_reads_back() {
        let ev = DamageEvent {
            channel: Channel::Kinetic,
            magnitude: 100.0,
            penetration: 50.0,
            pen_size: 2.0,
            point: Vec2::new(1.0, 2.0),
            dir: Vec2::new(1.0, 0.0),
            source: None,
        };
        assert_eq!(ev.channel, Channel::Kinetic);
        assert_eq!(ev.magnitude, 100.0);
        assert_eq!(ev.dir, Vec2::new(1.0, 0.0));
    }
}
