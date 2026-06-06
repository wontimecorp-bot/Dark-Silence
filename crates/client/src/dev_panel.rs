//! Live DEV tuning panel (Phase M6) — an egui overlay to view + change gameplay values while
//! playing, so feel can be tuned on the fly without a rebuild.
//!
//! **Where it writes.** The sim runs in the *embedded server's* world (the client's own `Tuning`
//! is vestigial), so every edit goes to `host.server.world_mut()` via [`crate::net::LoopbackHost`]
//! (`NonSendMut`). These are the **authoritative** values, so edits are **solo / server-side only**
//! — meaningless (and unauthorised) on a networked client; this panel only functions on the
//! embedded-server solo path and is gated behind the default-on `dev_panel` cargo feature.
//!
//! **Live vs. apply.** The per-frame sim reads `Tuning`/[`SimTuning`]/`PenetrationConfig`/… every
//! tick, so those edits take effect immediately. Editing the module/hull **catalog** or the
//! structural-cell mass/HP only changes a ship's cached `ShipStats`/`FitLayout` when it
//! **re-derives** — so the panel has an **Apply / Re-derive ships** button
//! ([`sim::fitting::force_rederive_all`]); note that re-deriving rebuilds a ship at full health
//! (previews new balance, but repairs battle damage).
//!
//! egui 0.39 note: UI runs in the [`EguiPrimaryContextPass`] schedule (multi-pass default), and
//! [`EguiContexts::ctx_mut`] returns a `Result` (we early-return before the context exists).

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPlugin, EguiPrimaryContextPass};

use sim::components::{Afterburner, ArmorHp, Energy, Heading, Health, Heat, Velocity};
use sim::damage::{
    default_resistance_matrix, HullStructure, PenetrationConfig, SalvageConfig, ShieldConfig,
    Shields, StatScalingConfig,
};
use sim::fitting::{
    force_rederive_all, seed_catalogs, Fit, FitLayout, HullCatalog, ModuleCatalog, ModuleSpecifics,
    ShipStats,
};
use sim::{MiningTuning, SimTuning, Tuning};

use crate::hud_bars::HudLayout;
use crate::net::{LoopbackHost, NetClientState};
use crate::starfield::StarfieldTuning;

/// Phase M6e — the single source of truth for every stat/knob the panel shows. A section refers to
/// a [`StatId`] instead of hand-writing its label/order/format, so a rename or reorder is a
/// one-place edit and sections can't drift apart. **The enum's declaration order IS the one global
/// sort order** (`id as usize`); `render_rows` sorts by it, so every group lists shared stats in the
/// same relative order. Display uses the `short` name; `long`/`code` are stored for reference.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatId {
    // Ship: locomotion / power / durability / weapon (shared across sections) — the master order.
    Mass,
    Thrust,
    Reverse,
    Strafe,
    Torque,
    TopSpeed,
    TurnRate,
    AngularInertia,
    LinearDrag,
    AngularDrag,
    TurnShare,
    PowerGen,
    PowerDraw,
    Cpu,
    Hp,
    ShieldHp,
    ShieldRegen,
    Armor,
    Dmg,
    Rof,
    Muzzle,
    Slug,
    LethalRam,
    // Defense / penetration tuning.
    RicochetAngle,
    OvermatchRatio,
    EffectiveArmorCap,
    PenTierFull,
    PenTierOver,
    PenTierNon,
    ShieldRegenDefault,
    UnpoweredDecay,
    StatHealthFloor,
    IntactFraction,
    ScrapFloor,
    ScrapPerMass,
    // Carve / structural / projectile / wreck / ram sim consts.
    StructCellHp,
    StructCellMass,
    CarveFalloff,
    CarvePenCost,
    CarveMinCellCost,
    RicochetMinNeighbors,
    SmoothNormalRadius,
    ProjMass,
    ProjDamage,
    ProjLifetime,
    PenPerDamage,
    PenSize,
    WreckLifetime,
    ShipRamMass,
    AsteroidRamMass,
    // Phase E energy/heat tuning.
    EnergyCapacitySecs,
    WeaponEnergyPerDamage,
    HeatCapacity,
    HeatDissipation,
    // Phase F energy-drain + afterburner tuning.
    ThrustEnergyPerInput,
    AfterburnerCapacity,
    AfterburnerDrainRate,
    AfterburnerRegenRate,
    AfterburnerBoostFactor,
    // Mining transport tuning (Refinement 3). Mass/Thrust/LinearDrag/Torque/AngularDrag/
    // AngularInertia are shared with the ship-flight group above.
    SlowRadius,
    ArriveRadius,
    DockSpeed,
    LoadRate,
    UnloadRate,
    CargoCapacity,
    // Hull capacities.
    BaseMass,
    PowerCap,
    CpuCap,
    MassCap,
    GridDims,
    // Runtime telemetry.
    Speed,
    Heading,
    Health,
    HullStruct,
    ShieldsState,
    ArmorState,
    Energy,
    Heat,
    AfterburnerState,
    Cells,
}

/// Display metadata for a [`StatId`]. `short` is what the panel shows; `long` + `code` (the Rust
/// field name) are stored for reference/future use per the M6e decision (not displayed today).
struct StatMeta {
    short: &'static str,
    #[allow(dead_code)]
    long: &'static str,
    #[allow(dead_code)]
    code: &'static str,
    /// Decimal places for `fmt`.
    decimals: u8,
    /// Unit suffix appended by `fmt` (carries its own leading space/symbol, e.g. `" rad/s"`, `"°"`).
    unit: &'static str,
}

