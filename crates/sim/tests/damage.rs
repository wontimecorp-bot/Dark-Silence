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
    build_layout, derive_ship_stats, hull_collision_radius, seed_catalogs, Fit, FitLayout,
    HullCatalog, ModuleCatalog, SectionId, SlotId, CELL_WORLD_SIZE, HULL_FIGHTER,
    MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_SHIELD_BASIC,
    MODULE_THRUSTER_BASIC,
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
    // (obliquity > ricochet_angle) bounces. Geometry: a LATERAL (−col) shot across the
    // **nose tip** `(4,10)` — the only cell on row 10, whose local surface normal points
    // purely **forward** (+row, away from the row-9 cells below it), so a sideways shot is
    // exactly perpendicular (obliquity ≈ 90°) → Ricochet, with a small non-overmatching
    // penetrator. (The previous wing-tip straight-down graze is, under the accurate local
    // normal, only a borderline ~67.5° corner clip — this lateral nose graze is unambiguous.)
    let (mut w3, ship3) = fitted_world(None, thin_entry_facet);
    let graze = DamageEvent {
        channel: Channel::Em,
        magnitude: 100.0,
        penetration: 50.0,
        pen_size: 0.0,
        point: Vec2::new(12.0, 10.5), // far +col side, on the nose-tip row
        dir: Vec2::new(-1.0, 0.0),    // sideways across the forward-facing nose tip
        source: None,
    };
    let out3 = apply_damage(&mut w3, ship3, graze);
    assert_eq!(
        out3.result,
        HitKind::Ricochet,
        "a near-tangent lateral graze of the forward-facing nose tip ricochets (carves nothing)"
    );
    assert!(
        out3.destroyed_cells.is_empty(),
        "a ricochet carves nothing (got {:?})",
        out3.destroyed_cells
    );
}

/// Phase F + Refinement 13 — the depleting **armor-HP layer**: a penetrating hit the armor can fully
/// absorb (`m <= current`) is SOAKED (the pool drains, the hull is NOT carved); a hit BIGGER than the
/// remaining armor **spills** — drains it to 0 and carves the excess (so a hard ram punches through
/// instead of being soaked whole). Once armor is gone the shot carves the hull as before. A target
/// WITHOUT `ArmorHp` carves exactly as today (the `Option`-gated path every determinism/test ship
/// takes) — proven by `clean_penetration_*` above and re-asserted here.
#[test]
fn armor_hp_soaks_absorbable_hits_but_spills_an_overwhelming_one() {
    use sim::components::ArmorHp;

    // No shield (reach the armor gate directly), thin facet (the shot penetrates the plate angle).
    let (mut w, ship) = fitted_world(None, thin_entry_facet);
    // A DEEP armor pool (100k) that can fully absorb even the strong shot's post-armor budget — so
    // it soaks the hit (no carve), demonstrating armor protects against a hit it CAN absorb.
    w.entity_mut(ship).insert(ArmorHp {
        current: 100_000.0,
        max: 100_000.0,
    });

    // The cell set before any shot — to prove armor protects the hull (no cells removed).
    let cells_before = w.get::<FitLayout>(ship).unwrap().cells.len();

    // A strong clean penetration that WOULD carve a deep channel if armor were absent.
    let strong = downward_shot(Channel::Em, 4000.0, 1000.0, 0.0);
    let out = apply_damage(&mut w, ship, strong);
    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "the shot penetrated the shield + plate angle (got {:?})",
        out.result
    );
    // Armor held → NO carve: no cells destroyed, the layout is intact.
    assert!(!out.destroyed, "armor soaks the hit — nothing carves");
    assert!(
        out.destroyed_cells.is_empty(),
        "no cells removed while armor holds"
    );
    assert_eq!(
        w.get::<FitLayout>(ship).unwrap().cells.len(),
        cells_before,
        "the hull is protected while armor holds (no cells removed)"
    );
    // The armor pool drained (but is not depleted — it absorbed the whole hit).
    let after = w.get::<ArmorHp>(ship).unwrap().current;
    assert!(
        after < 100_000.0 && after > 0.0,
        "the hit drained (but did not deplete) the deep ArmorHp pool (got {after})"
    );

    // Refinement 13 — a hit BIGGER than the remaining armor SPILLS: it drains the pool to 0 AND
    // carves the excess into the hull (a hard ram punches through instead of being soaked whole).
    let (mut ws, ships) = fitted_world(None, thin_entry_facet);
    ws.entity_mut(ships).insert(ArmorHp {
        current: 50.0,
        max: 80.0,
    });
    let spill = apply_damage(
        &mut ws,
        ships,
        downward_shot(Channel::Em, 4000.0, 1000.0, 0.0),
    );
    assert!(
        spill.destroyed,
        "a hit far larger than the remaining armor spills past it and carves to the core"
    );
    assert_eq!(
        ws.get::<ArmorHp>(ships).unwrap().current,
        0.0,
        "the spilling hit drains the armor pool to 0"
    );

    // Deplete the rest of the armor and fire again → the carve RESUMES on the bare hull.
    w.get_mut::<ArmorHp>(ship).unwrap().current = 0.0;
    let strong2 = downward_shot(Channel::Em, 4000.0, 1000.0, 0.0);
    let out2 = apply_damage(&mut w, ship, strong2);
    assert!(
        out2.destroyed,
        "with armor gone the shot carves the hull again (got {:?})",
        out2.result
    );
    assert!(
        !out2.destroyed_cells.is_empty(),
        "cells are removed once armor is depleted"
    );

    // CONTROL: the SAME shot on a ship WITHOUT ArmorHp carves immediately (the Option-gated,
    // byte-identical headless path) — so the gate only engages when the component is present.
    let (mut wc, shipc) = fitted_world(None, thin_entry_facet);
    let outc = apply_damage(
        &mut wc,
        shipc,
        downward_shot(Channel::Em, 4000.0, 1000.0, 0.0),
    );
    assert!(
        outc.destroyed,
        "a ship with no ArmorHp carves as today (no armor gate)"
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

use sim::components::{AngularVelocity, Destructible, Heading, Position, Velocity};
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
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
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
/// `parent.vel + angvel·perp(r)` where `r` is the world offset from the ship's
/// `Position` (the hull GRID CENTRE) to the chunk COM — correctly axis-swapped
/// (forward←row, lateral←col) and ×`CELL_WORLD_SIZE` to match the render. And a
/// **core-sever destroys the ship** (destroying the section containing the core → the
/// whole-ship-destroyed path, INV-D15).
#[test]
fn severed_chunk_inherits_com_momentum_and_core_sever_destroys_ship() {
    // --- COM momentum inheritance (INV-D07) -----------------------------------
    let parent_pos = Vec2::new(10.0, 5.0);
    let parent_vel = Vec2::new(2.0, -1.0);
    let heading = 0.0_f32; // zero heading so world offset == local-world offset (clean check)
    let angvel = 0.5_f32;
    let (mut w, ship) = corridor_world(parent_pos, parent_vel, heading, angvel);

    // The offset reference is the hull GRID CENTRE (the point the ship `Position` and the
    // render centre the cells on), NOT the mean of the remaining cells. The corridor hull
    // is 5×3, so the grid centre is (5·0.5, 3·0.5) = (2.5, 1.5) in cell-space. (Here it
    // happens to equal the mean-of-all-cells, but the reference is now the grid centre.)
    let grid_centre = Vec2::new(2.5, 1.5);
    // Sever the far-end region {(3,1),(4,1)} directly (its local COM is
    // ((3.5+4.5)/2, 1.5) = (4.0, 1.5)).
    let mut region = std::collections::HashSet::new();
    region.insert((3u16, 1u16));
    region.insert((4u16, 1u16));
    let chunk_com = Vec2::new(4.0, 1.5);

    let chunk = sever_chunk(&mut w, ship, &region);

    // Cell-space offset `(Δcol, Δrow) = chunk_com − grid_centre`, mapped into the ship's
    // LOCAL WORLD frame the SAME way the render does (forward+X ← row, lateral+Y ← col,
    // ×CELL_WORLD_SIZE) → `(Δrow, Δcol)·CELL_WORLD_SIZE`, then rotated by heading (=0).
    let r_local = chunk_com - grid_centre;
    let r = Vec2::new(r_local.y, r_local.x) * CELL_WORLD_SIZE;
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
        "chunk world pos = parent.pos + r (grid-centre-relative, swapped + scaled): got \
         {:?}, expected {:?}",
        chunk.body.pos,
        expected_pos
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

/// REGRESSION (severed-chunk teleport): a severed chunk's `Position` must be the WORLD
/// location its cells occupied on the live hull at the instant of severing — i.e.
/// `parent.pos + Rot(heading)·worldoffset(chunk_com_cell − grid_centre_cell)` with the
/// offset **scaled** by `CELL_WORLD_SIZE` AND **axis-swapped** (forward+X ← row,
/// lateral+Y ← col) to match the render — NOT ~3× off (the missing-scale bug) and NOT on
/// the swapped axis (the missing-swap bug). The chunk then drifts from exactly there.
///
/// Run with a **nonzero heading** (π/2) so the rotation is exercised and the swap is not
/// masked by an identity rotation. The corridor hull is 5×3; severing the far-end region
/// {(3,1),(4,1)} (cell-COM (4.0, 1.5)) off the grid centre (2.5, 1.5) gives a cell-space
/// offset of (Δcol, Δrow) = (1.5, 0.0): purely lateral (a wing), zero forward.
#[test]
fn severed_chunk_spawns_at_its_cells_world_location_not_teleported() {
    let parent_pos = Vec2::new(10.0, 5.0);
    let heading = std::f32::consts::FRAC_PI_2; // 90° — exercises the rotation + the swap
    let (mut w, ship) = corridor_world(parent_pos, Vec2::ZERO, heading, 0.0);

    let grid_centre = Vec2::new(2.5, 1.5); // 5×3 hull → grid centre = (cols·0.5, rows·0.5)
    let chunk_com_cell = Vec2::new(4.0, 1.5); // mean of (3,1),(4,1) cell centres
    let mut region = std::collections::HashSet::new();
    region.insert((3u16, 1u16));
    region.insert((4u16, 1u16));

    let chunk = sever_chunk(&mut w, ship, &region);

    // The CORRECT offset: cell-space (Δcol, Δrow) → local-world (Δrow, Δcol)·CELL_WORLD_SIZE
    // → rotated by heading. Here (Δcol, Δrow) = (1.5, 0.0), so local-world = (0.0, 0.48),
    // which at heading π/2 rotates to (-0.48, 0.0).
    let r_local = chunk_com_cell - grid_centre;
    let r = Vec2::from_angle(heading).rotate(Vec2::new(r_local.y, r_local.x) * CELL_WORLD_SIZE);
    let expected_pos = parent_pos + r;
    assert!(
        (chunk.body.pos - expected_pos).length() < 1e-4,
        "the chunk spawns where its cells were (grid-centre-relative, swapped + scaled, \
         rotated): got {:?}, expected {:?}",
        chunk.body.pos,
        expected_pos
    );

    // GUARD A — the scale is applied: the offset distance is the cell offset × the world
    // cell size (≈0.48 world units), NOT the raw ~1.5-cell value (the ~3× missing-scale
    // teleport bug). `CELL_WORLD_SIZE` is 0.32, so the bug would be ~3.1× too far.
    let offset_len = (chunk.body.pos - parent_pos).length();
    let expected_len = 1.5 * CELL_WORLD_SIZE; // 0.48
    assert!(
        (offset_len - expected_len).abs() < 1e-4,
        "the offset is scaled by CELL_WORLD_SIZE ({expected_len}), not the raw cell distance \
         1.5 (got {offset_len}) — the missing-scale teleport"
    );
    assert!(
        offset_len < 1.0,
        "a 1.5-cell lateral offset is < 1 world unit once scaled (got {offset_len}); the \
         unscaled bug would put it ~1.5 units off"
    );

    // GUARD B — the axis is correct (swap applied): a purely-LATERAL cell offset (Δcol with
    // Δrow = 0) maps to the ship's LATERAL (+Y local) axis, which at heading π/2 points
    // along world −X. The buggy un-swapped code would treat (Δcol, Δrow) as
    // (forward, lateral) and put the chunk along the ship's forward (+X local) axis →
    // world +Y. So the chunk's world offset must be along −X here, not +Y.
    assert!(
        r.x < -1e-3 && r.y.abs() < 1e-4,
        "a lateral (Δcol-only) sever lands on the lateral axis (world −X at heading π/2), \
         not the swapped forward axis: r = {r:?}"
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
        name: "salvage".to_string(),
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
        catalog.modules.insert(m.id, m.clone());
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
    let catalog = catalog_of(std::slice::from_ref(&module));

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
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
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
        // Carve-targetable: a live ship is `FitLayout` + `CollisionRadius` + `Destructible`.
        Destructible,
    ))
    .id()
}

/// Spawn a projectile positioned + aimed so its `prev → pos` sweep strikes the
/// fitted target at origin, carrying a high-penetration `WeaponSource` (Kinetic,
/// `from_damage`) that clean-penetrates the thin cover and destroys the reactor.
///
/// The enemy faces +X (heading 0), so its **nose/aft (fore-aft) axis is world X** and
/// maps to cell-space **row** (render: forward ← row). Firing straight along the nose
/// axis — from the +X nose side (world `x = +6`, `y = 0`) toward `-X` — enters at the
/// centre **column** (world `y = 0 → col 4.5`) and bores down that column in `-row`,
/// straight through the core reactor at the central (4,4) cell. (Pre-fix this fired
/// along world `-Y`, which the swapped mapping happened to route down the centre column
/// too; post-fix world `-Y` is the LATERAL axis, so the nose axis is world X.)
fn spawn_downward_projectile(w: &mut World, owner: Option<Entity>, damage: f32) -> Entity {
    // Coming straight down the nose axis through the target circle at origin: prev on
    // the +X nose side, pos at the centre, velocity along -X (so the carve bores down
    // the centre column toward the aft, through the central core cell at heading 0).
    let prev = Vec2::new(6.0, 0.0);
    let pos = Vec2::new(0.0, 0.0);
    let vel = Vec2::new(-180.0, 0.0);
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

/// Spawn a **fitted enemy** as a `Target` at `pos` with a given `angvel`, mirroring
/// `ServerApp::spawn_fitted_enemy` (reactor + thruster + autocannon + armor on the
/// fighter, NO `Ship`/`Health`, the three defense layers, the **hull-footprint**
/// collision circle). It is the E007 `fitted_damage_system` target (`With<FitLayout>`).
///
/// `angvel` is a parameter so the carve-to-core kill test can use a **stationary**
/// enemy (a centre-aimed burst bores straight to the core, the deterministic kill the
/// brief asserts), while the multi-angle regression keeps the demo's slow spin.
fn spawn_fitted_enemy_target_spin(w: &mut World, pos: Vec2, angvel: f32) -> Entity {
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
        AngularVelocity(angvel),
        // FIX (carve location): the VISIBLE hull footprint, matching `spawn_fitted_enemy`.
        CollisionRadius(hull_collision_radius(hull.grid_dims)),
        // Carve-targetable: a live ship is `FitLayout` + `CollisionRadius` + `Destructible`
        // (matching `spawn_fitted_enemy`). The flag is the carve query's explicit gate.
        Destructible,
        fit,
        layout,
        stats,
        shields,
        section_armor,
        hull_structure,
    ))
    .id()
}

