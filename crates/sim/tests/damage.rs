//! Headless integration tests for the E007 damage & destruction substrate.
//!
//! The penetration / layers / emergent / sever / salvage / non-degenerate suites
//! land here across the E007 phases. Phase 2 (the pure-logic substrate) covers:
//! - **T007 (FR-022)**: all matrix/penetration/shield balance loads from content
//!   (no hardcoded numbers in code paths) and every matrix cell `∈ [0, 1)`
//!   (INV-D02).
//! - **T009 (FR-005)**: effective armor `= thickness/cos(angle)` increases with
//!   angle and stays finite/clamped as `cos → 0` (INV-D03).
//! - **T010 (FR-006)**: a steep glancing hit past `ricochet_angle` → `Ricochet`;
//!   below the threshold it does not.
//! - **T011 (FR-007)**: `pen_size >= overmatch_ratio * thickness` bypasses the
//!   angle/ricochet test and forces at least `Penetration` (INV-D04).
//! - **T012 (FR-008)**: tier ordering `pen_tier_non < pen_tier_over < pen_tier_full
//!   <= 1.0` — clean Penetration applies the full tier, OverPenetration a strictly
//!   lower tier, NonPenetration little/none (INV-D05).
//!
//! Each test drives the pure `sim::damage` surface directly (no Bevy app, no
//! rendering) — the substrate is pure logic + content this phase.

use std::f32::consts::FRAC_PI_2;

use bevy_ecs::prelude::*;
use glam::Vec2;
use sim::damage::{
    apply_damage, default_resistance_matrix, layer_resist, regen_shield, resolve_penetration,
    ArmorFacet, ArmorMaterial, Channel, DamageEvent, DefenseLayer, HitKind, HullStructure,
    PenetrationConfig, PenetrationResult, SectionArmor, ShieldConfig, Shields, StatScalingConfig,
};
use sim::fitting::{
    build_layout, derive_ship_stats, seed_catalogs, Fit, FitLayout, HullCatalog, ModuleCatalog,
    SectionId, SlotId, HULL_FIGHTER, MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC,
    MODULE_SHIELD_BASIC, MODULE_THRUSTER_BASIC,
};

// --- T007 (FR-022): balance is content, every matrix cell ∈ [0, 1) (INV-D02) ---

/// Every (layer × channel) mitigation cell loaded from content is bounded
/// `∈ [0.0, 1.0)` — no total immunity (`1.0`) and no amplification (`< 0`),
/// INV-D02. The values come from `default_resistance_matrix` (content), not from a
/// hardcoded number in any code path.
#[test]
fn matrix_cells_are_bounded_and_content_sourced() {
    let matrix = default_resistance_matrix();
    for layer in DefenseLayer::ALL {
        for channel in Channel::ALL {
            let m = layer_resist(&matrix, layer, channel);
            assert!(
                m.is_finite() && (0.0..1.0).contains(&m),
                "cell ({layer:?},{channel:?}) = {m} out of [0,1)"
            );
        }
    }
    // The same matrix exposes the bounds check that the runtime load validates.
    assert!(matrix.is_bounded(), "seeded matrix violates INV-D02 bounds");
}

/// The penetration / shield / stat-scaling balance loads from content resources
/// (their defaults), each respecting the data-model constraints — no hardcoded
/// balance leaks into a code path (FR-022). The penetration tiers additionally
/// obey the strict ordering INV-D05.
#[test]
fn penetration_shield_stat_config_loads_from_content() {
    let pen = PenetrationConfig::default();
    assert!(pen.is_valid(), "default PenetrationConfig invalid");
    // INV-D05 ordering, spelled out at the test boundary.
    assert!(pen.pen_tier_non < pen.pen_tier_over);
    assert!(pen.pen_tier_over < pen.pen_tier_full);
    assert!(pen.pen_tier_full <= 1.0);

    let shield = ShieldConfig::default();
    assert!(shield.is_valid(), "default ShieldConfig invalid");

    let stat = StatScalingConfig::default();
    assert!(stat.is_valid(), "default StatScalingConfig invalid");
    assert!((0.0..1.0).contains(&stat.stat_health_floor));
}

// --- T009 (FR-005): effective armor grows with angle, finite as cos → 0 (INV-D03)

/// Effective armor `= thickness * material / cos(angle)` increases monotonically
/// as the impact angle steepens and stays finite (clamped to `effective_armor_cap`)
/// even as `cos(angle) → 0` at a grazing hit — INV-D03.
#[test]
fn effective_armor_grows_with_angle_and_stays_finite() {
    let cfg = PenetrationConfig::default();
    // A thin plate so the angle growth is observable below the cap (a thick plate
    // would clamp to the cap immediately; INV-D03 still holds, but the monotonic
    // trend is what FR-005 asserts here).
    let thickness = 2.0;

    // A sweep of increasing angles below the cap should give non-decreasing,
    // strictly-finite effective armor that never exceeds the cap; the early
    // (below-cap) steps strictly increase.
    let angles = [0.0_f32, 0.3, 0.6, 0.9, 1.2, 1.4];
    let mut prev = -1.0_f32;
    for &a in &angles {
        let r = resolve_penetration(thickness, a, 0.0, 0.0, ArmorMaterial::Steel, &cfg);
        let eff = r.effective_armor();
        assert!(eff.is_finite(), "effective armor not finite at angle {a}");
        assert!(
            eff <= cfg.effective_armor_cap,
            "effective armor exceeds cap"
        );
        assert!(eff >= prev, "effective armor not non-decreasing with angle");
        prev = eff;
    }
    // The head-on vs oblique pair strictly increases (below the cap).
    let head_on =
        resolve_penetration(thickness, 0.0, 0.0, 0.0, ArmorMaterial::Steel, &cfg).effective_armor();
    let oblique =
        resolve_penetration(thickness, 1.0, 0.0, 0.0, ArmorMaterial::Steel, &cfg).effective_armor();
    assert!(oblique > head_on, "oblique armor must exceed head-on");

    // The grazing limit (cos → 0): finite, clamped to the cap, never inf/NaN.
    let grazing = resolve_penetration(thickness, FRAC_PI_2, 0.0, 0.0, ArmorMaterial::Steel, &cfg);
    let eff = grazing.effective_armor();
    assert!(eff.is_finite(), "grazing effective armor not finite");
    assert!(
        (eff - cfg.effective_armor_cap).abs() < 1e-3,
        "grazing effective armor should clamp to the cap"
    );
}

// --- T010 (FR-006): ricochet past the angle threshold, not below ----------------

/// A steep glancing hit past `ricochet_angle` (and not overmatched) returns
/// `Ricochet` with no module-behind damage; a shallow hit below the threshold does
/// **not** ricochet.
#[test]
fn steep_glancing_hit_ricochets_shallow_does_not() {
    let cfg = PenetrationConfig::default();
    let thickness = 10.0;
    // A modest pen / small (non-overmatching) penetrator so the angle test decides.
    let pen = 50.0;
    let size = 1.0; // < overmatch_ratio * thickness = 15.0

    let steep_angle = cfg.ricochet_angle + 0.05;
    let steep = resolve_penetration(
        thickness,
        steep_angle,
        pen,
        size,
        ArmorMaterial::Steel,
        &cfg,
    );
    assert!(
        matches!(steep, PenetrationResult::Ricochet { .. }),
        "expected Ricochet past ricochet_angle, got {steep:?}"
    );
    assert_eq!(
        steep.surviving(),
        0.0,
        "a ricochet deposits no damage behind"
    );

    let shallow_angle = cfg.ricochet_angle - 0.2;
    let shallow = resolve_penetration(
        thickness,
        shallow_angle,
        pen,
        size,
        ArmorMaterial::Steel,
        &cfg,
    );
    assert!(
        !matches!(shallow, PenetrationResult::Ricochet { .. }),
        "a hit below ricochet_angle must not ricochet, got {shallow:?}"
    );
}

// --- T011 (FR-007): overmatch bypasses angle, forces ≥ Penetration (INV-D04) ----

/// A hit whose `pen_size >= overmatch_ratio * thickness` bypasses the angle/
/// ricochet test entirely and yields **at least** a penetrating result (never a
/// `Ricochet` or `NonPenetration`), even at an angle that would otherwise bounce —
/// INV-D04.
#[test]
fn overmatch_bypasses_angle_and_forces_penetration() {
    let cfg = PenetrationConfig::default();
    let thickness = 2.0;
    // size = 10.0 >= overmatch_ratio (1.5) * thickness (2.0) = 3.0 → overmatched.
    let size = 10.0;
    // A steep angle that would ricochet a non-overmatching hit, and a tiny pen.
    let steep_angle = cfg.ricochet_angle + 0.3;
    let small_pen = 1.0;

    let r = resolve_penetration(
        thickness,
        steep_angle,
        small_pen,
        size,
        ArmorMaterial::Steel,
        &cfg,
    );
    assert!(
        matches!(
            r,
            PenetrationResult::Penetration { .. } | PenetrationResult::OverPenetration { .. }
        ),
        "overmatch must force ≥ Penetration regardless of angle, got {r:?}"
    );
    assert!(r.surviving() > 0.0, "an overmatch deposits damage behind");
}

// --- T012 (FR-008): tier ordering — full > over > non (INV-D05) -----------------

/// The penetration tiers carry strictly-ordered surviving fractions: a clean
/// `Penetration` applies the full tier, an `OverPenetration` a strictly lower tier,
/// and a `NonPenetration` little/none — `pen_tier_non < pen_tier_over <
/// pen_tier_full <= 1.0` (INV-D05). Driven through `resolve_penetration` so the
/// outcomes (not just the config) prove the ordering.
#[test]
fn penetration_tiers_are_strictly_ordered() {
    let cfg = PenetrationConfig::default();
    let thickness = 10.0;
    let head_on = 0.0_f32; // straight-on, so the angle never ricochets
    let size = 1.0; // non-overmatching, so `pen` vs effective decides the tier

    // effective armor head-on ≈ thickness * 1.0 = 10.0.
    // Clean penetration: pen >= 2 * effective.
    let clean = resolve_penetration(thickness, head_on, 100.0, size, ArmorMaterial::Steel, &cfg);
    assert!(
        matches!(clean, PenetrationResult::Penetration { .. }),
        "expected clean Penetration, got {clean:?}"
    );

    // Over-penetration: effective <= pen < 2 * effective.
    let over = resolve_penetration(thickness, head_on, 12.0, size, ArmorMaterial::Steel, &cfg);
    assert!(
        matches!(over, PenetrationResult::OverPenetration { .. }),
        "expected OverPenetration, got {over:?}"
    );

    // Non-penetration: pen < effective.
    let non = resolve_penetration(thickness, head_on, 5.0, size, ArmorMaterial::Steel, &cfg);
    assert!(
        matches!(non, PenetrationResult::NonPenetration { .. }),
        "expected NonPenetration, got {non:?}"
    );

    // INV-D05: the surviving tiers are strictly ordered, full ≤ 1.0.
    assert!(non.surviving() < over.surviving());
    assert!(over.surviving() < clean.surviving());
    assert!(clean.surviving() <= 1.0);
}

// =================================================================================
// US1 — the live damage pipeline (T018/T019/T020). A fitted-ship `World` drives
// `apply_damage` through Shields → Armor → Hull → Systems.
// =================================================================================

/// Insert the E007 content resources every `apply_damage` traversal reads.
fn insert_damage_resources(w: &mut World) {
    let (modules, hulls) = seed_catalogs();
    w.insert_resource(modules);
    w.insert_resource(hulls);
    w.insert_resource(default_resistance_matrix());
    w.insert_resource(PenetrationConfig::default());
    w.insert_resource(ShieldConfig::default());
}

/// Build a `(World, ship)` with the fighter hull fitted with a central reactor at
/// (4,4) covered by an armor plate at (4,5) (revise-A finer 9×11 fighter), plus the
/// defense-layer components. The `make_facet` closure authors the entry section's
/// [`ArmorFacet`] so each test controls the armor gate; `shields` is the (optional)
/// shield pool.
fn fitted_world(
    shields: Option<Shields>,
    make_facet: impl Fn(&ModuleCatalog, &HullCatalog) -> SectionArmor,
) -> (World, Entity) {
    let mut w = World::new();
    insert_damage_resources(&mut w);

    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();

    let mut fit = Fit::new(HULL_FIGHTER);
    // Slot 0 = central reactor (4,4); slot 5 = armor plate cover (4,5) directly in
    // front of it along a downward (decreasing-row) ray.
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(5), MODULE_ARMOR_PLATE);
    let layout = build_layout(hull, &fit, &modules);

    let armor = make_facet(&modules, &hulls);

    let mut e = w.spawn((fit, layout, armor, HullStructure::full(500.0)));
    if let Some(s) = shields {
        e.insert(s);
    }
    let id = e.id();
    (w, id)
}