impl StatId {
    const fn meta(self) -> StatMeta {
        use StatId::*;
        let (short, long, code, decimals, unit): (
            &'static str,
            &'static str,
            &'static str,
            u8,
            &'static str,
        ) = match self {
            Mass => ("mass", "Total mass", "total_mass", 2, ""),
            Thrust => ("thrust", "Forward thrust", "thrust_force", 1, ""),
            Reverse => ("reverse", "Reverse thrust", "reverse_force", 1, ""),
            Strafe => ("strafe", "Lateral thrust", "strafe_force", 1, ""),
            Torque => ("torque", "Turn torque", "turn_torque", 1, ""),
            TopSpeed => ("top speed", "Top speed", "top_speed", 1, ""),
            TurnRate => ("turn rate", "Max turn rate", "max_turn_rate", 2, " rad/s"),
            AngularInertia => (
                "angular inertia",
                "Angular inertia",
                "angular_inertia",
                2,
                "",
            ),
            LinearDrag => ("linear drag", "Linear drag", "linear_drag", 2, ""),
            AngularDrag => ("angular drag", "Angular drag", "angular_drag", 1, ""),
            TurnShare => ("turn share", "Turn power share", "turn_power_share", 2, ""),
            PowerGen => ("power gen", "Power generated", "power_gen", 1, ""),
            PowerDraw => ("power draw", "Power draw", "power_draw", 1, ""),
            Cpu => ("cpu", "CPU draw", "cpu_draw", 1, ""),
            Hp => ("hp", "Health max", "health_max", 0, ""),
            ShieldHp => ("shield hp", "Shield HP", "shield_hp", 0, ""),
            ShieldRegen => ("shield regen", "Shield regen", "regen", 1, ""),
            Armor => ("armor", "Armor value", "armor_value", 0, ""),
            Dmg => ("dmg", "Weapon damage", "damage", 0, ""),
            Rof => ("rof", "Rate of fire", "fire_rate", 1, "/s"),
            Muzzle => ("muzzle", "Muzzle speed", "muzzle_speed", 0, ""),
            Slug => ("slug", "Projectile mass", "projectile_mass", 3, ""),
            LethalRam => ("lethal ram", "Lethal ram speed", "lethal_ram_speed", 0, ""),
            RicochetAngle => ("ricochet_angle", "Ricochet angle", "ricochet_angle", 0, "°"),
            OvermatchRatio => (
                "overmatch_ratio",
                "Overmatch ratio",
                "overmatch_ratio",
                1,
                "",
            ),
            EffectiveArmorCap => (
                "effective_armor_cap",
                "Effective armor cap",
                "effective_armor_cap",
                0,
                "",
            ),
            PenTierFull => ("pen_tier_full", "Pen tier (full)", "pen_tier_full", 2, ""),
            PenTierOver => ("pen_tier_over", "Pen tier (over)", "pen_tier_over", 2, ""),
            PenTierNon => ("pen_tier_non", "Pen tier (non)", "pen_tier_non", 2, ""),
            ShieldRegenDefault => (
                "shield_regen_default",
                "Shield regen default",
                "shield_regen_default",
                1,
                "",
            ),
            UnpoweredDecay => (
                "unpowered_decay",
                "Unpowered decay",
                "unpowered_decay",
                1,
                "",
            ),
            StatHealthFloor => (
                "stat_health_floor",
                "Stat health floor",
                "stat_health_floor",
                2,
                "",
            ),
            IntactFraction => (
                "intact_fraction",
                "Intact fraction",
                "intact_fraction",
                2,
                "",
            ),
            ScrapFloor => ("scrap_floor", "Scrap floor", "scrap_floor", 1, ""),
            ScrapPerMass => ("scrap_per_mass", "Scrap per mass", "scrap_per_mass", 1, ""),
            StructCellHp => (
                "struct_cell_hp",
                "Structural cell HP",
                "struct_cell_hp",
                1,
                "",
            ),
            StructCellMass => (
                "struct_cell_mass",
                "Structural cell mass",
                "struct_cell_mass",
                2,
                "",
            ),
            CarveFalloff => ("carve_falloff", "Carve falloff", "carve_falloff", 2, ""),
            CarvePenCost => ("carve_pen_cost", "Carve pen cost", "carve_pen_cost", 1, ""),
            CarveMinCellCost => (
                "carve_min_cell_cost",
                "Carve min cell cost",
                "carve_min_cell_cost",
                1,
                "",
            ),
            RicochetMinNeighbors => (
                "ricochet_min_neighbors",
                "Ricochet min neighbors",
                "ricochet_min_neighbors",
                0,
                "",
            ),
            SmoothNormalRadius => (
                "smooth_normal_radius",
                "Smooth normal radius",
                "smooth_normal_radius",
                0,
                "",
            ),
            ProjMass => (
                "projectile_mass",
                "Projectile mass (unfitted)",
                "projectile_mass",
                3,
                "",
            ),
            ProjDamage => (
                "projectile_damage",
                "Projectile damage (unfitted)",
                "projectile_damage",
                0,
                "",
            ),
            ProjLifetime => (
                "projectile_lifetime",
                "Projectile lifetime",
                "projectile_lifetime",
                1,
                " s",
            ),
            PenPerDamage => ("pen_per_damage", "Pen per damage", "pen_per_damage", 1, ""),
            PenSize => ("pen_size", "Pen size", "pen_size", 1, ""),
            WreckLifetime => (
                "wreck_lifetime_secs",
                "Wreck lifetime",
                "wreck_lifetime_secs",
                0,
                " s",
            ),
            ShipRamMass => ("ship_ram_mass", "Ship ram mass", "ship_ram_mass", 1, ""),
            AsteroidRamMass => (
                "asteroid_ram_mass",
                "Asteroid ram mass",
                "asteroid_ram_mass",
                1,
                "",
            ),
            EnergyCapacitySecs => (
                "energy_capacity_secs",
                "Energy capacitor (s of output)",
                "energy_capacity_secs",
                1,
                " s",
            ),
            WeaponEnergyPerDamage => (
                "weapon_energy_per_damage",
                "Weapon energy per damage",
                "weapon_energy_per_damage",
                2,
                "",
            ),
            HeatCapacity => ("heat_capacity", "Heat capacity", "heat_capacity", 0, ""),
            HeatDissipation => (
                "heat_dissipation",
                "Heat dissipation",
                "heat_dissipation",
                1,
                "/s",
            ),
            ThrustEnergyPerInput => (
                "thrust_energy_per_input",
                "Thrust energy per input",
                "thrust_energy_per_input",
                1,
                "/s",
            ),
            AfterburnerCapacity => (
                "afterburner_capacity",
                "Afterburner capacity",
                "afterburner_capacity",
                0,
                "",
            ),
            AfterburnerDrainRate => (
                "afterburner_drain",
                "Afterburner drain rate",
                "afterburner_drain_rate",
                1,
                "/s",
            ),
            AfterburnerRegenRate => (
                "afterburner_regen",
                "Afterburner regen rate",
                "afterburner_regen_rate",
                1,
                "/s",
            ),
            AfterburnerBoostFactor => (
                "afterburner_boost",
                "Afterburner boost factor",
                "afterburner_boost_factor",
                2,
                "×",
            ),
            BaseMass => ("hull_base_mass", "Hull base mass", "hull_base_mass", 1, ""),
            PowerCap => ("power_capacity", "Power capacity", "power_capacity", 1, ""),
            CpuCap => ("cpu_capacity", "CPU capacity", "cpu_capacity", 1, ""),
            MassCap => ("mass_capacity", "Mass capacity", "mass_capacity", 1, ""),
            GridDims => ("grid_dims", "Grid dims", "grid_dims", 0, ""),
            Speed => ("speed", "Speed", "speed", 1, ""),
            Heading => ("heading", "Heading", "heading", 0, "°"),
            Health => ("health", "Health", "health", 0, ""),
            HullStruct => ("hull", "Hull structure", "hull_structure", 0, ""),
            ShieldsState => ("shields", "Shields", "shields", 0, ""),
            ArmorState => ("armor hp", "Armor HP", "armor_hp", 0, ""),
            Energy => ("energy", "Energy", "energy", 0, ""),
            Heat => ("heat", "Heat", "heat", 0, ""),
            AfterburnerState => ("afterburner", "Afterburner", "afterburner", 0, ""),
            Cells => ("cells", "Cells", "cells", 0, ""),
            SlowRadius => ("slow radius", "Arrive slow radius", "slow_radius", 0, ""),
            ArriveRadius => ("arrive radius", "Arrive radius", "arrive_radius", 0, ""),
            DockSpeed => ("dock speed", "Dock speed", "dock_speed", 1, ""),
            LoadRate => ("load rate", "Cargo load rate", "load_rate", 1, "/s"),
            UnloadRate => ("unload rate", "Cargo unload rate", "unload_rate", 1, "/s"),
            CargoCapacity => ("cargo cap", "Cargo capacity", "cargo_capacity", 0, ""),
        };
        StatMeta {
            short,
            long,
            code,
            decimals,
            unit,
        }
    }
}

