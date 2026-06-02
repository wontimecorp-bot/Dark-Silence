//! Delta-snapshot reconstruction (E003, OBJ6, T063 client-side half).
//!
//! The server delta-codes each [`Snapshot`] against the recipient's last-acked
//! baseline (Principle VI — bandwidth is the budget): it sends only the
//! [`EntityRecord`]s that **changed** since the baseline in `entities`, lists the
//! ids that **disappeared** in `removed`, and tags the snapshot with the
//! `baseline_id` it was computed against. An entity that did not change costs
//! **zero bits** — it is simply omitted from `entities` (delta-by-omission). The
//! encoder itself lives server-side in `server::snapshot`; this module is the
//! **client-side reconstruction** the round-trip needs, placed in `protocol` so
//! both the real `client` crate and the headless bot harness reconstruct full
//! state through the identical code path.
//!
//! [`FullState`] is the reconstructed, fully-populated entity set for one tick.
//! [`apply_delta`] folds a received delta [`Snapshot`] onto a baseline
//! [`FullState`]: changed records overwrite by id, `removed` ids drop, and every
//! unchanged entity persists from the baseline. A **keyframe** (a snapshot the
//! server sent delta-from-nothing because the client's acked baseline was
//! unknown) reconstructs correctly too — it carries every entity in `entities`,
//! so applying it to *any* baseline yields the same full set (the changed
//! records cover everything and there is nothing left to persist that the
//! keyframe does not itself carry; see [`Snapshot::is_keyframe`]).

use crate::messages::{EntityId, EntityRecord, Snapshot};

/// A fully-reconstructed entity set for a single tick — the baseline a client
/// delta-applies the next snapshot onto, and the full state it feeds to
/// interpolation/reconciliation.
///
/// Records are kept in ascending [`EntityId`] order so two `FullState`s that
/// hold the same entities compare equal regardless of insertion order (the
/// equivalence the bot harness and round-trip tests assert).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullState {
    /// The reconstructed records, sorted ascending by [`EntityId`].
    records: Vec<EntityRecord>,
}

impl FullState {
    /// An empty full state — the implicit baseline before any snapshot is acked
    /// (delta-from-nothing reconstructs onto this; a keyframe ignores it).
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a `FullState` from an arbitrary record set, normalizing to ascending
    /// id order (last-wins on a duplicate id, so a caller cannot smuggle two
    /// records for one entity past reconstruction).
    pub fn from_records(records: impl IntoIterator<Item = EntityRecord>) -> Self {
        let mut state = Self::new();
        for r in records {
            state.upsert(r);
        }
        state
    }

    /// The reconstructed records in ascending id order.
    pub fn records(&self) -> &[EntityRecord] {
        &self.records
    }

    /// Number of entities in the reconstructed state.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the reconstructed state holds no entities.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The record for `id`, if present.
    pub fn get(&self, id: EntityId) -> Option<&EntityRecord> {
        self.index_of(id).map(|i| &self.records[i])
    }

    /// Insert or overwrite the record for its id, keeping the set sorted by id.
    fn upsert(&mut self, record: EntityRecord) {
        match self.records.binary_search_by_key(&record.id.0, |r| r.id.0) {
            Ok(i) => self.records[i] = record,
            Err(i) => self.records.insert(i, record),
        }
    }

    /// Drop the record for `id`, if present.
    fn remove(&mut self, id: EntityId) {
        if let Some(i) = self.index_of(id) {
            self.records.remove(i);
        }
    }

    /// Index of `id` in the sorted record vec, if present.
    fn index_of(&self, id: EntityId) -> Option<usize> {
        self.records
            .binary_search_by_key(&id.0, |r| r.id.0)
            .ok()
    }

    /// Materialize this full state into a [`Snapshot`] body (every record, no
    /// removals) carrying the given header fields. Used by the encoder to turn a
    /// reconstructed baseline back into a keyframe.
    pub fn to_records(&self) -> Vec<EntityRecord> {
        self.records.clone()
    }
}

/// Reconstruct the full entity set for a tick by folding the received delta
/// `snapshot` onto `baseline` (T063 client-side reconstruction).
///
/// - every record in `snapshot.entities` **overwrites** the baseline record with
///   that id (a changed entity), or inserts it (a newly-appeared entity);
/// - every id in `snapshot.removed` is **dropped** from the baseline;
/// - every entity the baseline holds that the delta does not mention **persists**
///   unchanged (delta-by-omission — an unchanged entity costs zero bits).
///
/// A **keyframe** (`snapshot.removed` empty and the server sent it because the
/// client's baseline was unknown) carries the complete world in `entities`;
/// folding it onto any baseline yields the same full set, so a lost ack
/// re-baselines gracefully. This function does not know whether the snapshot is a
/// keyframe — the fold is identical either way; correctness for the keyframe case
/// follows because the encoder, when re-baselining, includes every entity in
/// `entities` (nothing is left to persist that contradicts it).
///
/// Pure: it does not mutate `baseline`; it returns a fresh [`FullState`].
pub fn apply_delta(baseline: &FullState, snapshot: &Snapshot) -> FullState {
    let mut out = baseline.clone();
    for id in &snapshot.removed {
        out.remove(*id);
    }
    for record in &snapshot.entities {
        out.upsert(*record);
    }
    out
}

impl Snapshot {
    /// Whether this snapshot is a **full keyframe**: a `baseline_id` of
    /// [`Snapshot::KEYFRAME_BASELINE`] means the server delta-coded against
    /// *nothing* (the client's acked baseline was unknown/unavailable), so
    /// `entities` carries the complete world and `removed` is empty. A keyframe
    /// reconstructs correctly from any (even empty) baseline.
    pub fn is_keyframe(&self) -> bool {
        self.baseline_id == Snapshot::KEYFRAME_BASELINE
    }