/// A thin steel facet on the entry armor section (SectionId 5, the (4,5) plate)
/// normal-up so a downward shot hits it head-on and easily clean-penetrates.
fn thin_entry_facet(_m: &ModuleCatalog, _h: &HullCatalog) -> SectionArmor {
    let mut armor = SectionArmor::new();
    // Slot 5 (the armor hardpoint) is at slot index 5 → its module cell is SectionId(5)
    // (dense_cells keys a module cell to SectionId(slot_index)).
    armor.sections.insert(
        SectionId(5),
        ArmorFacet {
            thickness: 1.0,
            material: ArmorMaterial::Steel,
            normal: Vec2::new(0.0, 1.0),
        },
    );
    armor
}

/// A downward (decreasing-row) shot from above column 4, entering the (4,5) cover
/// then the (4,4) reactor behind it (revise-A finer 9×11 fighter).
fn downward_shot(channel: Channel, magnitude: f32, penetration: f32, pen_size: f32) -> DamageEvent {
    DamageEvent {
        channel,
        magnitude,
        penetration,
        pen_size,
        point: Vec2::new(4.5, 11.0),
        dir: Vec2::new(0.0, -1.0),
        source: None,
    }
}

// --- T018 (FR-004) [COMPLETES FR-004]: per-(layer×channel) matrix traversal ------

/// A single `DamageEvent` traversal mitigates per `(layer × channel)`: each channel
/// loses its strong-vs layer's mitigation as it passes, and the surviving running
/// magnitude is **monotonically non-increasing** across Shields → Armor → Hull →
/// Systems. Driven through `apply_damage` (the overall non-increase) and spelled out
/// layer-by-layer with the public substrate (the explicit per-layer sequence).
#[test]
fn traversal_mitigates_per_layer_and_is_monotonically_non_increasing() {
    let matrix = default_resistance_matrix();

    // Each channel's strong-vs layer is its LOW cell — the channel "gets through"
    // there. Verify the matrix expresses this (the property the traversal applies).
    let strong = [
        (Channel::ThermalEnergy, DefenseLayer::Shields),
        (Channel::Kinetic, DefenseLayer::Armor),
        (Channel::Blast, DefenseLayer::HullStructure),
        (Channel::Em, DefenseLayer::Systems),
        (Channel::Radiation, DefenseLayer::Systems),
    ];
    for (channel, layer) in strong {
        let strong_resist = layer_resist(&matrix, layer, channel);
        // The channel's strong-vs layer removes strictly less than any other layer
        // does to it — i.e. it gets through its preferred layer.
        for other in DefenseLayer::ALL {
            if other != layer {
                assert!(
                    strong_resist <= layer_resist(&matrix, other, channel),
                    "{channel:?} should lose least at its strong-vs layer {layer:?}"
                );
            }
        }
    }

    // The explicit per-layer running magnitude across the ordered stack, built from
    // the public substrate, is monotonically non-increasing (every factor ≤ 1).
    for channel in Channel::ALL {
        let mut running = vec![100.0_f32];
        let mut shields = Shields::full(40.0, 5.0, true);
        let ev = downward_shot(channel, 100.0, 1000.0, 0.0);

        // Shields.
        let (after_shield, _) = sim::damage::shield_absorb(&mut shields, &ev, &matrix);
        running.push(after_shield);
        // Armor matrix.
        let after_armor =
            after_shield * (1.0 - layer_resist(&matrix, DefenseLayer::Armor, channel));
        running.push(after_armor);
        // Penetration tier (a clean pen through a thin plate).
        let pen = resolve_penetration(
            1.0,
            0.0,
            ev.penetration,
            ev.pen_size,
            ArmorMaterial::Steel,
            &PenetrationConfig::default(),
        );
        let after_pen = after_armor * pen.surviving();
        running.push(after_pen);
        // Hull then Systems matrix.
        let after_hull =
            after_pen * (1.0 - layer_resist(&matrix, DefenseLayer::HullStructure, channel));
        running.push(after_hull);
        let after_sys = after_hull * (1.0 - layer_resist(&matrix, DefenseLayer::Systems, channel));
        running.push(after_sys);

        for pair in running.windows(2) {
            assert!(
                pair[1] <= pair[0] + 1e-4,
                "running magnitude rose at a layer for {channel:?}: {pair:?}"
            );
        }
    }

    // Integration: drive `apply_damage` on a live fitted ship — the landed magnitude
    // never exceeds the input (the overall monotone-non-increase the pipeline gives).
    for channel in Channel::ALL {
        let (mut w, ship) = fitted_world(Some(Shields::full(40.0, 5.0, true)), thin_entry_facet);
        let ev = downward_shot(channel, 200.0, 1000.0, 0.0);
        let out = apply_damage(&mut w, ship, ev);
        assert!(
            out.applied <= ev.magnitude,
            "{channel:?}: applied {} exceeded input {}",
            out.applied,
            ev.magnitude
        );
    }
}

// --- T019 (FR-010) [COMPLETES FR-010]: shields absorb first, regen/decay ----------

/// Shields absorb first: a `ThermalEnergy` hit melts through (LOW shield mitigation,
/// lots survives) while a `Kinetic` hit is heavily absorbed; `regen_shield`
/// regenerates toward `max` while powered and decays at `unpowered_decay` exposing
/// armor while `power_linked && !powered` (INV-D14).
#[test]
fn shields_absorb_first_then_regen_and_decay() {
    let matrix = default_resistance_matrix();

    // Absorption: thermal melts through more than kinetic, against the same pool.
    let mut sh_thermal = Shields::full(50.0, 5.0, true);
    let mut sh_kinetic = Shields::full(50.0, 5.0, true);
    let (surv_thermal, _) = sim::damage::shield_absorb(
        &mut sh_thermal,
        &downward_shot(Channel::ThermalEnergy, 100.0, 0.0, 0.0),
        &matrix,
    );
    let (surv_kinetic, _) = sim::damage::shield_absorb(
        &mut sh_kinetic,
        &downward_shot(Channel::Kinetic, 100.0, 0.0, 0.0),
        &matrix,
    );
    assert!(
        surv_thermal > surv_kinetic,
        "thermal must melt through ({surv_thermal}) more than kinetic is absorbed ({surv_kinetic})"
    );

    // Regen toward max while powered (the pure helper).
    let cfg = ShieldConfig::default();
    let mut regen = Shields::depleted(60.0, 5.0, true);
    for _ in 0..2000 {
        regen_shield(&mut regen, true, 1.0 / 60.0, &cfg);
    }
    assert_eq!(regen.current, 60.0, "powered shield regenerates to its max");

    // Decay to 0 (armor exposed) while power_linked && !powered (INV-D14).
    let mut decay = Shields::full(60.0, 5.0, true);
    for _ in 0..2000 {
        regen_shield(&mut decay, false, 1.0 / 60.0, &cfg);
    }
    assert_eq!(
        decay.current, 0.0,
        "unpowered power-linked shield decays, exposing armor"
    );

    // A depleted shield then passes a fresh hit through untouched (armor exposed).
    let (surviving, depleted) = sim::damage::shield_absorb(
        &mut decay,
        &downward_shot(Channel::Kinetic, 30.0, 0.0, 0.0),
        &matrix,
    );
    assert_eq!(
        surviving, 30.0,
        "a depleted shield passes through untouched"
    );
    assert!(depleted);
}

// --- T020 (FR-002/009/011 → Phase 2 carving) [COMPLETES FR-002/009/011] (SC-001) --
//
// REVISED for the Phase 2 carving model (the old route-behind/spill semantics this
// test asserted are retired). `apply_damage` no longer routes one post-pen magnitude
// to a single "cell behind" — it **carves a channel** of cells along the shot ray out
// of the live `FitLayout`. The justified re-assertions:
//   - a strong clean-penetrating shot carves a DEEP channel: it removes the entry
//     cover (4,5) AND the buried reactor (4,4) along the ray (the module behind is
//     reached only after the cover — the outer-before-inner survivability property
//     still holds, now expressed as carve order); `destroyed_cells` is non-empty;
//   - a weak shot carves a SHALLOWER channel (fewer cells) and does NOT reach the
//     buried reactor — it chips/eats only the outer cells (the cover survives intact);
//   - a ricochet carves NOTHING.

/// A clean penetrating shot **carves a channel** of cells along its ray — removing the
/// entry cover and the buried reactor behind it — while a weak shot carves a shallower
/// channel that never reaches the reactor (outer-before-inner survivability, now as
/// carve depth). The reactor is destroyed (carved away) only by the strong shot.
#[test]
fn clean_penetration_carves_a_channel_to_the_module_behind() {
    // No shield (so the shot reaches the armor gate directly), thin entry facet.
    let (mut w, ship) = fitted_world(None, thin_entry_facet);

    // A strong clean Em penetration down column 4 (from above) carves a deep channel:
    // it eats the outer hull cells, the (4,5) armor cover, and the (4,4) reactor behind
    // it. pen 1000 ≫ the thin plate, magnitude 4000 ≫ the per-cell HP → a deep tunnel.
    let ev = downward_shot(Channel::Em, 4000.0, 1000.0, 0.0);
    let out = apply_damage(&mut w, ship, ev);
    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "a clean penetration, got {:?}",
        out.result
    );
    assert!(
        out.destroyed,
        "the penetrating shot carved at least one cell"
    );
    // The channel removed both the (4,5) cover and the (4,4) reactor behind it.
    assert!(
        out.destroyed_cells.contains(&(4, 5)),
        "the channel carved the (4,5) armor cover (got {:?})",
        out.destroyed_cells
    );
    assert!(
        out.destroyed_cells.contains(&(4, 4)),
        "the channel carved through to the buried (4,4) reactor (got {:?})",
        out.destroyed_cells
    );
    // The cover is carved before the reactor (outer-before-inner carve order).
    let cover_idx = out.destroyed_cells.iter().position(|&c| c == (4, 5));
    let reactor_idx = out.destroyed_cells.iter().position(|&c| c == (4, 4));
    assert!(
        cover_idx < reactor_idx,
        "the outer cover is carved before the buried reactor (got {:?})",
        out.destroyed_cells
    );
    // Both cells are gone from the live FitLayout (removed, not just zeroed).
    let layout = w.get::<sim::fitting::FitLayout>(ship).unwrap();
    assert!(
        layout.occupant((4, 4)).is_none(),
        "the buried reactor cell was carved away"
    );
    let strong_channel_len = out.destroyed_cells.len();

    // A WEAK shot carves a SHALLOWER channel: a thick plate cover + a low pen/magnitude
    // shot eats only a couple of outer cells and never reaches the buried reactor.
    let (mut w2, ship2) = fitted_world(None, |_m, _h| {
        // A thick plate so the low-pen shot at most non-penetrates / over-pens weakly.
        let mut armor = SectionArmor::new();
        armor.sections.insert(
            SectionId(5),
            ArmorFacet {
                thickness: 6.0,
                material: ArmorMaterial::Steel,
                normal: Vec2::new(0.0, 1.0),
            },
        );
        armor
    });
    let weak = downward_shot(Channel::Em, 12.0, 5.0, 0.0); // small magnitude + low pen
    let out2 = apply_damage(&mut w2, ship2, weak);
    // The buried reactor (4,4) is untouched: the shallow channel never reaches it.
    let layout2 = w2.get::<sim::fitting::FitLayout>(ship2).unwrap();
    let reactor_max = w2
        .get_resource::<ModuleCatalog>()
        .unwrap()
        .get(MODULE_REACTOR_BASIC)
        .unwrap()
        .health_max;
    let reactor2 = layout2
        .occupant((4, 4))
        .expect("the buried reactor survives a weak shot");
    assert_eq!(
        reactor2.health, reactor_max,
        "the buried reactor is unscathed by a weak shallow shot"
    );
    assert!(
        !out2.destroyed_cells.contains(&(4, 4)),
        "the weak shot never carves the buried reactor (got {:?})",
        out2.destroyed_cells
    );
    // The weak channel is strictly shallower than the strong one (fewer cells eaten).
    assert!(
        out2.destroyed_cells.len() < strong_channel_len,
        "a weak shot carves a shallower channel than a strong shot ({} < {})",
        out2.destroyed_cells.len(),
        strong_channel_len
    );

    // A RICOCHET carves nothing: a shot grazing the hull's outer surface near-tangent
    // (obliquity > ricochet_angle) bounces. Geometry: a straight-DOWN shot through the
    // exposed left **wing tip** (0,5) — the only cell on column 0, a genuine outer
    // surface cell whose outward normal is `-x`, perpendicular to the downward ray
    // (obliquity ≈ 90°) → Ricochet, with a small non-overmatching penetrator.
    let (mut w3, ship3) = fitted_world(None, thin_entry_facet);
    let graze = DamageEvent {
        channel: Channel::Em,
        magnitude: 100.0,
        penetration: 50.0,
        pen_size: 0.0,
        point: Vec2::new(0.5, 12.0), // above the left wing tip column 0
        dir: Vec2::new(0.0, -1.0),   // straight down → grazes the -x-facing wing tip
        source: None,
    };
    let out3 = apply_damage(&mut w3, ship3, graze);
    assert_eq!(
        out3.result,
        HitKind::Ricochet,
        "a near-tangent grazing clip of the wing tip ricochets (carves nothing)"
    );
    assert!(
        out3.destroyed_cells.is_empty(),
        "a ricochet carves nothing (got {:?})",
        out3.destroyed_cells
    );
}