/// The displayed (short) label for a stat — the single naming reference (Phase M6e).
fn label(id: StatId) -> &'static str {
    id.meta().short
}

/// Format a scalar stat value using the registry's decimals + unit suffix (Phase M6e).
fn fmt(id: StatId, v: f32) -> String {
    let m = id.meta();
    format!("{:.*}{}", m.decimals as usize, v, m.unit)
}

/// Render a group of read-only rows **sorted by the global registry order** (Phase M6e) — so every
/// group lists its shared stats in the same relative order. Each row is `(StatId, pre-formatted)`;
/// composite values (pairs, "—", "none") are formatted by the caller, scalars via [`fmt`].
fn render_rows(ui: &mut egui::Ui, mut rows: Vec<(StatId, String)>) {
    rows.sort_by_key(|(id, _)| *id as usize);
    for (id, v) in &rows {
        stat(ui, label(*id), v);
    }
}

/// A read-only snapshot of the local player ship's derived stats + live state (Phase M6b),
/// plus its installed-equipment list + nominal equipment totals (Phase M6c), gathered from the
/// server world up front so the egui closure holds no `host` borrow.
struct ShipReadout {
    stats: ShipStats,
    speed: f32,
    heading: f32,
    health: Option<f32>,
    hull: Option<(f32, f32)>,
    shields: Option<(f32, f32)>,
    /// Phase F — (current, max) of the live armor-HP pool, if present.
    armor: Option<(f32, f32)>,
    /// Phase E — (current, max) of the live Energy capacitor / Heat pools, if present.
    energy: Option<(f32, f32)>,
    heat: Option<(f32, f32)>,
    /// Phase F — (current, max) of the live afterburner pool, if present.
    afterburner: Option<(f32, f32)>,
    cells: usize,
    /// The ship's installed modules (one row per `Fit` assignment), pre-formatted.
    equipment: Vec<EquipmentRow>,
    /// Nominal (full-health, catalog) sums over `equipment`.
    totals: EquipTotals,
}

/// One installed module on the local ship, pre-formatted for display (Phase M6c) — owned strings
/// so the egui closure holds no `host`/catalog borrow.
struct EquipmentRow {
    /// Hull slot id (`SlotId(pub u32)`).
    slot: u32,
    /// `"{kind:?} {ModuleId:?}"`, e.g. `Reactor ModuleId(1)` (modules carry no name).
    label: String,
    /// The module's stats as `(StatId, formatted value)` rows in master order — rendered via the
    /// registry like every other stats group (Phase M6e).
    stats: Vec<(StatId, String)>,
}

/// Nominal (full-health, catalog) sums over the ship's installed modules (Phase M6c). These are the
/// equipment's **raw** contributions; the authoritative *health-scaled* result (with the flight-feel
/// constants folded in) is the derived [`ShipStats`] shown above them in the readout.
#[derive(Default)]
struct EquipTotals {
    count: usize,
    mass: f32,
    thrust: f32,
    torque: f32,
    strafe: f32,
    power_gen: f32,
    power_draw: f32,
    cpu_draw: f32,
    shield_hp: f32,
    armor_value: f32,
    weapon_damage: f32,
}

/// Build the installed-equipment list + nominal totals from the ship's [`Fit`] against the (cloned)
/// server [`ModuleCatalog`]. Pure formatting/summation — no world or `host` borrow escapes.
fn build_equipment(fit: &Fit, catalog: Option<&ModuleCatalog>) -> (Vec<EquipmentRow>, EquipTotals) {
    let mut rows = Vec::new();
    let mut t = EquipTotals::default();
    for (slot, mid) in fit.assignments.iter() {
        let Some(m) = catalog.and_then(|c| c.get(*mid)) else {
            rows.push(EquipmentRow {
                slot: slot.0,
                label: format!("{:?}  (not in catalog)", mid),
                stats: Vec::new(),
            });
            continue;
        };
        t.count += 1;
        t.mass += m.mass;
        t.power_gen += m.power_gen;
        t.power_draw += m.power_draw;
        t.cpu_draw += m.cpu_draw;

        // Stats keyed by StatId in the canonical master order (Phase M6e): mass → [thruster:
        // thrust/strafe/torque] → power gen/draw → cpu → hp → [shield | armor | weapon].
        let mut stats: Vec<(StatId, String)> = vec![(StatId::Mass, fmt(StatId::Mass, m.mass))];
        if let ModuleSpecifics::Thruster {
            thrust_force,
            turn_torque,
            strafe_force,
            .. // Phase C `propulsion` tag — not surfaced in the readout.
        } = &m.specifics
        {
            t.thrust += *thrust_force;
            t.strafe += *strafe_force;
            t.torque += *turn_torque;
            stats.push((StatId::Thrust, fmt(StatId::Thrust, *thrust_force)));
            stats.push((StatId::Strafe, fmt(StatId::Strafe, *strafe_force)));
            stats.push((StatId::Torque, fmt(StatId::Torque, *turn_torque)));
        }
        stats.push((StatId::PowerGen, fmt(StatId::PowerGen, m.power_gen)));
        stats.push((StatId::PowerDraw, fmt(StatId::PowerDraw, m.power_draw)));
        stats.push((StatId::Cpu, fmt(StatId::Cpu, m.cpu_draw)));
        stats.push((StatId::Hp, fmt(StatId::Hp, m.health_max)));
        match &m.specifics {
            ModuleSpecifics::Shield { shield_hp, regen } => {
                t.shield_hp += *shield_hp;
                stats.push((StatId::ShieldHp, fmt(StatId::ShieldHp, *shield_hp)));
                stats.push((StatId::ShieldRegen, fmt(StatId::ShieldRegen, *regen)));
            }
            ModuleSpecifics::Armor { armor_value } => {
                t.armor_value += *armor_value;
                stats.push((StatId::Armor, fmt(StatId::Armor, *armor_value)));
            }
            ModuleSpecifics::Weapon {
                muzzle_speed,
                fire_rate,
                damage,
                projectile_mass,
                .. // Phase C class/ammo/damage_type/secondary — not in the equipment rows yet.
            } => {
                t.weapon_damage += *damage;
                stats.push((StatId::Dmg, fmt(StatId::Dmg, *damage)));
                stats.push((StatId::Rof, fmt(StatId::Rof, *fire_rate)));
                stats.push((StatId::Muzzle, fmt(StatId::Muzzle, *muzzle_speed)));
                stats.push((StatId::Slug, fmt(StatId::Slug, *projectile_mass)));
            }
            // Phase C: Sensor shows only its common cost rows (range/resolution have no StatId yet).
            ModuleSpecifics::Thruster { .. }
            | ModuleSpecifics::Reactor
            | ModuleSpecifics::Utility
            | ModuleSpecifics::Sensor { .. } => {}
        }
        rows.push(EquipmentRow {
            slot: slot.0,
            label: format!("{:?} {:?}", m.kind, mid),
            stats,
        });
    }
    (rows, t)
}