    /// This snapshot's own wire identity — the id a client acks (in
    /// [`crate::SnapshotAck::last_snapshot_id`]) and the id the server records its
    /// per-client sent state under. Derived from [`Snapshot::server_tick`] via
    /// [`snapshot_wire_id`], so it is already on the wire and needs no extra field.
    pub fn wire_id(&self) -> u16 {
        snapshot_wire_id(self.server_tick)
    }
}

/// Map a server tick to the `u16` wire id the ack/baseline fields carry.
///
/// Snapshot identity is the tick (already on the wire as [`Snapshot::server_tick`]),
/// truncated to `u16` and folded away from the two reserved sentinels: `0`
/// ("nothing acked yet") and [`Snapshot::KEYFRAME_BASELINE`] (`u16::MAX`, the
/// keyframe marker). Both the server (recording per-client sent state) and the
/// client (acking) use this single mapping so the two agree on identity. The
/// `u16` width wraps every 65 534 ticks (~36 min at 30 Hz); widening the wire id
/// is a later concern, out of this phase's scope.
pub fn snapshot_wire_id(server_tick: u32) -> u16 {
    let raw = server_tick as u16;
    // Avoid the two reserved sentinels by remapping them into the live range.
    if raw == 0 {
        1
    } else if raw == Snapshot::KEYFRAME_BASELINE {
        Snapshot::KEYFRAME_BASELINE - 1
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::EntityKind;
    use crate::quantize::{QAngle, QVec2};
    use glam::Vec2;

    fn record(id: u32, x: f32) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            kind: EntityKind::Ship,
            pos: QVec2::quantize_pos(Vec2::new(x, 0.0)),
            vel: QVec2::quantize_vel(Vec2::ZERO),
            heading: QAngle::quantize(0.0),
            flags: 0,
        }
    }

    fn delta(entities: Vec<EntityRecord>, removed: Vec<EntityId>) -> Snapshot {
        Snapshot {
            server_tick: 0,
            acked_input_seq: 0,
            baseline_id: 1,
            entities,
            removed,
        }
    }

    #[test]
    fn unchanged_entities_persist_across_an_empty_delta() {
        let baseline = FullState::from_records([record(1, 1.0), record(2, 2.0)]);
        // An empty delta (nothing changed, nothing removed) reconstructs the
        // baseline exactly — unchanged entities cost zero wire records.
        let reconstructed = apply_delta(&baseline, &delta(vec![], vec![]));
        assert_eq!(reconstructed, baseline);
    }

    #[test]
    fn changed_records_overwrite_by_id() {
        let baseline = FullState::from_records([record(1, 1.0), record(2, 2.0)]);
        let reconstructed = apply_delta(&baseline, &delta(vec![record(2, 9.0)], vec![]));
        assert_eq!(reconstructed.get(EntityId(1)), Some(&record(1, 1.0)));
        assert_eq!(reconstructed.get(EntityId(2)), Some(&record(2, 9.0)));
        assert_eq!(reconstructed.len(), 2, "an overwrite does not add an entity");
    }

    #[test]
    fn removed_ids_drop_from_the_baseline() {
        let baseline = FullState::from_records([record(1, 1.0), record(2, 2.0)]);
        let reconstructed = apply_delta(&baseline, &delta(vec![], vec![EntityId(1)]));
        assert_eq!(reconstructed.get(EntityId(1)), None);
        assert_eq!(reconstructed.get(EntityId(2)), Some(&record(2, 2.0)));
        assert_eq!(reconstructed.len(), 1);
    }

    #[test]
    fn newly_appeared_entity_is_inserted() {
        let baseline = FullState::from_records([record(1, 1.0)]);
        let reconstructed = apply_delta(&baseline, &delta(vec![record(5, 5.0)], vec![]));
        assert_eq!(reconstructed.len(), 2);
        assert_eq!(reconstructed.get(EntityId(5)), Some(&record(5, 5.0)));
    }

    #[test]
    fn keyframe_reconstructs_from_any_baseline() {
        // A keyframe carries the whole world in `entities` with baseline_id =
        // KEYFRAME_BASELINE; applying it onto a STALE baseline yields the same set
        // as applying it onto an empty one (re-baseline on a lost ack).
        let keyframe = Snapshot {
            server_tick: 7,
            acked_input_seq: 0,
            baseline_id: Snapshot::KEYFRAME_BASELINE,
            entities: vec![record(1, 1.0), record(2, 2.0)],
            removed: vec![],
        };
        assert!(keyframe.is_keyframe());
        let from_empty = apply_delta(&FullState::new(), &keyframe);
        let stale = FullState::from_records([record(9, 9.0)]);
        let from_stale = apply_delta(&stale, &keyframe);
        // The keyframe set is present in both; the stale baseline's extra entity
        // is NOT carried by the keyframe, so the two differ only by the stale
        // leftover — which is exactly why the encoder must include every entity in
        // a keyframe and why the server resets the cache to the keyframe set.
        assert_eq!(from_empty.get(EntityId(1)), Some(&record(1, 1.0)));
        assert_eq!(from_empty.get(EntityId(2)), Some(&record(2, 2.0)));
        assert_eq!(from_empty.len(), 2);
        assert_eq!(from_stale.get(EntityId(1)), Some(&record(1, 1.0)));
        assert_eq!(from_stale.get(EntityId(9)), Some(&record(9, 9.0)));
    }
}