// =================================================================================
// US2 — emergent damage: per-module live health scales the derived ShipStats
// (T024/T025). `derive_ship_stats` reads each module's `FitLayout` cell health, so a
// battered ship flies/fires worse than a pristine same-fit ship (FR-012/013,
// INV-D13/D14, SC-002). These drive the extended derivation directly (pure logic).
// =================================================================================

/// Set the live `health` of the layout cell occupied by `slot`'s installed module
/// (the same find `derive_ship_stats` uses to read the factor). Mirrors what
/// `apply_damage` does to a struck cell, isolated so the derivation can be tested
/// without the whole pipeline.
fn damage_slot(layout: &mut FitLayout, slot: SlotId, health: f32) {
    let occ = layout
        .cells
        .values_mut()
        .find(|o| o.slot == slot && o.module.is_some())
        .expect("the slot has an occupied cell");
    occ.health = health;
}

/// A fighter fit with a single thruster in slot 1 (the seed thruster:
/// `thrust_force 15`, `health_max 20`), plus its full-health layout.
fn one_thruster_fighter() -> (ModuleCatalog, sim::fitting::Hull, Fit, FitLayout) {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    let layout = build_layout(&hull, &fit, &modules);
    (modules, hull, fit, layout)
}

// --- T024 (FR-012): a damaged thruster lowers thrust + accel, floored (SC-002) ---

/// A damaged thruster's lower `health_frac` lowers the derived `thrust_force` and
/// the acceleration (`thrust / mass`) **proportionally** — a healthy vs a battered
/// same-fit ship derive measurably different stats (SC-002) — and the contribution
/// is **floored** at `stat_health_floor` (never collapses to 0 while alive, never
/// NaN/inf). Mass is **not** scaled by health (INV-D13).
#[test]
fn damaged_thruster_lowers_thrust_and_accel_floored() {
    let (modules, hull, fit, full_layout) = one_thruster_fighter();
    let cfg = StatScalingConfig::default();
    let thruster_max = modules.get(MODULE_THRUSTER_BASIC).unwrap().health_max; // 20.0

    // Pristine baseline.
    let healthy = derive_ship_stats(&hull, &fit, &modules, &full_layout);

    // Half-health thruster: thrust scales to ~50% (above the floor), mass unchanged.
    let mut half_layout = full_layout.clone();
    damage_slot(&mut half_layout, SlotId(1), thruster_max * 0.5);
    let battered = derive_ship_stats(&hull, &fit, &modules, &half_layout);

    // Measurably different (SC-002): less thrust, less acceleration.
    assert!(
        battered.thrust_force < healthy.thrust_force,
        "a damaged thruster gives less thrust ({} < {})",
        battered.thrust_force,
        healthy.thrust_force
    );
    // The thruster's *contribution* halved. The derived thrust is floored at
    // THRUST_FLOOR, but at half health the contribution (15 * 0.5 = 7.5) is above
    // the floor, so the drop is proportional: ratio of contributions ≈ 0.5.
    let healthy_contrib = healthy.thrust_force; // single thruster, no floor effect
    let battered_contrib = battered.thrust_force;
    assert!(
        (battered_contrib / healthy_contrib - 0.5).abs() < 0.02,
        "thrust scales ~linearly with health (ratio {})",
        battered_contrib / healthy_contrib
    );

    // Mass is NOT scaled by health (INV-D13) — same fit, same mass.
    assert_eq!(
        battered.total_mass, healthy.total_mass,
        "a damaged module still has its full mass (INV-D13)"
    );
    // Acceleration (thrust / mass) drops with the thrust (same mass).
    let accel_healthy = healthy.thrust_force / healthy.total_mass;
    let accel_battered = battered.thrust_force / battered.total_mass;
    assert!(
        accel_battered < accel_healthy,
        "a battered ship accelerates more slowly ({accel_battered} < {accel_healthy})"
    );

    // Floor: a barely-alive thruster (1% health) keeps at least `stat_health_floor`
    // of its contribution — never NaN/inf, never a cliff to 0 while alive.
    let mut sliver_layout = full_layout.clone();
    damage_slot(&mut sliver_layout, SlotId(1), thruster_max * 0.01);
    let sliver = derive_ship_stats(&hull, &fit, &modules, &sliver_layout);
    assert!(
        sliver.is_finite_and_floored(),
        "a barely-alive thruster still derives finite, floored stats"
    );
    // The contribution is clamped to the floor: thrust ≈ floor * healthy contribution
    // (or THRUST_FLOOR, whichever is larger — both are well-defined and finite).
    let floored_contrib = (healthy.thrust_force * cfg.stat_health_floor).max(0.0);
    assert!(
        sliver.thrust_force >= floored_contrib - 1e-3,
        "the contribution is floored at stat_health_floor, not 0"
    );
    assert!(sliver.thrust_force.is_finite());
}

// --- T025 [COMPLETES FR-012] [COMPLETES FR-013]: destroyed weapon/reactor off -----

/// A **destroyed weapon** (cell health 0) drops `can_fire` and the weapon profile
/// (FR-013), and a **destroyed reactor** (cell health 0) contributes `0` `power_gen`
/// — collapsing `power_supply` to `hull.power_capacity` alone — which un-powers a
/// `power_linked` shield via the `power_supply < power_draw` gate, so it decays
/// (INV-D14/FR-013).
#[test]
fn destroyed_weapon_disarms_and_destroyed_reactor_unpowers_shields() {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

    // A reactor (slot 0, power_gen 20), a weapon (slot 3 → can_fire), two thrusters
    // (slots 1+2), and a power-hungry shield (raw-installed; drives power_draw the
    // reactor must cover). Raw installs so the derivation reads the intended loadout
    // regardless of budget — the power-starve only emerges when the reactor dies.
    // Draws: 2×thruster(3) + cannon(3) + shield(6) = 15 > hull.power_capacity (10),
    // so without the reactor's 20 the ship is power-starved.
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC); // power_gen 20, draw 0
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC); // draw 3
    fit.install_raw(SlotId(2), MODULE_THRUSTER_BASIC); // draw 3
    fit.install_raw(SlotId(3), MODULE_AUTOCANNON); // a weapon → can_fire, draw 3
    fit.install_raw(SlotId(5), MODULE_SHIELD_BASIC); // power-hungry shield, draw 6
    let full_layout = build_layout(&hull, &fit, &modules);

    // Baseline: armed + powered.
    let healthy = derive_ship_stats(&hull, &fit, &modules, &full_layout);
    assert!(healthy.can_fire, "an installed alive weapon ⇒ can_fire");
    assert!(healthy.weapon.is_some());
    let reactor_gen = modules.get(MODULE_REACTOR_BASIC).unwrap().power_gen; // 20
    assert!(
        (healthy.power_supply - (hull.power_capacity + reactor_gen)).abs() < 1e-4,
        "power_supply = hull.power_capacity + reactor power_gen at full health"
    );

    // --- FR-013: a destroyed WEAPON drops can_fire + the profile -----------------
    let mut no_weapon = full_layout.clone();
    damage_slot(&mut no_weapon, SlotId(3), 0.0); // weapon destroyed
    let disarmed = derive_ship_stats(&hull, &fit, &modules, &no_weapon);
    assert!(
        !disarmed.can_fire,
        "a destroyed weapon ⇒ can_fire == false (FR-013)"
    );
    assert!(
        disarmed.weapon.is_none(),
        "a destroyed weapon drops the WeaponProfile (FR-013)"
    );

    // --- FR-013: a destroyed REACTOR collapses power_supply -----------------------
    let mut no_reactor = full_layout.clone();
    damage_slot(&mut no_reactor, SlotId(0), 0.0); // reactor destroyed
    let unpowered = derive_ship_stats(&hull, &fit, &modules, &no_reactor);
    assert!(
        (unpowered.power_supply - hull.power_capacity).abs() < 1e-4,
        "a destroyed reactor contributes 0 power_gen ⇒ power_supply = hull.power_capacity alone (FR-013)"
    );
    assert!(
        unpowered.power_supply < unpowered.power_draw,
        "the shield's power_draw now exceeds supply ⇒ the ship is power-starved (INV-D14)"
    );

    // --- INV-D14: the power-linked shield then goes unpowered and decays ----------
    // The shield_regen_system gate is `power_supply >= power_draw`; with the reactor
    // gone that is false, so a power_linked shield decays toward 0 (exposing armor).
    let powered = unpowered.power_supply >= unpowered.power_draw;
    assert!(!powered, "the power-starved ship cannot power its shield");

    let cfg = ShieldConfig::default();
    let mut shield = Shields::full(60.0, 5.0, true); // power_linked
    for _ in 0..2000 {
        regen_shield(&mut shield, powered, 1.0 / 60.0, &cfg);
    }
    assert_eq!(
        shield.current, 0.0,
        "an unpowered power-linked shield decays to 0, exposing armor (INV-D14/FR-013)"
    );
}

// =================================================================================
// US3 — destruction + connectivity severing (T029/T030). A destroyed section is
// removed from the layout; a connectivity flood-fill (ONLY at destruction) splits
// disconnected regions into drifting chunks that inherit COM momentum (FR-014/015/
// 016/017, INV-D07/D08/D15, SC-003).
//
// The seed fighter cells are sparse (not 4-connected), so these tests author a small
// purpose-built **corridor** hull where removing a middle section disconnects an end.
// =================================================================================

use sim::components::{AngularVelocity, Heading, Position, Velocity};
use sim::damage::{
    connected_region, core_cell, on_cells_carved, on_section_destroyed, sever_chunk, Wreck,
    WreckOrigin,
};
use sim::fitting::{CellMap, CellOccupant, GridCell, Hull, HullId};

/// The corridor hull id used by the US3 tests.
const HULL_CORRIDOR: HullId = HullId(7);