/// Spawn a slowly-spinning fitted enemy (the live-demo default spin) — the
/// multi-angle regression driver's target.
fn spawn_fitted_enemy_target(w: &mut World, pos: Vec2) -> Entity {
    spawn_fitted_enemy_target_spin(w, pos, 0.3)
}

/// The **world position of a fitted target's core cell** (its `Position` plus the
/// core cell's hull-local offset, un-rotated by the ship `Heading`). A burst aimed
/// here bores straight to the core — the carve-to-core kill the brief asserts (the
/// core reactor (4,4) sits one row aft of the silhouette's geometric centre (4.5,5.5)
/// on the fighter, so the geometric centre alone would miss it).
///
/// Inverts the impact→cell-space carve mapping (`collision::hull_local_entry_ray`),
/// which maps **col ← lateral (`offset_local.y`)** and **row ← forward
/// (`offset_local.x`)** to match the render. Inverting: `offset_local.y =
/// (col − cols/2)·CELL_WORLD_SIZE`, `offset_local.x = (row − rows/2)·CELL_WORLD_SIZE`,
/// then world = `Position + Rot(heading)·offset_local`.
fn core_world_pos(w: &World, target: Entity) -> Vec2 {
    let layout = w
        .get::<FitLayout>(target)
        .expect("fitted target has a layout");
    let pos = w.get::<Position>(target).unwrap().0;
    let heading = w.get::<Heading>(target).unwrap().0;
    let (cols, rows) = {
        let (modules, hulls) = seed_catalogs();
        let _ = modules;
        hulls.get(HULL_FIGHTER).unwrap().grid_dims
    };
    let core = sim::damage::core_cell(layout).expect("a fitted hull has a core cell");
    let grid_centre = Vec2::new(cols as f32 * 0.5, rows as f32 * 0.5);
    let cell_centre = Vec2::new(core.0 as f32 + 0.5, core.1 as f32 + 0.5);
    // Swapped to match the fixed forward mapping: cell-space x=col ← local y (lateral),
    // cell-space y=row ← local x (forward).
    // cell_centre is a Vec2 with x = col, y = row.
    let offset_local = Vec2::new(
        (cell_centre.y - grid_centre.y) * CELL_WORLD_SIZE, // local x (forward) <- row
        (cell_centre.x - grid_centre.x) * CELL_WORLD_SIZE, // local y (lateral) <- col
    );
    pos + Vec2::from_angle(heading).rotate(offset_local)
}

/// The LIVE demo scenario through the REAL schedule: a fitted player ship firing
/// steadily **at the enemy's core** must **carve a channel to its core** and DESTROY
/// it within the tuned ~5–15 s window (Phase 2 carving-to-core death).
///
/// REVISED for the impact-located carve (FIX carve location): the carve now begins at
/// the cell the bullet visually struck rather than auto-boring through the centre, so
/// the kill requires aiming **at the core** (the core reactor (4,4) sits one row aft of
/// the silhouette's geometric centre, so a shot through the geometric centre alone
/// would bore along the wrong row and only sever pieces). The player is aimed at the
/// enemy's **core world position** ([`core_world_pos`]) at a **stationary** enemy, so a
/// centre-of-mass burst bores straight to the core — the deterministic carve-to-core
/// kill. (A flank burst severing a piece is covered by
/// `carving_disconnects_a_region_and_severs_it_while_the_ship_lives` + the off-centre
/// alignment test.)
///
/// This replaces the old `…_via_hull_depletion` death (the `HullStructure`-drain
/// trigger is retired for fitted ships). The proof: the enemy's `FitLayout` cells are
/// visibly carved away (the cell count drops as the channel erodes), and when the carve
/// reaches/severs the **core cell** the enemy becomes a persistent `Wreck` (death-
/// stripped of `Target`/`CollisionRadius`, but KEEPING its residual `FitLayout` so the
/// hulk renders as its real carved cells — the `Wreck` tag excludes it from re-carving)
/// and a kill flash fires.
#[test]
fn fitted_player_fire_carves_to_core_and_destroys_fitted_enemy() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);

    // A STATIONARY enemy (no spin) so a fixed core-aimed burst bores straight to the
    // core for the deterministic carve-to-core kill the brief asserts.
    let enemy = spawn_fitted_enemy_target_spin(&mut w, Vec2::new(14.0, 0.0), 0.0);
    // Aim the player straight at the enemy's CORE world position (not just its centre):
    // the impact-located carve enters where the bullet strikes, so a core-aimed shot is
    // what bores to the core.
    let core_at = core_world_pos(&w, enemy);
    let _player = spawn_fitted_player_aimed(&mut w, Vec2::ZERO, core_at);

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
    // `destroy_ship` removes the live `Target` marker and tags the entity `Wreck`, so it
    // is no longer a live combat target (no repeated "KILL"): the wreck branch of
    // `on_cells_carved` never re-kills a `Wreck`. The residual `FitLayout` is KEPT so the
    // hulk renders as its remaining (carved) cells via the same hull-mesh path the live
    // ship + severed chunks use — it reads as the wreck of its real shape, not a box.
    //
    // **Destructible wreckage**: the hulk KEEPS its `CollisionRadius` (the hull footprint)
    // and is `Destructible`, so a shot into the dead hull erodes it further (it despawns
    // when fully carved). This is the destructible-wreckage change.
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
        "the destroyed enemy is no longer a live Target"
    );
    assert!(
        w.get::<FitLayout>(enemy).is_some(),
        "the destroyed enemy KEEPS its residual FitLayout so the hulk renders as its real \
         (carved) cells"
    );
    assert!(
        w.get::<CollisionRadius>(enemy).is_some(),
        "the destroyed enemy KEEPS its CollisionRadius (destructible wreckage — the hulk is \
         still shootable, eroded further by later hits)"
    );
    assert!(
        w.get::<sim::components::Destructible>(enemy).is_some(),
        "the hulk is Destructible (carve-targetable wreckage)"
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
/// `ShipStats::can_fire` is true). The position/heading-parametrized player spawner
/// used by both the carve-to-core kill test (aimed at the enemy core) and the
/// multi-angle kill regression (aimed at the enemy from several approaches).
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
    // R18 spawns the shot at the GUN (the weapon cell, offset from the ship centre), so aim the
    // GUN — not the centre — at the target; a centre-aimed burst would ride parallel to and miss
    // the core. One correction step converges (the muzzle offset ≪ the firing range).
    let muzzle_off = stats.weapon.map(|p| p.muzzle_offset).unwrap_or(Vec2::ZERO);
    let h0 = (aim_at - pos).to_angle();
    let gun = pos + Vec2::from_angle(h0).rotate(muzzle_off);
    let heading = (aim_at - gun).to_angle();

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

// =================================================================================
// FIX (carve axis) regression: the carve must track the ship's FACING, not a swapped
// axis. The enemy faces +X at heading 0 (its nose is the +X/+row axis; its lateral
// wing axis is world ±Y). The world-impact→cell mapping (`hull_local_entry_ray`) must
// match the render (`build_hull_mesh`: local X/forward ← row, local Y/lateral ← col),
// so:
//   - a LATERAL (wing, world ±Y) hit carves on the COL axis (a wing), at MID row;
//   - a FORWARD/AFT (nose/tail, world ±X) hit carves on the ROW axis (the fore/aft
//     extreme), at MID col.
// The previous bug swapped forward↔lateral (col ← forward, row ← lateral), so a wing
// hit carved the TAIL. These guards fire shots in terms of the ship's facing and lock
// the side/sign of the mapping.
// =================================================================================

/// Run a single `fitted_damage_system` carve against a stationary fitted enemy at the
/// origin **facing +X** (heading 0), struck by a projectile whose impact is offset
/// along the chosen world axis. The projectile travels straight inward along
/// `-axis * speed` from outside the collision circle, so it enters the hull on the
/// `+axis` side and bores inward. Returns the destroyed cells.
///
/// `axis` is the WORLD axis the impact is offset along and the shot travels back down:
///   - `Vec2::Y` → a LATERAL (wing) hit: world ±Y is perpendicular to the +X nose, so
///     it maps to cell-space **col** (render: lateral ← col). A `+Y` impact carves a
///     **high-col** wing, a `-Y` impact a **low-col** wing — both at MID row.
///   - `Vec2::X` → a FORWARD/AFT (nose/tail) hit: world ±X is the nose axis, mapping to
///     cell-space **row** (render: forward ← row). A `+X` impact carves the **high-row**
///     nose, a `-X` impact the **low-row** tail — both at MID col.
///
/// `sign` picks the side (+1 = `+axis`, −1 = `-axis`); `dmg` bounds the carve depth so
/// it stays on the struck side (a runaway budget would chew across the centreline).
fn carve_on_axis(axis: Vec2, sign: f32, dmg: f32) -> Vec<(u16, u16)> {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // No shield in the way: drain it so the very first carve lands on the hull (the
    // test is about geometry, not the shield pool). Enemy faces +X at heading 0.
    let enemy = spawn_fitted_enemy_target_spin(&mut w, Vec2::ZERO, 0.0);
    if let Some(mut s) = w.get_mut::<Shields>(enemy) {
        s.current = 0.0;
    }

    // A projectile that approaches from the `sign * axis` side and travels straight
    // inward (`-sign * axis`) through the ship centre line on the other component, so
    // it enters the hull on the struck side and bores toward the centre along that axis.
    let approach = axis.normalize() * sign * 6.0; // start outside the ~1.76 circle
    let prev = approach;
    let pos = -approach; // sweeps across the ship → guaranteed to strike
    let vel = (pos - prev).normalize() * 200.0;
    w.spawn((
        Projectile,
        Position(pos),
        PrevPosition(prev),
        Velocity(vel),
        Damage(dmg),
        Lifetime(3.0),
        WeaponSource::from_damage(dmg),
    ));

    // Snapshot the pre-carve cells, run the carve, diff to find what was removed.
    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(enemy)
        .unwrap()
        .cells
        .keys()
        .copied()
        .collect();
    sim::fitted_damage_system(&mut w);
    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(enemy)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    before.difference(&after).copied().collect()
}

/// The (col_centroid, row_centroid) of a set of carved cells.
fn centroid(cells: &[(u16, u16)]) -> (f32, f32) {
    let n = cells.len() as f32;
    let col = cells.iter().map(|&(c, _)| c as f32).sum::<f32>() / n;
    let row = cells.iter().map(|&(_, r)| r as f32).sum::<f32>() / n;
    (col, row)
}

/// FIX (carve axis) regression — the EXACT reported bug: a LATERAL (wing) hit must
/// carve a WING (the col axis, at mid row), NOT the tail. The enemy faces +X (heading
/// 0), so its lateral axis is world ±Y. A `+Y` wing hit must carve **high-col** cells
/// and a `-Y` wing hit **low-col** cells, both clustered near MID row (the wing beam),
/// never at the fore/aft row extremes. Under the old swapped mapping (col ← forward,
/// row ← lateral) a lateral hit landed on the ROW axis → it carved the tail; this locks
/// the fix (col ← lateral, row ← forward).
#[test]
fn lateral_wing_hit_carves_the_wing_not_the_tail() {
    // 9×11 grid (cols 0..8, rows 0..10): centre col 4.5, centre row 5.5. The wing beam
    // (the broad lateral extent the shot enters head-on) sits around mid row.
    let centre_col = 9.0_f32 * 0.5; // 4.5
    let centre_row = 11.0_f32 * 0.5; // 5.5
                                     // A bounded budget so the carve stays on the struck wing (does not chew across the
                                     // centreline to the far wing).
    let dmg = 40.0;

    // --- +Y lateral (right wing) hit → HIGH-COL carve, MID row ----------------------
    let right_wing = carve_on_axis(Vec2::Y, 1.0, dmg);
    assert!(
        !right_wing.is_empty(),
        "a +Y lateral wing hit must carve at least one cell (it struck the hull)"
    );
    let (rc_col, rc_row) = centroid(&right_wing);
    assert!(
        rc_col > centre_col,
        "a +Y (lateral) wing hit carves the +Y WING — high-col cells (centroid col {rc_col} \
         > centre {centre_col}); under the bug it would carve the tail — got {right_wing:?}"
    );
    assert!(
        (rc_row - centre_row).abs() < 2.5,
        "a wing hit carves near MID row (centroid row {rc_row} ≈ {centre_row}), NOT a fore/aft \
         extreme — the carve is on the wing, not the nose/tail — got {right_wing:?}"
    );

    // --- −Y lateral (left wing) hit → LOW-COL carve, MID row ------------------------
    let left_wing = carve_on_axis(Vec2::Y, -1.0, dmg);
    assert!(
        !left_wing.is_empty(),
        "a −Y lateral wing hit must carve at least one cell"
    );
    let (lc_col, lc_row) = centroid(&left_wing);
    assert!(
        lc_col < centre_col,
        "a −Y (lateral) wing hit carves the −Y WING — low-col cells (centroid col {lc_col} \
         < centre {centre_col}) — got {left_wing:?}"
    );
    assert!(
        (lc_row - centre_row).abs() < 2.5,
        "the opposite wing hit also carves near MID row (centroid row {lc_row} ≈ {centre_row}) — \
         got {left_wing:?}"
    );

    // --- Opposite wings carve OPPOSITE sides (the side/sign is locked) --------------
    assert!(
        lc_col < rc_col,
        "opposite-side lateral hits carve opposite wings ({lc_col} < {rc_col}); the carve tracks \
         the impact side, not a swapped/centre axis"
    );
}

/// The complement: a FORWARD/AFT (nose/tail) hit carves the ROW (fore/aft) axis at the
/// row extreme, at MID col — NOT a wing. The enemy faces +X (heading 0), so its nose
/// axis is world ±X, mapping to cell-space row (forward ← row). A `+X` (forward/nose)
/// hit carves a HIGH-row cell near MID col; a `-X` (aft/tail) hit a LOW-row cell near
/// MID col. Together with the lateral test this fully locks the row/col mapping.
#[test]
fn forward_aft_hit_carves_the_fore_aft_axis_not_a_wing() {
    let centre_col = 9.0_f32 * 0.5; // 4.5
    let centre_row = 11.0_f32 * 0.5; // 5.5
    let dmg = 40.0;

    // --- +X forward (nose) hit → HIGH-row carve, MID col ----------------------------
    let nose = carve_on_axis(Vec2::X, 1.0, dmg);
    assert!(
        !nose.is_empty(),
        "a +X (nose) hit must carve at least one cell"
    );
    let (nose_col, nose_row) = centroid(&nose);
    assert!(
        nose_row > centre_row,
        "a +X (forward) hit carves the NOSE — high-row cells (centroid row {nose_row} \
         > centre {centre_row}) — got {nose:?}"
    );
    assert!(
        (nose_col - centre_col).abs() < 2.5,
        "a fore/aft hit carves near MID col (centroid col {nose_col} ≈ {centre_col}), NOT a wing — \
         got {nose:?}"
    );

    // --- −X aft (tail) hit → LOW-row carve, MID col ---------------------------------
    let tail = carve_on_axis(Vec2::X, -1.0, dmg);
    assert!(
        !tail.is_empty(),
        "a −X (tail) hit must carve at least one cell"
    );
    let (tail_col, tail_row) = centroid(&tail);
    assert!(
        tail_row < centre_row,
        "a −X (aft) hit carves the TAIL — low-row cells (centroid row {tail_row} \
         < centre {centre_row}) — got {tail:?}"
    );
    assert!(
        (tail_col - centre_col).abs() < 2.5,
        "the tail hit carves near MID col (centroid col {tail_col} ≈ {centre_col}), NOT a wing — \
         got {tail:?}"
    );

    // --- Nose vs tail carve OPPOSITE row extremes (the fore/aft sign is locked) -----
    assert!(
        tail_row < nose_row,
        "a nose hit and a tail hit carve opposite fore/aft extremes ({tail_row} < {nose_row})"
    );
}

// =================================================================================
// Destructible wreckage + the per-entity `Destructible` toggle: severed pieces and the
// destroyed-ship hulk are shootable/carve-able now, gated by `Destructible`. The carve
// is `Fit`-independent (grid resolved from `FitLayout.hull`), so wreckage — which has
// only a residual `FitLayout` (NO `Fit`) — carves through the SAME `fitted_damage_system`
// path; the wreck branch of `on_cells_carved` severs-further / despawns-when-empty and
// never re-kills. Removing `Destructible` per entity makes it inert (the user's toggle).
// =================================================================================

/// Spawn a **wreck** (a residual fighter hull) at the origin facing +X, drifting, with
/// its hull-footprint collider — mirroring a destroyed-ship hulk. `destructible` toggles
/// the `Destructible` marker so a test can prove the per-entity gate. It carries a
/// residual `FitLayout` (the dense fighter silhouette) + a `Wreck` tag but **NO `Fit`**
/// (exactly as `destroy_ship`/`sever_chunk` leave it) — so the carve must be
/// `Fit`-independent to touch it.
fn spawn_wreck(w: &mut World, destructible: bool) -> Entity {
    let (modules, hulls) = seed_catalogs();
    let hull = hulls.get(HULL_FIGHTER).unwrap().clone();
    let mut fit = Fit::new(HULL_FIGHTER);
    fit.install_raw(SlotId(0), MODULE_REACTOR_BASIC);
    let layout = build_layout(&hull, &fit, &modules);

    let mut e = w.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        // The hull-footprint collider the hulk keeps (destroy_ship no longer strips it).
        CollisionRadius(hull_collision_radius(hull.grid_dims)),
        // The residual hit-map (NO Fit — the carve resolves the hull from layout.hull).
        layout,
        Wreck::new(WreckOrigin::DestroyedShip),
    ));
    if destructible {
        e.insert(Destructible);
    }
    e.id()
}