/// Visibility of the two dev windows — the editable **Dev Tuning** panel and the read-only **Ship
/// Stats** panel (Phase M6c). Backtick (`` ` ``) toggles both at once; each window's egui `[x]`
/// closes just that one. Default **open** (this is the solo dev client).
#[derive(Resource)]
pub struct DevPanelState {
    pub tuning_open: bool,
    pub stats_open: bool,
}

impl Default for DevPanelState {
    fn default() -> Self {
        Self {
            tuning_open: true,
            stats_open: true,
        }
    }
}

/// Adds the egui-based live tuning panel (Phase M6). Registered only under the `dev_panel` feature.
pub struct DevPanelPlugin;

impl Plugin for DevPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default())
            .init_resource::<DevPanelState>()
            .add_systems(Update, toggle_dev_panel)
            // egui 0.39 multi-pass default: UI systems belong in EguiPrimaryContextPass.
            .add_systems(EguiPrimaryContextPass, dev_panel_ui);
    }
}

/// Flip BOTH dev windows on a fresh backtick (`` ` ``) press — the universal dev-console key, unused
/// by flight/fitting (W/A/S/D/Q/E/Space/F/V/C/Tab/=/-). If either window is open the key hides both;
/// if both are closed it shows both (so a per-window `[x]` close is recoverable with one key).
fn toggle_dev_panel(keys: Res<ButtonInput<KeyCode>>, mut state: ResMut<DevPanelState>) {
    if keys.just_pressed(KeyCode::Backquote) {
        let show = !(state.tuning_open || state.stats_open);
        state.tuning_open = show;
        state.stats_open = show;
    }
}

/// A usable drag increment for an editable range field, scaled to the range span so the
/// `DragValue` feels right whether the range is tiny (`0.1..=10`) or large (`0..=120`).
fn drag_speed(lo: f32, hi: f32) -> f64 {
    ((hi - lo).abs().max(1.0) as f64) * 0.01
}

/// f32 slider row with **editable min/max range fields** (Refinement 9). The slider's range
/// endpoints are exposed as small `DragValue` number fields flanking the slider, so a value
/// can be pushed past its built-in cap (e.g. raise the thrust max above 120 and drag past
/// it). The per-slider `(min, max)` override is stored in egui's temp memory keyed by
/// `label`, so it persists for the session AND every existing caller keeps its signature
/// unchanged (the passed `range` is just the default). The range is always widened to
/// contain the live value so opening the panel never silently clamps a value down; min is
/// kept ≤ max.
fn slider(ui: &mut egui::Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>) {
    let id = ui.make_persistent_id(("dev_slider_limits", label));
    let (mut lo, mut hi) = ui
        .data_mut(|d| d.get_temp::<(f32, f32)>(id))
        .unwrap_or((*range.start(), *range.end()));
    // Never let the slider clamp the current value down (the value may already exceed the
    // default cap, or a prior edit raised it) — widen the range to include it.
    lo = lo.min(*v);
    hi = hi.max(*v);
    let speed = drag_speed(lo, hi);
    ui.horizontal(|ui| {
        // Editable lower bound, the value slider over the live range, then the editable upper
        // bound — all on one line.
        ui.add_sized([56.0, 18.0], egui::DragValue::new(&mut lo).speed(speed));
        ui.add(egui::Slider::new(v, lo..=hi).text(label));
        ui.add_sized([56.0, 18.0], egui::DragValue::new(&mut hi).speed(speed));
    });
    if hi < lo {
        hi = lo;
    }
    ui.data_mut(|d| d.insert_temp(id, (lo, hi)));
}

/// One read-only stat row (Phase M6c-fix): the label left-padded in a fixed-width **monospace**
/// column, then the value — so every stats group lines up in the same columns regardless of label
/// length. Egui's default font is proportional, so the alignment relies on the monospace style.
fn stat(ui: &mut egui::Ui, label: &str, value: impl std::fmt::Display) {
    ui.label(egui::RichText::new(format!("{label:<16}{value}")).monospace());
}