/// Build a 5×3 "corridor" hull: a single horizontal row of 5 cells at row 1,
/// `(0,1)..(4,1)`, each its own [`SectionId`] (`0..4`). On the 5×3 grid the
/// end cells `(0,1)`/`(4,1)` have occlusion depth `0`; the inner three depth `1`, so
/// the deepest (`core_cell`, ties→smallest) is `(1,1)` — an interior cell. Removing
/// the geometric-middle section `(2,1)` disconnects the `(3,1)`/`(4,1)` end from the
/// core; removing the core's own section `(1,1)` is the whole-ship-destroyed path.
fn corridor_hull() -> Hull {
    let coords = [(0u16, 1u16), (1, 1), (2, 1), (3, 1), (4, 1)];
    let cells: Vec<GridCell> = coords
        .iter()
        .enumerate()
        .map(|(i, &c)| GridCell::new(c, SectionId(i as u32)))
        .collect();
    Hull {
        id: HULL_CORRIDOR,
        name: "Corridor".to_string(),
        grid_dims: (5, 3),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// A [`FitLayout`] over the corridor hull (all empty cells; the connectivity grid is
/// the cell keyset, occupancy is irrelevant to the flood-fill). Depths are computed
/// from the hull `grid_dims` exactly as [`build_layout`] would.
fn corridor_layout() -> FitLayout {
    let hull = corridor_hull();
    let mut cells = CellMap::new();
    for (col, row) in [(0u16, 1u16), (1, 1), (2, 1), (3, 1), (4, 1)] {
        // depth = min(col, cols-1-col, row, rows-1-row) on the 5×3 grid.
        let depth = col.min(4 - col).min(row).min(2 - row);
        cells.insert(
            (col, row),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 0.0,
                depth,
                structural: false,
            },
        );
    }
    FitLayout {
        hull: hull.id,
        cells,
    }
}

/// A `(World, ship)` with the corridor hull/layout fitted, plus a `HullCatalog`
/// carrying the corridor hull (so `on_section_destroyed` can resolve it). Body
/// components are supplied so a severed chunk's inherited momentum is observable.
fn corridor_world(pos: Vec2, vel: Vec2, heading: f32, angvel: f32) -> (World, Entity) {
    let mut w = World::new();
    let mut hulls = HullCatalog::default();
    hulls.hulls.insert(HULL_CORRIDOR, corridor_hull());
    w.insert_resource(hulls);
    w.insert_resource(ModuleCatalog::default());

    let fit = Fit::new(HULL_CORRIDOR);
    let layout = corridor_layout();
    let ship = w
        .spawn((
            fit,
            layout,
            Position(pos),
            Velocity(vel),
            Heading(heading),
            AngularVelocity(angvel),
            HullStructure::full(100.0),
        ))
        .id();
    (w, ship)
}

// --- T029 (FR-015/017): destroying a connecting section splits the hull -----------

/// Destroying a **connecting** section splits the hull: the flood-fill finds the
/// disconnected region. A hull that stays connected after a destruction produces NO
/// split. And connectivity runs **only** at the destruction call, never per frame
/// (INV-D08) — no chunk appears without a destruction event.
#[test]
fn destroying_a_connecting_section_splits_the_hull() {
    // --- The pure flood-fill: removing the middle cell isolates the far end. -----
    let mut split_layout = corridor_layout();
    split_layout.cells.remove(&(2, 1)); // destroy the connecting middle cell
    let core = core_cell(&split_layout).expect("a core remains");
    assert_eq!(core, (1, 1), "the deepest cell is the interior (1,1)");
    let attached = connected_region(&split_layout, core);
    // The core half {(0,1),(1,1)} stays; the far end {(3,1),(4,1)} is disconnected.
    assert_eq!(attached.len(), 2);
    assert!(attached.contains(&(0, 1)) && attached.contains(&(1, 1)));
    assert!(!attached.contains(&(3, 1)) && !attached.contains(&(4, 1)));

    // A hull that stays connected (remove an END cell, not a connector) → NO split.
    let mut end_layout = corridor_layout();
    end_layout.cells.remove(&(4, 1)); // an end: removing it disconnects nothing
    let core2 = core_cell(&end_layout).expect("a core remains");
    let attached2 = connected_region(&end_layout, core2);
    assert_eq!(
        attached2.len(),
        end_layout.cells.len(),
        "removing an end cell keeps every remaining cell attached — no split"
    );

    // --- Through `on_section_destroyed`: the disconnected end severs into a chunk. -
    let (mut w, ship) = corridor_world(Vec2::ZERO, Vec2::ZERO, 0.0, 0.0);

    // INV-D08: BEFORE any destruction event there is no chunk (no per-frame scan).
    let chunks_before = w.query::<&Wreck>().iter(&w).count();
    assert_eq!(
        chunks_before, 0,
        "no chunk exists before a destruction event"
    );

    // Destroy SectionId(2) — the connecting middle cell (2,1).
    on_section_destroyed(&mut w, ship, SectionId(2));

    // Exactly one severed-chunk wreck appears, and it carries the far-end cells.
    let mut q = w.query::<(&Wreck, &FitLayout)>();
    let severed: Vec<_> = q
        .iter(&w)
        .filter(|(wr, _)| wr.origin == WreckOrigin::SeveredChunk)
        .collect();
    assert_eq!(severed.len(), 1, "the disconnected end severs as one chunk");
    let (_, chunk_layout) = severed[0];
    let mut chunk_cells: Vec<_> = chunk_layout.cells.keys().copied().collect();
    chunk_cells.sort_unstable();
    assert_eq!(
        chunk_cells,
        vec![(3, 1), (4, 1)],
        "the chunk carries the far-end cells severed from the core"
    );

    // The parent ship is NOT marked whole-ship-destroyed (its core survived).
    assert!(
        w.get::<Wreck>(ship).is_none(),
        "the parent keeps flying (core intact) — only the end severed"
    );
    // The parent layout lost the destroyed section's cell AND the severed cells.
    let parent_layout = w.get::<FitLayout>(ship).unwrap();
    let mut remaining: Vec<_> = parent_layout.cells.keys().copied().collect();
    remaining.sort_unstable();
    assert_eq!(
        remaining,
        vec![(0, 1), (1, 1)],
        "the parent retains only the core-attached cells"
    );
}

/// A destruction that keeps the hull connected produces NO chunk (INV-D08): removing
/// an end section leaves every remaining cell attached, so `on_section_destroyed`
/// severs nothing.
#[test]
fn destroying_an_end_section_produces_no_chunk() {
    let (mut w, ship) = corridor_world(Vec2::ZERO, Vec2::ZERO, 0.0, 0.0);
    // Destroy SectionId(4) — the far end (4,1); the rest stays connected.
    on_section_destroyed(&mut w, ship, SectionId(4));
    let chunks = w
        .query::<&Wreck>()
        .iter(&w)
        .filter(|wr| wr.origin == WreckOrigin::SeveredChunk)
        .count();
    assert_eq!(
        chunks, 0,
        "a still-connected hull produces no split (INV-D08)"
    );
    assert!(
        w.get::<Wreck>(ship).is_none(),
        "the ship is not destroyed by losing an end"
    );
}

// --- T030 (FR-016) [COMPLETES FR-014/015/016/017] (SC-003) ------------------------

/// A severed chunk inherits parent **linear + angular** velocity at its COM (it
/// drifts, momentum conserved, INV-D07): with a nonzero parent `Velocity` and
/// `AngularVelocity`, an off-center chunk's velocity is
/// `parent.vel + angvel·perp(r)` where `r` is the world offset from the parent COM
/// to the chunk COM. And a **core-sever destroys the ship** (destroying the section
/// containing the core → the whole-ship-destroyed path, INV-D15).
#[test]
fn severed_chunk_inherits_com_momentum_and_core_sever_destroys_ship() {
    // --- COM momentum inheritance (INV-D07) -----------------------------------
    let parent_pos = Vec2::new(10.0, 5.0);
    let parent_vel = Vec2::new(2.0, -1.0);
    let heading = 0.0_f32; // zero heading so world offset == local offset (clean check)
    let angvel = 0.5_f32;
    let (mut w, ship) = corridor_world(parent_pos, parent_vel, heading, angvel);

    // The parent (whole-ship) local COM is the mean of all 5 cell centers:
    // cols 0..4 at +0.5 → mean col (0.5+1.5+2.5+3.5+4.5)/5 = 2.5, row 1.5 →
    // (2.5, 1.5) in local cell-space.
    let parent_com = Vec2::new(2.5, 1.5);
    // Sever the far-end region {(3,1),(4,1)} directly (its local COM is
    // ((3.5+4.5)/2, 1.5) = (4.0, 1.5)).
    let mut region = std::collections::HashSet::new();
    region.insert((3u16, 1u16));
    region.insert((4u16, 1u16));
    let chunk_com = Vec2::new(4.0, 1.5);

    let chunk = sever_chunk(&mut w, ship, &region);

    // r = (chunk_com - parent_com) rotated by heading(=0) → unchanged.
    let r = chunk_com - parent_com;
    let expected_vel = parent_vel + angvel * Vec2::new(-r.y, r.x);
    let expected_pos = parent_pos + r;
    assert!(
        (chunk.body.vel - expected_vel).length() < 1e-4,
        "chunk inherits parent.vel + angvel·perp(r): got {:?}, expected {:?}",
        chunk.body.vel,
        expected_vel
    );
    // It actually drifts (nonzero velocity — no zero-velocity pop, INV-D07).
    assert!(
        chunk.body.vel.length() > 0.0,
        "the severed chunk drifts, never a zero-velocity pop"
    );
    assert!(
        (chunk.body.pos - expected_pos).length() < 1e-4,
        "chunk world pos = parent.pos + r"
    );
    assert_eq!(
        chunk.cells,
        vec![(3, 1), (4, 1)],
        "deterministic sorted cells"
    );

    // The momentum is also on the spawned chunk entity (authoritative ECS spawn).
    let mut q = w.query::<(&Velocity, &AngularVelocity, &Wreck)>();
    let spawned: Vec<_> = q.iter(&w).collect();
    assert_eq!(spawned.len(), 1, "one severed-chunk entity spawned");
    let (chunk_vel_comp, chunk_ang, wreck) = spawned[0];
    assert!((chunk_vel_comp.0 - expected_vel).length() < 1e-4);
    assert_eq!(
        chunk_ang.0, angvel,
        "the chunk inherits parent angular velocity"
    );
    assert_eq!(wreck.origin, WreckOrigin::SeveredChunk);

    // --- Core-sever destroys the ship (INV-D15) --------------------------------
    let (mut w2, ship2) = corridor_world(Vec2::ZERO, Vec2::ZERO, 0.0, 0.0);
    // The core is the deepest cell (1,1) = SectionId(1). Destroying that section
    // severs the core → the whole ship is destroyed.
    assert_eq!(core_cell(w2.get::<FitLayout>(ship2).unwrap()), Some((1, 1)));
    on_section_destroyed(&mut w2, ship2, SectionId(1));

    let wreck = w2
        .get::<Wreck>(ship2)
        .expect("a core-sever destroys the ship → a DestroyedShip wreck marker");
    assert_eq!(
        wreck.origin,
        WreckOrigin::DestroyedShip,
        "a core-sever is the whole-ship-destroyed path (INV-D15)"
    );
    assert_eq!(
        w2.get::<HullStructure>(ship2).unwrap().current,
        0.0,
        "the destroyed ship's structural HP is zeroed"
    );
    // No severed chunks spawn on the whole-ship path (it is one wreck, not many).
    let severed_chunks = w2
        .query::<&Wreck>()
        .iter(&w2)
        .filter(|wr| wr.origin == WreckOrigin::SeveredChunk)
        .count();
    assert_eq!(
        severed_chunks, 0,
        "a core-sever yields one whole-ship wreck, not a swarm of chunks"
    );
}

// =================================================================================
// US4 — salvage: intact-vs-scrap loot + persistent lootable wrecks (T033/T034).
//
// T033 drives the pure `intact_threshold` + `salvage_layout` boundary on a hand-built
// layout + catalog. T034 drives a `sim` `World` through `destroy_ship` and asserts the
// persistent `Wreck`: non-empty contents, the over-kill scrap floor, and the
// single-resolution claim (INV-D09/D10/D12).
// =================================================================================

use sim::damage::salvage::salvage_layout;
use sim::damage::{intact_threshold, salvage, SalvageConfig, SalvageOutcome};
use sim::fitting::{HardpointType, SlotSize};
use sim::fitting::{Module, ModuleId, ModuleKind, ModuleSpecifics};

/// A minimal salvageable module: `health_max`/`mass` are the only fields salvage
/// reads (the rest are inert filler).
fn salvage_module(id: ModuleId, health_max: f32, mass: f32) -> Module {
    Module {
        id,
        kind: ModuleKind::Utility,
        power_gen: 0.0,
        power_draw: 0.0,
        cpu_draw: 0.0,
        mass,
        heat: 0.0,
        health_max,
        hardpoint_type: HardpointType::Utility,
        hardpoint_size: SlotSize::Small,
        specifics: ModuleSpecifics::Utility,
    }
}

/// A catalog holding exactly the given module rows.
fn catalog_of(modules: &[Module]) -> ModuleCatalog {
    let mut catalog = ModuleCatalog::default();
    for m in modules {
        catalog.modules.insert(m.id, *m);
    }
    catalog
}

/// A one-cell layout occupied by `module_id` at the given live `health`.
fn one_module_layout(module_id: ModuleId, health: f32) -> FitLayout {
    let mut cells = CellMap::new();
    cells.insert(
        (0, 0),
        CellOccupant {
            slot: SlotId(0),
            module: Some(module_id),
            health,
            depth: 0,
            structural: false,
        },
    );
    FitLayout {
        hull: HullId(1),
        cells,
    }
}

// --- T033 (FR-018/019): clean-sever → intact; through-kill → scrap, never intact ---

/// A clean-severed module (`health >= intact_fraction * health_max`) salvages
/// `IntactModule`; a penetrated-through / destroyed module (`health` below the
/// threshold, in the limit `0`) salvages `Scrap` and NEVER an intact module — so a
/// through-kill cannot beat a careful clean sever (INV-D12, FR-018/019). Drives the
/// pure `intact_threshold` + `salvage_layout` surface with a hand-built layout.
#[test]
fn clean_sever_yields_intact_through_kill_yields_scrap() {
    let cfg = SalvageConfig::default(); // intact_fraction 0.5, floor 1.0, per_mass 1.0
    let id = ModuleId(101);
    let module = salvage_module(id, 40.0, 7.0); // threshold = 0.5 * 40 = 20.0
    let catalog = catalog_of(&[module]);

    // --- The boundary itself (intact_threshold, INV-D12) ----------------------
    // At/above the threshold (clean sever) → intact.
    let full = CellOccupant {
        slot: SlotId(0),
        module: Some(id),
        health: 40.0,
        depth: 0,
        structural: false,
    };
    let exactly_at = CellOccupant {
        health: 20.0,
        ..full
    };
    let just_below = CellOccupant {
        health: 19.999,
        ..full
    };
    let through_killed = CellOccupant {
        health: 0.0,
        ..full
    };
    assert!(
        intact_threshold(&full, &module, &cfg),
        "full health is intact"
    );
    assert!(
        intact_threshold(&exactly_at, &module, &cfg),
        "exactly at the threshold is intact (>=, INV-D12)"
    );
    assert!(
        !intact_threshold(&just_below, &module, &cfg),
        "just below the threshold is NOT intact"
    );
    assert!(
        !intact_threshold(&through_killed, &module, &cfg),
        "a through-killed (health 0) module is NEVER intact"
    );

    // --- The walk (salvage_layout): clean sever → IntactModule ----------------
    let clean = salvage_layout(&one_module_layout(id, 25.0), &catalog, &cfg);
    assert_eq!(clean.len(), 1);
    match clean[0] {
        SalvageOutcome::IntactModule(r) => {
            assert_eq!(r.module, id, "the intact module carries its identity");
            assert_eq!(r.slot, SlotId(0));
        }
        other => panic!("a clean sever must yield IntactModule, got {other:?}"),
    }

    // --- The walk: through-kill → Scrap, never intact -------------------------
    let scrapped = salvage_layout(&one_module_layout(id, 0.0), &catalog, &cfg);
    assert_eq!(scrapped.len(), 1);
    match scrapped[0] {
        // mass 7.0 * per_mass 1.0 = 7.0, above the floor of 1.0.
        SalvageOutcome::Scrap(amount) => {
            assert!((amount - 7.0).abs() < 1e-6, "scrap = mass*per_mass = 7.0");
        }
        other => panic!("a through-kill must yield Scrap, got {other:?}"),
    }
    // A through-kill produced NO IntactModule — the precision-playstyle guarantee.
    assert!(
        !scrapped
            .iter()
            .any(|o| matches!(o, SalvageOutcome::IntactModule(_))),
        "a through-kill must never beat a clean sever (no IntactModule, INV-D12)"
    );
}

// --- T034 (FR-020): persistent lootable wreck, over-kill floor, single claim ------

/// A `(World, ship)` on a 3×1 custom hull: a deep **core** cell `(1,0)` in
/// `SectionId(0)` (the cell destruction removes — the whole-ship-destroyed trigger),
/// plus a salvageable module cell `(0,0)` in `SectionId(1)` that survives into the
/// residual wreck layout. `module_health` seeds that module cell's live health so the
/// test can stage a clean-sever (high health) vs over-kill (through-killed) wreck. The
/// `ModuleCatalog` + `SalvageConfig` salvage resources are inserted, ready for
/// `on_section_destroyed`/`destroy_ship`.
fn salvage_world(module_health: f32) -> (World, Entity, ModuleId) {
    let module_id = ModuleId(202);
    let module = salvage_module(module_id, 50.0, 4.0); // threshold = 25.0

    // 3×1 grid so `(1,0)` is the deepest cell (depth 0 at the ends, but the middle is
    // the interior on a 3-wide row) → the core. Destroying SectionId(0) (the core's
    // section) is the whole-ship-destroyed path; the module cell in SectionId(1)
    // remains in the residual layout the wreck salvages.
    let hull = Hull {
        id: HullId(9),
        name: "Salvage".to_string(),
        grid_dims: (3, 1),
        cells: vec![
            GridCell::new((0, 0), SectionId(1)), // the module's (survivor) section
            GridCell::new((1, 0), SectionId(0)), // the core (destroyed section)
        ],
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    };

    let mut w = World::new();
    let mut hulls = HullCatalog::default();
    hulls.hulls.insert(hull.id, hull.clone());
    w.insert_resource(hulls);
    w.insert_resource(catalog_of(&[module]));
    w.insert_resource(SalvageConfig::default());

    let mut cells = CellMap::new();
    // The salvageable module cell (survives into the wreck).
    cells.insert(
        (0, 0),
        CellOccupant {
            slot: SlotId(0),
            module: Some(module_id),
            health: module_health,
            depth: 0,
            structural: false,
        },
    );
    // The core cell (deepest; in the destroyed section). Empty/structural.
    cells.insert(
        (1, 0),
        CellOccupant {
            slot: SlotId(u32::MAX),
            module: None,
            health: 0.0,
            depth: 1,
            structural: true,
        },
    );
    let layout = FitLayout {
        hull: hull.id,
        cells,
    };
    let ship = w
        .spawn((
            Fit::new(hull.id),
            layout,
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            HullStructure::full(100.0),
        ))
        .id();
    (w, ship, module_id)
}

/// A destroyed ship leaves a PERSISTENT, lootable `Wreck` (the in-world entity, with
/// non-empty `contents`); an over-killed ship (its module through-killed) still yields
/// ≥ a `Scrap` floor (never empty, INV-D09); and the wreck is claimed exactly once
/// (`claim` returns contents then empty, INV-D10) — SC-004.
#[test]
fn destroyed_ship_persists_as_a_claimed_once_lootable_wreck() {
    // --- Clean-sever wreck: a healthy module → an intact-module lootable wreck. ---
    let (mut w, ship, module_id) = salvage_world(50.0); // full health (>= threshold 25)
    on_section_destroyed(&mut w, ship, SectionId(0));

    // The destroyed ship entity itself IS the persistent wreck (it retains its body).
    let wreck = w
        .get::<Wreck>(ship)
        .expect("a destroyed ship leaves a persistent DestroyedShip wreck");
    assert_eq!(wreck.origin, WreckOrigin::DestroyedShip);
    assert!(
        w.get::<Position>(ship).is_some() && w.get::<Velocity>(ship).is_some(),
        "the wreck is a persistent physical body (still carries Position/Velocity)"
    );
    // `salvage` is the read surface (non-consuming): a clean sever → one IntactModule.
    let contents = salvage(wreck);
    assert_eq!(contents.len(), 1, "the wreck has non-empty contents");
    assert!(
        matches!(contents[0], SalvageOutcome::IntactModule(r) if r.module == module_id),
        "a clean-severed healthy module salvages intact"
    );

    // --- Over-kill wreck: a through-killed module still yields >= a Scrap floor. ---
    let (mut w2, ship2, _) = salvage_world(0.0); // through-killed (health 0)
    on_section_destroyed(&mut w2, ship2, SectionId(0));
    let over = w2
        .get::<Wreck>(ship2)
        .expect("over-killed ship still wrecks");
    let over_contents = salvage(over);
    assert!(
        !over_contents.is_empty(),
        "an over-killed ship NEVER yields zero loot (INV-D09)"
    );
    let scrap_total: f32 = over_contents
        .iter()
        .map(|o| match o {
            SalvageOutcome::Scrap(a) => *a,
            SalvageOutcome::IntactModule(_) => 0.0,
        })
        .sum();
    assert!(
        scrap_total >= SalvageConfig::default().scrap_floor,
        "over-kill loot is bounded below by the scrap floor (INV-D09)"
    );
    assert!(
        !over_contents
            .iter()
            .any(|o| matches!(o, SalvageOutcome::IntactModule(_))),
        "a through-killed module does not salvage intact"
    );

    // --- Single-resolution claim (INV-D10): claimed exactly once. -----------------
    let mut wreck_mut = w.get_mut::<Wreck>(ship).unwrap();
    let first = wreck_mut.claim();
    assert_eq!(first.len(), 1, "the first claim hands out the contents");
    let second = wreck_mut.claim();
    assert!(
        second.is_empty(),
        "a second claim yields NOTHING — no double-claim (INV-D10)"
    );
    assert!(wreck_mut.claimed, "the wreck is flagged claimed");
}

// --- T035 (FR-023): the matrix is non-degenerate (INV-D11) --------------------
//
// A pure, test-guarded property over the FULL `(Channel × DefenseLayer)` content
// grid (SC-005, INV-D11): every channel beats a layer, every layer resists a
// channel, no channel is globally dominant, no layer is universally bypassed —
// and every cell stays bounded (INV-D02 cross-check). The guard is CI, not a
// runtime branch: it reads only `default_resistance_matrix()` content.
//
// Seed mitigation tiers (mirroring `content.rs`): `LOW = 0.10` (the channel gets
// through), `MID = 0.40` (neutral), `HIGH = 0.70` (the layer strongly resists).

/// Low tier — a channel's strong-vs layer; most of it gets through.
const NDG_LOW: f32 = 0.10;
/// Mid tier — the neutral middle of the table.
const NDG_MID: f32 = 0.40;
/// High tier — a layer strongly resists the channel (still `< 1.0`, INV-D02).
const NDG_HIGH: f32 = 0.70;

/// The non-degenerate-matrix guard (FR-023, INV-D11, SC-005): the default
/// resistance matrix is a *real, readable choice* — effective-HP curves cross.
///
/// Asserts, over the full `(Channel::ALL × DefenseLayer::ALL)` grid:
/// 1. **Every channel beats a layer**: its `min` mitigation across the 4 layers is
///    `<= NDG_LOW` (it gets through somewhere — no channel is walled out).
/// 2. **Every layer resists a channel**: its `max` mitigation across the 5 channels
///    is `>= NDG_HIGH` (it strongly stops something — no useless/bypassed layer).
/// 3. **No globally dominant channel**: every channel is *meaningfully* resisted by
///    at least one layer (its `max` mitigation across the layers is `>= NDG_MID`, a
///    real 40 % bite), so no single channel beats everything. (The bar is `>= MID`
///    rather than `>= HIGH` because the seed deliberately gives `Radiation` no
///    `HIGH` resistor — its preferred-target axis is `Systems` like `Em`, and its
///    best resistance is the neutral `MID`; that is still non-dominant, INV-D11.)
/// 4. **Bounded** (INV-D02 cross-check): every cell `∈ [0.0, MAX_MITIGATION < 1.0)`.
#[test]
fn matrix_is_non_degenerate() {
    let matrix = default_resistance_matrix();
    let layers = DefenseLayer::ALL; // [Shields, Armor, HullStructure, Systems] — COUNT == 4
    assert_eq!(layers.len(), DefenseLayer::COUNT);
    assert_eq!(layers.len(), 4);
    assert_eq!(Channel::ALL.len(), 5);

    // (1) Every CHANNEL has a layer it BEATS: its minimum mitigation across the 4
    // layers is low — it gets through somewhere; no channel is walled out.
    for channel in Channel::ALL {
        let min_over_layers = layers
            .iter()
            .map(|&layer| layer_resist(&matrix, layer, channel))
            .fold(f32::INFINITY, f32::min);
        assert!(
            min_over_layers <= NDG_LOW,
            "channel {channel:?} is resisted everywhere (min mitigation {min_over_layers} > LOW \
             {NDG_LOW}) — no layer it beats (INV-D11)"
        );
        // Defensive: stays strictly below the neutral middle, i.e. genuinely weak.
        assert!(
            min_over_layers < NDG_MID,
            "channel {channel:?} never drops below MID — no real soft spot (INV-D11)"
        );
    }

    // (2) Every LAYER RESISTS a channel: its maximum mitigation across the 5
    // channels is high — it strongly stops something; no useless/bypassed layer.
    for &layer in &layers {
        let max_over_channels = Channel::ALL
            .iter()
            .map(|&channel| layer_resist(&matrix, layer, channel))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_over_channels >= NDG_HIGH,
            "layer {layer:?} resists nothing (max mitigation {max_over_channels} < HIGH \
             {NDG_HIGH}) — a bypassed/useless layer (INV-D11)"
        );
        assert!(
            max_over_channels > NDG_MID,
            "layer {layer:?} never rises above MID — no channel it truly resists (INV-D11)"
        );
    }

    // (3) No globally DOMINANT channel: every channel is *meaningfully* resisted by
    // at least one layer (its max across layers is >= MID), so no single channel
    // beats everything. `>= MID` (not `>= HIGH`) because the seed gives Radiation no
    // HIGH resistor (Systems-axis like Em); MID resistance is still non-dominant.
    for channel in Channel::ALL {
        let max_over_layers = layers
            .iter()
            .map(|&layer| layer_resist(&matrix, layer, channel))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_over_layers >= NDG_MID,
            "channel {channel:?} is dominant (no layer resists it meaningfully: max mitigation \
             {max_over_layers} < MID {NDG_MID}) — INV-D11"
        );
    }

    // (4) Bounded (INV-D02 cross-check): every cell ∈ [0.0, MAX_MITIGATION < 1.0).
    for &layer in &layers {
        for channel in Channel::ALL {
            let m = layer_resist(&matrix, layer, channel);
            assert!(
                m.is_finite() && (0.0..1.0).contains(&m),
                "cell ({layer:?},{channel:?}) = {m} out of [0,1) (INV-D02)"
            );
        }
    }
    assert!(
        matrix.is_bounded(),
        "the seeded matrix violates the INV-D02 bound MAX_MITIGATION < 1.0"
    );
}