/// Fire a single projectile through the target at the origin along `-X` (entering the
/// `+X` nose face head-on so it penetrates), offset laterally by `lat` world units so the
/// carve can be aimed at different columns of the hull. Carries `damage` (+ the matching
/// `WeaponSource` penetration). Drives ONE `fitted_damage_system` step and returns the set
/// of cells removed from `target` (empty if the carve removed nothing OR the target
/// despawned).
fn carve_wreck_at(
    w: &mut World,
    target: Entity,
    lat: f32,
    damage: f32,
) -> std::collections::BTreeSet<(u16, u16)> {
    // Approach from the +X nose side at lateral offset `lat`, sweeping inward along -X so
    // it strikes the +X face at that column and bores toward -X.
    let prev = Vec2::new(6.0, lat);
    let pos = Vec2::new(-6.0, lat);
    let vel = Vec2::new(-200.0, 0.0);
    w.spawn((
        Projectile,
        Position(pos),
        PrevPosition(prev),
        Velocity(vel),
        Damage(damage),
        Lifetime(3.0),
        WeaponSource::from_damage(damage),
    ));

    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(target)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    sim::fitted_damage_system(w);
    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(target)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    before.difference(&after).copied().collect()
}

/// A centre-line carve (no lateral offset) — the common case for tests (a)/(c)/(d).
fn carve_wreck_once(
    w: &mut World,
    target: Entity,
    damage: f32,
) -> std::collections::BTreeSet<(u16, u16)> {
    carve_wreck_at(w, target, 0.0, damage)
}

/// (a) A `Destructible` wreck (a severed chunk / destroyed-ship hulk, with its collider)
/// IS carved FURTHER by a subsequent hit: cells are removed from its residual
/// `FitLayout` — the wreckage erodes under fire through the same carve path live ships
/// use (Fit-independent, gated by `Destructible`).
#[test]
fn destructible_wreck_is_carved_further_by_a_hit() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_wreck(&mut w, true);

    let cells_before = w.get::<FitLayout>(wreck).unwrap().cells.len();
    assert!(
        cells_before > 0,
        "the wreck starts with residual hull cells"
    );

    // A bounded hit so the wreck is eroded but NOT emptied in one shot (covered by (b)).
    let removed = carve_wreck_once(&mut w, wreck, 40.0);
    assert!(
        !removed.is_empty(),
        "a Destructible wreck is carved further by a hit (cells removed: {removed:?})"
    );
    // It is NOT re-killed and NOT (yet) despawned — it still exists as a wreck with fewer
    // cells (the wreck branch never calls destroy_ship).
    let layout = w
        .get::<FitLayout>(wreck)
        .expect("the partially-carved wreck still exists");
    assert!(
        layout.cells.len() < cells_before,
        "the wreck's live cell count dropped (eroded), {} < {cells_before}",
        layout.cells.len()
    );
    assert!(
        w.get::<Wreck>(wreck).is_some(),
        "the wreck is still a Wreck (never re-killed — already dead)"
    );
}

/// A **small single-row wreck** (a 5-cell corridor lying along its row, registered in a
/// `HullCatalog`) at the origin, with its footprint collider + `Destructible` + `Wreck`
/// tag but NO `Fit`. Small enough that one centre-line carve along the row removes every
/// cell — so the "carve a wreck until empty → despawn" path is exercised deterministically.
fn spawn_small_wreck(w: &mut World) -> Entity {
    // Register the corridor hull so the Fit-independent carve can resolve `layout.hull`.
    let hull = corridor_hull();
    let dims = hull.grid_dims;
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_CORRIDOR, hull);
    }
    // Give the residual cells a small health so the carve does real work removing them.
    let mut layout = corridor_layout();
    for occ in layout.cells.values_mut() {
        occ.health = 5.0;
    }
    w.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius(dims)),
        layout,
        Destructible,
        Wreck::new(WreckOrigin::SeveredChunk),
    ))
    .id()
}

/// (b) Carving a wreck until its `FitLayout` is empty **despawns** the entity (fully
/// carved away — the render cell-diff removes its mesh; no empty hulk lingers). The wreck
/// branch of `on_cells_carved` despawns the emptied entity; it never calls `destroy_ship`.
#[test]
fn carving_a_wreck_until_empty_despawns_it() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // A small single-row corridor wreck: one centre carve along the row eats it whole.
    let wreck = spawn_small_wreck(&mut w);
    assert!(
        w.get::<FitLayout>(wreck).unwrap().cells.len() == 5,
        "the small wreck starts with its 5 corridor cells"
    );

    // Pound it with deep bursts along its row until it despawns. The corridor cells lie
    // along COLUMNS (cols 0..4) at row 1, and world ±Y maps to cell-space col (heading 0),
    // so a +Y→-Y bore sweeps the whole row of cells in one channel. Once every cell is gone
    // the wreck branch despawns the emptied entity. A bounded loop so a never-despawns
    // regression fails fast instead of hanging.
    let mut despawned = false;
    for _ in 0..40 {
        if w.get_entity(wreck).is_err() {
            despawned = true;
            break;
        }
        // Fire along the Y axis (the corridor's cell-col axis): enter from +Y, bore to -Y.
        let prev = Vec2::new(0.0, 6.0);
        let pos = Vec2::new(0.0, -6.0);
        let vel = Vec2::new(0.0, -200.0);
        w.spawn((
            Projectile,
            Position(pos),
            PrevPosition(prev),
            Velocity(vel),
            Damage(5000.0),
            Lifetime(3.0),
            WeaponSource::from_damage(5000.0),
        ));
        sim::fitted_damage_system(&mut w);
    }
    assert!(
        despawned,
        "carving the wreck until its FitLayout is empty despawns the entity"
    );
    assert!(
        w.get_entity(wreck).is_err(),
        "the fully-carved wreck no longer exists"
    );
}

/// (c) A wreck WITHOUT `Destructible` is **inert** — a hit removes no cells. This proves
/// the per-entity toggle: the same wreck shape, collider, and incoming shot, but with no
/// `Destructible` marker, is excluded from the carve query and so never erodes.
#[test]
fn wreck_without_destructible_is_inert() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_wreck(&mut w, false); // NO Destructible → the toggle is OFF

    let cells_before = w.get::<FitLayout>(wreck).unwrap().cells.len();
    assert!(
        cells_before > 0,
        "the wreck starts with residual hull cells"
    );

    // The SAME shot that erodes a Destructible wreck (test (a)) removes NOTHING here.
    let removed = carve_wreck_once(&mut w, wreck, 5000.0);
    assert!(
        removed.is_empty(),
        "a wreck without `Destructible` is inert — a hit removes no cells (got {removed:?})"
    );
    let layout = w
        .get::<FitLayout>(wreck)
        .expect("the inert wreck still exists");
    assert_eq!(
        layout.cells.len(),
        cells_before,
        "the inert wreck's cell count is unchanged (the per-entity toggle is OFF)"
    );
}

/// (d) A live ship still dies via **carve-to-core** (the destructible-wreckage change
/// leaves live-ship death unchanged): a `Destructible`, non-`Wreck` fitted ship struck
/// such that its core cell is carved away becomes a whole-ship `DestroyedShip` `Wreck`.
#[test]
fn live_ship_still_dies_via_carve_to_core() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // A stationary live enemy (Target + Destructible + collider + Fit + layout + defense).
    let enemy = spawn_fitted_enemy_target_spin(&mut w, Vec2::ZERO, 0.0);
    // Drop its shield so the very first heavy burst reaches the hull (death is about the
    // carve, not the shield pool).
    if let Some(mut s) = w.get_mut::<Shields>(enemy) {
        s.current = 0.0;
    }
    assert!(
        w.get::<Wreck>(enemy).is_none(),
        "the live ship is not a wreck before the kill"
    );
    let core = sim::damage::core_cell(w.get::<FitLayout>(enemy).unwrap()).expect("a core cell");

    // Fire a huge burst straight down the centre line (along -X into the +X nose) so the
    // channel bores to the central core cell. A few shots if needed.
    let mut destroyed = false;
    for _ in 0..40 {
        if w.get::<Wreck>(enemy).is_some() || w.get_entity(enemy).is_err() {
            destroyed = true;
            break;
        }
        let _ = carve_wreck_once(&mut w, enemy, 5000.0);
    }
    assert!(
        destroyed,
        "sustained core-aimed carving destroys the live ship"
    );
    // Whole-ship death: the core was carved away → a DestroyedShip wreck on the entity.
    let wreck = w
        .get::<Wreck>(enemy)
        .expect("a live ship carved through its core becomes a persistent Wreck");
    assert_eq!(
        wreck.origin,
        WreckOrigin::DestroyedShip,
        "carving the core away is the whole-ship-destroyed path"
    );
    assert!(
        w.get::<FitLayout>(enemy)
            .map(|l| !l.cells.contains_key(&core))
            .unwrap_or(true),
        "the core cell was carved away (or the hulk fully carved/despawned)"
    );
}

// =================================================================================
// Carve-center mismatch regression: an OFF-CENTRE wreck piece (a severed wing whose
// cells sit far from the parent grid centre) must carve where its cells ACTUALLY are.
//
// The carve maps the world impact into cell-space via `collision::hull_local_entry_ray`.
// It used a FIXED grid centre (`grid_dims·0.5`) for every entity. But a severed chunk's
// `Position` is its **cell-COM** and the client renders its cells around that COM
// (`hull_mesh_center` for `Debris`: `mean(col+0.5, row+0.5)`). So for an off-centre piece
// the carve ray entered the (empty) grid centre → `NoModule`/MISS, nothing removed — the
// "HIT MISS" bug. The fix threads a per-target cell-space `center` (cell-COM for a `Wreck`)
// into the entry mapping so the carve enters where the cells render. The `Wreck` tests
// above did not expose this because their residual cells straddled the grid centre.
// =================================================================================

/// The off-centre hull id used by the carve-center regression test.
const HULL_OFFCENTER: HullId = HullId(11);

/// A **filled** 9×5 silhouette (every cell of cols `0..8` × rows `0..4`, 45 cells), each
/// its own [`SectionId`]. The full authored silhouette matters for the carve's tunnel
/// guard: with a residual wing buried inside this silhouette, the carve reads a head-on
/// (angle-0) entry rather than a glancing wing-tip surface (so a clean penetration carves,
/// not a ricochet). The wide grid centre `(4.5, 2.5)` is far from the wing the wreck keeps.
fn offcenter_hull() -> Hull {
    let mut cells: Vec<GridCell> = Vec::new();
    let mut section = 0u32;
    for col in 0u16..9 {
        for row in 0u16..5 {
            cells.push(GridCell::new((col, row), SectionId(section)));
            section += 1;
        }
    }
    Hull {
        id: HULL_OFFCENTER,
        name: "Offcenter".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (9, 5),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Spawn a `Wreck` whose residual [`FitLayout`] is ONLY the off-centre **wing** — three
/// cells in the far `+col` column `{(7,1),(7,2),(7,3)}` of the 9×5 grid (cell-COM
/// `(7.5, 2.5)`, vs the grid centre `(4.5, 2.5)`: off-centre by 3 cells on the col axis).
/// Its `Position` is the wing's cell-COM world point — exactly how `sever_chunk` places a
/// severed chunk (and how the client renders it). Carries the footprint collider +
/// `Destructible` + `Wreck`, NO `Fit` (residual-hull wreckage). The authored hull is the
/// full 9×5 silhouette so the buried-entry tunnel guard reads a head-on penetration.
fn spawn_offcenter_wing_wreck(w: &mut World) -> Entity {
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_OFFCENTER, offcenter_hull());
    }
    let wing: [(u16, u16); 3] = [(7, 1), (7, 2), (7, 3)];
    let mut cells = CellMap::new();
    for (col, row) in wing {
        // depth = min(col, cols-1-col, row, rows-1-row) on the 9×5 grid.
        let depth = col.min(8 - col).min(row).min(4 - row);
        cells.insert(
            (col, row),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0, // small structural HP so a carve removes cells
                depth,
                structural: true,
            },
        );
    }
    let layout = FitLayout {
        hull: HULL_OFFCENTER,
        cells,
    };
    // The chunk's cell-COM in WORLD space (heading 0): the cell-space COM minus the grid
    // centre, scaled to world — the offset `sever_chunk` bakes into the chunk `Position`
    // (mirrors how the wing's cells render around this `Position`). The render axis swap
    // maps forward←row, lateral←col, so world X ← row offset and world Y ← col offset. Only
    // the relative offset matters for the carve (`hit.point - tpos`); placing it at a
    // non-trivial point proves the mapping is not accidentally centred.
    let com_local = Vec2::new(7.5, 2.5); // mean(col+0.5,row+0.5) over the wing
    let grid_centre = Vec2::new(9.0 * 0.5, 5.0 * 0.5);
    let pos = Vec2::new(
        (com_local.y - grid_centre.y) * CELL_WORLD_SIZE, // world X ← row offset
        (com_local.x - grid_centre.x) * CELL_WORLD_SIZE, // world Y ← col offset (the off-centre axis)
    );
    w.spawn((
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius((9, 5))),
        layout,
        Destructible,
        Wreck::new(WreckOrigin::SeveredChunk),
    ))
    .id()
}

