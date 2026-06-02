//! T017 {TR-013, TR-045} — quantization round-trips within the documented field
//! tolerance across representative values (including bounds), and the encoded
//! size of a fixed message shape is deterministic per build (same input → same
//! byte length).

use glam::Vec2;
use protocol::quantize::{QAngle, QVec2};
use protocol::{
    EntityId, EntityKind, EntityRecord, Message, Snapshot, ANGLE_TOLERANCE, POS_RANGE,
    POS_TOLERANCE, VEL_RANGE, VEL_TOLERANCE,
};

#[test]
fn position_roundtrips_within_tolerance() {
    let samples = [
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, -1.0),
        Vec2::new(POS_RANGE, POS_RANGE),
        Vec2::new(-POS_RANGE, -POS_RANGE),
        Vec2::new(POS_RANGE / 2.0, -POS_RANGE / 3.0),
        Vec2::new(123.456, -789.012),
    ];
    for v in samples {
        let back = QVec2::quantize_pos(v).dequantize_pos();
        assert!(
            (back.x - v.x).abs() <= POS_TOLERANCE + f32::EPSILON,
            "pos x out of tolerance: {v:?} -> {back:?} (tol {POS_TOLERANCE})"
        );
        assert!(
            (back.y - v.y).abs() <= POS_TOLERANCE + f32::EPSILON,
            "pos y out of tolerance: {v:?} -> {back:?} (tol {POS_TOLERANCE})"
        );
    }
}

#[test]
fn velocity_roundtrips_within_tolerance() {
    let samples = [
        Vec2::new(0.0, 0.0),
        Vec2::new(VEL_RANGE, -VEL_RANGE),
        Vec2::new(-VEL_RANGE, VEL_RANGE),
        Vec2::new(12.34, -56.78),
    ];
    for v in samples {
        let back = QVec2::quantize_vel(v).dequantize_vel();
        assert!(
            (back.x - v.x).abs() <= VEL_TOLERANCE + f32::EPSILON,
            "vel x out of tolerance: {v:?} -> {back:?} (tol {VEL_TOLERANCE})"
        );
        assert!(
            (back.y - v.y).abs() <= VEL_TOLERANCE + f32::EPSILON,
            "vel y out of tolerance: {v:?} -> {back:?} (tol {VEL_TOLERANCE})"
        );
    }
}

#[test]
fn position_clamps_out_of_range() {
    // Beyond the range, quantization saturates to the nearest edge rather than
    // wrapping. The decoded value stays within tolerance of the clamped edge.
    let v = Vec2::new(POS_RANGE * 4.0, -POS_RANGE * 4.0);
    let back = QVec2::quantize_pos(v).dequantize_pos();
    assert!((back.x - POS_RANGE).abs() <= POS_TOLERANCE + f32::EPSILON);
    assert!((back.y + POS_RANGE).abs() <= POS_TOLERANCE + f32::EPSILON);
}

#[test]
fn angle_roundtrips_within_tolerance() {
    use core::f32::consts::PI;
    let samples = [
        0.0,
        PI / 4.0,
        PI / 2.0,
        -PI / 2.0,
        PI - 0.001,
        -PI + 0.001,
        2.0,
        -2.0,
    ];
    for a in samples {
        let back = QAngle::quantize(a).dequantize();
        // Compare on the circle: smallest signed angular difference.
        let mut diff = (back - a) % core::f32::consts::TAU;
        if diff > PI {
            diff -= core::f32::consts::TAU;
        } else if diff < -PI {
            diff += core::f32::consts::TAU;
        }
        assert!(
            diff.abs() <= ANGLE_TOLERANCE + f32::EPSILON,
            "angle out of tolerance: {a} -> {back} (diff {diff}, tol {ANGLE_TOLERANCE})"
        );
    }
}

#[test]
fn angle_wraps_near_pi() {
    use core::f32::consts::PI;
    // Headings just past +π should land near −π, not jump to a far value.
    let a = PI + 0.01;
    let back = QAngle::quantize(a).dequantize();
    assert!(back < 0.0, "angle past +pi should wrap to negative: {back}");
}

/// A fixed snapshot shape must encode to a fixed byte length on a given build:
/// quantized fields are fixed-width, so identical input → identical size (and a
/// second identical message encodes to the same length).
#[test]
fn fixed_snapshot_encodes_to_deterministic_size() {
    let make = || {
        Message::Snapshot(Snapshot {
            server_tick: 1000,
            acked_input_seq: 42,
            baseline_id: 3,
            entities: vec![
                EntityRecord {
                    id: EntityId(1),
                    kind: EntityKind::Ship,
                    pos: QVec2::quantize_pos(Vec2::new(10.0, 20.0)),
                    vel: QVec2::quantize_vel(Vec2::new(1.0, 2.0)),
                    heading: QAngle::quantize(0.7),
                    flags: 1,
                },
                EntityRecord {
                    id: EntityId(2),
                    kind: EntityKind::Target,
                    pos: QVec2::quantize_pos(Vec2::new(-5.0, 5.0)),
                    vel: QVec2::quantize_vel(Vec2::new(0.0, 0.0)),
                    heading: QAngle::quantize(-1.0),
                    flags: 0,
                },
            ],
            removed: vec![EntityId(7)],
        })
    };

    let len_a = make().encode().len();
    let len_b = make().encode().len();
    assert_eq!(
        len_a, len_b,
        "identical snapshot must encode to the same byte length"
    );

    // Same entity COUNT but different quantized field VALUES must keep the same
    // length, because quantized fields are fixed-width (deterministic size).
    let other = Message::Snapshot(Snapshot {
        server_tick: 1000,
        acked_input_seq: 42,
        baseline_id: 3,
        entities: vec![
            EntityRecord {
                id: EntityId(1),
                kind: EntityKind::Ship,
                pos: QVec2::quantize_pos(Vec2::new(-100.0, 200.0)),
                vel: QVec2::quantize_vel(Vec2::new(-3.0, 4.0)),
                heading: QAngle::quantize(2.9),
                flags: 1,
            },
            EntityRecord {
                id: EntityId(2),
                kind: EntityKind::Target,
                pos: QVec2::quantize_pos(Vec2::new(50.0, -50.0)),
                vel: QVec2::quantize_vel(Vec2::new(9.0, -9.0)),
                heading: QAngle::quantize(0.1),
                flags: 0,
            },
        ],
        removed: vec![EntityId(7)],
    });
    assert_eq!(
        len_a,
        other.encode().len(),
        "fixed-width quantized fields must give a value-independent size"
    );
}