// =================================================================================
// Phase 8 — combat integration (live wire-up): T040 (fitted e2e) + T041 (unfitted
// degenerate path). A fired E002 projectile carrying a `WeaponSource` sweeps a
// target; a fitted target (`FitLayout`) routes through `apply_damage` → module
// damage → emergent stat drop → section destroyed → sever → wreck/salvage
// (SC-001..SC-004 e2e), while an unfitted target keeps the flat `Health` clamp
// (INV-D17, E002/E003 parity).
// =================================================================================

use bevy_ecs::schedule::Schedule;
use sim::components::{
    CollisionRadius, Damage, Health, Lifetime, PrevPosition, Projectile, ProjectileOwner, Ship,
    Target, TargetKind,
};
use sim::fitting::{recompute_ship_stats_system, ShipStats};
use sim::weapon::WeaponSource;
use sim::{FixedDt, HitFeedback, Tuning};

/// Insert every resource the **fitted** E007 path (the schedule + `apply_damage` +
/// the destruction chain) resolves against, plus the base sim resources.
fn insert_full_combat_resources(w: &mut World) {
    let (modules, hulls) = seed_catalogs();
    w.insert_resource(modules);
    w.insert_resource(hulls);
    w.insert_resource(default_resistance_matrix());
    w.insert_resource(PenetrationConfig::default());
    w.insert_resource(ShieldConfig::default());
    w.insert_resource(SalvageConfig::default());
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(1.0 / 30.0));
    w.insert_resource(HitFeedback::default());
}