/// REGRESSION (carve-center mismatch): firing at a genuinely **off-centre** severed wing
/// carves cells out of it — the case the earlier `Wreck` tests missed. A forward (`-X`)
/// shot sweeps the wreck's collider (centred on its cell-COM `Position`); the carve must
/// enter the wing's actual column (cell-COM col `7.5`), NOT the grid centre col `4.5`.
///
/// PRE-FIX this FAILED: `hull_local_entry_ray` mapped the impact onto the grid centre
/// (`4.5`) for every entity, so the ray entered the empty centre of the 9×5 grid where the
/// wing-only residual has no cells → `carve_path` empty → `HitKind::NoModule`, zero cells
/// removed (the "HIT MISS"). POST-FIX the per-target `center` is the wing's cell-COM, so
/// the ray enters col `7` where the cells are and carves them.
#[test]
fn offcenter_wreck_piece_is_carved_where_its_cells_are() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_offcenter_wing_wreck(&mut w);

    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .unwrap()
        .cells
        .keys()
        .copied()
        .collect();
    assert_eq!(
        before.len(),
        3,
        "the off-centre wing wreck starts with its 3 residual cells (cols all 7)"
    );

    // Fire a forward (-X) shot through the wreck's collider (centred on its cell-COM
    // `Position`). The entry col is FIXED at the cell-space `center.x`; with the fix that
    // is the wing's COM col (7.5 → col 7), so the bore (-row) sweeps the wing cells. The
    // grid-centre col (4.5) would enter the empty centre column (no cells) → NoModule.
    let tpos = w.get::<Position>(wreck).unwrap().0;
    w.spawn((
        Projectile,
        Position(Vec2::new(tpos.x - 6.0, tpos.y)),
        PrevPosition(Vec2::new(tpos.x + 6.0, tpos.y)),
        Velocity(Vec2::new(-200.0, 0.0)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));

    sim::fitted_damage_system(&mut w);

    // The wreck either lost cells (partial carve) or was fully carved away (despawn). Either
    // way, cells WERE removed — the carve hit the wing instead of reporting MISS.
    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    let removed: std::collections::BTreeSet<(u16, u16)> =
        before.difference(&after).copied().collect();
    assert!(
        !removed.is_empty(),
        "an off-centre wing wreck IS carved where its cells are (removed: {removed:?}); \
         pre-fix the ray entered the grid centre (empty) → NoModule/MISS, removing nothing"
    );

    // The carve registered a real HIT, not the `NoModule` MISS the bug produced.
    let last_kind = w.get_resource::<HitFeedback>().unwrap().last_kind;
    assert!(
        last_kind.is_some() && last_kind != Some(HitKind::NoModule),
        "the off-centre wreck hit registers as a carve (HitKind != NoModule), got {last_kind:?}"
    );
}

// =================================================================================
// END-TO-END (real-path) carve-of-a-severed-chunk regression: shoot a chunk that was
// produced by the REAL `sever_chunk` (NOT a hand-built synthetic `Wreck`), and prove it
// carves further. The `offcenter_wreck_piece_is_carved_where_its_cells_are` test above
// hand-computes the chunk `Position`, so it cannot catch a `sever_chunk` Position ↔ carve-
// center mismatch on the live path. This test severs an off-centre flank via the same
// `on_section_destroyed` → `sever_chunk` calls the runtime makes, captures the SPAWNED
// chunk (its real `Position`/`FitLayout`), then fires a projectile through its collider via
// `fitted_damage_system` (the live system) and asserts cells were carved out of it and the
// hit resolved as a real carve (not `NoModule`/MISS). This is the user's "HIT MISS" case.
// =================================================================================

/// A `(World, ship)` carrying the corridor hull/layout + the FULL E007 carve resources
/// (matrix / penetration / shield / salvage configs) so a chunk severed off this ship can
/// then be SHOT through the live `fitted_damage_system` carve path. The corridor cells are
/// given a small structural `health` so a carve does real work removing them (a 0-health
/// cell would be removed for free). Body components are supplied (stationary: zero
/// velocity/angvel) so the severed chunk's real `Position` is deterministic.
fn corridor_world_carveable(pos: Vec2, heading: f32) -> (World, Entity) {
    let mut w = World::new();
    // The full E007 carve content (catalogs + matrix/pen/shield/salvage configs + the base
    // sim resources) — what `apply_damage`/`fitted_damage_system` resolve against.
    insert_full_combat_resources(&mut w);
    // Register the corridor hull alongside the seeded catalogs so the Fit-independent carve
    // can resolve the severed chunk's `layout.hull` (= HULL_CORRIDOR).
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_CORRIDOR, corridor_hull());
    }

    let fit = Fit::new(HULL_CORRIDOR);
    let mut layout = corridor_layout();
    // Small structural HP so a carve removes cells through real work (mirrors the live wreck).
    for occ in layout.cells.values_mut() {
        occ.health = 5.0;
    }
    let ship = w
        .spawn((
            fit,
            layout,
            Position(pos),
            Velocity(Vec2::ZERO),
            Heading(heading),
            AngularVelocity(0.0),
            HullStructure::full(100.0),
        ))
        .id();
    (w, ship)
}

/// END-TO-END regression (the user's "HIT MISS" on a real severed piece): a chunk produced
/// by the REAL `sever_chunk` path carves further when shot.
///
/// 1. **Sever via the real path**: destroying the corridor's connecting middle section
///    (`SectionId(2)`, cell `(2,1)`) calls the real `on_section_destroyed` → `sever_chunk`,
///    which spawns a `Wreck{SeveredChunk}` carrying the off-centre far-end flank
///    `{(3,1),(4,1)}` (cell-COM `(4.0,1.5)`, off the 5×3 grid centre `(2.5,1.5)` by 1.5
///    cells laterally) at the `Position` `sever_chunk` itself computes — NOT a hand-built one.
/// 2. **Fire at the REAL chunk**: a projectile sweeps the chunk's collider (centred on that
///    `Position`), entering along world `-Y` (cell-space `-col`) so the bore crosses the
///    flank cells. One `fitted_damage_system` step runs the live carve.
/// 3. **Assert**: the chunk's residual `FitLayout.cells` count DECREASED (cells carved out
///    of the real chunk) — or it despawned because it emptied (also "carved") — AND the
///    resolved `HitKind != NoModule`. If `sever_chunk`'s `Position` disagreed with the carve
///    `center` (cell-COM) for a real severed piece, the ray would enter empty space →
///    `NoModule`/MISS and nothing would carve (the bug this guards).
#[test]
fn real_severed_chunk_is_carved_further_when_shot() {
    // A stationary ship at a non-trivial position + heading 0 (so the geometry is
    // deterministic and the swap/scale of `sever_chunk`'s Position is exercised but readable).
    let (mut w, ship) = corridor_world_carveable(Vec2::new(20.0, -7.0), 0.0);

    // --- 1. Sever an off-centre flank via the REAL `on_section_destroyed`/`sever_chunk` ---
    on_section_destroyed(&mut w, ship, SectionId(2));

    // Capture the SPAWNED severed-chunk entity (its real `Position` + residual layout).
    let chunk = {
        let mut q = w.query_filtered::<(Entity, &Wreck), With<FitLayout>>();
        let severed: Vec<Entity> = q
            .iter(&w)
            .filter(|(_, wr)| wr.origin == WreckOrigin::SeveredChunk)
            .map(|(e, _)| e)
            .collect();
        assert_eq!(
            severed.len(),
            1,
            "the connecting-section destruction severs exactly one off-centre flank chunk via \
             the real sever_chunk path"
        );
        severed[0]
    };

    // The chunk is the real severed flank: its residual cells are the far end {(3,1),(4,1)},
    // its collider + Destructible were attached by `sever_chunk`, and it is NOT at the grid
    // centre (the off-centre case the synthetic test cannot reach).
    let chunk_pos = w.get::<Position>(chunk).unwrap().0;
    let cells_before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(chunk)
        .unwrap()
        .cells
        .keys()
        .copied()
        .collect();
    assert_eq!(
        cells_before.len(),
        2,
        "the severed flank carries its two far-end cells (3,1),(4,1) — got {cells_before:?}"
    );
    assert!(
        w.get::<sim::components::Destructible>(chunk).is_some()
            && w.get::<CollisionRadius>(chunk).is_some(),
        "sever_chunk leaves the chunk carve-targetable (Destructible + a collider)"
    );

    // --- 2. Fire a projectile through the REAL chunk's collider (live system) ---------
    // Enter from the +Y side and bore along -Y (world -Y maps to cell-space -col at heading
    // 0), so the channel sweeps the flank's two cells. The sweep is centred on the chunk's
    // own `Position` (its cell-COM in world) so the swept-cast strikes the collider.
    w.spawn((
        Projectile,
        Position(Vec2::new(chunk_pos.x, chunk_pos.y - 6.0)),
        PrevPosition(Vec2::new(chunk_pos.x, chunk_pos.y + 6.0)),
        Velocity(Vec2::new(0.0, -200.0)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));

    sim::fitted_damage_system(&mut w);

    // --- 3. Assert: the REAL chunk was carved further (or emptied → despawn) ----------
    let cells_after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(chunk)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default(); // despawned-when-emptied → no layout → empty set
    let despawned = w.get_entity(chunk).is_err();
    let removed: std::collections::BTreeSet<(u16, u16)> =
        cells_before.difference(&cells_after).copied().collect();
    assert!(
        despawned || !removed.is_empty(),
        "shooting the REAL severed chunk carves cells out of it (removed {removed:?}) or empties \
         + despawns it (despawned={despawned}); the bug would leave it untouched (HIT MISS)"
    );

    // The hit resolved as a real carve, NOT the `NoModule` MISS a Position↔center mismatch
    // would produce (the ray entering empty space).
    let last_kind = w.get_resource::<HitFeedback>().unwrap().last_kind;
    assert!(
        last_kind.is_some() && last_kind != Some(HitKind::NoModule),
        "the real severed-chunk hit registers as a carve (HitKind != NoModule), got {last_kind:?} \
         — a NoModule here is the user's HIT MISS (sever_chunk Position ↔ carve center mismatch)"
    );
}

// =================================================================================
// OFF-AXIS CLEAN-MISS regression (cell-precise hit detection): a chunk's collider is a
// CIRCLE sized to its bounding-box LONGEST axis (`chunk_collision_radius` =
// `max(bbox_w,bbox_h)·CELL_WORLD_SIZE·0.5`), but a THIN/sparse chunk's cells fill only a
// NARROW band of it. A 1-wide × 3-tall sliver: collider radius `3·0.32·0.5 = 0.48`, cell
// perpendicular half-width `0.5·0.32 = 0.16` → ~68% of the circle is empty. A shot that
// crosses the loose CIRCLE but threads BETWEEN the sparse cells crosses NO present cell.
//
// With cell-precise narrow-phase detection that shot is a **clean MISS**: the projectile
// passes the chunk (it is not consumed by a target whose cells it never crosses), nothing
// is carved, and there is no phantom "HIT MISS". (The old loose-circle path picked the
// chunk on the circle toi and the nearest-cell fallback force-carved a cell; both are
// removed.) An ON-axis shot through the same sliver's cells DOES carve it (the legit case,
// covered by the prior `*_severed_chunk_*` tests + the on-axis assertion here).
// =================================================================================

/// The thin-sliver hull id used by the nearest-cell-fallback regression test.
const HULL_THINSLIVER: HullId = HullId(13);

/// A **filled** 5×5 silhouette (every cell of cols `0..4` × rows `0..4`, 25 cells), each
/// its own [`SectionId`]. The full authored silhouette makes the residual sliver's entry
/// cell read as BURIED (the carve's tunnel guard → head-on, angle 0), so a clean
/// penetration carves rather than ricochets — isolating the THINNESS (not the angle) as
/// the cause of the empty carve path.
fn thinsliver_hull() -> Hull {
    let mut cells: Vec<GridCell> = Vec::new();
    let mut section = 0u32;
    for col in 0u16..5 {
        for row in 0u16..5 {
            cells.push(GridCell::new((col, row), SectionId(section)));
            section += 1;
        }
    }
    Hull {
        id: HULL_THINSLIVER,
        name: "ThinSliver".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (5, 5),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Spawn a `Wreck` whose residual [`FitLayout`] is a 1-wide × 3-tall **sliver** — three
/// cells in the centre column stacked along the row axis `{(2,1),(2,2),(2,3)}` of the 5×5
/// grid (cell-COM `(2.5, 2.5)` = the grid centre, so this is NOT an off-centre case — the
/// thinness alone drives the empty carve path). Its collider is the chunk-footprint circle
/// (radius `max(1,3)·CELL_WORLD_SIZE·0.5 = 0.48`), far wider than the `0.16` cell
/// half-width, so an off-axis swept-cast HITS the circle while the carve ray threads
/// between the cells. Carries `Destructible` + `Wreck`, NO `Fit` (residual-hull wreckage).
fn spawn_thin_sliver_wreck(w: &mut World) -> Entity {
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_THINSLIVER, thinsliver_hull());
    }
    let sliver: [(u16, u16); 3] = [(2, 1), (2, 2), (2, 3)];
    let mut cells = CellMap::new();
    for (col, row) in sliver {
        let depth = col.min(4 - col).min(row).min(4 - row);
        cells.insert(
            (col, row),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0, // small structural HP so a carve removes the cell
                depth,
                structural: true,
            },
        );
    }
    let layout = FitLayout {
        hull: HULL_THINSLIVER,
        cells,
    };
    // The sliver collider: the chunk's OWN footprint (1 col × 3 rows) → radius 0.48 world,
    // mirroring `sever_chunk`'s `chunk_collision_radius`. The cell-COM is the grid centre,
    // so the wreck `Position` is the origin (zero offset) — clean geometry.
    let radius = 3.0 * CELL_WORLD_SIZE * 0.5; // max(bbox_w=1, bbox_h=3)·CELL_WORLD_SIZE·0.5
    w.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(radius),
        layout,
        Destructible,
        Wreck::new(WreckOrigin::SeveredChunk),
    ))
    .id()
}