/// Render the read-only **Ship Stats** window body. Three groups, every one in the SAME canonical
/// field order (Phase M6c-fix): **Applied** = the cached [`ShipStats`] the ship currently flies on
/// (only refreshes on Apply / damage), **Runtime** = live dynamic telemetry, then the installed
/// **Equipment** list + its nominal (full-health catalog) summed contributions.
fn render_ship_stats(ui: &mut egui::Ui, r: &ShipReadout) {
    // Canonical order: mass → thrust → reverse → strafe → turn_torque → top_speed → max_turn_rate
    // → angular_inertia → power(gen/supply) → power_draw → cpu_draw → shield_hp → armor → weapon.
    let s = &r.stats;
    ui.label(egui::RichText::new("Applied — ship flies on this (refresh: Apply)").strong());
    let mut applied = vec![
        (StatId::Mass, fmt(StatId::Mass, s.total_mass)),
        (StatId::Thrust, fmt(StatId::Thrust, s.thrust_force)),
        (StatId::Reverse, fmt(StatId::Reverse, s.reverse_force)),
        (StatId::Strafe, fmt(StatId::Strafe, s.strafe_force)),
        (StatId::Torque, fmt(StatId::Torque, s.turn_torque)),
        (StatId::TopSpeed, fmt(StatId::TopSpeed, s.top_speed())),
        (StatId::TurnRate, fmt(StatId::TurnRate, s.max_turn_rate())),
        (
            StatId::AngularInertia,
            fmt(StatId::AngularInertia, s.angular_inertia),
        ),
        (StatId::PowerGen, fmt(StatId::PowerGen, s.power_supply)),
        (StatId::PowerDraw, fmt(StatId::PowerDraw, s.power_draw)),
        (StatId::Cpu, fmt(StatId::Cpu, s.cpu_draw)),
    ];
    if let Some(w) = &s.weapon {
        applied.push((StatId::Dmg, fmt(StatId::Dmg, w.damage)));
        applied.push((StatId::Rof, fmt(StatId::Rof, w.fire_rate)));
        applied.push((StatId::Muzzle, fmt(StatId::Muzzle, w.muzzle_speed)));
        applied.push((StatId::Slug, fmt(StatId::Slug, w.projectile_mass)));
    }
    render_rows(ui, applied);
    if s.weapon.is_none() {
        stat(ui, "weapon", format!("none (can_fire {})", s.can_fire));
    }

    ui.separator();
    ui.label(egui::RichText::new("Runtime").strong());
    render_rows(
        ui,
        vec![
            (StatId::Speed, fmt(StatId::Speed, r.speed)),
            (
                StatId::Heading,
                fmt(StatId::Heading, r.heading.to_degrees()),
            ),
            (
                StatId::Health,
                r.health.map_or("—".to_string(), |h| fmt(StatId::Health, h)),
            ),
            (
                StatId::HullStruct,
                r.hull
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (
                StatId::ShieldsState,
                r.shields
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (
                StatId::ArmorState,
                r.armor
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (
                StatId::Energy,
                r.energy
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (
                StatId::Heat,
                r.heat
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (
                StatId::AfterburnerState,
                r.afterburner
                    .map_or("—".to_string(), |(c, m)| format!("{:.0} / {:.0}", c, m)),
            ),
            (StatId::Cells, format!("{}", r.cells)),
        ],
    );

    ui.separator();
    egui::CollapsingHeader::new(format!("Equipment (installed) — {}", r.equipment.len()))
        .default_open(true)
        .show(ui, |ui| {
            if r.equipment.is_empty() {
                ui.label("none");
            }
            for row in &r.equipment {
                ui.label(egui::RichText::new(format!("slot {}  {}", row.slot, row.label)).strong());
                ui.indent(("equip", row.slot), |ui| {
                    for (id, v) in &row.stats {
                        stat(ui, label(*id), v);
                    }
                });
            }
        });

    ui.separator();
    let t = &r.totals;
    ui.label(egui::RichText::new("Equipment totals (nominal: full-health catalog sums)").strong());
    stat(ui, "modules", format!("{}", t.count));
    render_rows(
        ui,
        vec![
            (StatId::Mass, fmt(StatId::Mass, t.mass)),
            (StatId::Thrust, fmt(StatId::Thrust, t.thrust)),
            (StatId::Strafe, fmt(StatId::Strafe, t.strafe)),
            (StatId::Torque, fmt(StatId::Torque, t.torque)),
            (StatId::PowerGen, fmt(StatId::PowerGen, t.power_gen)),
            (StatId::PowerDraw, fmt(StatId::PowerDraw, t.power_draw)),
            (StatId::Cpu, fmt(StatId::Cpu, t.cpu_draw)),
            (StatId::ShieldHp, fmt(StatId::ShieldHp, t.shield_hp)),
            (StatId::Armor, fmt(StatId::Armor, t.armor_value)),
            (StatId::Dmg, fmt(StatId::Dmg, t.weapon_damage)),
        ],
    );
}

/// The panel: read resource copies/clones from the server world up front, build the egui window
/// against the locals (egui borrow rule — never hold a `world_mut()` borrow across the closure),
/// then write the locals back. `host` is `Option` so the first frames (before the embedded server
/// exists) are a no-op.
fn dev_panel_ui(
    mut contexts: EguiContexts,
    host: Option<NonSendMut<LoopbackHost>>,
    // The local player's wire id → resolve the ship in the server world (M6b readout).
    net: Option<NonSend<NetClientState>>,
    mut state: ResMut<DevPanelState>,
    // Refinement 24: the CLIENT-side live HUD layout (edited here, applied by the HUD apply systems).
    // A direct resource param — NOT in the embedded server world.
    mut hud_layout: ResMut<HudLayout>,
    // Refinement 25: CLIENT-side live starfield + bloom tuning (applied by `update_starfield`).
    mut starfield: ResMut<StarfieldTuning>,
) {
    if !state.tuning_open && !state.stats_open {
        return;
    }
    let Some(mut host) = host else {
        return;
    };
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // --- read copies/clones (immutable borrow ends with this block) ---------------
    let world = host.server.world();
    let mut tuning = world.get_resource::<Tuning>().copied().unwrap_or_default();
    let mut sim = world
        .get_resource::<SimTuning>()
        .copied()
        .unwrap_or_default();
    let mut pen = world
        .get_resource::<PenetrationConfig>()
        .copied()
        .unwrap_or_default();
    let mut shield = world
        .get_resource::<ShieldConfig>()
        .copied()
        .unwrap_or_default();
    let mut salvage = world
        .get_resource::<SalvageConfig>()
        .copied()
        .unwrap_or_default();
    let mut scaling = world
        .get_resource::<StatScalingConfig>()
        .copied()
        .unwrap_or_default();
    let mut modules = world.get_resource::<ModuleCatalog>().cloned();
    let mut hulls = world.get_resource::<HullCatalog>().cloned();
    let mut mining = world
        .get_resource::<MiningTuning>()
        .copied()
        .unwrap_or_default();

    // M6b: snapshot the LOCAL player ship's derived stats + live state (read-only). Resolve the
    // server entity from the client's wire `local_id`; gather into an owned struct so the egui
    // closure holds no `host` borrow. `None` until the ship exists / is resolvable.
    let ship_readout: Option<ShipReadout> = net.as_ref().and_then(|net| {
        let e = host.server.ship_entity_for(net.local_id)?;
        let w = host.server.world();
        let stats = w.get::<ShipStats>(e).copied()?;
        let speed = w.get::<Velocity>(e).map(|v| v.0.length()).unwrap_or(0.0);
        let heading = w.get::<Heading>(e).map(|h| h.0).unwrap_or(0.0);
        let health = w.get::<Health>(e).map(|h| h.0);
        let hull = w.get::<HullStructure>(e).map(|h| (h.current, h.max));
        let shields = w.get::<Shields>(e).map(|s| (s.current, s.max));
        let armor = w.get::<ArmorHp>(e).map(|a| (a.current, a.max));
        let energy = w.get::<Energy>(e).map(|p| (p.current, p.max));
        let heat = w.get::<Heat>(e).map(|p| (p.current, p.max));
        let afterburner = w.get::<Afterburner>(e).map(|p| (p.current, p.max));
        let cells = w.get::<FitLayout>(e).map_or(0, |l| l.cells.len());
        // M6c: the ship's installed equipment + nominal summed contributions (from its Fit against
        // the cloned catalog). Owned strings/scalars → no borrow escapes the closure.
        let (equipment, totals) = w
            .get::<Fit>(e)
            .map(|fit| build_equipment(fit, modules.as_ref()))
            .unwrap_or_default();
        Some(ShipReadout {
            stats,
            speed,
            heading,
            health,
            hull,
            shields,
            armor,
            energy,
            heat,
            afterburner,
            cells,
            equipment,
            totals,
        })
    });

    let mut tuning_open = state.tuning_open;
    let mut stats_open = state.stats_open;
    let mut rederive = false;
    let mut reset = false;

    // M6c — read-only Ship Stats in its OWN draggable window: derived stats + live state, then the
    // ship's installed equipment and its nominal summed contributions.
    egui::Window::new("📊 Ship Stats")
        .open(&mut stats_open)
        .default_width(300.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match &ship_readout {
                None => {
                    ui.label("no local ship yet");
                }
                Some(r) => render_ship_stats(ui, r),
            });
        });

    egui::Window::new("🛠 Dev Tuning")
        .open(&mut tuning_open)
        .default_width(340.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(
                    "Solo / server-authoritative edits. Catalog + struct-cell edits need Apply.",
                );

                egui::CollapsingHeader::new(
                    "Flight — Tuning (unfitted bodies; fitted ships derive from the catalog)",
                )
                .show(ui, |ui| {
                    slider(ui, label(StatId::Mass), &mut tuning.mass, 0.1..=10.0);
                    slider(
                        ui,
                        label(StatId::Thrust),
                        &mut tuning.thrust_force,
                        1.0..=120.0,
                    );
                    slider(
                        ui,
                        label(StatId::Reverse),
                        &mut tuning.reverse_force,
                        1.0..=80.0,
                    );
                    slider(
                        ui,
                        label(StatId::Strafe),
                        &mut tuning.strafe_force,
                        1.0..=80.0,
                    );
                    slider(
                        ui,
                        label(StatId::Torque),
                        &mut tuning.turn_torque,
                        1.0..=40.0,
                    );
                    slider(
                        ui,
                        label(StatId::AngularInertia),
                        &mut tuning.angular_inertia,
                        0.1..=5.0,
                    );
                    slider(
                        ui,
                        label(StatId::LinearDrag),
                        &mut tuning.linear_drag,
                        0.05..=2.0,
                    );
                    slider(
                        ui,
                        label(StatId::AngularDrag),
                        &mut tuning.angular_drag,
                        0.5..=16.0,
                    );
                    slider(
                        ui,
                        label(StatId::TurnShare),
                        &mut tuning.turn_power_share,
                        0.0..=1.0,
                    );
                    slider(ui, label(StatId::Rof), &mut tuning.fire_rate, 0.5..=30.0);
                    slider(
                        ui,
                        label(StatId::Muzzle),
                        &mut tuning.muzzle_speed,
                        20.0..=600.0,
                    );
                    slider(
                        ui,
                        label(StatId::LethalRam),
                        &mut tuning.lethal_ram_speed,
                        5.0..=120.0,
                    );
                });

                egui::CollapsingHeader::new("Mining transport — MiningTuning (live)").show(
                    ui,
                    |ui| {
                        // Newtonian flight: mass + thrust + drag set the emergent cruise speed; turn
                        // torque / drag / inertia set the (ponderous) turn feel.
                        slider(ui, label(StatId::Mass), &mut mining.mass, 0.5..=40.0);
                        slider(
                            ui,
                            label(StatId::Thrust),
                            &mut mining.thrust_force,
                            1.0..=80.0,
                        );
                        slider(
                            ui,
                            label(StatId::LinearDrag),
                            &mut mining.linear_drag,
                            0.05..=2.0,
                        );
                        slider(
                            ui,
                            label(StatId::Torque),
                            &mut mining.turn_torque,
                            0.5..=30.0,
                        );
                        slider(
                            ui,
                            label(StatId::AngularDrag),
                            &mut mining.angular_drag,
                            0.5..=16.0,
                        );
                        slider(
                            ui,
                            label(StatId::AngularInertia),
                            &mut mining.angular_inertia,
                            0.5..=20.0,
                        );
                        // Read-only emergent cruise speed (thrust / drag) — the same relation ships use.
                        stat(
                            ui,
                            "cruise≈",
                            format!("{:.0}", mining.thrust_force / mining.linear_drag.max(1e-3)),
                        );
                        // Arrive / dock geometry.
                        slider(
                            ui,
                            label(StatId::SlowRadius),
                            &mut mining.slow_radius,
                            20.0..=600.0,
                        );
                        slider(
                            ui,
                            label(StatId::ArriveRadius),
                            &mut mining.arrive_radius,
                            5.0..=200.0,
                        );
                        slider(
                            ui,
                            label(StatId::DockSpeed),
                            &mut mining.dock_speed,
                            0.5..=30.0,
                        );
                        // Economy.
                        slider(
                            ui,
                            label(StatId::LoadRate),
                            &mut mining.load_rate,
                            1.0..=200.0,
                        );
                        slider(
                            ui,
                            label(StatId::UnloadRate),
                            &mut mining.unload_rate,
                            1.0..=200.0,
                        );
                        slider(
                            ui,
                            label(StatId::CargoCapacity),
                            &mut mining.cargo_capacity,
                            10.0..=1000.0,
                        );
                    },
                );

                egui::CollapsingHeader::new(
                    "Sim consts — SimTuning (carve / mass / projectile / wreck / ram)",
                )
                .show(ui, |ui| {
                    slider(
                        ui,
                        &format!("{} ⟳", label(StatId::StructCellHp)),
                        &mut sim.struct_cell_hp,
                        0.5..=40.0,
                    );
                    slider(
                        ui,
                        &format!("{} ⟳", label(StatId::StructCellMass)),
                        &mut sim.struct_cell_mass,
                        0.01..=2.0,
                    );
                    slider(
                        ui,
                        label(StatId::CarveFalloff),
                        &mut sim.carve_falloff,
                        0.0..=1.0,
                    );
                    slider(
                        ui,
                        label(StatId::CarvePenCost),
                        &mut sim.carve_pen_cost,
                        0.0..=40.0,
                    );
                    slider(
                        ui,
                        label(StatId::CarveMinCellCost),
                        &mut sim.carve_min_cell_cost,
                        0.0..=10.0,
                    );
                    ui.add(
                        egui::Slider::new(&mut sim.ricochet_min_neighbors, 0..=8)
                            .text(label(StatId::RicochetMinNeighbors)),
                    );
                    ui.add(
                        egui::Slider::new(&mut sim.smooth_normal_radius, 0..=5)
                            .text(label(StatId::SmoothNormalRadius)),
                    );
                    slider(
                        ui,
                        &format!("{} (unfitted)", label(StatId::ProjMass)),
                        &mut sim.projectile_mass,
                        0.001..=1.0,
                    );
                    slider(
                        ui,
                        &format!("{} (unfitted)", label(StatId::ProjDamage)),
                        &mut sim.projectile_damage,
                        1.0..=100.0,
                    );
                    slider(
                        ui,
                        label(StatId::ProjLifetime),
                        &mut sim.projectile_lifetime,
                        0.2..=10.0,
                    );
                    slider(
                        ui,
                        label(StatId::PenPerDamage),
                        &mut sim.pen_per_damage,
                        0.0..=10.0,
                    );
                    slider(ui, label(StatId::PenSize), &mut sim.pen_size, 0.1..=5.0);
                    slider(
                        ui,
                        label(StatId::WreckLifetime),
                        &mut sim.wreck_lifetime_secs,
                        1.0..=300.0,
                    );
                    slider(
                        ui,
                        label(StatId::ShipRamMass),
                        &mut sim.ship_ram_mass,
                        0.1..=20.0,
                    );
                    slider(
                        ui,
                        label(StatId::AsteroidRamMass),
                        &mut sim.asteroid_ram_mass,
                        0.1..=40.0,
                    );
                    // Phase E energy/heat feel (live — no Apply needed; energy_system reads it each tick).
                    slider(
                        ui,
                        label(StatId::EnergyCapacitySecs),
                        &mut sim.energy_capacity_secs,
                        0.5..=20.0,
                    );
                    slider(
                        ui,
                        label(StatId::WeaponEnergyPerDamage),
                        &mut sim.weapon_energy_per_damage,
                        0.0..=5.0,
                    );
                    slider(
                        ui,
                        label(StatId::HeatCapacity),
                        &mut sim.heat_capacity,
                        5.0..=300.0,
                    );
                    slider(
                        ui,
                        label(StatId::HeatDissipation),
                        &mut sim.heat_dissipation,
                        0.0..=60.0,
                    );
                    // Phase F drains + afterburner (live — energy_system/afterburner_system read each tick).
                    slider(
                        ui,
                        label(StatId::ThrustEnergyPerInput),
                        &mut sim.thrust_energy_per_input,
                        0.0..=150.0,
                    );
                    slider(
                        ui,
                        label(StatId::AfterburnerCapacity),
                        &mut sim.afterburner_capacity,
                        10.0..=400.0,
                    );
                    slider(
                        ui,
                        label(StatId::AfterburnerDrainRate),
                        &mut sim.afterburner_drain_rate,
                        1.0..=200.0,
                    );
                    slider(
                        ui,
                        label(StatId::AfterburnerRegenRate),
                        &mut sim.afterburner_regen_rate,
                        1.0..=200.0,
                    );
                    slider(
                        ui,
                        label(StatId::AfterburnerBoostFactor),
                        &mut sim.afterburner_boost_factor,
                        0.0..=3.0,
                    );
                    ui.label("⟳ = needs Apply / Re-derive to update existing ships");
                });

                egui::CollapsingHeader::new("Armor / Penetration").show(ui, |ui| {
                    let mut deg = pen.ricochet_angle.to_degrees();
                    if ui
                        .add(
                            egui::Slider::new(&mut deg, 0.0..=90.0)
                                .text(format!("{} (deg)", label(StatId::RicochetAngle))),
                        )
                        .changed()
                    {
                        pen.ricochet_angle = deg.to_radians();
                    }
                    slider(
                        ui,
                        label(StatId::OvermatchRatio),
                        &mut pen.overmatch_ratio,
                        0.1..=5.0,
                    );
                    slider(
                        ui,
                        label(StatId::EffectiveArmorCap),
                        &mut pen.effective_armor_cap,
                        1.0..=32.0,
                    );
                    slider(
                        ui,
                        label(StatId::PenTierFull),
                        &mut pen.pen_tier_full,
                        0.0..=1.0,
                    );
                    slider(
                        ui,
                        label(StatId::PenTierOver),
                        &mut pen.pen_tier_over,
                        0.0..=1.0,
                    );
                    slider(
                        ui,
                        label(StatId::PenTierNon),
                        &mut pen.pen_tier_non,
                        0.0..=1.0,
                    );
                    if !(pen.pen_tier_non <= pen.pen_tier_over
                        && pen.pen_tier_over <= pen.pen_tier_full)
                    {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "tier order non ≤ over ≤ full violated (INV-D05)",
                        );
                    }
                });

                egui::CollapsingHeader::new("Shield / Salvage / Scaling").show(ui, |ui| {
                    slider(
                        ui,
                        label(StatId::ShieldRegenDefault),
                        &mut shield.shield_regen_default,
                        0.0..=50.0,
                    );
                    slider(
                        ui,
                        label(StatId::UnpoweredDecay),
                        &mut shield.unpowered_decay,
                        0.0..=50.0,
                    );
                    slider(
                        ui,
                        label(StatId::StatHealthFloor),
                        &mut scaling.stat_health_floor,
                        0.0..=0.99,
                    );
                    slider(
                        ui,
                        label(StatId::IntactFraction),
                        &mut salvage.intact_fraction,
                        0.0..=1.0,
                    );
                    slider(
                        ui,
                        label(StatId::ScrapFloor),
                        &mut salvage.scrap_floor,
                        0.1..=20.0,
                    );
                    slider(
                        ui,
                        label(StatId::ScrapPerMass),
                        &mut salvage.scrap_per_mass,
                        0.0..=10.0,
                    );
                });

                if let Some(modules) = modules.as_mut() {
                    egui::CollapsingHeader::new("Module catalog ⟳").show(ui, |ui| {
                        for (id, m) in modules.modules.iter_mut() {
                            egui::CollapsingHeader::new(format!("{:?} {:?}", m.kind, id))
                                .id_salt(("mod", id.0))
                                .show(ui, |ui| {
                                    // Same canonical order + short names as the read-only groups
                                    // (Phase M6d): mass → [thruster thrust/strafe/torque] → power
                                    // gen/draw → cpu → hp → [weapon | shield | armor].
                                    slider(ui, label(StatId::Mass), &mut m.mass, 0.0..=80.0);
                                    if let ModuleSpecifics::Thruster {
                                        thrust_force,
                                        turn_torque,
                                        strafe_force,
                                        .. // Phase C `propulsion` tag — not edited here.
                                    } = &mut m.specifics
                                    {
                                        slider(ui, label(StatId::Thrust), thrust_force, 0.0..=60.0);
                                        slider(ui, label(StatId::Strafe), strafe_force, 0.0..=40.0);
                                        slider(ui, label(StatId::Torque), turn_torque, 0.0..=40.0);
                                    }
                                    slider(
                                        ui,
                                        label(StatId::PowerGen),
                                        &mut m.power_gen,
                                        0.0..=100.0,
                                    );
                                    slider(
                                        ui,
                                        label(StatId::PowerDraw),
                                        &mut m.power_draw,
                                        0.0..=50.0,
                                    );
                                    slider(ui, label(StatId::Cpu), &mut m.cpu_draw, 0.0..=50.0);
                                    slider(ui, label(StatId::Hp), &mut m.health_max, 1.0..=200.0);
                                    match &mut m.specifics {
                                        ModuleSpecifics::Shield { shield_hp, regen } => {
                                            slider(
                                                ui,
                                                label(StatId::ShieldHp),
                                                shield_hp,
                                                0.0..=300.0,
                                            );
                                            slider(
                                                ui,
                                                label(StatId::ShieldRegen),
                                                regen,
                                                0.0..=50.0,
                                            );
                                        }
                                        ModuleSpecifics::Armor { armor_value } => {
                                            slider(
                                                ui,
                                                label(StatId::Armor),
                                                armor_value,
                                                0.0..=300.0,
                                            );
                                        }
                                        ModuleSpecifics::Weapon {
                                            muzzle_speed,
                                            fire_rate,
                                            damage,
                                            projectile_mass,
                                            .. // Phase C class/ammo/damage_type/secondary not edited here yet.
                                        } => {
                                            slider(ui, label(StatId::Dmg), damage, 1.0..=100.0);
                                            slider(ui, label(StatId::Rof), fire_rate, 0.5..=30.0);
                                            slider(
                                                ui,
                                                label(StatId::Muzzle),
                                                muzzle_speed,
                                                20.0..=600.0,
                                            );
                                            slider(
                                                ui,
                                                label(StatId::Slug),
                                                projectile_mass,
                                                0.001..=2.0,
                                            );
                                        }
                                        // Phase C: Sensor range/resolution have no slider yet (no StatId).
                                        ModuleSpecifics::Thruster { .. }
                                        | ModuleSpecifics::Reactor
                                        | ModuleSpecifics::Utility
                                        | ModuleSpecifics::Sensor { .. } => {}
                                    }
                                });
                        }
                    });
                }

                if let Some(hulls) = hulls.as_mut() {
                    egui::CollapsingHeader::new("Hull catalog ⟳").show(ui, |ui| {
                        for (id, h) in hulls.hulls.iter_mut() {
                            egui::CollapsingHeader::new(format!("{} {:?}", h.name, id))
                                .id_salt(("hull", id.0))
                                .show(ui, |ui| {
                                    ui.label(format!(
                                        "{} {:?} (read-only)",
                                        label(StatId::GridDims),
                                        h.grid_dims
                                    ));
                                    slider(
                                        ui,
                                        &format!("{} (budget axis)", label(StatId::BaseMass)),
                                        &mut h.hull_base_mass,
                                        0.1..=120.0,
                                    );
                                    slider(
                                        ui,
                                        label(StatId::PowerCap),
                                        &mut h.power_capacity,
                                        1.0..=200.0,
                                    );
                                    slider(
                                        ui,
                                        label(StatId::CpuCap),
                                        &mut h.cpu_capacity,
                                        1.0..=200.0,
                                    );
                                    slider(
                                        ui,
                                        label(StatId::MassCap),
                                        &mut h.mass_capacity,
                                        1.0..=300.0,
                                    );
                                });
                        }
                    });
                }

                // Refinement 24: live HUD layout (client-side). These sliders mutate the `HudLayout`
                // ResMut directly, so `apply_bar_layout` / `apply_readout_layout` reposition the bars +
                // Energy readout next frame — no Apply needed (drag and watch it move).
                egui::CollapsingHeader::new("HUD layout (client, live)").show(ui, |ui| {
                    ui.label("Bars: camera-local units (x / y / extent).");
                    slider(ui, "Energy x", &mut hud_layout.energy.x_center, -8.0..=8.0);
                    slider(ui, "Energy y", &mut hud_layout.energy.y_base, -8.0..=8.0);
                    slider(
                        ui,
                        "Energy extent",
                        &mut hud_layout.energy.extent,
                        0.5..=8.0,
                    );
                    slider(ui, "Heat x", &mut hud_layout.heat.x_center, -8.0..=8.0);
                    slider(ui, "Heat y", &mut hud_layout.heat.y_base, -8.0..=8.0);
                    slider(ui, "Heat extent", &mut hud_layout.heat.extent, 0.5..=8.0);
                    slider(
                        ui,
                        "Afterburner x",
                        &mut hud_layout.afterburner.x_center,
                        -8.0..=8.0,
                    );
                    slider(
                        ui,
                        "Afterburner y",
                        &mut hud_layout.afterburner.y_base,
                        -8.0..=8.0,
                    );
                    slider(
                        ui,
                        "Afterburner extent",
                        &mut hud_layout.afterburner.extent,
                        0.5..=8.0,
                    );
                    slider(ui, "Shield x", &mut hud_layout.shield.x_center, -8.0..=8.0);
                    slider(ui, "Shield y", &mut hud_layout.shield.y_base, -8.0..=8.0);
                    slider(
                        ui,
                        "Shield extent",
                        &mut hud_layout.shield.extent,
                        0.5..=8.0,
                    );
                    slider(ui, "Armor x", &mut hud_layout.armor.x_center, -8.0..=8.0);
                    slider(ui, "Armor y", &mut hud_layout.armor.y_base, -8.0..=8.0);
                    slider(ui, "Armor extent", &mut hud_layout.armor.extent, 0.5..=8.0);
                    slider(ui, "Hull x", &mut hud_layout.hull.x_center, -8.0..=8.0);
                    slider(ui, "Hull y", &mut hud_layout.hull.y_base, -8.0..=8.0);
                    slider(ui, "Hull extent", &mut hud_layout.hull.extent, 0.5..=8.0);
                    ui.separator();
                    ui.label("Energy readout (viewport % / px).");
                    slider(
                        ui,
                        "readout left %",
                        &mut hud_layout.readout_left_pct,
                        0.0..=100.0,
                    );
                    slider(
                        ui,
                        "readout width %",
                        &mut hud_layout.readout_width_pct,
                        0.0..=100.0,
                    );
                    slider(
                        ui,
                        "readout bottom px",
                        &mut hud_layout.readout_bottom_px,
                        0.0..=400.0,
                    );
                });

                // Refinement 25: live starfield + bloom (client-side). Editing mutates the
                // `StarfieldTuning` ResMut directly → `update_starfield` applies it next frame.
                egui::CollapsingHeader::new("Starfield / Bloom (client, live)").show(ui, |ui| {
                    slider(
                        ui,
                        "bloom intensity",
                        &mut starfield.bloom_intensity,
                        0.0..=1.0,
                    );
                    slider(
                        ui,
                        "star brightness",
                        &mut starfield.star_brightness,
                        0.0..=4.0,
                    );
                    slider(ui, "star density", &mut starfield.star_density, 0.0..=1.0);
                    slider(ui, "twinkle", &mut starfield.twinkle_amount, 0.0..=2.0);
                    slider(ui, "layers (4-16)", &mut starfield.layer_count, 4.0..=16.0);
                });
            });

            // Refinement 24: Apply/Reset PINNED below the scroll area (outside the `ScrollArea`
            // closure) so they stay visible while the sliders scroll.
            ui.separator();
            if ui
                .button("Apply / Re-derive ships (repairs to full health)")
                .clicked()
            {
                rederive = true;
            }
            if ui.button("Reset ALL to defaults").clicked() {
                reset = true;
            }
        });

    // --- write back (mutable borrow) ----------------------------------------------
    let world = host.server.world_mut();
    if reset {
        world.insert_resource(Tuning::default());
        world.insert_resource(SimTuning::default());
        world.insert_resource(PenetrationConfig::default());
        world.insert_resource(ShieldConfig::default());
        world.insert_resource(SalvageConfig::default());
        world.insert_resource(StatScalingConfig::default());
        world.insert_resource(default_resistance_matrix());
        world.insert_resource(MiningTuning::default());
        let (m, h) = seed_catalogs();
        world.insert_resource(m);
        world.insert_resource(h);
        // Refinement 24/25: "Reset ALL" also restores the HUD layout + starfield/bloom defaults.
        *hud_layout = HudLayout::default();
        *starfield = StarfieldTuning::default();
        rederive = true;
    } else {
        world.insert_resource(tuning);
        world.insert_resource(sim);
        world.insert_resource(pen);
        world.insert_resource(shield);
        world.insert_resource(salvage);
        world.insert_resource(scaling);
        world.insert_resource(mining);
        if let Some(m) = modules {
            world.insert_resource(m);
        }
        if let Some(h) = hulls {
            world.insert_resource(h);
        }
    }
    if rederive {
        force_rederive_all(world);
    }
    state.tuning_open = tuning_open;
    state.stats_open = stats_open;
}