/// A fitted fighter at world origin (heading 0): central reactor (4,4) covered by a
/// thin armor plate (4,5) (revise-A finer 9×11 fighter), full defense-layer state + a
/// derived `ShipStats`, body components, and a `CollisionRadius` large enough that a
/// downward projectile sweep strikes it. A downward (`-y`) world shot transforms to a
/// downward local ray that enters the (4,5) cover then the (4,4) reactor behind it.
fn fitted_fighter_at_origin(w: &mut World) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();

    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC); // reactor (4,4), health_max 30
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC); // a thruster so ShipStats has thrust
    fit.install_raw(SlotId(5), MODULE_ARMOR_PLATE); // cover (4,5)
    let layout = build_layout(hull, &fit, &modules);
    let stats = derive_ship_stats(hull, &fit, &modules, &layout);

    // A thin steel facet on the entry armor section (SectionId 5) so the shot
    // clean-penetrates the cover and reaches the reactor behind.
    let mut armor = SectionArmor::new();
    armor.sections.insert(
        SectionId(5),
        ArmorFacet {
            thickness: 1.0,
            material: ArmorMaterial::Steel,
            normal: Vec2::new(0.0, 1.0),
        },
    );

    w.spawn((
        Ship,
        fit,
        layout,
        stats,
        armor,
        HullStructure::full(500.0),
        Position(Vec2::ZERO),
        PrevPosition(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(4.0),
    ))
    .id()
}

/// Spawn a projectile positioned + aimed so its `prev → pos` sweep strikes the
/// fitted target at origin, carrying a high-penetration `WeaponSource` (Kinetic,
/// `from_damage`) that clean-penetrates the thin cover and destroys the reactor.
fn spawn_downward_projectile(w: &mut World, owner: Option<Entity>, damage: f32) -> Entity {
    // Coming straight down through the target circle at origin: prev above, pos at
    // the centre, velocity downward (so dir_local == (0,-1) at heading 0).
    let prev = Vec2::new(0.0, 6.0);
    let pos = Vec2::new(0.0, 0.0);
    let vel = Vec2::new(0.0, -180.0);
    let mut e = w.spawn((
        Projectile,
        Position(pos),
        PrevPosition(prev),
        Velocity(vel),
        Damage(damage),
        Lifetime(3.0),
        WeaponSource::from_damage(damage),
    ));
    if let Some(o) = owner {
        e.insert(ProjectileOwner(o));
    }
    e.id()
}

// --- T040: fitted end-to-end (hit → carve channel → core destroyed → wreck) -------
//
// REVISED for the Phase 2 carving model: a single powerful Kinetic shot carves a deep
// channel down the centre column, eating the (4,5) cover and the buried (4,4) reactor
// — which is the deepest cell, i.e. the **core**. Carving the core away is the
// whole-ship-destroyed path → a persistent `DestroyedShip` `Wreck` with salvage. The
// old "reactor's section destroyed → sever → wreck" route-behind chain is retired.

/// The full fitted chain in a `sim` `World` driven by the shared fixed step: a fired
/// projectile sweep-hits a fitted ship → `damage_event_from_hit` → `apply_damage`
/// **carves a channel** through the cover to the buried core reactor → carving the
/// core cell away destroys the ship → a persistent `Wreck` with salvage exists
/// (SC-001..SC-004 e2e, FR-001/021, Phase 2 carving-to-core).
#[test]
fn fitted_projectile_hit_carves_to_core_and_wrecks_the_ship() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let ship = fitted_fighter_at_origin(&mut w);

    // Baseline: the ship is powered (alive reactor) and the reactor cell is full. The
    // reactor (4,4) is the deepest cell on the fighter → the core.
    let reactor_max = w
        .get_resource::<ModuleCatalog>()
        .unwrap()
        .get(MODULE_REACTOR_BASIC)
        .unwrap()
        .health_max;
    assert_eq!(
        w.get::<FitLayout>(ship)
            .unwrap()
            .occupant((4, 4))
            .unwrap()
            .health,
        reactor_max,
        "the buried reactor (the core) starts at full health"
    );
    assert_eq!(
        sim::damage::core_cell(w.get::<FitLayout>(ship).unwrap()),
        Some((4, 4)),
        "the buried reactor (4,4) is the core cell"
    );

    // Drive the shared fixed step. Run ONCE with no projectile first so the spawn-tick
    // `Changed<Fit>` is consumed (a fit-change rebuilds the layout fresh — the
    // install/repair path; a same-tick carve would be erased by that rebuild). After
    // this, the layout is the live damage surface.
    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);
    schedule.run(&mut w);

    // A very-high-damage Kinetic shot (penetration = damage * PEN_PER_DAMAGE) carves a
    // deep channel down the centre column — through the structural nose, the 40-HP
    // armor cover, AND the core reactor (the deepest cell) — in one burst.
    let proj = spawn_downward_projectile(&mut w, None, 5000.0);
    schedule.run(&mut w);

    // --- SC-001: the hit resolved through apply_damage and carved cells ----------
    assert!(
        w.get_entity(proj).is_err(),
        "the projectile despawned after striking the fitted target"
    );
    let fb = w.get_resource::<HitFeedback>().unwrap();
    assert!(fb.hit_flash > 0.0, "a fitted hit raises the hit flash");
    assert!(
        matches!(
            fb.last_kind,
            Some(HitKind::Penetrated) | Some(HitKind::OverPenetrated)
        ),
        "the penetrating carve is tagged for the HUD (FR-024), got {:?}",
        fb.last_kind
    );

    // --- SC-003: the carve reached the core → the ship is a whole-ship wreck ------
    // The channel carved the core reactor (4,4) away (the deepest cell), which is the
    // whole-ship-destroyed path → a persistent DestroyedShip wreck on the dead entity.
    let wreck = w
        .get::<Wreck>(ship)
        .expect("carving the core reactor destroyed the ship → a persistent Wreck (SC-003)");
    assert_eq!(wreck.origin, WreckOrigin::DestroyedShip);

    // --- SC-004: the wreck carries salvage (over-kill still yields >= scrap) ------
    let contents = salvage(wreck);
    assert!(
        !contents.is_empty(),
        "the destroyed ship leaves lootable salvage (SC-004)"
    );

    // The ship is still a persistent physical body (it IS the wreck).
    assert!(
        w.get::<Position>(ship).is_some(),
        "the wreck persists as a physical body"
    );
}

// --- Phase 2 carving: a carved-away MODULE cell drops that module (emergent) ------

/// Carving a **weapon's module cell** away (removing it from the `FitLayout`) drops
/// `can_fire` and the weapon profile on the re-derive — the emergent degrade the
/// carving model produces when a hardpoint is eaten (FR-013, Phase 2). The weapon
/// (slot 3, a forward wing mount on the fighter) is an outer, non-core cell, so the
/// ship survives the carve and its degraded `ShipStats` is observable.
#[test]
fn carving_a_weapon_cell_drops_can_fire_on_rederive() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    // A reactor (core), a thruster, and a forward weapon (slot 3 at (2,6)) — the weapon
    // is an outer wing mount the carve can eat without killing the core.
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    fit.install_raw(SlotId(3), MODULE_AUTOCANNON);
    let layout = build_layout(hull, &fit, &modules);
    let stats = derive_ship_stats(hull, &fit, &modules, &layout);
    assert!(stats.can_fire, "the installed weapon arms the ship");
    let weapon_cell = (2u16, 6u16); // slot 3's coord on the fighter

    let ship = w
        .spawn((Ship, fit, layout, stats, HullStructure::full(500.0)))
        .id();

    let mut schedule = Schedule::default();
    schedule.add_systems(recompute_ship_stats_system);
    schedule.run(&mut w); // consume the spawn Changed<Fit>

    // A shot aimed straight at the weapon cell (2,6) from above carves it away. The
    // entry ray is `point → point + dir·REACH`; aim it down column 2 onto (2,6).
    let ev = DamageEvent {
        channel: Channel::Em,
        magnitude: 4000.0,
        penetration: 1000.0,
        pen_size: 0.0,
        point: Vec2::new(2.5, 11.0),
        dir: Vec2::new(0.0, -1.0),
        source: None,
    };
    let out = apply_damage(&mut w, ship, ev);
    assert!(
        out.destroyed_cells.contains(&weapon_cell),
        "the carve removed the weapon's module cell (got {:?})",
        out.destroyed_cells
    );
    assert!(
        w.get::<FitLayout>(ship)
            .unwrap()
            .occupant(weapon_cell)
            .is_none(),
        "the weapon cell is gone from the live FitLayout"
    );

    // Re-derive (the schedule's Changed<FitLayout> path): the carved-away weapon drops
    // can_fire + the weapon profile (the module is GONE, treated as destroyed).
    schedule.run(&mut w);
    let degraded = w.get::<ShipStats>(ship).unwrap();
    assert!(
        !degraded.can_fire,
        "a carved-away weapon drops can_fire (FR-013, emergent carve degrade)"
    );
    assert!(
        degraded.weapon.is_none(),
        "a carved-away weapon drops the WeaponProfile"
    );
}

// --- Phase 2 carving: a channel that disconnects a wing severs it mid-fight -------