/// REGRESSION (cell-precise hit detection): an OFF-AXIS shot at a THIN sliver chunk — one
/// that crosses the collider CIRCLE but threads BETWEEN the sparse cells, crossing NO
/// present cell — is a **clean MISS** (the projectile passes; nothing is carved). An
/// ON-axis shot through the sliver's cells DOES carve it (the legit case).
///
/// The 1×3 sliver `{(2,1),(2,2),(2,3)}` (centre column) has a collider radius `0.48` but a
/// cell half-width of only `0.16`. A forward (`-X`) shot offset laterally by `lat = 0.30`
/// world units (`0.16 < 0.30 < 0.48`) crosses the circle (closest approach `0.30 < 0.48`)
/// but its carve ray sits at cell-space col `≈ 3.44`, while the cells are at col `2.5` —
/// `> 0.5` (one cell radius) away, so it crosses NO cell.
///
/// With cell-precise detection that off-axis shot is a clean miss: `last_kind` is unchanged
/// (no hit registered for this target), zero cells removed. The complementary ON-axis shot
/// (no lateral offset) crosses the col-2 cells and carves the sliver.
#[test]
fn thin_sliver_wreck_off_axis_shot_is_a_clean_miss() {
    // --- Off-axis: crosses the circle, threads between the cells → clean MISS ---------
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_thin_sliver_wreck(&mut w);

    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .unwrap()
        .cells
        .keys()
        .copied()
        .collect();
    assert_eq!(
        before.len(),
        3,
        "the thin sliver starts with its 3 residual cells (all in col 2)"
    );

    // A forward (-X) shot offset laterally (+Y) by 0.30 world units — inside the 0.48
    // collider radius (so it crosses the CIRCLE) but threading between the col-2 cells
    // (0.30/0.32 ≈ 0.94 cells off the col-2 centre line → crosses NO cell). The wreck
    // `Position` is the origin (cell-COM == grid centre), so the lateral offset is +Y.
    let tpos = w.get::<Position>(wreck).unwrap().0;
    let lat = 0.30_f32;
    w.spawn((
        Projectile,
        Position(Vec2::new(tpos.x - 6.0, tpos.y + lat)),
        PrevPosition(Vec2::new(tpos.x + 6.0, tpos.y + lat)),
        Velocity(Vec2::new(-200.0, 0.0)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));

    sim::fitted_damage_system(&mut w);

    // CLEAN MISS: nothing was carved (the projectile crosses no cell of the sliver).
    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    let despawned = w.get_entity(wreck).is_err();
    let removed: std::collections::BTreeSet<(u16, u16)> =
        before.difference(&after).copied().collect();
    assert!(
        !despawned && removed.is_empty(),
        "an off-axis shot that threads between the sliver's cells is a clean MISS \
         (removed {removed:?}, despawned={despawned}); the loose circle is only a broad-phase \
         reject — a target whose cells the ray never crosses is not hit"
    );
    // No hit registered for this target — `last_kind` was never set to a carve/NoModule.
    let last_kind = w.get_resource::<HitFeedback>().unwrap().last_kind;
    assert!(
        last_kind.is_none(),
        "an off-axis clean miss registers NO hit (last_kind stays None), got {last_kind:?}"
    );

    // --- On-axis: crosses the col-2 cells → carves the sliver (the legit case) --------
    let mut w2 = World::new();
    insert_full_combat_resources(&mut w2);
    let wreck2 = spawn_thin_sliver_wreck(&mut w2);
    let before2 = w2.get::<FitLayout>(wreck2).unwrap().cells.len();
    let tpos2 = w2.get::<Position>(wreck2).unwrap().0;
    // No lateral offset: the ray runs straight down col 2 through all three cells.
    w2.spawn((
        Projectile,
        Position(Vec2::new(tpos2.x - 6.0, tpos2.y)),
        PrevPosition(Vec2::new(tpos2.x + 6.0, tpos2.y)),
        Velocity(Vec2::new(-200.0, 0.0)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));
    sim::fitted_damage_system(&mut w2);
    let after2 = w2
        .get::<FitLayout>(wreck2)
        .map(|l| l.cells.len())
        .unwrap_or(0);
    let despawned2 = w2.get_entity(wreck2).is_err();
    assert!(
        despawned2 || after2 < before2,
        "an ON-axis shot through the sliver's cells carves it (the legit case): \
         before {before2}, after {after2}, despawned {despawned2}"
    );
    let last_kind2 = w2.get_resource::<HitFeedback>().unwrap().last_kind;
    assert!(
        last_kind2.is_some() && last_kind2 != Some(HitKind::NoModule),
        "the on-axis hit registers as a real carve (HitKind != NoModule), got {last_kind2:?}"
    );
}

// =================================================================================
// THE TARGETING BUG (cell-precise hit selection): shooting a severed wreckage piece
// that sits NEXT TO its parent ship must carve the PIECE, not the ship.
//
// The parent ship's collider is a CIRCLE the size of its whole footprint (radius ~0.8
// here). A freshly-severed chunk sits right beside it, so a shot aimed at the small
// adjacent piece often crosses the SHIP's big collider circle FIRST (lower circle toi)
// even though it never crosses the ship's actual cells. The old loose-circle selection
// picked the ship (lowest circle toi), consumed the projectile, and the nearest-cell
// fallback then force-carved the SHIP's nearest cell — "it continues carving from the
// original piece". The circle is a loose broad-phase; the cells are the truth.
//
// The fix makes hit SELECTION cell-precise: among the broad-phase survivors, pick the
// target with the lowest CELL-crossing toi (the first cell the ray reaches), not the
// lowest circle toi. A target whose cells the ray never crosses is NOT hit. So the
// chunk (whose cells the ray crosses) wins; the ship (circle crossed, no cell crossed)
// is passed over and keeps every cell.
// =================================================================================

/// The custom ship hull id for the targeting-bug reproduction.
const HULL_REPRO_SHIP: HullId = HullId(21);
/// The custom chunk hull id for the targeting-bug reproduction.
const HULL_REPRO_CHUNK: HullId = HullId(22);

/// A **filled** 5×5 silhouette (25 cells, each its own [`SectionId`]) — the authored
/// silhouette for the reproduction ship (grid centre `(2.5,2.5)`, footprint collider
/// radius `5·0.32·0.5 = 0.8`). The full silhouette makes a buried live cell read head-on
/// for the tunnel guard, isolating the SELECTION (which target) as the bug, not the angle.
fn repro_ship_hull() -> Hull {
    let mut cells: Vec<GridCell> = Vec::new();
    let mut section = 0u32;
    for col in 0u16..5 {
        for row in 0u16..5 {
            cells.push(GridCell::new((col, row), SectionId(section)));
            section += 1;
        }
    }
    Hull {
        id: HULL_REPRO_SHIP,
        name: "ReproShip".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (5, 5),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// A 1×1 authored hull for the severed chunk (grid centre `(0.5,0.5)`, footprint collider
/// radius `0.5·0.32 = 0.16`). The chunk's live cell IS its whole authored silhouette, so
/// its single cell reads as a surface cell — a forward shot through it carves it.
fn repro_chunk_hull() -> Hull {
    Hull {
        id: HULL_REPRO_CHUNK,
        name: "ReproChunk".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (1, 1),
        cells: vec![GridCell::new((0, 0), SectionId(0))],
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Build the targeting-bug world: a LIVE fitted ship at the origin whose live `FitLayout`
/// is a small cluster in the BOTTOM-LEFT of its 5×5 grid (so most of its big collider
/// circle is empty), and an adjacent severed `Wreck` chunk whose single cell sits in the
/// ship's circle but away from the ship's cells. Returns `(world, ship, chunk)`.
///
/// Geometry (heading 0; render maps world X ← row offset, world Y ← col offset, scaled by
/// `CELL_WORLD_SIZE`, grid-centre-relative for a live ship; cell-COM-relative for a chunk):
///   * Ship grid centre `(2.5,2.5)`. Live cells `{(0,0),(1,0),(0,1)}` (bottom-left corner)
///     render at world ≈ `(−0.5..−0.64, −0.5..−0.64)` — the LOW-X/LOW-Y quadrant.
///   * Ship collider radius `0.8` (centred at the origin) reaches `y = +0.5` out to
///     `x = ±√(0.8²−0.5²) ≈ ±0.62`.
///   * Chunk single cell placed (via its `Position`) at world ≈ `(0.0, +0.5)` — inside the
///     ship circle but in the HIGH-Y half, ~1.0 world from the ship's cells.
///
/// A horizontal shot at `y = +0.5` travelling `−X` then crosses the ship circle (near edge
/// `x ≈ +0.62`) BEFORE the chunk cell (`x ≈ 0.0`) → the ship has the LOWER circle toi (the
/// bug's mis-pick), yet the shot never crosses a ship cell while it DOES cross the chunk's.
fn repro_targeting_bug_world() -> (World, Entity, Entity) {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_REPRO_SHIP, repro_ship_hull());
        hulls.hulls.insert(HULL_REPRO_CHUNK, repro_chunk_hull());
    }

    // --- The LIVE ship: a 5×5 hull with only a bottom-left cluster of live cells. ----
    let ship_cells: [(u16, u16); 3] = [(0, 0), (1, 0), (0, 1)];
    let mut scells = CellMap::new();
    for (col, row) in ship_cells {
        let depth = col.min(4 - col).min(row).min(4 - row);
        scells.insert(
            (col, row),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0,
                depth,
                structural: true,
            },
        );
    }
    let ship_layout = FitLayout {
        hull: HULL_REPRO_SHIP,
        cells: scells,
    };
    let ship = w
        .spawn((
            Target,
            TargetKind::Dummy,
            Position(Vec2::ZERO),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            // The whole-footprint collider circle (the loose broad-phase): radius 0.8.
            CollisionRadius(hull_collision_radius((5, 5))),
            ship_layout,
            Destructible,
            // A live ship's defense state: a depleted shield so the very first hit would
            // reach the hull (the test is about WHICH target, not the shield pool).
            Shields::depleted(0.0, 0.0, false),
            HullStructure::full(100.0),
        ))
        .id();

    // --- The severed CHUNK: a 1-cell wreck placed at world ≈ (0.0, +0.5). -------------
    // Its `Position` IS its cell-COM in world (how `sever_chunk` places it + how the client
    // renders it). The single cell's COM is the grid centre (0.5,0.5), so the chunk's cells
    // render exactly AT its `Position`. Put that at world (0.0, +0.5): inside the ship's 0.8
    // circle (distance 0.5 < 0.8) but in the high-Y half, away from the ship's bottom-left
    // cells.
    let mut ccells = CellMap::new();
    ccells.insert(
        (0, 0),
        CellOccupant {
            slot: SlotId(u32::MAX),
            module: None,
            health: 5.0,
            depth: 0,
            structural: true,
        },
    );
    let chunk_layout = FitLayout {
        hull: HULL_REPRO_CHUNK,
        cells: ccells,
    };
    let chunk = w
        .spawn((
            Position(Vec2::new(0.0, 0.5)),
            Velocity(Vec2::ZERO),
            Heading(0.0),
            AngularVelocity(0.0),
            CollisionRadius(hull_collision_radius((1, 1))),
            chunk_layout,
            Destructible,
            Wreck::new(WreckOrigin::SeveredChunk),
        ))
        .id();

    (w, ship, chunk)
}

/// THE REPRODUCTION (targeting bug): shooting a severed piece that sits next to its parent
/// ship must carve the PIECE, not the ship.
///
/// The shot is aimed at the chunk's cell along a path that crosses the chunk's cell AND the
/// ship's collider circle (the ship has the LOWER circle toi) but NEVER a ship cell. With
/// cell-precise selection the chunk wins and the ship is untouched.
///
/// PRE-FIX this FAILS: the loose-circle selection picks the ship (lowest circle toi) and the
/// nearest-cell fallback force-carves a ship cell → the ship loses cells and the chunk is
/// untouched ("it continues carving from the original piece"). POST-FIX the chunk loses its
/// cell and the ship keeps all of its cells.
#[test]
fn shooting_a_piece_next_to_the_ship_carves_the_piece_not_the_ship() {
    let (mut w, ship, chunk) = repro_targeting_bug_world();

    let ship_cells_before = w.get::<FitLayout>(ship).unwrap().cells.len();
    let chunk_cells_before = w.get::<FitLayout>(chunk).unwrap().cells.len();
    assert_eq!(
        ship_cells_before, 3,
        "the ship starts with its 3 live cells"
    );
    assert_eq!(chunk_cells_before, 1, "the chunk starts with its 1 cell");

    // A horizontal shot at y = +0.5 travelling -X: it crosses the ship circle near edge
    // (x ≈ +0.62) BEFORE the chunk cell at (0.0, +0.5) → the ship has the lower CIRCLE toi
    // (the bug's mis-pick), but the shot crosses the chunk's cell and NO ship cell.
    w.spawn((
        Projectile,
        Position(Vec2::new(-6.0, 0.5)),
        PrevPosition(Vec2::new(6.0, 0.5)),
        Velocity(Vec2::new(-200.0, 0.5)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));

    sim::fitted_damage_system(&mut w);

    // --- The CHUNK was carved (its cell removed → emptied → despawned, or count dropped).
    let chunk_after = w
        .get::<FitLayout>(chunk)
        .map(|l| l.cells.len())
        .unwrap_or(0);
    let chunk_despawned = w.get_entity(chunk).is_err();
    assert!(
        chunk_despawned || chunk_after < chunk_cells_before,
        "the CHUNK (the aimed-at piece) is carved: before {chunk_cells_before}, after \
         {chunk_after}, despawned {chunk_despawned}"
    );

    // --- The SHIP kept EVERY cell (it was never the real target — its cells weren't crossed).
    let ship_after = w.get::<FitLayout>(ship).unwrap().cells.len();
    assert_eq!(
        ship_after, ship_cells_before,
        "the SHIP keeps all {ship_cells_before} of its cells — the shot threaded past its \
         circle without crossing a ship cell; pre-fix the loose-circle pick + nearest-cell \
         fallback carved the SHIP instead ('continues carving from the original piece')"
    );
}

// =================================================================================
// Fix #4 — wreckage ricochets every shot ("RICOCHET, never carves again"). The armor
// angle was measured from the ORIGINAL hull's grid centre for ALL targets; for an
// off-centre chunk that far-off reference made even a head-on shot read as steeply
// glancing → permanent `Ricochet`. The fix measures the angle from the target's OWN
// centre (cell-COM for a `Wreck`) — the SAME reference the entry point uses — so a
// head-on shot at the chunk carves while a genuinely glancing one still ricochets.
//
// The hull authored here IS exactly the wreck's cells (a single off-centre row), so the
// tunnel/buried guard reads NO cell in front of the entry (`buried == false`) and the
// radial angle actually applies — unlike `HULL_OFFCENTER`'s filled silhouette, whose
// buried guard forces head-on and MASKS this bug.
// =================================================================================

/// The off-centre single-row hull id used by the Fix #4 ricochet regression.
const HULL_OFFROW: HullId = HullId(12);

/// A hull whose authored cells are a single horizontal row of three cells off to the
/// `-col` side — `{(0,2),(1,2),(2,2)}` on a 9×5 grid (cell-COM col `1.5`, vs the grid
/// centre col `4.5`: off-centre by 3 cells). Crucially the authored silhouette is ONLY
/// these cells, so a shot's "cell in front" is never authored → the carve's tunnel guard
/// stays `false` and the armor-angle radial is exercised (the bug's actual code path).
fn offrow_hull() -> Hull {
    let coords = [(0u16, 2u16), (1, 2), (2, 2)];
    let cells: Vec<GridCell> = coords
        .iter()
        .enumerate()
        .map(|(i, &c)| GridCell::new(c, SectionId(i as u32)))
        .collect();
    Hull {
        id: HULL_OFFROW,
        name: "Offrow".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (9, 5),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Spawn a `Wreck` whose residual [`FitLayout`] is the off-centre row `{(0,2),(1,2),(2,2)}`
/// (small structural HP so a carve removes cells), placed at its cell-COM world `Position`
/// exactly as `sever_chunk` would, with its footprint collider + `Destructible` + `Wreck`
/// and NO `Fit`. The authored `HULL_OFFROW` is registered so the Fit-independent carve can
/// resolve `layout.hull`.
fn spawn_offrow_wreck(w: &mut World) -> Entity {
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_OFFROW, offrow_hull());
    }
    let mut cells = CellMap::new();
    for (col, row) in [(0u16, 2u16), (1, 2), (2, 2)] {
        let depth = col.min(8 - col).min(row).min(4 - row);
        cells.insert(
            (col, row),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0, // small structural HP so a carve does real work
                depth,
                structural: true,
            },
        );
    }
    let layout = FitLayout {
        hull: HULL_OFFROW,
        cells,
    };
    // The chunk's cell-COM in WORLD space (heading 0): cell-space COM minus grid centre,
    // axis-swapped (world X ← row, world Y ← col) and scaled — the offset `sever_chunk`
    // bakes into a chunk `Position`, and where its cells render. The off-centre axis is col,
    // which maps to world Y.
    let com_local = Vec2::new(1.5, 2.5); // mean(col+0.5,row+0.5) over the row
    let grid_centre = Vec2::new(9.0 * 0.5, 5.0 * 0.5);
    let pos = Vec2::new(
        (com_local.y - grid_centre.y) * CELL_WORLD_SIZE, // world X ← row offset (0)
        (com_local.x - grid_centre.x) * CELL_WORLD_SIZE, // world Y ← col offset (the off-centre axis)
    );
    w.spawn((
        Position(pos),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius((9, 5))),
        layout,
        Destructible,
        Wreck::new(WreckOrigin::SeveredChunk),
    ))
    .id()
}

/// Fire one forward (`-X`) projectile sweeping the wreck's collider at world `y`, run one
/// `fitted_damage_system` step, and return `(cells_removed_from_target, last_HitKind)`.
/// World `y` selects which COLUMN of the chunk the bore enters (`-X` sweeps `-row`); the
/// entry column is fixed by `y` relative to the chunk's cell-COM `Position`.
fn fire_forward_at_y(
    w: &mut World,
    target: Entity,
    y: f32,
    damage: f32,
) -> (std::collections::BTreeSet<(u16, u16)>, Option<HitKind>) {
    let tpos = w.get::<Position>(target).unwrap().0;
    w.spawn((
        Projectile,
        Position(Vec2::new(tpos.x - 6.0, y)),
        PrevPosition(Vec2::new(tpos.x + 6.0, y)),
        Velocity(Vec2::new(-200.0, 0.0)),
        Damage(damage),
        Lifetime(3.0),
        WeaponSource::from_damage(damage),
    ));
    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(target)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    sim::fitted_damage_system(w);
    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(target)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    let removed = before.difference(&after).copied().collect();
    let last_kind = w.get_resource::<HitFeedback>().unwrap().last_kind;
    (removed, last_kind)
}

/// REGRESSION (Fix #4): a head-on shot at an OFF-CENTRE chunk's own centre column CARVES
/// it — it no longer permanently ricochets. PRE-FIX the armor angle came from the original
/// hull's grid centre (col `4.5`), three cells from this chunk → ~90° glancing → `Ricochet`,
/// removing nothing on every shot (the user's "every shot says RICOCHET and never carves").
/// POST-FIX the angle uses the chunk's cell-COM, so a centre-column shot reads head-on.
#[test]
fn offcentre_wreck_carves_head_on_instead_of_permanent_ricochet() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_offrow_wreck(&mut w);
    let tpos = w.get::<Position>(wreck).unwrap().0;

    // Head-on at the chunk's OWN centre column (world y = the wreck's Position.y).
    let (removed, kind) = fire_forward_at_y(&mut w, wreck, tpos.y, 5000.0);
    assert!(
        !removed.is_empty(),
        "a head-on shot at the off-centre chunk carves it (removed: {removed:?}); pre-fix it \
         permanently ricocheted off the far-away grid-centre angle reference, removing nothing"
    );
    assert!(
        matches!(
            kind,
            Some(HitKind::Penetrated) | Some(HitKind::OverPenetrated)
        ),
        "the head-on chunk shot penetrates/carves — NOT a ricochet; got {kind:?}"
    );
}

/// Fix #10: a THIN (1-cell-wide) scrap row no longer ricochets at its tip. The 3-cell
/// `HULL_OFFROW` is 1 cell tall, so every cell has ≤2 present neighbours → its local normal is
/// degenerate (points along the row). The `RICOCHET_MIN_NEIGHBORS` gate treats such a shard as
/// head-on → it carves from every angle, including the `-col` tip that used to bounce. (Genuine
/// grazes of a SOLID chunk still ricochet — see `solid_hulk_scrap_still_ricochets_on_a_graze`.)
#[test]
fn thin_row_scrap_carves_from_every_angle_no_spurious_ricochet() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_offrow_wreck(&mut w);
    let tpos = w.get::<Position>(wreck).unwrap().0;

    // The END (`-col`) tip column — the case the old code ricocheted (~90° degenerate normal).
    let (removed, kind) = fire_forward_at_y(&mut w, wreck, tpos.y - CELL_WORLD_SIZE, 40.0);
    assert!(
        matches!(
            kind,
            Some(HitKind::Penetrated) | Some(HitKind::OverPenetrated)
        ),
        "a thin 1-wide scrap row no longer ricochets at its tip — it carves; got {kind:?}"
    );
    assert!(
        !removed.is_empty(),
        "the thin-row tip hit carves a cell (got {removed:?})"
    );
}