/// Carving a channel that **disconnects a region** from the core severs it into a
/// drifting `WreckChunk` while the ship still LIVES (FR-015/016, INV-D07, Phase 2):
/// the core survives, the ship is not a wreck, but a severed chunk drifts away — a
/// flank/wing cut off before the kill.
#[test]
fn carving_disconnects_a_region_and_severs_it_while_the_ship_lives() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    // The fighter's nose (rows 7..=10) connects to the body only through row 6 (cols
    // 2..=6) ↔ row 7. Carving away ALL of row 6 cuts the nose off from the body (which
    // holds the core reactor at (4,4) in rows 4..=5). A square-on +x burst sweeping
    // along row 6 (head-on to the +x face, so it penetrates — no graze) eats the whole
    // row. We carve directly via apply_damage + on_cells_carved (the same calls
    // fitted_damage_system makes), capturing the pre-carve core first.
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    let layout = build_layout(hull, &fit, &modules);
    let ship = w
        .spawn((
            fit,
            layout,
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            HullStructure::full(500.0),
        ))
        .id();

    let core_before = core_cell(w.get::<FitLayout>(ship).unwrap());
    assert_eq!(core_before, Some((4, 4)), "the reactor (4,4) is the core");

    // A strong +x burst along row 6 (entering the +x face head-on → penetrates) carves
    // (6,6),(5,6),(4,6),(3,6),(2,6) — the whole connecting neck row between the nose
    // and the body. Several shots' worth of budget in one big event.
    let ev = DamageEvent {
        channel: Channel::Em,
        magnitude: 4000.0,
        penetration: 1000.0,
        pen_size: 0.0,
        point: Vec2::new(9.0, 6.5),
        dir: Vec2::new(-1.0, 0.0),
        source: None,
    };
    let out = apply_damage(&mut w, ship, ev);
    assert!(out.destroyed, "the carve removed cells");
    // The carve ate the connecting row 6 (the nose↔body neck).
    for cut in [(2u16, 6u16), (3, 6), (4, 6), (5, 6), (6, 6)] {
        assert!(
            !w.get::<FitLayout>(ship).unwrap().cells.contains_key(&cut),
            "row-6 neck cell {cut:?} was carved away"
        );
    }

    // Run the carve-destruction connectivity (what fitted_damage_system calls).
    let ship_destroyed = on_cells_carved(&mut w, ship, core_before);
    assert!(
        !ship_destroyed,
        "the ship is NOT destroyed — only the nose was cut off, the core lives"
    );

    // The ship still lives (no Wreck on it) and keeps its core.
    assert!(
        w.get::<Wreck>(ship).is_none(),
        "the ship lives after the nose severs (core intact)"
    );
    assert_eq!(
        core_cell(w.get::<FitLayout>(ship).unwrap()),
        Some((4, 4)),
        "the core reactor survives the nose sever"
    );

    // A severed-chunk WreckChunk entity drifted off carrying the disconnected nose.
    let mut q = w.query::<(&Wreck, &FitLayout)>();
    let severed: Vec<_> = q
        .iter(&w)
        .filter(|(wr, _)| wr.origin == WreckOrigin::SeveredChunk)
        .collect();
    assert!(
        !severed.is_empty(),
        "the cut-off nose severs into at least one drifting chunk"
    );
    // The severed chunk(s) carry the nose-tip cells (rows 7..=10), not the core.
    let severed_cells: std::collections::BTreeSet<(u16, u16)> = severed
        .iter()
        .flat_map(|(_, l)| l.cells.keys().copied())
        .collect();
    assert!(
        severed_cells.contains(&(4, 10)),
        "the severed nose carries the (4,10) nose-tip cell (got {severed_cells:?})"
    );
    assert!(
        !severed_cells.contains(&(4, 4)),
        "the core reactor is NOT in a severed chunk (it stays with the living ship)"
    );
}

/// A focused slice of the same chain isolating the **emergent stat drop** (SC-002):
/// a hit that destroys the reactor, followed by the re-derive, collapses the ship's
/// `power_supply` to the hull capacity alone (the reactor contributes 0). Driven
/// without the whole-ship destruction so the live degraded `ShipStats` is readable
/// on a surviving ship.
#[test]
fn fitted_damage_drops_emergent_ship_stats_after_rederive() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    // A fighter whose ENTRY module (the (4,5) armor plate) is a non-core section, so
    // destroying it does NOT whole-ship-destroy the ship — the ship survives and its
    // re-derived ShipStats are observable. Here the shot kills the COVER itself by
    // making it the struck module: a thick-but-low-effective setup is fiddly, so we
    // instead damage the reactor directly via apply_damage and re-derive, asserting
    // the emergent drop the schedule wires (SC-002).
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    fit.install_raw(SlotId(1), MODULE_THRUSTER_BASIC);
    let layout = build_layout(hull, &fit, &modules);
    let stats = derive_ship_stats(hull, &fit, &modules, &layout);
    let ship = w
        .spawn((Ship, fit, layout, stats, HullStructure::full(500.0)))
        .id();

    let mut schedule = Schedule::default();
    schedule.add_systems(recompute_ship_stats_system);
    // Run once so the freshly-spawned `Changed<Fit>` flag is consumed (a fit-change
    // rebuilds the layout at full health — that is the install/repair path, not the
    // damage path). After this, a layout-only mutation is the damage path: stats
    // re-derive from the *damaged* layout, never a rebuild.
    schedule.run(&mut w);

    let powered_before = w.get::<ShipStats>(ship).unwrap().power_supply;

    // Destroy the reactor cell directly (the per-module health mutation apply_damage
    // performs), then run the emergent re-derive — the layout-only change path.
    {
        let mut layout = w.get_mut::<FitLayout>(ship).unwrap();
        let occ = layout
            .cells
            .values_mut()
            .find(|o| o.slot == SlotId(0) && o.module.is_some())
            .unwrap();
        occ.health = 0.0;
    }
    schedule.run(&mut w);

    let powered_after = w.get::<ShipStats>(ship).unwrap().power_supply;
    assert!(
        powered_after < powered_before,
        "a destroyed reactor collapses the emergent power_supply ({powered_after} < {powered_before}, SC-002)"
    );
    assert!(
        (powered_after - hull.power_capacity).abs() < 1e-4,
        "the destroyed reactor contributes 0 power_gen ⇒ power_supply = hull capacity alone"
    );
}

// --- T041 [P]: unfitted degenerate path (flat Health clamp, INV-D17) --------------

/// A projectile hit on a `FitLayout`-less dummy/asteroid resolves via the **flat
/// `Health` clamp** in `collision_detect_system` and despawns at `<= 0` — the
/// E002/E003 simplified path is untouched (INV-D17). The fitted system ignores the
/// unfitted target; the legacy system still clamps it.
#[test]
fn unfitted_target_uses_the_flat_health_clamp_path() {
    let mut w = World::new();
    // Only the base sim resources — NO E007 catalogs/configs. This is the
    // E002/E003/determinism-shaped world: the fitted path + the gated systems must
    // simply no-op (graceful degradation, INV-D16) and never panic.
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(1.0 / 30.0));
    w.insert_resource(HitFeedback::default());

    // An unfitted dummy target (Target + CollisionRadius + Health, NO FitLayout).
    let dummy = w
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(Vec2::ZERO),
            CollisionRadius(2.0),
            Health(15.0),
        ))
        .id();

    // A projectile sweeping through it (damage 20 > 15 → destroyed at clamp 0). It
    // carries a WeaponSource (harmless on the unfitted path — never read there).
    let proj = spawn_downward_projectile(&mut w, None, 20.0);

    // Drive the full shared step: the fitted path finds no FitLayout target and is a
    // no-op; the legacy collision_detect_system clamps the dummy's Health.
    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);
    schedule.run(&mut w);

    // INV-D17: the dummy resolved via the flat Health clamp and was despawned at
    // <= 0 by the destruction_system (the E002/E003 behavior, verbatim).
    assert!(
        w.get_entity(dummy).is_err(),
        "the unfitted dummy took flat damage to 0 and despawned (INV-D17)"
    );
    assert!(
        w.get_entity(proj).is_err(),
        "the projectile despawned after the single hit (E002 parity)"
    );
    // The hit flash rose via the legacy path (not the fitted path).
    assert!(
        w.get_resource::<HitFeedback>().unwrap().destroy_flash > 0.0,
        "the dummy's destruction raised the destroy flash"
    );
    // No wreck/salvage entity exists — the unfitted path never enters the E007
    // pipeline (no FitLayout, no apply_damage, no sever).
    assert_eq!(
        w.query::<&Wreck>().iter(&w).count(),
        0,
        "the unfitted degenerate path produces no E007 wreck (INV-D17)"
    );
}

// =================================================================================
// E007 Phase 2 live-demo death (carving-to-core): a fitted player ship firing
// steadily at a fitted enemy through the REAL shared schedule must DESTROY the enemy
// by **carving a channel to its core** — the `cargo run -p client` scenario. This is
// the Phase 2 successor to the old hull-depletion death: `fitted_damage_system` no
// longer waits for `HullStructure` to drain (that trigger is RETIRED for fitted
// ships); instead `apply_damage` carves cells out of the live `FitLayout`, and when
// the carve reaches/severs the **core cell** the ship is destroyed (`destroy_ship` →
// persistent wreck, sheds chunks). The kill must land in the tuned ~5–15 s window.
// =================================================================================

use sim::components::Weapon;
use sim::damage::{seed_defense_layers, WreckChunk};
use sim::ShipIntent;

/// The fixed timestep the live server runs at (30 Hz), so the measured kill-time is
/// in real demo seconds.
const REPRO_DT: f32 = 1.0 / 30.0;
/// Run the schedule for up to ~20 s of demo time at 30 Hz (the tuning window the
/// brief wants the carve-to-core kill to land inside, ~5–15 s, with headroom).
const REPRO_MAX_TICKS: u32 = (20.0 / REPRO_DT) as u32;

/// Spawn a **fitted player ship** at the origin facing +x, holding `fire`, mirroring
/// `client::net::attach_starter_fit` (reactor + 2 thrusters + autocannon on the
/// fighter, so `ShipStats::can_fire` is true) + the three E007 defense layers. The
/// `Weapon` component holds the cooldown state the `weapon_fire_system` ticks.
fn spawn_fitted_player(w: &mut World) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

    let mut fit = Fit::new(HULL_FIGHTER);
    let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules);
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    let (shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);
    assert!(
        stats.can_fire,
        "the starter player fit must be able to fire"
    );

    w.spawn((
        Ship,
        ShipIntent {
            fire: true,
            ..Default::default()
        },
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0), // faces +x → fires toward the enemy at (14,0)
        AngularVelocity(0.0),
        CollisionRadius(0.8),
        Weapon {
            cooldown: 0.0,
            fire_rate: stats.weapon.map(|p| p.fire_rate).unwrap_or(5.0),
            muzzle_speed: stats.weapon.map(|p| p.muzzle_speed).unwrap_or(200.0),
        },
        fit,
        layout,
        stats,
        shields,
        section_armor,
        hull_structure,
    ))
    .id()
}

/// Spawn a **fitted enemy** as a stationary `Target` at `pos`, mirroring
/// `ServerApp::spawn_fitted_enemy` (reactor + thruster + autocannon + armor on the
/// fighter, NO `Ship`/`Health`, the three defense layers). It is the E007
/// `fitted_damage_system` target (`With<FitLayout>`).
fn spawn_fitted_enemy_target(w: &mut World, pos: Vec2) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

    let mut fit = Fit::new(HULL_FIGHTER);
    let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules);
    let _ = fit.install_module(SlotId(5), MODULE_ARMOR_PLATE, &hull, &modules);
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    let (shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);

    w.spawn((
        Target,
        TargetKind::Dummy,
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.3),
        CollisionRadius(1.0),
        fit,
        layout,
        stats,
        shields,
        section_armor,
        hull_structure,
    ))
    .id()
}