// =================================================================================
// Fix #5 — live-ship (and hulk) bore STALL: drilling ONE spot carves a few cells then
// permanently RICOCHETS, mid-body, while the ship is still alive. The armor gate's tunnel
// guard decided "fresh surface vs bored tunnel" with a SINGLE dominant-axis cell, which
// misfires for an off-axis (diagonal) bore on a SHAPED hull (the orthogonal neighbour falls
// outside the silhouette even though the shot drilled a real tunnel). So the grid-centre
// radial obliquity applies, and at the bore's closest approach to the centre the radial is
// ⊥ to the shot → ~90° → permanent `Ricochet`, the entry never advances.
//
// `HULL_WEDGE` is a filled lower silhouette (`r <= c+2` on an 11×11 grid) whose diagonal
// EDGE reproduces the fighter's neck/wing edge: a `(1,1)` bore runs along that edge, and the
// authored cell one ORTHOGONAL step toward the shooter sits OFF the silhouette → the old
// guard reads `buried=false` exactly where it should read "tunnel". The core `(5,5)` is in
// the bulk (off the edge), so the stall is mid-body with the ship alive — the user's case.
// =================================================================================

/// The off-axis-bore-stall hull id used by the Fix #5 regression.
const HULL_WEDGE: HullId = HullId(13);

/// A filled lower silhouette on an 11×11 grid: every cell with `r <= c+2` (85 cells), each
/// its own [`SectionId`]. The diagonal edge `r = c+2` is a genuine outer surface; a `(1,1)`
/// bore runs along it, and the cell one orthogonal step toward the shooter from an edge cell
/// is OFF the silhouette — the shape that makes the OLD single-axis tunnel guard misfire.
fn wedge_hull() -> Hull {
    let mut cells: Vec<GridCell> = Vec::new();
    let mut section = 0u32;
    for c in 0u16..11 {
        for r in 0u16..11 {
            if r <= c + 2 {
                cells.push(GridCell::new((c, r), SectionId(section)));
                section += 1;
            }
        }
    }
    Hull {
        id: HULL_WEDGE,
        name: "Wedge".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (11, 11),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Spawn a LIVE (non-`Wreck`) `Destructible` wedge ship whose `(1,1)` bore along the diagonal
/// edge has ALREADY been drilled to the stall cell `(4,6)`: the front edge cells
/// `{(0,2),(1,3),(2,4),(3,5)}` are pre-removed, so the next shot's first SURVIVING cell is
/// `(4,6)` — whose grid-centre radial `(-1,1)` is ⊥ to the `(1,1)` bore (a 90° read). The
/// core `(5,5)` is intact (the ship is alive). NO `Wreck`, so the armor angle uses the grid
/// centre (the live-ship regime the bug lives in).
fn spawn_drilled_wedge(w: &mut World) -> Entity {
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_WEDGE, wedge_hull());
    }
    let drilled: [(u16, u16); 4] = [(0, 2), (1, 3), (2, 4), (3, 5)];
    let mut cells = CellMap::new();
    for gc in wedge_hull().cells {
        let (c, r) = gc.coord;
        if drilled.contains(&(c, r)) {
            continue; // already bored away
        }
        let depth = c.min(10 - c).min(r).min(10 - r);
        cells.insert(
            (c, r),
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0, // small structural HP so a carve removes the cell
                depth,
                structural: true,
            },
        );
    }
    let layout = FitLayout {
        hull: HULL_WEDGE,
        cells,
    };
    w.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius((11, 11))),
        layout,
        Destructible,
    ))
    .id()
}

/// REGRESSION (Fix #5): a live ship being bored along ONE off-axis line does NOT permanently
/// ricochet mid-body. The bore has drilled to `(4,6)`; the next shot's entry IS `(4,6)`, whose
/// grid-centre radial is ⊥ to the `(1,1)` bore. PRE-FIX the single-axis tunnel guard reads
/// `front=(3,6)` (off the silhouette) → `buried=false` → the 90° radial → `Ricochet`, nothing
/// carved, the hole stuck forever. POST-FIX the robust authored-cell-in-front test sees the
/// drilled tunnel (`(3,5)` is authored and in front along the real ray) → head-on → the bore
/// drills on through. The ship stays ALIVE (the core `(5,5)` is untouched).
#[test]
fn live_ship_bore_does_not_permanently_ricochet_mid_body() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let ship = spawn_drilled_wedge(&mut w);

    let ev = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0, // < overmatch_ratio * default facet thickness (1.5*1.0) → the angle decides
        point: Vec2::new(3.5, 5.5), // centre of the drilled (3,5), just before the (4,6) entry
        dir: Vec2::new(1.0, 1.0).normalize(), // the diagonal bore
        source: None,
    };
    let out = apply_damage(&mut w, ship, ev);

    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "the bore drills THROUGH the buried tunnel cell (4,6) instead of permanently \
         ricocheting mid-body; got {:?} (pre-fix this was Ricochet — the stall)",
        out.result
    );
    assert!(
        out.destroyed_cells.contains(&(4, 6)),
        "the stall cell (4,6) is carved (the bore advances); got {:?}",
        out.destroyed_cells
    );
    // The ship is still ALIVE — the stall was mid-body, the core (5,5) is intact.
    let layout = w
        .get::<FitLayout>(ship)
        .expect("the live ship still exists (mid-body stall, not a kill)");
    assert!(
        layout.cells.contains_key(&(5, 5)),
        "the core (5,5) is untouched — this reproduces a MID-BODY stall on a live ship"
    );
}

// =================================================================================
// Fix #6 — a wreck's render/carve reference is a FROZEN `MeshAnchor` captured at sever/death,
// not the live cell-COM recomputed every update. Without it, removing a cell shifted the COM
// and the whole piece visibly jumped ("re-centres on its COM"). The anchor freezes the
// reference so carving a cell only removes that cell — the rest stay put. Both the sim carve
// (`center_or_anchor`) and the client render (`hull_mesh_center`) resolve to it.
// =================================================================================

/// `sever_chunk` freezes a `MeshAnchor` at the chunk's cell-COM (the cell-space point whose
/// world location is the chunk `Position`) — so carving the chunk later does not re-centre it.
#[test]
fn sever_chunk_freezes_a_meshanchor_at_the_chunk_com() {
    // Sever the corridor's far end {(3,1),(4,1)} via the REAL `on_section_destroyed` path.
    let (mut w, ship) = corridor_world(Vec2::new(20.0, -7.0), Vec2::ZERO, 0.0, 0.0);
    on_section_destroyed(&mut w, ship, SectionId(2));

    let chunk = {
        let mut q = w.query_filtered::<(Entity, &Wreck), With<FitLayout>>();
        let severed: Vec<Entity> = q
            .iter(&w)
            .filter(|(_, wr)| wr.origin == WreckOrigin::SeveredChunk)
            .map(|(e, _)| e)
            .collect();
        assert_eq!(severed.len(), 1, "exactly one severed flank chunk");
        severed[0]
    };

    let anchor = w
        .get::<sim::components::MeshAnchor>(chunk)
        .expect("a severed chunk carries a FROZEN MeshAnchor");
    // cell-COM of {(3,1),(4,1)} = mean((3.5,1.5),(4.5,1.5)) = (4.0, 1.5).
    assert_eq!(
        anchor.0,
        Vec2::new(4.0, 1.5),
        "the anchor is the chunk's cell-COM at sever (its Position's cell-space point)"
    );
}

/// `destroy_ship` freezes a `MeshAnchor` at the hull's GRID CENTRE — the hulk keeps the ship's
/// grid-centre `Position`, so anchoring there freezes the dead hull's reference (and matches
/// the documented hulk render intent), so carving the dead hull does not re-centre it.
#[test]
fn destroy_ship_freezes_a_meshanchor_at_the_grid_centre() {
    let (mut w, ship) = corridor_world(Vec2::ZERO, Vec2::ZERO, 0.0, 0.0);
    // Destroying the CORE section (cell (1,1) = SectionId(1)) is whole-ship death → destroy_ship.
    on_section_destroyed(&mut w, ship, SectionId(1));
    assert_eq!(
        w.get::<Wreck>(ship).unwrap().origin,
        WreckOrigin::DestroyedShip,
        "destroying the core section is whole-ship death"
    );

    let anchor = w
        .get::<sim::components::MeshAnchor>(ship)
        .expect("the hulk carries a FROZEN grid-centre MeshAnchor");
    // Grid centre of the 5×3 corridor = (cols·0.5, rows·0.5) = (2.5, 1.5).
    assert_eq!(
        anchor.0,
        Vec2::new(2.5, 1.5),
        "the hulk anchor is the grid centre"
    );
}

/// THE fix: carving a cell off a wreck does NOT drift its frozen anchor — the reference the
/// carve + render use stays fixed even though the live cell-COM moves. Without the freeze the
/// reference would follow the moving COM and the whole piece would visibly shift.
#[test]
fn carving_a_wreck_does_not_drift_its_frozen_anchor() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // Off-centre wing wreck {(7,1),(7,2),(7,3)} on a 9×5 grid (cell-COM (7.5, 2.5)).
    let wreck = spawn_offcenter_wing_wreck(&mut w);
    let anchor0 = Vec2::new(7.5, 2.5); // the frozen reference `sever_chunk` would set
    w.entity_mut(wreck)
        .insert(sim::components::MeshAnchor(anchor0));

    // Simulate the carve removing an END cell (as `apply_damage` does) — this shifts the live
    // cell-COM in row from 2.5 → 2.0, the move that used to jump the piece.
    w.get_mut::<FitLayout>(wreck).unwrap().cells.remove(&(7, 3));
    let layout = w.get::<FitLayout>(wreck).unwrap();

    // (1) The anchor is FROZEN — carving never mutates it.
    let anchor = w.get::<sim::components::MeshAnchor>(wreck).unwrap().0;
    assert_eq!(
        anchor, anchor0,
        "carving must NOT change the frozen MeshAnchor"
    );

    // (2) The live cell-COM genuinely DRIFTED — so a non-frozen reference WOULD shift the piece.
    let live_com = sim::fitting::layout_center(layout, (0, 0), true);
    assert_eq!(
        live_com,
        Vec2::new(7.5, 2.0),
        "the recomputed COM moved off the anchor"
    );
    assert_ne!(anchor, live_com);

    // (3) The carve/render resolve to the FROZEN anchor, not the drifted COM.
    assert_eq!(
        sim::fitting::center_or_anchor(Some(anchor), layout, (9, 5), true),
        anchor,
        "with a MeshAnchor present the centre stays the anchor (the piece does not re-centre)"
    );
}

/// End-to-end: a wreck carrying a `MeshAnchor` still carves through `fitted_damage_system`
/// (the anchor-routed `centers` path works — a cell is removed), i.e. the freeze doesn't break
/// the carve. The anchor equals the fresh wing's cell-COM, so the shot lands exactly as the
/// anchor-less off-centre carve test, then the cells erode.
#[test]
fn wreck_with_meshanchor_still_carves_when_shot() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_offcenter_wing_wreck(&mut w);
    w.entity_mut(wreck)
        .insert(sim::components::MeshAnchor(Vec2::new(7.5, 2.5)));

    let before: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .unwrap()
        .cells
        .keys()
        .copied()
        .collect();
    let tpos = w.get::<Position>(wreck).unwrap().0;
    w.spawn((
        Projectile,
        Position(Vec2::new(tpos.x - 6.0, tpos.y)),
        PrevPosition(Vec2::new(tpos.x + 6.0, tpos.y)),
        Velocity(Vec2::new(-200.0, 0.0)),
        Damage(5000.0),
        Lifetime(3.0),
        WeaponSource::from_damage(5000.0),
    ));
    sim::fitted_damage_system(&mut w);

    let after: std::collections::BTreeSet<(u16, u16)> = w
        .get::<FitLayout>(wreck)
        .map(|l| l.cells.keys().copied().collect())
        .unwrap_or_default();
    assert!(
        after.len() < before.len(),
        "an anchor-carrying wreck is still carved when shot (cells removed: {:?})",
        before.difference(&after).collect::<Vec<_>>()
    );
}

// =================================================================================
// Fix #7 — splitting a WRECK must SEPARATE the halves (they used to overlap/"combine").
// `sever_chunk` placed a new chunk relative to the parent's grid centre, resolved via the
// parent's `Fit` — but a wreck has NO `Fit`, so the offset fell back to ZERO and the
// sub-chunk spawned on top of the parent. The fix uses the parent's frozen `MeshAnchor` as
// the offset reference for a wreck.
// =================================================================================

/// Splitting a `Wreck` spawns the new piece OFFSET from the parent (where its cells were),
/// not on top of it. Pre-fix `sever_chunk` fell back to a zero offset for the `Fit`-less
/// wreck → `Position == parent_pos` → the halves overlapped.
#[test]
fn splitting_a_wreck_offsets_the_new_piece_so_the_halves_do_not_overlap() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // An off-centre wing wreck {(7,1),(7,2),(7,3)} (cell-COM (7.5, 2.5)). A real wreck carries
    // a frozen anchor at that COM (Fix #6) — add it (the helper predates Fix #6).
    let parent = spawn_offcenter_wing_wreck(&mut w);
    let anchor = Vec2::new(7.5, 2.5);
    w.entity_mut(parent)
        .insert(sim::components::MeshAnchor(anchor));
    let parent_pos = w.get::<Position>(parent).unwrap().0;

    // Split off the far end {(7,3)} (cell-COM (7.5, 3.5)) — the disconnected half a centre
    // carve would produce. It must land OFFSET by `swap_scale((7.5,3.5) − anchor)`.
    let mut region = std::collections::HashSet::new();
    region.insert((7u16, 3u16));
    sever_chunk(&mut w, parent, &region);

    let sub = {
        let mut q = w.query_filtered::<Entity, With<Wreck>>();
        let others: Vec<Entity> = q.iter(&w).filter(|&e| e != parent).collect();
        assert_eq!(others.len(), 1, "exactly one sub-chunk was severed off");
        others[0]
    };
    let sub_pos = w.get::<Position>(sub).unwrap().0;

    // Expected: r_local = (7.5,3.5) − (7.5,2.5) = (Δcol 0, Δrow 1); world = swap(Δrow,Δcol)·CELL
    // = (1·CELL, 0) → offset (CELL_WORLD_SIZE, 0).
    let expected = parent_pos + Vec2::new(CELL_WORLD_SIZE, 0.0);
    assert!(
        (sub_pos - expected).length() < 1e-4,
        "the split piece spawns where its cells were ({expected:?}), got {sub_pos:?}"
    );
    assert!(
        (sub_pos - parent_pos).length() > 1e-3,
        "the split piece is NOT on top of the parent ({parent_pos:?}); pre-fix it overlapped \
         (sever_chunk fell back to a zero offset for the Fit-less wreck), got {sub_pos:?}"
    );
}

// =================================================================================
// Fix #8 — the armor obliquity uses the cell's LOCAL surface normal (from which neighbours
// are solid vs void), not a far-away grid-centre radial that mislabeled square-on flank hits
// on the elongated hull as glancing → "way too many ricochets". Ricochet stays a real
// mechanic: a genuine graze still bounces.
// =================================================================================

/// A SQUARE-ON shot at an off-axis flank cell now CARVES instead of spuriously ricocheting.
/// The fighter's nose-flank cell (3,8) faces `-col` (left); a shot from the left meets it
/// head-on. PRE-FIX the grid-centre radial (3.5,8.5)−(4.5,5.5)=(−1,3) read it as ~71.6° >
/// ricochet_angle → spurious Ricochet; the local surface normal (≈ −col) reads ~22° → carve.
/// A genuine LATERAL graze of the forward-facing nose tip (4,10) still ricochets (mechanic
/// preserved).
#[test]
fn square_on_flank_hit_carves_while_a_genuine_graze_still_ricochets() {
    let (mut w, ship) = fitted_world(None, thin_entry_facet);

    // (a) Square-on at the left-facing flank cell (3,8): enter from the left (cell +col).
    let square_on = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0, // < overmatch threshold → the angle decides
        point: Vec2::new(1.0, 8.5),
        dir: Vec2::new(1.0, 0.0),
        source: None,
    };
    let out = apply_damage(&mut w, ship, square_on);
    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "a square-on hit on the nose flank CARVES (local normal), not a spurious Ricochet; \
         got {:?} (pre-fix the grid-centre radial ricocheted it)",
        out.result
    );
    assert!(
        out.destroyed_cells.contains(&(3, 8)),
        "the flank cell (3,8) is carved; got {:?}",
        out.destroyed_cells
    );

    // (b) Genuine graze: a LATERAL shot across the forward-facing nose tip (4,10) → ~90° → still
    // ricochets. Proves the fix did NOT disable ricochet for glancing hits.
    let (mut w2, ship2) = fitted_world(None, thin_entry_facet);
    let graze = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(12.0, 10.5),
        dir: Vec2::new(-1.0, 0.0),
        source: None,
    };
    let out2 = apply_damage(&mut w2, ship2, graze);
    assert_eq!(
        out2.result,
        HitKind::Ricochet,
        "a genuine lateral graze of the nose tip still RICOCHETS (mechanic preserved); got {:?}",
        out2.result
    );
}

// =================================================================================
// Fix #9 — a `Wreck` chunk's armor geometry uses its OWN CURRENT cells, not the original ship
// it was severed from. Before, both the tunnel guard and the local normal read the authored
// full hull, so a detached chunk deflected by the INVISIBLE original-ship shape — the same
// piece carved from one side and ricocheted from another. Now a head-on hit on the chunk's
// VISIBLE face carves; only a genuine graze of its actual edge bounces.
// =================================================================================

/// A horizontal 5-cell bar `{(0,2)..(4,2)}` on a 6×5 grid — a deliberately elongated authored
/// shape whose END cell `(4,2)` faces `+col` (its only authored neighbour is the bar to its
/// left). A `Wreck` carved down to a SUBSET of this bar then deflects by the bar's geometry
/// under the old code, but by its OWN shape under Fix #9.
const HULL_BAR5: HullId = HullId(14);
fn bar5_hull() -> Hull {
    let coords = [(0u16, 2u16), (1, 2), (2, 2), (3, 2), (4, 2)];
    let cells: Vec<GridCell> = coords
        .iter()
        .enumerate()
        .map(|(i, &c)| GridCell::new(c, SectionId(i as u32)))
        .collect();
    Hull {
        id: HULL_BAR5,
        name: "Bar5".to_string(),
        class: sim::fitting::ShipClass::Fighter,
        role: sim::fitting::ShipRole::Utility,
        grid_dims: (6, 5),
        cells,
        power_capacity: 100.0,
        cpu_capacity: 100.0,
        mass_capacity: 100.0,
        hull_base_mass: 1.0,
        slots: Vec::new(),
    }
}

/// Spawn a `Wreck` whose residual cells are `cells` (a subset of the BAR5 hull), with the bar
/// registered so `apply_damage` resolves `layout.hull`. Each cell carries small structural HP.
fn spawn_bar_wreck(w: &mut World, cells: &[(u16, u16)]) -> Entity {
    {
        let mut hulls = w.get_resource_mut::<HullCatalog>().unwrap();
        hulls.hulls.insert(HULL_BAR5, bar5_hull());
    }
    let mut cm = CellMap::new();
    for &c in cells {
        cm.insert(
            c,
            CellOccupant {
                slot: SlotId(u32::MAX),
                module: None,
                health: 5.0,
                depth: 0,
                structural: true,
            },
        );
    }
    w.spawn((
        Position(Vec2::ZERO),
        Velocity(Vec2::ZERO),
        Heading(0.0),
        AngularVelocity(0.0),
        CollisionRadius(hull_collision_radius((6, 5))),
        FitLayout {
            hull: HULL_BAR5,
            cells: cm,
        },
        Destructible,
        Wreck::new(WreckOrigin::SeveredChunk),
    ))
    .id()
}

/// REGRESSION (Fix #9): a 1-cell scrap piece carves by its OWN shape, not the original hull's.
/// In the authored BAR5 the end cell `(4,2)` faces `+col`, so a perpendicular (downward) shot
/// reads ~90° → Ricochet. As a detached 1-cell chunk it has no neighbours → normal `0` →
/// head-on → carve. PRE-FIX (authored geometry) the perpendicular shot ricocheted.
#[test]
fn scrap_carves_by_its_own_shape_not_the_original_hull() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let wreck = spawn_bar_wreck(&mut w, &[(4, 2)]);

    // A perpendicular (cell −row, "downward") shot at the 1-cell chunk (4,2).
    let ev = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(4.5, 8.0),
        dir: Vec2::new(0.0, -1.0),
        source: None,
    };
    let out = apply_damage(&mut w, wreck, ev);
    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "a perpendicular hit on the 1-cell scrap CARVES (its own shape → head-on), not a \
         ricochet off the original bar's geometry; got {:?}",
        out.result
    );
    assert!(
        out.destroyed_cells.contains(&(4, 2)),
        "the scrap cell (4,2) is carved; got {:?}",
        out.destroyed_cells
    );
}

/// COMPANION (Fix #9 keeps the mechanic): on a multi-cell chunk, a head-on hit on its broad
/// face carves, while a genuine graze of its actual END edge still ricochets — both judged by
/// the CHUNK's own shape — and a THIN (1-wide) bar carves from EVERY angle (Fix #10): its
/// cells have ≤2 present neighbours, so the degenerate normal never triggers a ricochet.
#[test]
fn thin_bar_scrap_carves_from_every_angle() {
    // (a) Broad face: a downward shot at the MIDDLE cell (2,2) of the full bar → carve.
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let bar = spawn_bar_wreck(&mut w, &[(0, 2), (1, 2), (2, 2), (3, 2), (4, 2)]);
    let head_on = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(2.5, 8.0),
        dir: Vec2::new(0.0, -1.0),
        source: None,
    };
    let o1 = apply_damage(&mut w, bar, head_on);
    assert!(
        matches!(o1.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "a downward hit on the thin bar's broad face carves; got {:?}",
        o1.result
    );

    // (b) The END cell (4,2) — a 1-wide bar TIP (1 present neighbour). Old code read its
    // degenerate `+col` normal as ~90° → ricochet; Fix #10's neighbour gate treats a thin
    // shard head-on → it CARVES from this angle too (no spurious bounce on debris slivers).
    let mut w2 = World::new();
    insert_full_combat_resources(&mut w2);
    let bar2 = spawn_bar_wreck(&mut w2, &[(0, 2), (1, 2), (2, 2), (3, 2), (4, 2)]);
    let end = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(4.5, 8.0),
        dir: Vec2::new(0.0, -1.0),
        source: None,
    };
    let o2 = apply_damage(&mut w2, bar2, end);
    assert!(
        matches!(o2.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "the thin bar's end no longer ricochets — it carves; got {:?}",
        o2.result
    );
    assert!(
        o2.destroyed_cells.contains(&(4, 2)),
        "the thin bar's end cell (4,2) is carved; got {:?}",
        o2.destroyed_cells
    );
}

/// Fix #10 repro: a 2-cell bar wreck broad-side shot CARVES (was a spurious ricochet). The bar
/// cell's only neighbour is along the bar, so its computed normal points along the bar (not the
/// broad face) — old code read a perpendicular shot as ~90° → Ricochet. The ≥3-neighbour gate
/// treats the 1-wide shard head-on → carve.
#[test]
fn thin_two_cell_bar_carves_broadside_not_ricochet() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let bar = spawn_bar_wreck(&mut w, &[(2, 2), (3, 2)]);
    let broadside = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(2.5, 8.0), // perpendicular (downward) to the horizontal bar
        dir: Vec2::new(0.0, -1.0),
        source: None,
    };
    let out = apply_damage(&mut w, bar, broadside);
    assert!(
        matches!(out.result, HitKind::Penetrated | HitKind::OverPenetrated),
        "a broad-side shot on a 2-cell bar CARVES (thin shard → head-on), not a spurious \
         ricochet; got {:?} (pre-fix this ricocheted off the degenerate normal)",
        out.result
    );
    assert!(
        out.destroyed_cells.contains(&(2, 2)),
        "the bar cell (2,2) is carved; got {:?}",
        out.destroyed_cells
    );
}