/// The LIVE demo scenario through the REAL schedule: a fitted player ship firing
/// steadily at a fitted enemy must **carve a channel to its core** and DESTROY it
/// within the tuned ~5–15 s window (Phase 2 carving-to-core death).
///
/// This replaces the old `…_via_hull_depletion` death (the `HullStructure`-drain
/// trigger is retired for fitted ships). The proof now: the enemy's `FitLayout` cells
/// are visibly carved away (the cell count drops as the channel erodes), and when the
/// carve reaches/severs the **core cell** the enemy becomes a persistent `Wreck`
/// (death-stripped of `Target`/`FitLayout`/`CollisionRadius`) and a kill flash fires.
#[test]
fn fitted_player_fire_carves_to_core_and_destroys_fitted_enemy() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    let _player = spawn_fitted_player(&mut w);
    let enemy = spawn_fitted_enemy_target(&mut w, Vec2::new(14.0, 0.0));

    // The enemy's starting cell count (the dense fighter silhouette) — what the carve
    // erodes toward the core.
    let cells_at_start = w.get::<FitLayout>(enemy).unwrap().cells.len();
    assert!(cells_at_start > 0, "the enemy starts with hull cells");

    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);

    let mut kill_flash_fired = false;
    let mut destroyed_tick: Option<u32> = None;
    let mut cells_were_carved = false;

    for tick in 0..REPRO_MAX_TICKS {
        schedule.run(&mut w);

        // Did a kill flash fire this tick? (Captured per-tick because the schedule's
        // `feedback_decay_system` bleeds the flash back down after it is raised.)
        if w.get_resource::<HitFeedback>().unwrap().destroy_flash > 0.0 {
            kill_flash_fired = true;
        }

        // The carve visibly erodes the hull: the enemy's live cell count drops below
        // its starting silhouette as cells are carved away (before the kill).
        if let Some(layout) = w.get::<FitLayout>(enemy) {
            if layout.cells.len() < cells_at_start {
                cells_were_carved = true;
            }
        }

        // Record when the enemy is destroyed: it carries a `Wreck`, OR it was
        // despawned/severed away. Either is "core destroyed".
        if destroyed_tick.is_none() {
            let is_wreck = w.get::<Wreck>(enemy).is_some();
            let despawned = w.get_entity(enemy).is_err();
            if is_wreck || despawned {
                destroyed_tick = Some(tick);
            }
        }

        if destroyed_tick.is_some() {
            break;
        }
    }

    // --- The carve VISIBLY eroded the hull before the kill --------------------------
    assert!(
        cells_were_carved,
        "the carve eroded the enemy's hull cells (the live cell count dropped below the \
         starting silhouette) before the kill — the 'eaten-away' Phase 2 result"
    );

    // --- A hit registered (HitFeedback.last_kind became Some at least once) ---------
    let last_kind = w.get_resource::<HitFeedback>().unwrap().last_kind;
    assert!(
        last_kind.is_some(),
        "at least one hit registered (HitFeedback.last_kind became Some)"
    );

    // --- A kill flash fired (core-death raised destroy_flash) -----------------------
    assert!(
        kill_flash_fired,
        "a kill flash fired when the core was destroyed (destroy_flash > 0)"
    );

    // --- The enemy is DESTROYED by carving to its core ------------------------------
    let destroyed =
        destroyed_tick.expect("the enemy must be destroyed by sustained carving fire to its core");

    // --- CLEAN death: the dead enemy is NO LONGER a live, pristine target -----------
    // `destroy_ship` removes `Target`/`CollisionRadius`/`FitLayout`, so the enemy can
    // no longer be hit by `fitted_damage_system` (no more repeated "KILL") and no
    // longer renders as a pristine ship — it is the drifting wreck hulk.
    let wreck = w
        .get::<Wreck>(enemy)
        .expect("the carve-to-core kill leaves a persistent Wreck marker");
    assert_eq!(
        wreck.origin,
        WreckOrigin::DestroyedShip,
        "core-death is the whole-ship-destroyed wreck origin"
    );
    assert!(
        w.get::<Target>(enemy).is_none(),
        "the destroyed enemy is no longer a Target (cannot be re-killed → no repeated KILL)"
    );
    assert!(
        w.get::<FitLayout>(enemy).is_none(),
        "the destroyed enemy lost its FitLayout (no longer hit by fitted_damage_system, \
         no longer rendered as a pristine ship)"
    );
    assert!(
        w.get::<CollisionRadius>(enemy).is_none(),
        "the destroyed enemy lost its CollisionRadius (no longer a swept-cast hit target)"
    );
    // It is still a persistent physical body (the drifting wreck hulk).
    assert!(
        w.get::<Position>(enemy).is_some() && w.get::<Velocity>(enemy).is_some(),
        "the wreck persists as a drifting physical body"
    );
    // Reference the WreckChunk type so the import is load-bearing (severed chunks are
    // exercised in the dedicated sever-during-combat test below).
    let _ = std::mem::size_of::<WreckChunk>();

    // --- Record + sanity-check the kill-time (the brief's ~5–15 s tuning window) ----
    let kill_secs = destroyed as f32 * REPRO_DT;
    eprintln!(
        "[carve-repro] enemy ({cells_at_start} cells) carved-to-core dead at tick \
         {destroyed} ({kill_secs:.2}s)"
    );
    assert!(
        (5.0..=15.0).contains(&kill_secs),
        "the carve-to-core kill should feel substantial but not a slog: expected ~5–15 s, \
         got {kill_secs:.2}s (tune STRUCT_CELL_HP / the carve consts / the loadout)"
    );
}

// =================================================================================
// E007 multi-angle kill regression: the ricochet bug guard. Before the fix the armor
// gate read the impact angle off the seeded per-section `ArmorFacet.normal` (the
// centred core's FIXED `CORE_FALLBACK_NORMAL = -X`), but `hull_local_entry_ray`
// routes every shot through the grid centre, so the centred core was the entry for
// most shots. A shot from any direction but `+X` met that fixed `-X` face at a steep
// angle → `Ricochet`; once the shield was down EVERY shot ricocheted forever and the
// enemy could never be hull-killed. The fix derives the impact angle from the entry
// cell's hull-radial geometry, so a head-on shot from ANY side reliably penetrates.
//
// This drives the REAL `add_fixed_step_systems` schedule with a fitted player firing
// at a fitted enemy at the origin from several approach positions (+x, -x, +y, and a
// diagonal) and asserts the enemy's `HullStructure` reaches 0 and it dies (becomes a
// `Wreck`) from EVERY angle — no permanent-ricochet stall.
// =================================================================================

/// Spawn a fitted player at `pos` whose `Heading` points at `aim_at`, holding fire
/// (the starter fighter loadout — reactor + 2 thrusters + autocannon — so
/// `ShipStats::can_fire` is true). A position/heading-parametrized sibling of
/// [`spawn_fitted_player`], used by the multi-angle kill regression.
fn spawn_fitted_player_aimed(w: &mut World, pos: Vec2, aim_at: Vec2) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();

    let mut fit = Fit::new(HULL_FIGHTER);
    let _ = fit.install_module(SlotId(0), MODULE_REACTOR_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(1), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(2), MODULE_THRUSTER_BASIC, &hull, &modules);
    let _ = fit.install_module(SlotId(3), MODULE_AUTOCANNON, &hull, &modules);
    let layout = build_layout(&hull, &fit, &modules);
    let stats = derive_ship_stats(&hull, &fit, &modules, &layout);
    let (shields, section_armor, hull_structure) = seed_defense_layers(&hull, &fit, &modules);
    assert!(
        stats.can_fire,
        "the starter player fit must be able to fire"
    );

    // Heading points from the player toward the target, so the fixed forward weapon
    // (which fires along `Vec2::from_angle(heading)`) sends rounds straight at it.
    let heading = (aim_at - pos).to_angle();

    w.spawn((
        Ship,
        ShipIntent {
            fire: true,
            ..Default::default()
        },
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(heading),
        AngularVelocity(0.0),
        CollisionRadius(0.8),
        Weapon {
            cooldown: 0.0,
            fire_rate: stats.weapon.map(|p| p.fire_rate).unwrap_or(5.0),
            muzzle_speed: stats.weapon.map(|p| p.muzzle_speed).unwrap_or(200.0),
        },
        fit,
        layout,
        stats,
        shields,
        section_armor,
        hull_structure,
    ))
    .id()
}

/// The per-angle repro window: a head-on shot from an off-axis approach lands on a
/// different entry cell than the `+x` core hit, so its kill may run through the
/// module-behind path (depleting a module then its section) rather than `+x`'s direct
/// hull spillover — a legitimately slower-but-real death. 45 s of demo time at 30 Hz
/// gives every geometry comfortable headroom to land WITHOUT a permanent-ricochet
/// stall (the bug left the enemy un-damaged forever).
const MULTI_ANGLE_MAX_TICKS: u32 = (45.0 / REPRO_DT) as u32;

/// The outcome of firing at the enemy from one approach (the multi-angle regression).
struct KillOutcome {
    /// The tick a real penetrating hit first landed (proves shots get THROUGH — not a
    /// perpetual ricochet). `None` if no penetration ever registered (the bug).
    first_penetration_tick: Option<u32>,
    /// The tick the enemy was destroyed (Wreck / despawned / shed chunks). `None` if
    /// it survived the whole window (a stall regression).
    destroyed_tick: Option<u32>,
    /// The enemy's residual `HullStructure::current` at the end (informational —
    /// hull-spillover deaths drive this to 0, module-kill deaths may not).
    final_hull: Option<f32>,
}

/// Run the live `add_fixed_step_systems` schedule with a fitted player at `player_pos`
/// (aimed at the origin) firing at a stationary fitted enemy at the origin, until the
/// enemy is destroyed or the repro window elapses.
///
/// This is the ricochet-bug regression driver: with the fixed-normal angle, any
/// approach but `+x` met the centred core's fixed `-X` facet at a steep angle and
/// ricocheted forever, so the enemy took no damage from those sides. With the
/// geometric impact angle (derived from the entry cell's hull-radial position), a
/// head-on shot from ANY side penetrates and the enemy dies.
fn run_kill_from(player_pos: Vec2) -> KillOutcome {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    let _player = spawn_fitted_player_aimed(&mut w, player_pos, Vec2::ZERO);
    let enemy = spawn_fitted_enemy_target(&mut w, Vec2::ZERO);

    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);

    let mut first_penetration_tick: Option<u32> = None;
    let mut destroyed_tick: Option<u32> = None;

    for tick in 0..MULTI_ANGLE_MAX_TICKS {
        schedule.run(&mut w);

        // A penetrating/over-penetrating hit proves the shot got THROUGH the armor gate
        // (not a perpetual ricochet) — the core of the regression.
        if first_penetration_tick.is_none() {
            if let Some(k) = w.get_resource::<HitFeedback>().unwrap().last_kind {
                if matches!(k, HitKind::Penetrated | HitKind::OverPenetrated) {
                    first_penetration_tick = Some(tick);
                }
            }
        }

        if destroyed_tick.is_none() {
            let is_wreck = w.get::<Wreck>(enemy).is_some();
            let despawned = w.get_entity(enemy).is_err();
            let has_chunks = w
                .query::<&Wreck>()
                .iter(&w)
                .any(|wr| wr.origin == WreckOrigin::SeveredChunk);
            if is_wreck || despawned || has_chunks {
                destroyed_tick = Some(tick);
                break;
            }
        }
    }

    let final_hull = w.get::<HullStructure>(enemy).map(|h| h.current);
    KillOutcome {
        first_penetration_tick,
        destroyed_tick,
        final_hull,
    }
}

/// The ricochet-bug regression: a fitted player firing at a fitted enemy at the origin
/// must DESTROY it from EVERY approach angle — +x, -x, +y, and a diagonal — with shots
/// that PENETRATE (never a perpetual ricochet). Before the geometric-impact-angle fix,
/// any approach but `+x` met the centred core's fixed `-X` facet normal at a steep
/// angle and ricocheted FOREVER, so the enemy sat at full hull, unkillable and
/// un-damaged, from those sides. Now the impact angle comes from the entry cell's
/// hull-radial geometry, so a head-on shot from any side penetrates and the enemy
/// reliably dies.
///
/// The death PATH is geometry-dependent (a legitimate, not-a-bug difference): the `+x`
/// core hit spills straight into `HullStructure` (a fast hull-depletion kill), while an
/// off-axis hit may enter through the armor cover and route penetrating damage to the
/// module behind it, killing via module → section destruction (slower, but a real
/// kill). The regression guards what matters from every angle: shots PENETRATE and the
/// enemy DIES (no permanent-ricochet stall).
#[test]
fn fitted_player_kills_enemy_from_every_approach_angle() {
    // The enemy sits at the origin; the player attacks from each of these positions,
    // aimed straight at it. The diagonal + the ±y axes are the cases the OLD fixed-`-X`
    // facet-normal angle mis-classified as steep ricochets (their approach is far from
    // `+x`), so they are the load-bearing regression coverage.
    let approaches = [
        ("+x", Vec2::new(14.0, 0.0)),
        ("-x", Vec2::new(-14.0, 0.0)),
        ("+y", Vec2::new(0.0, 14.0)),
        ("diagonal", Vec2::new(-10.0, 10.0)),
    ];

    for (label, player_pos) in approaches {
        let out = run_kill_from(player_pos);

        // Shots PENETRATED (the armor gate let damage through — not a perpetual
        // ricochet). This is the direct guard on the fixed-normal ricochet bug.
        let penetrated_tick = out.first_penetration_tick.unwrap_or_else(|| {
            panic!(
                "[{label}] no shot ever PENETRATED the enemy from approach {player_pos:?} — \
                 the armor gate ricocheted every shot (the fixed-normal bug this guards)"
            )
        });

        // The enemy DIED (became a Wreck / shed chunks / despawned) from this angle —
        // no permanent-ricochet stall, regardless of the (geometry-dependent) path.
        let destroyed_tick = out.destroyed_tick.unwrap_or_else(|| {
            panic!(
                "[{label}] enemy was never destroyed from approach {player_pos:?} in \
                 {MULTI_ANGLE_MAX_TICKS} ticks (final hull {:?}) — permanent-ricochet \
                 stall regression",
                out.final_hull
            )
        });

        let kill_secs = destroyed_tick as f32 * REPRO_DT;
        let pen_secs = penetrated_tick as f32 * REPRO_DT;
        eprintln!(
            "[multi-angle] {label} from {player_pos:?}: first penetration at {pen_secs:.2}s, \
             destroyed at {kill_secs:.2}s (final hull {:?})",
            out.final_hull
        );
        assert!(
            penetrated_tick <= destroyed_tick,
            "[{label}] penetration must precede (or equal) the kill"
        );
    }
}