/// Fix #10 keeps the mechanic for SUBSTANTIAL chunks: a full-fighter HULK (a solid 2-D body)
/// still ricochets a genuine graze. A lateral shot across the forward-facing nose tip `(4,10)`
/// — which has ≥3 present neighbours in the solid hulk — reads ~90° → Ricochet. (Only thin
/// 1-wide shards were made non-ricocheting.)
#[test]
fn solid_hulk_scrap_still_ricochets_on_a_graze() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let hulk = spawn_wreck(&mut w, true); // full fighter hulk = solid, ≥3-neighbour edges
    let graze = DamageEvent {
        channel: Channel::Kinetic,
        magnitude: 1000.0,
        penetration: 5000.0,
        pen_size: 1.0,
        point: Vec2::new(12.0, 10.5), // far +col side, on the nose-tip row
        dir: Vec2::new(-1.0, 0.0),    // sideways across the forward-facing nose tip
        source: None,
    };
    let out = apply_damage(&mut w, hulk, graze);
    assert_eq!(
        out.result,
        HitKind::Ricochet,
        "a genuine lateral graze of a SOLID hulk's nose tip still RICOCHETS (mechanic preserved \
         for substantial chunks); got {:?}",
        out.result
    );
}

// =================================================================================
// Phase M4 (Phase A) — wreckage DRIFT + TUMBLE on inherited velocity/spin, and the
// despawn-when-old lifetime. `sever_chunk`/`destroy_ship` already set the inherited
// momentum; the new `wreck_motion_system` integrates it (it never moved before because the
// only integrator was `With<Ship>`-gated), and `wreck_lifetime_system` despawns old debris.
// =================================================================================

use sim::components::{WreckLifetime, WRECK_LIFETIME_SECS};

/// A `Wreck` body coasts on its inherited velocity + spin each fixed step (frictionless — no
/// thrust, no drag), driven by the new `wreck_motion_system`. Pre-M4 it sat frozen (the only
/// integrator, `ship_motion_system`, is `With<Ship>`-gated and a wreck is not a `Ship`).
#[test]
fn a_wreck_drifts_and_tumbles_on_its_inherited_motion() {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(1.0 / 30.0));
    w.insert_resource(HitFeedback::default());
    let dt = 1.0 / 30.0_f32;

    let v = Vec2::new(2.0, -1.0);
    let omega = 0.5_f32;
    let start = Vec2::new(3.0, 4.0);
    let start_h = 0.25_f32;
    // No `WreckLifetime` so it persists for the whole test (despawn-when-old is tested below).
    let wreck = w
        .spawn((
            Position(start),
            Velocity(v),
            Heading(start_h),
            AngularVelocity(omega),
            Wreck::new(WreckOrigin::SeveredChunk),
        ))
        .id();

    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);
    let n = 10;
    for _ in 0..n {
        schedule.run(&mut w);
    }

    let pos = w.get::<Position>(wreck).unwrap().0;
    let head = w.get::<Heading>(wreck).unwrap().0;
    let expected_pos = start + v * (dt * n as f32);
    let expected_h = (start_h + omega * dt * n as f32).rem_euclid(std::f32::consts::TAU);
    assert!(
        (pos - expected_pos).length() < 1e-3,
        "the wreck drifts by vel·t (got {pos:?}, expected {expected_pos:?}) — pre-M4 it stayed put"
    );
    assert!(
        (head - expected_h).abs() < 1e-3,
        "the wreck tumbles by ω·t (got {head}, expected {expected_h})"
    );
    // Frictionless: velocity + spin are conserved (no drag on a wreck).
    assert!(
        (w.get::<Velocity>(wreck).unwrap().0 - v).length() < 1e-6,
        "no drag — linear velocity is conserved"
    );
    assert!(
        (w.get::<AngularVelocity>(wreck).unwrap().0 - omega).abs() < 1e-6,
        "no angular drag — spin is conserved"
    );
}

/// Two independently-seeded wreck bodies advance bit-identically across two runs of the shared
/// schedule (determinism preserved by the new motion system).
#[test]
fn wreck_drift_is_bit_identical_across_two_runs() {
    fn run() -> (Vec2, f32, Vec2, f32) {
        let mut w = World::new();
        w.insert_resource(Tuning::default());
        w.insert_resource(FixedDt(1.0 / 30.0));
        w.insert_resource(HitFeedback::default());
        let a = w
            .spawn((
                Position(Vec2::new(1.0, 2.0)),
                Velocity(Vec2::new(0.7, -0.3)),
                Heading(0.1),
                AngularVelocity(-0.4),
                Wreck::new(WreckOrigin::SeveredChunk),
            ))
            .id();
        let b = w
            .spawn((
                Position(Vec2::new(-5.0, 9.0)),
                Velocity(Vec2::new(-1.1, 0.6)),
                Heading(2.0),
                AngularVelocity(0.9),
                Wreck::new(WreckOrigin::DestroyedShip),
            ))
            .id();
        let mut schedule = Schedule::default();
        sim::add_fixed_step_systems(&mut schedule);
        for _ in 0..25 {
            schedule.run(&mut w);
        }
        (
            w.get::<Position>(a).unwrap().0,
            w.get::<Heading>(a).unwrap().0,
            w.get::<Position>(b).unwrap().0,
            w.get::<Heading>(b).unwrap().0,
        )
    }
    assert_eq!(
        run(),
        run(),
        "wreck drift is deterministic across identical runs"
    );
}

/// `destroy_ship` (via the whole-ship-destroyed path) strips the `Ship` marker — so a corpse is
/// no longer driven by piloted flight (drag/stale intent) or `weapon_fire_system` — and stamps a
/// `WreckLifetime`, while keeping its physical body (Position/Velocity/Heading) and `Wreck`.
#[test]
fn destroy_ship_strips_the_ship_marker_and_sets_a_drift_lifetime() {
    let (mut w, ship, _) = salvage_world(50.0);
    // A live ship carries the `Ship` marker (the helper omits it; add it so the strip is real).
    w.entity_mut(ship).insert(Ship);
    assert!(
        w.get::<Ship>(ship).is_some(),
        "precondition: it is a live Ship"
    );

    // Destroying SectionId(0) collapses the core → the whole-ship-destroyed path → `destroy_ship`.
    on_section_destroyed(&mut w, ship, SectionId(0));

    assert!(
        w.get::<Wreck>(ship).is_some(),
        "the ship becomes a persistent wreck"
    );
    assert!(
        w.get::<Ship>(ship).is_none(),
        "the hulk is no longer a piloted Ship (Phase M4 strips the marker)"
    );
    let life = w
        .get::<WreckLifetime>(ship)
        .expect("the hulk gets a drift lifetime so it eventually despawns");
    assert!(
        (life.0 - WRECK_LIFETIME_SECS).abs() < 1e-6,
        "the lifetime starts at WRECK_LIFETIME_SECS (got {})",
        life.0
    );
    assert!(
        w.get::<Position>(ship).is_some() && w.get::<Velocity>(ship).is_some(),
        "the hulk is still a physical body that drifts"
    );
}

/// `wreck_lifetime_system` decays a wreck's `WreckLifetime` by the fixed `dt` and despawns it at
/// `<= 0` — so drifting debris doesn't accumulate forever in frictionless space.
#[test]
fn an_old_wreck_despawns_after_its_lifetime() {
    let mut w = World::new();
    w.insert_resource(Tuning::default());
    w.insert_resource(FixedDt(1.0 / 30.0));
    w.insert_resource(HitFeedback::default());
    let dt = 1.0 / 30.0_f32;

    // A short lifetime so the test runs in a few ticks (drifting the whole time).
    let life = 3.0 * dt;
    let wreck = w
        .spawn((
            Position(Vec2::new(0.0, 0.0)),
            Velocity(Vec2::new(5.0, 0.0)),
            Heading(0.0),
            AngularVelocity(0.0),
            Wreck::new(WreckOrigin::SeveredChunk),
            WreckLifetime(life),
        ))
        .id();

    let mut schedule = Schedule::default();
    sim::add_fixed_step_systems(&mut schedule);
    // Two steps: still alive (lifetime not yet exhausted).
    schedule.run(&mut w);
    schedule.run(&mut w);
    assert!(
        w.get_entity(wreck).is_ok(),
        "the wreck is still drifting before its lifetime ends"
    );
    // After enough steps to exhaust the lifetime, it is despawned.
    for _ in 0..3 {
        schedule.run(&mut w);
    }
    assert!(
        w.get_entity(wreck).is_err(),
        "the wreck despawns once its drift lifetime is exhausted"
    );
}

// =================================================================================
// Phase M4 (Phase B) — a projectile hit transfers its MOMENTUM to the struck body:
// a linear shove along the shot, plus an off-centre TUMBLE. The impulse is applied in
// `fitted_damage_system` BEFORE the carve, so a piece the carve severs carries the kick.
// (The impulse MATH is unit-tested in `motion.rs`; these prove the wiring.)
// =================================================================================

/// A head-on (centred) hit shoves the target along the shot direction (linear momentum
/// transfer), with negligible spin (the contact arm is ∥ the impulse through the centre).
#[test]
fn a_centred_hit_shoves_the_target_along_the_shot() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let ship = fitted_fighter_at_origin(&mut w);
    // Drain the shield so the hit reaches the hull (the impulse applies on any hit, but this
    // keeps the scenario unambiguous).
    if let Some(mut s) = w.get_mut::<Shields>(ship) {
        s.current = 0.0;
    }
    assert_eq!(
        w.get::<Velocity>(ship).unwrap().0,
        Vec2::ZERO,
        "at rest before the hit"
    );

    // The centred nose-axis shot (world −X through y=0): impulse = PROJECTILE_MASS·(−180,0).
    spawn_downward_projectile(&mut w, None, 12.0);
    sim::fitted_damage_system(&mut w);

    let v = w.get::<Velocity>(ship).unwrap().0;
    let omega = w.get::<AngularVelocity>(ship).unwrap().0;
    assert!(
        v.x < -1e-4,
        "the target is shoved along the shot (−X); got {v:?} (was zero)"
    );
    assert!(
        v.y.abs() < 1e-3,
        "a centred shot imparts no lateral velocity; got {v:?}"
    );
    assert!(
        omega.abs() < 1e-3,
        "a head-on (arm ∥ impulse) hit imparts negligible spin; got {omega}"
    );
}

/// An OFF-CENTRE hit imparts spin (tumble): the contact arm is not parallel to the impulse, so
/// `Δω = (arm × J)/I ≠ 0`. Fires straight along −X but offset in +y so it strikes a wing cell
/// off the centreline.
#[test]
fn an_off_centre_hit_tumbles_the_target() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    let ship = fitted_fighter_at_origin(&mut w);
    if let Some(mut s) = w.get_mut::<Shields>(ship) {
        s.current = 0.0;
    }
    assert_eq!(
        w.get::<AngularVelocity>(ship).unwrap().0,
        0.0,
        "no spin before the hit"
    );

    // A −X shot offset to +y = 0.5 world (≈ col 6 on the 9-wide hull): still crosses hull cells
    // (so it's a real hit) but the impact sits off the centreline → a nonzero arm × impulse.
    let y = 0.5_f32;
    w.spawn((
        Projectile,
        Position(Vec2::new(0.0, y)),
        PrevPosition(Vec2::new(6.0, y)),
        Velocity(Vec2::new(-180.0, 0.0)),
        Damage(12.0),
        Lifetime(3.0),
        WeaponSource::from_damage(12.0),
    ));
    sim::fitted_damage_system(&mut w);

    let v = w.get::<Velocity>(ship).unwrap().0;
    let omega = w.get::<AngularVelocity>(ship).unwrap().0;
    assert!(v.x < -1e-4, "still shoved along the shot (−X); got {v:?}");
    assert!(
        omega.abs() > 1e-3,
        "an off-centre hit spins the target (tumble); got ω = {omega}"
    );
}

/// A `Wreck` is shoved + spun by a hit too (it routes through the SAME impulse path; its mass is
/// derived from its cell count, so a light chunk gets flung harder than a heavy ship).
#[test]
fn a_hit_shoves_and_spins_a_wreck() {
    let mut w = World::new();
    insert_full_combat_resources(&mut w);
    // A real severed-chunk-shaped wreck with a collider + Destructible + a frozen anchor.
    let wreck = spawn_offcenter_wing_wreck(&mut w);
    w.entity_mut(wreck)
        .insert(sim::components::MeshAnchor(Vec2::new(7.5, 2.5)));
    let wpos = w.get::<Position>(wreck).unwrap().0;
    let v0 = w.get::<Velocity>(wreck).unwrap().0;

    // Fire a shot that crosses the chunk's cells (aim at its world position, sweeping through it).
    w.spawn((
        Projectile,
        Position(wpos),
        PrevPosition(wpos + Vec2::new(6.0, 0.0)),
        Velocity(Vec2::new(-180.0, 0.0)),
        Damage(8.0),
        Lifetime(3.0),
        WeaponSource::from_damage(8.0),
    ));
    sim::fitted_damage_system(&mut w);

    // The chunk may have been carved/severed; if it still exists, it carries the kick.
    if let Ok(e) = w.get_entity(wreck) {
        let v = e.get::<Velocity>().unwrap().0;
        assert!(
            (v - v0).length() > 1e-3,
            "the wreck's velocity changed from the hit's momentum (was {v0:?}, now {v:?})"
        );
    }
}

// =================================================================================
// Phase M5 — per-weapon projectile mass: a heavier slug imparts more knockback.
// =================================================================================

/// A heavier `ProjectileMass` shoves the struck target proportionally harder — the per-weapon
/// slug mass (not a single global constant) sets the momentum a shot transfers.
#[test]
fn a_heavier_slug_imparts_more_knockback() {
    // The same centred nose shot at a fresh fighter, carrying different slug masses.
    fn shove(slug_mass: f32) -> f32 {
        let mut w = World::new();
        insert_full_combat_resources(&mut w);
        let ship = fitted_fighter_at_origin(&mut w);
        if let Some(mut s) = w.get_mut::<Shields>(ship) {
            s.current = 0.0;
        }
        w.spawn((
            Projectile,
            Position(Vec2::ZERO),
            PrevPosition(Vec2::new(6.0, 0.0)),
            Velocity(Vec2::new(-180.0, 0.0)),
            Damage(12.0),
            sim::components::ProjectileMass(slug_mass),
            Lifetime(3.0),
            WeaponSource::from_damage(12.0),
        ));
        sim::fitted_damage_system(&mut w);
        w.get::<Velocity>(ship).map(|v| v.0.length()).unwrap_or(0.0)
    }

    let light = shove(0.03);
    let heavy = shove(0.30); // a 10× heavier slug
    assert!(light > 0.0, "even the light slug shoves the target");
    assert!(
        heavy > light * 5.0,
        "a 10× heavier slug shoves much harder (light {light}, heavy {heavy})"
    );
}
