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
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

// T038 (TR-020a) — the per-brain score/transition capture, compiled only when the `ai_debug`
// feature is on (the default `dev_panel` build enables it; see Cargo.toml).
#[cfg(feature = "ai_debug")]
use sim::ai::AiDebugCapture;
use sim::ai::{
    cadence_for_tier, AiBrain, AiStableId, AiTuning, AoiTier, CombatStance, ContactList,
    GlideState, LinkState, MovementProfile, Objective, OrderKind, PlayerOrder, Posture,
    SensorNetworks, Squad, SquadObjective, Tier, WingObjective,
};
use sim::components::{
    Afterburner, ArmorHp, Energy, Faction, Heading, Health, Heat, Position, Velocity,
};
use sim::damage::{
    default_resistance_matrix, HullStructure, PenetrationConfig, ResistanceMatrix, SalvageConfig,
    ShieldConfig, Shields, StatScalingConfig,
};
use sim::fitting::{
    derive_weapon, force_rederive_all, force_rederive_keep_health, seed_catalogs, Fit, FitLayout,
    HullCatalog, ModuleCatalog, ModuleId, ModuleKind, ModuleSpecifics, ShipStats, SlotId,
};
use sim::{CurrentTick, FactionSpawns, FixedDt, MiningTuning, SimTuning, Tuning};

use crate::hud_bars::HudLayout;
use crate::net::{LoopbackHost, NetClientState};
use crate::starfield::{GalaxyTuning, StarfieldTuning, MAX_LAYERS, NUM_CLASSES};
use crate::tuning_io;

// Refinement 28: shared hover-tooltip text for the render-tuning sliders whose labels repeat across
// bars/layers (so the same explanation isn't duplicated per call).
const TIP_BAR_X: &str = "Camera-local X (cross-axis) of this HUD bar — lower = left, higher = right. Bars stay a fixed on-screen size at any zoom.";
const TIP_BAR_Y: &str =
    "Camera-local Y (baseline) of this HUD bar — lower = toward the bottom of the screen.";
const TIP_BAR_EXTENT: &str = "Bar size along its main axis — length for the EHA row bars, height for the SAH stacks. Bigger = longer/taller.";
const TIP_LAYER_PARALLAX: &str = "This layer's depth/parallax: 0 ≈ screen-locked / infinitely far (barely moves), toward 1 = world-anchored / drifts fast as you fly. Spread layers across 0..~0.5 for depth.";
const TIP_LAYER_FREQUENCY: &str = "This layer's cell frequency (cells per world unit) = star SPACING. Higher = denser/closer-packed stars. (Star pixel SIZE is the 'size' knob.)";
const TIP_LAYER_DENSITY: &str = "This layer's star density (0..1) = fraction of candidate cells that host a star, × the cellular clustering map (and, in spectral mode, the galactic-core boost).";
const TIP_LAYER_BRIGHTNESS: &str = "This layer's brightness — a DEPTH multiplier on each star's class brightness (dim far layers); brighter stars bloom (HDR).";
const TIP_LAYER_SIZE: &str = "This layer's size — a DEPTH multiplier on each star's class pixel-radius. Bigger = chunkier stars on this layer. (Star SPACING is the 'frequency' knob.)";
const TIP_LAYER_TINT: &str = "OPTIONAL per-layer color TINT hue — blended on top of the star's class color by 'tint strength' below (0/off by default, so this swatch does nothing until you raise it). Pushes a whole depth plane toward a hue.";
const TIP_LAYER_TINT_STRENGTH: &str = "How strongly the layer 'tint' applies — a SECONDARY, off-by-default effect. 0 = off (pure class color); partial = a subtle hue push; 1 = full tint multiply. For overall dimming use 'brightness', not this.";

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
    // R42 — weapon real-spec authoring inputs + a derived projectile-radius readout.
    CaliberMm,
    MuzzleVelocityMs,
    Rpm,
    SpinUp,
    DispersionDeg,
    RangeUnits,
    ProjRadius,
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
    // R42 — global ballistic weapon-physics scales (real specs → game space).
    MmToWorld,
    VelocityScale,
    RpmScale,
    ProjDensity,
    DamagePerJoule,
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
            Muzzle => ("muzzle", "Muzzle speed (derived)", "muzzle_speed", 0, ""),
            Slug => (
                "slug",
                "Projectile mass (derived)",
                "projectile_mass",
                4,
                "",
            ),
            // R42 weapon real-spec authoring + derived readout.
            CaliberMm => ("caliber", "Bore caliber", "caliber_mm", 2, " mm"),
            MuzzleVelocityMs => (
                "muzzle m/s",
                "Muzzle velocity (real)",
                "muzzle_velocity_ms",
                0,
                " m/s",
            ),
            Rpm => ("rpm", "Rounds per minute", "rpm", 0, " rpm"),
            SpinUp => ("spin-up", "Rotary spool-up time", "spin_up_time", 2, " s"),
            DispersionDeg => (
                "dispersion",
                "Shot dispersion half-angle",
                "dispersion_deg",
                2,
                "°",
            ),
            RangeUnits => ("range", "Projectile travel range", "range_units", 0, " u"),
            ProjRadius => (
                "proj radius",
                "Projectile radius (derived)",
                "projectile_radius",
                3,
                "",
            ),
            // R42 global weapon-physics scales.
            MmToWorld => (
                "mm→world",
                "Projectile radius per mm caliber",
                "mm_to_world",
                5,
                "",
            ),
            VelocityScale => (
                "vel scale",
                "Real m/s → game speed",
                "velocity_scale",
                3,
                "",
            ),
            RpmScale => ("rpm scale", "RPM → shots/s", "rpm_scale", 5, ""),
            ProjDensity => (
                "proj density",
                "Slug mass per mm³ caliber",
                "projectile_density",
                8,
                "",
            ),
            DamagePerJoule => (
                "dmg/joule",
                "Damage per joule of muzzle KE",
                "damage_per_joule",
                5,
                "",
            ),
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

    /// Every variant, for the label→desc reverse lookup in [`desc_for_label`] (R28). Keep in sync
    /// with the enum — a missing entry only drops that stat's slider auto-tooltip (read-only rows
    /// look up `desc()` directly, so they're unaffected).
    const ALL: &'static [StatId] = {
        use StatId::*;
        &[
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
            CaliberMm,
            MuzzleVelocityMs,
            Rpm,
            SpinUp,
            DispersionDeg,
            RangeUnits,
            ProjRadius,
            LethalRam,
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
            MmToWorld,
            VelocityScale,
            RpmScale,
            ProjDensity,
            DamagePerJoule,
            PenPerDamage,
            PenSize,
            WreckLifetime,
            ShipRamMass,
            AsteroidRamMass,
            EnergyCapacitySecs,
            WeaponEnergyPerDamage,
            HeatCapacity,
            HeatDissipation,
            ThrustEnergyPerInput,
            AfterburnerCapacity,
            AfterburnerDrainRate,
            AfterburnerRegenRate,
            AfterburnerBoostFactor,
            SlowRadius,
            ArriveRadius,
            DockSpeed,
            LoadRate,
            UnloadRate,
            CargoCapacity,
            BaseMass,
            PowerCap,
            CpuCap,
            MassCap,
            GridDims,
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
        ]
    };

    /// Rich hover-tooltip text for this stat (Refinement 28) — what it is, how it works, and how it
    /// differs from related knobs. Shown on hover for both the tuning slider and the read-only row.
    fn desc(self) -> &'static str {
        use StatId::*;
        match self {
            // --- Ship locomotion / power ---
            Mass => "Total ship mass (hull + modules). Higher mass = more inertia: slower acceleration and turning for the same thrust/torque, and harder to shove in collisions. It's the divisor in accel = force / mass.",
            Thrust => "Forward thrust force. Acceleration = thrust / mass; emergent top speed ≈ thrust / linear-drag. Raise for snappier, faster flight.",
            Reverse => "Reverse thrust force (braking / backing up) — usually weaker than forward thrust.",
            Strafe => "Lateral thrust force for sliding sideways without turning.",
            Torque => "Turn torque. Angular accel = torque / angular-inertia; steady turn rate ≈ torque / angular-drag. Higher = sharper turning.",
            TopSpeed => "Read-only: emergent top speed (≈ thrust / linear-drag). Not set directly — change thrust or linear drag.",
            TurnRate => "Read-only: max steady turn rate (≈ torque / angular-drag), in rad/s.",
            AngularInertia => "Rotational inertia — resistance to changing spin. Higher = slower to start/stop a turn (heavier feel); the steady turn rate is unchanged (that's torque vs angular-drag).",
            LinearDrag => "Linear drag. Velocity decays toward thrust/drag, so higher drag = lower top speed AND a faster stop.",
            AngularDrag => "Angular drag. Spin decays toward torque/drag, so higher = lower top turn rate AND a quicker settle.",
            TurnShare => "Share of the control budget given to turning vs forward thrust (0..1).",
            PowerGen => "Power generated by working, core-connected reactor cells (health-scaled). Drives runtime energy regen and the shield 'powered' threshold; 0 if no working reactor.",
            PowerDraw => "Continuous power draw of active modules. If draw exceeds generation, energy drains and shields lose power.",
            Cpu => "CPU load of the fitted modules (compared against the hull's CPU capacity — a fitting budget).",
            // --- Durability / weapon ---
            Hp => "Flat health max for UNFITTED bodies. Fitted ships ignore this and use the cell / armor / shield layers instead.",
            ShieldHp => "Shield capacity from the shield generator (health-scaled). 0 with no working generator.",
            ShieldRegen => "Shield regeneration per second — only while the ship is powered (generation ≥ draw).",
            Armor => "Armor-HP from armor modules. Soaks hits before the hull carves; a single hit larger than the remaining armor spills its excess into the carve.",
            Dmg => "Damage per projectile hit.",
            Rof => "Weapon rate of fire (shots per second).",
            Muzzle => "Projectile muzzle speed. Higher = flatter trajectory, less lead needed, and less render lag.",
            Slug => "Projectile mass (derived from caliber³) — affects momentum / knockback and recoil.",
            CaliberMm => "R42: bore caliber (mm). Drives projectile size (visual + collision) and the caliber³ slug mass; with muzzle velocity it sets kinetic energy → damage.",
            MuzzleVelocityMs => "R42: real muzzle velocity (m/s), scaled to game muzzle speed by the velocity-scale.",
            Rpm => "R42: rounds per minute, scaled to shots/second by the rpm-scale (a rotary gun also spools up).",
            SpinUp => "R42: rotary spool-up time (s) to reach full RPM while firing; 0 = instant (non-rotary). Vulcan/gatling wind-up.",
            DispersionDeg => "R42: shot dispersion half-angle (degrees) — a cone of fire; 0 = pinpoint. Deterministic per-shot scatter, no RNG.",
            RangeUnits => "R42: projectile travel range in game units (lifetime = range / muzzle speed).",
            ProjRadius => "Read-only: the derived projectile radius (caliber × mm→world).",
            LethalRam => "Closing speed at or above which a ram is a one-shot kill (scenario ram tuning).",
            // --- Defense / penetration ---
            RicochetAngle => "Glancing-hit threshold (degrees): shots striking the surface steeper than this bounce off instead of penetrating.",
            OvermatchRatio => "Damage-to-armor ratio above which a hit overmatches the armor and defeats it outright.",
            EffectiveArmorCap => "Cap on the effective armor thickness any single hit can be stopped by (limits how much armor one shot 'sees').",
            PenTierFull => "Penetration-vs-resistance threshold for a FULL penetration (the shot punches in and carves cells).",
            PenTierOver => "Threshold for an OVER-penetration (the shot passes through with reduced effect).",
            PenTierNon => "Threshold below which a hit is a NON-penetration (no carve — absorbed or ricocheted).",
            ShieldRegenDefault => "Fallback shield regen for ships that don't specify a generator regen value.",
            UnpoweredDecay => "Rate at which shields decay while the ship is unpowered (generation < draw).",
            StatHealthFloor => "Minimum health-scale a damaged module keeps, so a nearly-destroyed module still contributes a little to derived stats.",
            IntactFraction => "Live-cell fraction above which a hull still counts as 'intact' (vs visibly damaged) for stat/visual purposes.",
            ScrapFloor => "Minimum scrap/salvage a wreck yields regardless of its mass.",
            ScrapPerMass => "Scrap/salvage yielded per unit of wreck mass.",
            // --- Carve / structural / projectile / wreck / ram ---
            StructCellHp => "Per-cell hit points of voxelized structures (asteroid / outpost / transport). Higher = tougher to dig through.",
            StructCellMass => "Per-cell mass of voxelized structures — feeds momentum and ram force.",
            CarveFalloff => "How quickly carve damage falls off along a shot's channel (higher = shallower craters).",
            CarvePenCost => "Penetration spent to carve each cell along the channel (higher = shots stop sooner / dig less deep).",
            CarveMinCellCost => "Floor on the per-cell penetration cost, so each carved cell always costs at least this much.",
            RicochetMinNeighbors => "Minimum solid neighbours a surface cell needs for a glancing hit to ricochet off it (rather than bite in).",
            SmoothNormalRadius => "Cell radius sampled to estimate a smoothed surface normal for the ricochet-angle test.",
            ProjMass => "Default projectile mass (the unfitted-weapon path).",
            ProjDamage => "Default projectile damage (the unfitted-weapon path).",
            ProjLifetime => "Seconds a projectile lives before despawning (the unfitted gun; fitted weapons derive lifetime from their range).",
            MmToWorld => "R42 scale: projectile radius per mm of caliber (visual + collision size).",
            VelocityScale => "R42 scale: real m/s → game muzzle speed (≈0.2 keeps real proportions at arcade scale).",
            RpmScale => "R42 scale: RPM → shots/second (1/60 = the literal real rate; lower to tame projectile spam).",
            ProjDensity => "R42 scale: slug mass per mm³ of caliber (mass = density × caliber³). Drives recoil/knockback + KE damage.",
            DamagePerJoule => "R42 scale: damage per joule of muzzle kinetic energy (½ · mass · velocity²).",
            PenPerDamage => "Penetration gained per point of damage — how deep a shot carves for its damage.",
            PenSize => "Projectile size factor used in the penetration calculation.",
            WreckLifetime => "Seconds a wreck / debris chunk drifts before it fades and despawns.",
            ShipRamMass => "Effective ram mass of ships — governs collision / ram momentum (who shoves whom).",
            AsteroidRamMass => "Effective ram mass of asteroids / structures — how hard they shove (and resist being shoved).",
            // --- Energy / heat ---
            EnergyCapacitySecs => "Energy capacitor size, expressed as seconds of full draw it can sustain.",
            WeaponEnergyPerDamage => "Energy consumed per point of weapon damage fired.",
            HeatCapacity => "Heat the ship can hold before overheating (firing adds heat).",
            HeatDissipation => "Heat shed per second (cooling rate).",
            // --- Afterburner / thrust energy ---
            ThrustEnergyPerInput => "Energy drained per unit of thrust input — flying itself costs energy.",
            AfterburnerCapacity => "Size of the afterburner / boost pool.",
            AfterburnerDrainRate => "Boost pool drained per second while boosting.",
            AfterburnerRegenRate => "Boost pool refilled per second when not boosting.",
            AfterburnerBoostFactor => "Thrust multiplier while boosting (e.g. 1.5 = +50% thrust).",
            // --- Mining transport ---
            SlowRadius => "Distance from its dock where the transport starts throttling down to arrive smoothly.",
            ArriveRadius => "Distance within which the transport counts as 'arrived' at a dock.",
            DockSpeed => "Speed below which (inside the arrive radius) the transport counts as docked and begins loading/unloading.",
            LoadRate => "Cargo loaded per second at the asteroid.",
            UnloadRate => "Cargo unloaded per second at the outpost — this is what grows the faction's refined-resources total.",
            CargoCapacity => "Maximum cargo the transport carries per run.",
            // --- Hull capacities (fitting budgets) ---
            BaseMass => "The hull's own base mass, before any modules.",
            PowerCap => "Hull power capacity — the fitting budget for module power draw.",
            CpuCap => "Hull CPU capacity — the fitting budget for module CPU load.",
            MassCap => "Hull mass capacity — the fitting budget for total module mass.",
            GridDims => "Hull grid dimensions (cols × rows) — the fitting / voxel footprint.",
            // --- Runtime telemetry (read-only) ---
            Speed => "Read-only: current speed (velocity magnitude).",
            Heading => "Read-only: current facing, in degrees.",
            Health => "Read-only: flat health (unfitted bodies); shows '—' for fitted ships, which use the cell/armor/shield layers.",
            HullStruct => "Read-only: hull structure pool (current / max).",
            ShieldsState => "Read-only: live shields (current / max).",
            ArmorState => "Read-only: live armor-HP (current / max).",
            Energy => "Read-only: live energy capacitor (current / max).",
            Heat => "Read-only: live heat (current / max); full = overheated.",
            AfterburnerState => "Read-only: live afterburner / boost pool (current / max).",
            Cells => "Read-only: live structural cell count — drops as the hull is carved apart.",
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
        // R28: each read-only readout gets the same hover tooltip as its slider.
        stat(ui, label(*id), v).on_hover_text(id.desc());
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
    // R92 — thrusters author ONE jet force; turn/strafe are placement-derived (no nominal sums).
    thrust: f32,
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
            .. // Phase C `propulsion` tag — not surfaced in the readout.
        } = &m.specifics
        {
            // R92 — a thruster authors ONE jet force; turn/strafe now come from placement+facing.
            t.thrust += *thrust_force;
            stats.push((StatId::Thrust, fmt(StatId::Thrust, *thrust_force)));
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
            ModuleSpecifics::Weapon { .. } => {
                // R42: the weapon's game stats are PHYSICS-DERIVED from its real specs. Show the
                // derived values (at default scales — the live re-derive uses the live `SimTuning`).
                if let Some(d) = derive_weapon(&m.specifics, &SimTuning::default()) {
                    t.weapon_damage += d.damage;
                    stats.push((StatId::Dmg, fmt(StatId::Dmg, d.damage)));
                    stats.push((StatId::Rof, fmt(StatId::Rof, d.fire_rate)));
                    stats.push((StatId::Muzzle, fmt(StatId::Muzzle, d.muzzle_speed)));
                    stats.push((StatId::Slug, fmt(StatId::Slug, d.projectile_mass)));
                    stats.push((
                        StatId::ProjRadius,
                        fmt(StatId::ProjRadius, d.projectile_radius),
                    ));
                }
            }
            // Phase C: Sensor shows only its common cost rows (range/resolution have no StatId yet).
            // R92 — EnergyStore/CargoBay likewise show only the common rows here (their one stat is
            // edited in the Module Designs section + reflected in the derived ShipStats).
            ModuleSpecifics::Thruster { .. }
            | ModuleSpecifics::Reactor
            | ModuleSpecifics::Utility
            | ModuleSpecifics::Sensor { .. }
            | ModuleSpecifics::EnergyStore { .. }
            | ModuleSpecifics::CargoBay { .. }
            | ModuleSpecifics::Cockpit
            | ModuleSpecifics::FlightComputer { .. }
            | ModuleSpecifics::ReactionWheel { .. }
            | ModuleSpecifics::ControlRelay => {}
        }
        rows.push(EquipmentRow {
            slot: slot.0,
            label: format!("{:?} {:?}", m.kind, mid),
            stats,
        });
    }
    (rows, t)
}

// ---------------------------------------------------------------------------
// T038/T039 (TR-020) — AI inspection + runtime metrics snapshot
// ---------------------------------------------------------------------------

/// One row of the AI-ship list (T038): every `AiBrain` carrier, sorted by distance to the player
/// ship. Owned/pre-formatted so the egui closure holds no `host` borrow.
struct AiShipRow {
    entity: Entity,
    /// Sim-stable spawn-order id (the selection key); `None` for a brain spawned outside the
    /// allocator path (test-style worlds).
    stable_id: Option<u64>,
    /// Distance to the player ship; `INFINITY` when either position is missing.
    dist: f32,
    behavior: String,
}

/// One `ContactList` entry of the inspected ship, pre-formatted (T038).
struct AiContactRow {
    target: String,
    /// Distance from the inspected ship to the contact's last-known position.
    dist: f32,
    last_seen_tick: u64,
    signature: f32,
}

/// Squad/wing membership of the inspected ship (T038): the squad brain's order decision + lifecycle
/// state (TR-020a "squad-driven ships show the squad/wing brain's order decision; dormant
/// aggregates show tier + glide state").
struct AiSquadInfo {
    squad: String,
    order: String,
    members: usize,
    pace_anchor: Option<String>,
    anchor_speed: f32,
    wing: Option<String>,
    gliding: bool,
    last_think_tick: u64,
    /// R97 Phase 2 — the squad's STRATEGIC objective, if it carries a
    /// [`SquadObjective`]: `"{goal:?} (plan_idx {i}, planned @{tick})"`. `None`
    /// when the squad has no strategic tier (the planner skips it).
    objective: Option<String>,
    /// R97 Phase 2 — the parent wing's STRATEGIC objective, if the squad has a
    /// `wing` AND that wing entity carries a [`WingObjective`]: `"{goal:?}"`.
    wing_objective: Option<String>,
}

/// The last-think score breakdown (T038, TR-020a) — a render copy of [`AiDebugCapture`], captured
/// at the last ACTUAL think by `ai_think_system` (never a live recompute).
#[cfg(feature = "ai_debug")]
struct AiCaptureView {
    /// Final per-candidate scores `select_behavior` compared (momentum included on the incumbent).
    last_scores: Vec<(String, f32)>,
    winner: String,
    momentum_applied: f32,
    /// Bounded transition ring, newest first: `(tick, from, to)`.
    transitions: Vec<(u64, String, String)>,
    /// R97 Stage D — the per-channel decision of the LAST executed tick (written
    /// every tick, not on the think cadence), pre-formatted into one line: which
    /// way each channel drove + WHY fire did/didn't happen.
    channels: String,
}

/// Full detail of the selected AI ship (T038). All strings pre-formatted in the read phase.
struct AiShipDetail {
    entity: Entity,
    stable_id: Option<u64>,
    behavior: String,
    archetype: String,
    /// R96 — the RESOLVED movement profile this brain executes (`brain.movement_profile`).
    movement_profile: String,
    /// R96 — the RESOLVED combat stance this brain executes (`brain.combat_stance`).
    combat_stance: String,
    /// `"Active (since @123)"` — or the untiered note (absent `AoiTier` = treated Active).
    tier: String,
    throttle_cap: f32,
    last_think_tick: u64,
    commit_until_tick: u64,
    /// R97 — the tick this brain was last fired upon (the threat-recency stamp
    /// Stage B/C reads against `AiTuning::threat_recency_window_ticks`).
    last_damaged_tick: u64,
    thinks_total: u64,
    target: Option<String>,
    /// R99 Phase B — the selected ship's live [`PlayerOrder`], pre-formatted (kind +
    /// the 3 style fields), or `"none"` when the component is absent. Surfaced as a
    /// read-only inspection line + read back into the command controls.
    player_order: String,
    /// R99 Phase B — the raw style overrides from the ship's `PlayerOrder` (all `None`
    /// when the component is absent), so the B4 dropdowns can show the live selection.
    order_profile: Option<MovementProfile>,
    order_stance: Option<CombatStance>,
    order_posture: Option<Posture>,
    contacts_total: usize,
    last_scan_tick: u64,
    /// Nearest few contacts (by distance to the ship).
    contacts: Vec<AiContactRow>,
    /// `Some((jammed, severed))` when a `LinkState` is present; `None` = linked (no component).
    link: Option<(bool, bool)>,
    /// `(faction_key, component_index, member_count, fused_count)` of the sensor-network component
    /// containing this ship, if any.
    network: Option<(u8, usize, usize, usize)>,
    squad: Option<AiSquadInfo>,
    /// Active degraded-state cause (mirrors `ai_execute_system`'s guards).
    degraded: &'static str,
    #[cfg(feature = "ai_debug")]
    capture: Option<AiCaptureView>,
}

/// Runtime AI metrics (T039, TR-020c) — cheap whole-world counts gathered each panel frame.
#[derive(Default)]
struct AiMetrics {
    now: u64,
    /// Fixed-step rate (Hz) for tick→seconds rate conversion.
    tick_hz: f32,
    brains: usize,
    tier_active: usize,
    tier_mid: usize,
    tier_dormant: usize,
    /// Σ `AiBrain::thinks_total` over all brains.
    thinks_total: u64,
    /// Brains whose `last_think_tick == now` (thought THIS tick — a sampled signal).
    thinks_this_tick: usize,
    /// Of those, thinks landing ON their fallback-cadence slot
    /// (`(now + phase_bucket) % cadence_for_tier(think_tier) == 0` — the scheduler's rule).
    thinks_cadence: usize,
    /// Thinks OFF the cadence slot — only an event could have triggered them.
    thinks_event: usize,
    squads: usize,
    /// Squads currently collapsed to a cheap-glide aggregate (`GlideState` present).
    gliding_aggregates: usize,
    /// R97 Phase 2 — squads carrying a `SquadObjective` (the strategic tier).
    squad_objectives: usize,
    /// Breakdown of those by objective kind: Hold / DefendZone / DestroyTarget /
    /// PatrolRoute / Withdraw / Regroup. Withdraw+Regroup are reported together as
    /// "withdrawing/regrouping" (the disengage states).
    obj_holding: usize,
    obj_defending: usize,
    obj_destroying: usize,
    obj_patrolling: usize,
    obj_withdrawing: usize,
    /// STF-001 live signal: EXPANDED squads (no `GlideState`) farther from the player than
    /// `AiTuning::aoi_radius_mid` — off-screen promoted battles.
    offscreen_battles: usize,
    /// `AoiTier` carriers (ships + squads) whose `since_tick == now` — promotions + demotions
    /// landing this tick (the deterministic transition proxy).
    tier_transitions_this_tick: usize,
    /// Σ `ContactList` lengths (per-ship perception memory).
    contact_sum: usize,
    /// Ships whose own scan ran this tick (`ContactList::last_scan_tick == now`).
    scans_this_tick: usize,
    /// Sensor-network connected components across all factions.
    network_components: usize,
    /// Σ fused-picture contacts over all network components.
    fused_total: usize,
}

/// Everything the AI panel sections render this frame.
struct AiPanelData {
    rows: Vec<AiShipRow>,
    detail: Option<AiShipDetail>,
    metrics: AiMetrics,
}

/// R99 Phase B — pre-format a ship's [`PlayerOrder`] (kind + the 3 style overrides)
/// into one inspection line, or `"none"` when the component is absent (= no order).
/// `Vec2` has no `Display`, so the nav points are formatted component-wise.
fn format_player_order(order: Option<&PlayerOrder>) -> String {
    let Some(o) = order else {
        return "none".to_string();
    };
    let kind = match &o.kind {
        None => "settings-only".to_string(),
        Some(OrderKind::MoveTo(p)) => format!("MoveTo({:.0}, {:.0})", p.x, p.y),
        Some(OrderKind::HoldAt { anchor, radius }) => {
            format!("HoldAt(({:.0}, {:.0}) r{:.0})", anchor.x, anchor.y, radius)
        }
        Some(OrderKind::Attack(t)) => format!("Attack({t:?})"),
        Some(OrderKind::Patrol { points, index }) => {
            format!("Patrol({} pts @{index})", points.len())
        }
    };
    let style = |label: &str, s: Option<String>| match s {
        Some(v) => format!(" | {label}: {v}"),
        None => format!(" | {label}: (inherit)"),
    };
    format!(
        "{kind}{}{}{}",
        style("profile", o.profile.map(|p| format!("{p:?}"))),
        style("stance", o.stance.map(|s| format!("{s:?}"))),
        style("posture", o.posture.map(|p| format!("{p:?}"))),
    )
}

/// R99 Phase B — a command captured in the UI (read/draw) phase and applied in the
/// write-back phase (where `host.server.world_mut()` is mutable), mirroring the R43
/// `equip_weapon` deferral. Each variant carries the SERVER ship entity it targets.
enum CommandRequest {
    /// "Hold here" — `PlayerOrder::hold_at(ship_pos, radius)` (read the ship's pos at apply).
    HoldHere(Entity),
    /// "Clear command" — REMOVE the `PlayerOrder` so the ship reverts to scenario behaviour.
    Clear(Entity),
    /// Merge a movement-profile override (`None` = `(inherit)`).
    SetProfile(Entity, Option<MovementProfile>),
    /// Merge a combat-stance override (`None` = `(inherit)`).
    SetStance(Entity, Option<CombatStance>),
    /// Merge a posture override (`None` = `(inherit)`).
    SetPosture(Entity, Option<Posture>),
}

/// R99 Phase B (B5) — a live Team-join request captured in the UI phase: re-faction
/// + reposition the player ship to `faction`'s base, mirroring `net.rs` auto-join.
struct TeamJoinRequest {
    faction: Faction,
}

/// The hold radius (world units) a "Hold here" command guards around the ship's
/// current position — a small default, tunable for feel.
const HOLD_RADIUS: f32 = 30.0;

/// R99 Phase B — the combat-stance dropdown choices. `Orbit` carries a `ccw: bool`,
/// so it is split into two concrete picks; the rest are field-less. `None` =
/// `(inherit)`. The fixed list drives both the label and the merged override.
fn stance_choices() -> [(&'static str, Option<CombatStance>); 6] {
    [
        ("(inherit)", None),
        ("Charge", Some(CombatStance::Charge)),
        ("Orbit CCW", Some(CombatStance::Orbit { ccw: true })),
        ("Orbit CW", Some(CombatStance::Orbit { ccw: false })),
        ("Standoff", Some(CombatStance::Standoff)),
        ("Kite", Some(CombatStance::Kite)),
    ]
}

/// R99 Phase B — the movement-profile dropdown choices (`None` = `(inherit)`).
fn profile_choices() -> [(&'static str, Option<MovementProfile>); 4] {
    [
        ("(inherit)", None),
        ("Rush", Some(MovementProfile::Rush)),
        ("Cruise", Some(MovementProfile::Cruise)),
        ("Leisurely", Some(MovementProfile::Leisurely)),
    ]
}

/// R99 Phase B — the posture dropdown choices (`None` = `(inherit)`).
fn posture_choices() -> [(&'static str, Option<Posture>); 4] {
    [
        ("(inherit)", None),
        ("FreeEngage", Some(Posture::FreeEngage)),
        ("DefensiveOnly", Some(Posture::DefensiveOnly)),
        ("HoldFire", Some(Posture::HoldFire)),
    ]
}

/// The label for a currently-selected dropdown value, found by matching against the
/// choice list (so the closed combo shows the live override). Falls back to the
/// `(inherit)` head when nothing matches.
fn choice_label<T: PartialEq + Copy>(
    choices: &[(&'static str, Option<T>)],
    cur: Option<T>,
) -> &'static str {
    choices
        .iter()
        .find(|(_, v)| *v == cur)
        .map(|(l, _)| *l)
        .unwrap_or(choices[0].0)
}

/// Gather the T038 inspection snapshot + T039 metrics from the embedded server world.
///
/// **Strictly read-only on sim state (TR-020)**: the `&mut World` is required only to construct
/// `QueryState`s (Bevy registers query/archetype metadata — no component, resource, or tick is
/// written), so viewing never triggers thinks or perturbs decision order/determinism.
///
/// `selected` is the panel's stable-id override text: blank/unparseable → the ship nearest the
/// player (the TR-020a default).
fn gather_ai(
    world: &mut World,
    player: Option<Entity>,
    selected: &str,
    tuning: &AiTuning,
) -> AiPanelData {
    let now = world.get_resource::<CurrentTick>().map_or(0, |t| t.0);
    let tick_hz =
        world
            .get_resource::<FixedDt>()
            .map_or(30.0, |d| if d.0 > 0.0 { 1.0 / d.0 } else { 30.0 });
    let player_pos = player.and_then(|e| world.get::<Position>(e)).map(|p| p.0);

    let mut metrics = AiMetrics {
        now,
        tick_hz,
        ..AiMetrics::default()
    };

    // Pass 1 — every AiBrain carrier: the ship list + the per-brain metrics.
    let mut rows: Vec<AiShipRow> = Vec::new();
    let mut ships = world.query::<(
        Entity,
        &AiBrain,
        Option<&AiStableId>,
        Option<&AoiTier>,
        Option<&Position>,
    )>();
    for (entity, brain, sid, aoi, pos) in ships.iter(world) {
        metrics.brains += 1;
        metrics.thinks_total += brain.thinks_total;
        // Absent AoiTier = treated Active (the documented ai_execute/ai_think rule).
        match aoi.map_or(Tier::Active, |a| a.tier) {
            Tier::Active => metrics.tier_active += 1,
            Tier::Mid => metrics.tier_mid += 1,
            Tier::Dormant => metrics.tier_dormant += 1,
        }
        if brain.thinks_total > 0 && brain.last_think_tick == now {
            metrics.thinks_this_tick += 1;
            // Event-vs-cadence proxy: re-run the scheduler's fallback test for this tick. A think
            // landing OFF its cadence slot can only have been event-triggered; an ON-slot think is
            // counted as cadence (a coalesced same-tick event is indistinguishable — acceptable for
            // a readout).
            let cadence = cadence_for_tier(brain.think_tier, tuning);
            if (now + u64::from(brain.phase_bucket)).is_multiple_of(cadence) {
                metrics.thinks_cadence += 1;
            } else {
                metrics.thinks_event += 1;
            }
        }
        let dist = match (player_pos, pos) {
            (Some(pp), Some(p)) => (p.0 - pp).length(),
            _ => f32::INFINITY,
        };
        rows.push(AiShipRow {
            entity,
            stable_id: sid.map(|s| s.0),
            dist,
            behavior: format!("{:?}", brain.behavior),
        });
    }
    // Nearest-first; stable-id tiebreak so equal-distance (or player-less) lists don't jitter.
    rows.sort_by(|a, b| {
        a.dist
            .total_cmp(&b.dist)
            .then(a.stable_id.cmp(&b.stable_id))
    });

    // Selection: a parseable stable-id override pins that ship; else nearest (TR-020a default).
    let chosen: Option<Entity> = match selected.trim().parse::<u64>() {
        Ok(id) => rows
            .iter()
            .find(|r| r.stable_id == Some(id))
            .or(rows.first())
            .map(|r| r.entity),
        Err(_) => rows.first().map(|r| r.entity),
    };

    // Pass 2 — AoiTier carriers (ships AND squads): transitions landing this tick.
    let mut tiers = world.query::<&AoiTier>();
    metrics.tier_transitions_this_tick = tiers
        .iter(world)
        .filter(|a| now > 0 && a.since_tick == now)
        .count();

    // Pass 3 — squads: counts, glide aggregates, off-screen battles, the R97 strategic-objective
    // tally + the selected ship's squad. `SquadObjective` is the strategic tier over `Squad`.
    let mut squad_info: Option<AiSquadInfo> = None;
    // The chosen squad's parent wing, if any — read its `WingObjective` AFTER the squad query
    // releases its `world` borrow (the loop can't `world.get` while iterating).
    let mut chosen_wing: Option<Entity> = None;
    let mut squads = world.query::<(
        Entity,
        &Squad,
        Option<&GlideState>,
        Option<&Position>,
        Option<&SquadObjective>,
    )>();
    for (se, squad, glide, spos, objective) in squads.iter(world) {
        metrics.squads += 1;
        if glide.is_some() {
            metrics.gliding_aggregates += 1;
        } else if let (Some(pp), Some(sp)) = (player_pos, spos) {
            // STF-001: an EXPANDED (non-gliding) squad beyond the Mid radius is a promoted
            // off-screen battle running full squad AI.
            if (sp.0 - pp).length() > tuning.aoi_radius_mid {
                metrics.offscreen_battles += 1;
            }
        }
        // R97 Phase 2 — cheap strategic tally: one match on the objective kind per squad.
        if let Some(o) = objective {
            metrics.squad_objectives += 1;
            match o.goal {
                Objective::Hold => metrics.obj_holding += 1,
                Objective::DefendZone { .. } => metrics.obj_defending += 1,
                Objective::DestroyTarget(_) => metrics.obj_destroying += 1,
                Objective::PatrolRoute(_) => metrics.obj_patrolling += 1,
                // Withdraw + Regroup are the disengage states — reported together.
                Objective::Withdraw(_) | Objective::Regroup { .. } => metrics.obj_withdrawing += 1,
            }
        }
        if chosen.is_some_and(|c| squad.members.contains(&c)) {
            chosen_wing = squad.wing;
            squad_info = Some(AiSquadInfo {
                squad: format!("{se:?}"),
                order: format!("{:?}", squad.order),
                members: squad.members.len(),
                pace_anchor: squad.pace_anchor.map(|a| format!("{a:?}")),
                anchor_speed: squad.anchor_speed,
                wing: squad.wing.map(|w| format!("{w:?}")),
                gliding: glide.is_some(),
                last_think_tick: squad.last_think_tick,
                // R97 Phase 2 — the squad's strategic objective line (goal + plan cursor +
                // last-plan tick); `None` when the squad has no strategic tier.
                objective: objective.map(|o| {
                    format!(
                        "{:?} (plan_idx {}, planned @{})",
                        o.goal, o.plan_index, o.last_plan_tick
                    )
                }),
                // Filled in below from the parent wing's `WingObjective` (read after the query).
                wing_objective: None,
            });
        }
    }
    // R97 Phase 2 — the parent wing's strategic objective (read after the squad query released
    // its `world` borrow). Only shown when the squad has a wing AND that wing carries one.
    if let (Some(info), Some(wing)) = (squad_info.as_mut(), chosen_wing) {
        info.wing_objective = world
            .get::<WingObjective>(wing)
            .map(|w| format!("{:?} (planned @{})", w.goal, w.last_plan_tick));
    }

    // Pass 4 — perception: contact memory + own-scan counts.
    let mut contact_lists = world.query::<&ContactList>();
    for cl in contact_lists.iter(world) {
        metrics.contact_sum += cl.contacts.len();
        if now > 0 && cl.last_scan_tick == now {
            metrics.scans_this_tick += 1;
        }
    }

    // Pass 5 — sensor networks: component + fused totals.
    if let Some(nets) = world.get_resource::<SensorNetworks>() {
        for comps in nets.by_faction.values() {
            metrics.network_components += comps.len();
            metrics.fused_total += comps.iter().map(|c| c.fused.len()).sum::<usize>();
        }
    }

    // Detail of the selected ship (plain immutable `world.get`s — the queries above are done).
    let detail = chosen.map(|e| {
        let brain = world.get::<AiBrain>(e).copied().unwrap_or_default();
        let pos = world.get::<Position>(e).map(|p| p.0);
        let aoi = world.get::<AoiTier>(e).copied();
        let stats = world.get::<ShipStats>(e).copied();
        let tier = aoi.map_or(Tier::Active, |a| a.tier);
        // Degraded-state cause — mirrors ai_execute_system's guards in their exact order.
        let degraded = match stats {
            Some(s) if s.control_fitted && !s.has_control => {
                "derelict (control fitted, none alive) → pinned Hold, zero intent"
            }
            Some(s) if s.power_supply <= 0.0 => {
                "no power (dead reactor) → pinned Hold, zero intent"
            }
            _ if tier == Tier::Dormant => {
                "dormant tier → execution skipped (glide aggregate owns it)"
            }
            _ => "none",
        };
        let mut contacts: Vec<AiContactRow> = Vec::new();
        let (contacts_total, last_scan_tick) = world.get::<ContactList>(e).map_or((0, 0), |cl| {
            contacts = cl
                .contacts
                .iter()
                .map(|c| AiContactRow {
                    target: format!("{:?}", c.target),
                    dist: pos.map_or(f32::INFINITY, |p| (c.last_pos - p).length()),
                    last_seen_tick: c.last_seen_tick,
                    signature: c.signature,
                })
                .collect();
            contacts.sort_by(|a, b| a.dist.total_cmp(&b.dist));
            contacts.truncate(4);
            (cl.contacts.len(), cl.last_scan_tick)
        });
        let network = world.get_resource::<SensorNetworks>().and_then(|nets| {
            nets.by_faction.iter().find_map(|(fk, comps)| {
                comps.iter().enumerate().find_map(|(i, c)| {
                    c.members
                        .contains(&e)
                        .then_some((*fk, i, c.members.len(), c.fused.len()))
                })
            })
        });
        #[cfg(feature = "ai_debug")]
        let capture = world.get::<AiDebugCapture>(e).map(|c| AiCaptureView {
            last_scores: c
                .last_scores
                .iter()
                .map(|(b, s)| (format!("{b:?}"), *s))
                .collect(),
            winner: format!("{:?}", c.winner),
            momentum_applied: c.momentum_applied,
            transitions: c
                .transitions
                .iter()
                .rev()
                .map(|(t, from, to)| (*t, format!("{from:?}"), format!("{to:?}")))
                .collect(),
            // R97 Stage D — the live per-channel readout: MOVE direction + throttle,
            // the AIM drive + facing, the FIRE decision + WHY, and collision imminence.
            // Vec2 has no Display, so format the components; the enums Debug-format.
            channels: format!(
                "MOVE ({:.2}, {:.2}) t{:.2} · AIM {:?} ({:.2}, {:.2}) · FIRE {} ({:?}) · collision {:.2}",
                c.move_dir.x,
                c.move_dir.y,
                c.move_throttle,
                c.aim_drive,
                c.aim_dir.x,
                c.aim_dir.y,
                c.fire,
                c.fire_reason,
                c.collision_imminence,
            ),
        });
        AiShipDetail {
            entity: e,
            stable_id: world.get::<AiStableId>(e).map(|s| s.0),
            behavior: format!("{:?}", brain.behavior),
            archetype: format!("{:?}", brain.archetype),
            movement_profile: format!("{:?}", brain.movement_profile),
            combat_stance: format!("{:?}", brain.combat_stance),
            tier: aoi.map_or_else(
                || "untiered (treated Active)".to_string(),
                |a| format!("{:?} (since @{})", a.tier, a.since_tick),
            ),
            throttle_cap: brain.throttle_cap,
            last_think_tick: brain.last_think_tick,
            commit_until_tick: brain.commit_until_tick,
            last_damaged_tick: brain.last_damaged_tick,
            thinks_total: brain.thinks_total,
            target: brain.target.map(|t| format!("{t:?}")),
            player_order: format_player_order(world.get::<PlayerOrder>(e)),
            order_profile: world.get::<PlayerOrder>(e).and_then(|o| o.profile),
            order_stance: world.get::<PlayerOrder>(e).and_then(|o| o.stance),
            order_posture: world.get::<PlayerOrder>(e).and_then(|o| o.posture),
            contacts_total,
            last_scan_tick,
            contacts,
            link: world.get::<LinkState>(e).map(|l| (l.jammed, l.severed)),
            network,
            squad: squad_info,
            degraded,
            #[cfg(feature = "ai_debug")]
            capture,
        }
    });

    AiPanelData {
        rows,
        detail,
        metrics,
    }
}

/// Render the selected ship's inspection detail (T038, TR-020a). Read-only rows via [`stat`].
fn render_ai_detail(ui: &mut egui::Ui, d: &AiShipDetail, now: u64) {
    stat(
        ui,
        "ship",
        format!(
            "{:?}  (stable id {})",
            d.entity,
            d.stable_id.map_or("—".to_string(), |i| i.to_string())
        ),
    )
    .on_hover_text("The inspected AI ship: its ECS entity + sim-stable spawn-order id.");
    stat(
        ui,
        "behavior",
        format!(
            "{}  ({} · throttle ×{:.2})",
            d.behavior, d.archetype, d.throttle_cap
        ),
    )
    .on_hover_text("Current behavior state, the cached fit-archetype, and the squad pace throttle cap applied to forward intent.");
    stat(
        ui,
        "style",
        format!("move {} · stance {}", d.movement_profile, d.combat_stance),
    )
    .on_hover_text("R96 — the RESOLVED movement profile (how it paces/brakes onto nav goals: Rush/Cruise/Leisurely) and combat stance (how it fights: Charge/Orbit/Kite/Strafe) chosen this think (squad ← role ← archetype default).");
    stat(ui, "tier", &d.tier)
        .on_hover_text("AOI sim-LOD tier (Active = full AI, Mid = squad-driven, Dormant = glide aggregate) + the tick it was entered.");
    stat(
        ui,
        "staleness",
        format!(
            "think @{} (now @{})  commit→@{}  thinks {}",
            d.last_think_tick, now, d.commit_until_tick, d.thinks_total
        ),
    )
    .on_hover_text("last_think_tick vs the current tick (how stale the shown decision is) + the commitment window end + lifetime completed thinks.");
    stat(
        ui,
        "recency",
        if d.last_damaged_tick == 0 {
            "never fired upon".to_string()
        } else {
            format!(
                "damaged @{} ({} ticks ago)",
                d.last_damaged_tick,
                now.saturating_sub(d.last_damaged_tick)
            )
        },
    )
    .on_hover_text("R97 — the tick this ship was last fired upon (last_damaged_tick) + how many ticks ago. The survival-pressure recency Stage B/C read against AiTuning::threat_recency_window_ticks.");
    stat(
        ui,
        "target",
        d.target.clone().unwrap_or_else(|| "none".to_string()),
    )
    .on_hover_text(
        "The brain's current engage/follow target entity (pruned the tick it despawns).",
    );
    stat(
        ui,
        "link",
        d.link.map_or("linked (no LinkState)".to_string(), |(j, s)| {
            format!("jammed {j} · severed {s}")
        }),
    )
    .on_hover_text("Datalink seam flags: EITHER true excludes the ship from sensor-network fusion (own picture only). No component = linked.");
    stat(
        ui,
        "network",
        d.network
            .map_or("none (own picture only)".to_string(), |(fk, i, n, f)| {
                format!("faction {fk} comp #{i}: {n} members · {f} fused")
            }),
    )
    .on_hover_text("The sensor-network connected component containing this ship: faction key, component index, member count, fused-picture size.");
    stat(ui, "degraded", d.degraded)
        .on_hover_text("Active degraded-state cause, mirroring ai_execute_system's guards: derelict / dead reactor → pinned Hold; Dormant → execution skipped.");
    // ContactList summary: count + the nearest few entries.
    stat(
        ui,
        "contacts",
        format!("{} (own scan @{})", d.contacts_total, d.last_scan_tick),
    )
    .on_hover_text("This ship's own ContactList: total known hostiles + the tick its own scan last ran. Nearest entries below.");
    for c in &d.contacts {
        let dist = if c.dist.is_finite() {
            format!("{:.0}u", c.dist)
        } else {
            "—".to_string()
        };
        ui.label(
            egui::RichText::new(format!(
                "    {:<8} d {:<7} seen @{:<8} sig {:.2}",
                c.target, dist, c.last_seen_tick, c.signature
            ))
            .monospace(),
        );
    }
    // Squad/wing membership + the squad brain's order decision (TR-020a).
    match &d.squad {
        None => {
            stat(ui, "squad", "none (solo brain)").on_hover_text(
                "No squad's member list contains this ship — it thinks and steers solo.",
            );
        }
        Some(s) => {
            stat(
                ui,
                "squad",
                format!(
                    "{} order {} · {} members · think @{}",
                    s.squad, s.order, s.members, s.last_think_tick
                ),
            )
            .on_hover_text("The owning squad entity, its current order decision, member count, and the squad brain's last think tick.");
            stat(
                ui,
                "squad pace",
                format!(
                    "anchor {} ({:.1} u/s) · wing {} · gliding {}",
                    s.pace_anchor.clone().unwrap_or_else(|| "—".to_string()),
                    s.anchor_speed,
                    s.wing.clone().unwrap_or_else(|| "—".to_string()),
                    if s.gliding { "yes" } else { "no" }
                ),
            )
            .on_hover_text("Pace anchor (slowest essential member) + cached anchor speed, the parent wing (if any), and whether the squad is collapsed to a glide aggregate.");
            // R97 Phase 2 — the STRATEGIC objective driving this squad's order (the planned
            // strategy, not just the resulting order). Shown only when the squad carries a
            // SquadObjective; the wing line only when the squad's wing carries a WingObjective.
            if let Some(obj) = &s.objective {
                stat(ui, "objective", obj)
                    .on_hover_text("R97 Phase 2 — the squad's STRATEGIC objective (SquadObjective): the goal the HTN planner decomposes into the squad order above, its plan cursor (PatrolRoute waypoint index), and the tick it was last re-planned (the SLOW strategic cadence).");
            }
            if let Some(wobj) = &s.wing_objective {
                stat(ui, "wing objective", wobj)
                    .on_hover_text("R97 Phase 2 — the parent WING's strategic objective (WingObjective): the wing-level goal decomposed onto each member squad's objective (e.g. DefendZone → lead anchors, others patrol the perimeter).");
            }
        }
    }
    // T038 score breakdown — the AiDebugCapture read, feature-gated exactly like the sim seam
    // (AD-006): both cfg arms compile; the default dev_panel build enables ai_debug.
    #[cfg(feature = "ai_debug")]
    {
        ui.separator();
        ui.label(
            egui::RichText::new("Score breakdown — captured at the LAST think (never recomputed)")
                .strong(),
        );
        match &d.capture {
            None => {
                ui.label(
                    egui::RichText::new("no capture yet (no completed think since spawn)").weak(),
                );
            }
            Some(c) => {
                stat(
                    ui,
                    "winner",
                    format!(
                        "{} (momentum +{:.0}% on incumbent)",
                        c.winner,
                        c.momentum_applied * 100.0
                    ),
                )
                .on_hover_text("The behavior the last think selected + the momentum bonus that was applied to the incumbent's score (0% when the incumbent wasn't a candidate).");
                for (b, s) in &c.last_scores {
                    let marker = if *b == c.winner { "►" } else { " " };
                    ui.label(egui::RichText::new(format!("  {marker} {b:<14} {s:.3}")).monospace());
                }
                // R97 Stage D — the live per-channel readout (written EVERY tick, so it
                // shows the action even between thinks): MOVE / AIM / FIRE (+ why) / collision.
                ui.separator();
                ui.label(egui::RichText::new("channels (live, this tick)").weak());
                ui.label(egui::RichText::new(format!("  {}", c.channels)).monospace())
                    .on_hover_text("R97 per-channel decision: MOVE translate dir + throttle, the AIM drive (None/Move/Target/Threat) + facing dir, the FIRE decision + WHY (NoTarget/PostureBlocked/Unarmed/OutOfRange/NotAligned/NoEnergy/Overheated/Fired), and the collision imminence [0,1].");
                ui.label(
                    egui::RichText::new(format!(
                        "transitions (newest first, ring of {}):",
                        c.transitions.len()
                    ))
                    .weak(),
                );
                for (t, from, to) in &c.transitions {
                    ui.label(egui::RichText::new(format!("    @{t}  {from} → {to}")).monospace());
                }
            }
        }
    }
    #[cfg(not(feature = "ai_debug"))]
    {
        ui.separator();
        ui.label(
            egui::RichText::new(
                "score breakdown unavailable — build with --features ai_debug \
                 (the default dev_panel build enables it)",
            )
            .weak(),
        );
    }
}

/// Visibility of the two dev windows — the editable **Dev Tuning** panel and the read-only **Ship
/// Stats** panel (Phase M6c). Backtick (`` ` ``) toggles both at once; each window's egui `[x]`
/// closes just that one. Default **open** (this is the solo dev client).
#[derive(Resource)]
pub struct DevPanelState {
    pub tuning_open: bool,
    pub stats_open: bool,
    /// Refinement 27: last result of the "Save tuning → RON" button (shown by the button).
    pub save_status: String,
    /// Refinement 43: last result of the "Equip weapon → player ship" quick-swap (shown by it).
    pub equip_status: String,
    /// T038 (TR-020a): AI-inspection stable-id override as typed text. Blank = the ship nearest the
    /// player (the spec default); a parseable sim-stable id pins that ship. Applied at the NEXT
    /// frame's read phase (the panel reads it before the UI edits it).
    pub ai_selected: String,
    /// T039 (TR-020c) rolling-rate bookkeeping: the last sampled `(tick, Σ thinks_total)` pair.
    /// `None` until the first frame with AI data.
    ai_rate_sample: Option<(u64, u64)>,
    /// T039: latest derived thinks/second (sim time) between panel samples.
    ai_thinks_per_s: f32,
    /// T039: accumulated tier transitions OBSERVED at sampled ticks (the panel runs at render rate,
    /// so ticks between frames are not seen — a sampled rolling signal, not an exact census).
    ai_transitions_seen: u64,
    /// T039: tick of the first metrics sample — the rolling-rate window anchor.
    ai_metrics_since: Option<u64>,
    /// R99 Phase B (command mode): when ON, in-world left-click selects the nearest
    /// allied AI ship and right-click commands it (move / attack). The SELECTION is
    /// the existing [`Self::ai_selected`] stable-id string — shared with the AI
    /// inspection list, never a second selection. The picking/ring/commands all live
    /// in [`crate::command_mode`] and read this flag.
    pub command_mode: bool,
    /// R99 Phase B (B5) — last status of a Team "Join Red/Blue" click (shown by the buttons).
    pub team_status: String,
}

impl Default for DevPanelState {
    fn default() -> Self {
        Self {
            tuning_open: true,
            stats_open: true,
            save_status: String::new(),
            equip_status: String::new(),
            ai_selected: String::new(),
            ai_rate_sample: None,
            ai_thinks_per_s: 0.0,
            ai_transitions_seen: 0,
            ai_metrics_since: None,
            command_mode: false,
            team_status: String::new(),
        }
    }
}

/// Adds the egui-based live tuning panel (Phase M6). Registered only under the `dev_panel` feature.
pub struct DevPanelPlugin;

impl Plugin for DevPanelPlugin {
    fn build(&self, app: &mut App) {
        // R44: `EguiPlugin` is added once in `lib::run()` (egui is always-on now), not here.
        app.init_resource::<DevPanelState>()
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
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> egui::Response {
    let id = ui.make_persistent_id(("dev_slider_limits", label));
    let (mut lo, mut hi) = ui
        .data_mut(|d| d.get_temp::<(f32, f32)>(id))
        .unwrap_or((*range.start(), *range.end()));
    // Never let the slider clamp the current value down (the value may already exceed the
    // default cap, or a prior edit raised it) — widen the range to include it.
    lo = lo.min(*v);
    hi = hi.max(*v);
    let speed = drag_speed(lo, hi);
    // R28b: capture the SLIDER WIDGET's response (not the `horizontal` row) — egui only shows a
    // hover tooltip when THAT response is `hovered()`, and hovering the slider marks the slider
    // widget, not the row. The slider senses drag (incl. hover), so the tooltip fires correctly;
    // callers chaining `.on_hover_text(...)` land on the same widget.
    let mut slider_resp = None;
    ui.horizontal(|ui| {
        // Editable lower bound, the value slider over the live range, then the editable upper
        // bound — all on one line.
        ui.add_sized([56.0, 18.0], egui::DragValue::new(&mut lo).speed(speed));
        slider_resp = Some(ui.add(egui::Slider::new(v, lo..=hi).text(label)));
        ui.add_sized([56.0, 18.0], egui::DragValue::new(&mut hi).speed(speed));
    });
    let resp = slider_resp.expect("the slider widget was added in the horizontal layout");
    if hi < lo {
        hi = lo;
    }
    ui.data_mut(|d| d.insert_temp(id, (lo, hi)));
    // R28: auto-attach the hover tooltip when the label is exactly a StatId short (covers the StatId
    // tuning sliders with no call-site change). Decorated / ad-hoc callers chain `.on_hover_text(...)`.
    let tip = desc_for_label(label);
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Reverse-lookup a slider's hover tooltip from its label: if `label` is exactly a [`StatId`]'s short
/// name, return that stat's [`StatId::desc`]; else `""` (R28). Used by [`slider`] for auto-tooltips.
fn desc_for_label(label: &str) -> &'static str {
    StatId::ALL
        .iter()
        .copied()
        .find(|id| id.meta().short == label)
        .map_or("", |id| id.desc())
}

/// One read-only stat row (Phase M6c-fix): the label left-padded in a fixed-width **monospace**
/// column, then the value — so every stats group lines up in the same columns regardless of label
/// length. Egui's default font is proportional, so the alignment relies on the monospace style.
fn stat(ui: &mut egui::Ui, label: &str, value: impl std::fmt::Display) -> egui::Response {
    ui.label(egui::RichText::new(format!("{label:<16}{value}")).monospace())
}

/// Refinement 35: the galaxy (spectral-population) controls — the 7-class table + the galactic band,
/// haze/dust, core bulge and bright-star glare. All live + RON-persisted. Shown only when spectral
/// mode is on (the per-layer temp/tint/twinkle sliders take over in legacy mode).
fn galaxy_controls(ui: &mut egui::Ui, g: &mut GalaxyTuning) {
    const CLASS_NAMES: [&str; NUM_CLASSES] = [
        "M (red dwarf)",
        "K (orange)",
        "G (yellow / solar)",
        "F (yellow-white)",
        "A (white)",
        "B (blue-white giant)",
        "O (blue supergiant)",
    ];
    egui::CollapsingHeader::new("Spectral classes (M–O)").show(ui, |ui| {
        ui.label(
            egui::RichText::new("Population weights set the mix; everything else is per-class look.")
                .weak(),
        );
        for (ci, name) in CLASS_NAMES.iter().enumerate() {
            let c = &mut g.classes[ci];
            egui::CollapsingHeader::new(*name).show(ui, |ui| {
                slider(ui, &format!("C{ci} weight %"), &mut c.weight, 0.0..=100.0).on_hover_text(
                    "This class's share of the population (relative weight; the CDF is normalized from all 7). M ~76, O ~0.00003 in reality (O is boosted here so it shows).",
                );
                slider(ui, &format!("C{ci} temp min K"), &mut c.temp_min, 1000.0..=45000.0)
                    .on_hover_text("Cool end of this class's blackbody temperature (most stars sit here).");
                slider(ui, &format!("C{ci} temp max K"), &mut c.temp_max, 1000.0..=45000.0)
                    .on_hover_text("Hot end of this class's temperature (the rarer, hotter members).");
                slider(ui, &format!("C{ci} brightness"), &mut c.brightness, 0.0..=10.0)
                    .on_hover_text("Base HDR brightness — >1 blooms. The brightest member of the class (magnitude spread fades the rest).");
                slider(ui, &format!("C{ci} size"), &mut c.size, 0.2..=5.0)
                    .on_hover_text("Star pixel radius for this class (× the per-layer size depth multiplier).");
                ui.horizontal(|ui| {
                    ui.label("tint").on_hover_text("Flat color multiply on the blackbody color (e.g. nudge O toward violet, which blackbody can't reach). White = none.");
                    ui.color_edit_button_rgb(&mut c.tint);
                });
                slider(ui, &format!("C{ci} clustering"), &mut c.clustering, 0.0..=1.0).on_hover_text(
                    "0 = spread uniformly (M/K/G) … 1 = confined to the galactic band (hot O/B/A young stars).",
                );
                slider(ui, &format!("C{ci} twinkle depth"), &mut c.twinkle, 0.0..=2.0)
                    .on_hover_text("Scintillation depth (0 = steady; space realism = low).");
                slider(ui, &format!("C{ci} twinkle speed"), &mut c.twinkle_speed, 0.0..=5.0)
                    .on_hover_text("Scintillation pulse rate.");
                slider(ui, &format!("C{ci} softness"), &mut c.softness, 0.0..=3.0).on_hover_text(
                    "Edge anti-aliasing px: ~0 = a hard point (M; but hard points shimmer on motion), higher = a soft Gaussian (O). ~0.4+ stays crisp AND stable.",
                );
                slider(ui, &format!("C{ci} mag spread"), &mut c.mag_spread, 0.0..=1.0).on_hover_text(
                    "Within-class brightness spread: 0 = every star equal; higher = a few much brighter, many faint.",
                );
            });
        }
    });
    egui::CollapsingHeader::new("Galactic band").show(ui, |ui| {
        slider(
            ui,
            "band angle",
            &mut g.band_angle,
            0.0..=std::f32::consts::PI,
        )
        .on_hover_text("Orientation (radians) of the Milky-Way lane across the field.");
        slider(ui, "band width", &mut g.band_width, 0.05..=2.0)
            .on_hover_text("Thickness of the band (Gaussian across its axis).");
        slider(ui, "band offset", &mut g.band_offset, -1.0..=1.0)
            .on_hover_text("Shift the band off-center (perpendicular to its axis).");
        slider(ui, "band strength", &mut g.band_strength, 0.0..=1.0)
            .on_hover_text("How strongly high-clustering classes are confined to the band.");
        slider(ui, "band clumpiness", &mut g.band_clumpiness, 0.0..=1.0)
            .on_hover_text("Patchiness along the band (0 = smooth lane, 1 = clumpy).");
    });
    egui::CollapsingHeader::new("Galactic haze & dust").show(ui, |ui| {
        slider(ui, "haze brightness", &mut g.haze_brightness, 0.0..=0.5).on_hover_text(
            "Faint milky glow along the band (unresolved-star haze). The Milky Way read.",
        );
        ui.horizontal(|ui| {
            ui.label("haze color")
                .on_hover_text("Tint of the milky haze glow.");
            ui.color_edit_button_rgb(&mut g.haze_color);
        });
        slider(ui, "dust depth", &mut g.dust_depth, 0.0..=1.0)
            .on_hover_text("How dark the dust lanes carve into the haze (occlusion strength).");
        slider(ui, "dust scale", &mut g.dust_scale, 0.01..=0.5)
            .on_hover_text("Dust-lane feature size (smaller = finer, more lanes).");
        slider(ui, "dust contrast", &mut g.dust_contrast, 0.2..=4.0)
            .on_hover_text("Dust lane contrast (higher = sharper dark veins).");
    });
    egui::CollapsingHeader::new("Galactic core").show(ui, |ui| {
        slider(ui, "core along band", &mut g.core_along, -1.0..=1.0)
            .on_hover_text("Position of the bright core bulge along the band axis.");
        slider(ui, "core size", &mut g.core_size, 0.02..=1.0)
            .on_hover_text("Radius of the core bulge glow.");
        slider(ui, "core brightness", &mut g.core_brightness, 0.0..=1.0)
            .on_hover_text("Brightness of the warm galactic-center bulge.");
        ui.horizontal(|ui| {
            ui.label("core color")
                .on_hover_text("Color of the core bulge (warm/yellow looks galactic).");
            ui.color_edit_button_rgb(&mut g.core_color);
        });
        slider(
            ui,
            "core density boost",
            &mut g.core_density_boost,
            0.0..=3.0,
        )
        .on_hover_text("Extra star density near the core (denser center).");
    });
    egui::CollapsingHeader::new("Bright-star glare").show(ui, |ui| {
        slider(ui, "glare threshold", &mut g.glare_threshold, 0.0..=8.0).on_hover_text(
            "HDR brightness a star must exceed to get diffraction glare (so only A/B/O glare).",
        );
        slider(ui, "glare halo size", &mut g.glare_halo_size, 1.0..=40.0)
            .on_hover_text("Radius (px) of the soft glow halo around a bright star.");
        slider(
            ui,
            "glare halo intensity",
            &mut g.glare_halo_intensity,
            0.0..=1.0,
        )
        .on_hover_text("Strength of the halo glow.");
        slider(ui, "glare spike length", &mut g.glare_spike_len, 1.0..=60.0)
            .on_hover_text("Length (px) of the diffraction spikes.");
        slider(ui, "glare spike count", &mut g.glare_spike_count, 0.0..=8.0)
            .on_hover_text("4 = a cross; >5 adds diagonals (6/8-point).");
        slider(
            ui,
            "glare spike intensity",
            &mut g.glare_spike_intensity,
            0.0..=1.0,
        )
        .on_hover_text("Strength of the diffraction spikes.");
    });
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
        // R92 — directional channels: show the weaker side of each pair (the limiting authority).
        (
            StatId::Strafe,
            fmt(StatId::Strafe, s.strafe_port.min(s.strafe_starboard)),
        ),
        (
            StatId::Torque,
            fmt(StatId::Torque, s.turn_ccw.min(s.turn_cw)),
        ),
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
        stat(ui, "weapon", format!("none (can_fire {})", s.can_fire))
            .on_hover_text("Read-only: this ship's weapon summary — 'none' = no fitted weapon; can_fire reflects the power/heat/cooldown gate.");
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
    stat(ui, "modules", format!("{}", t.count))
        .on_hover_text("Read-only: number of installed modules in this equipment group.");
    render_rows(
        ui,
        vec![
            (StatId::Mass, fmt(StatId::Mass, t.mass)),
            // R92 — only the jet force is authored per thruster; turn/strafe are placement-derived
            // (see the Applied block above for the live channels).
            (StatId::Thrust, fmt(StatId::Thrust, t.thrust)),
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
    // R49: CLIENT-side live ship-visual tuning (applied by `apply_ship_visuals` + `update_engine_flames`).
    mut ship_visual: ResMut<crate::ShipVisualTuning>,
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
    // R39: read the current resistance matrix so the dev-settings save round-trips it (the panel has
    // no resistance editor yet, so this is normally the default — harmless to persist + reapply).
    let resistance = world
        .get_resource::<ResistanceMatrix>()
        .copied()
        .unwrap_or_else(default_resistance_matrix);
    // R66: the typed per-cell hull/armor materials catalog (edited by the "Cell materials" section).
    let mut cell_materials = world
        .get_resource::<sim::fitting::CellMaterials>()
        .cloned()
        .unwrap_or_default();
    // T038 (TR-020b): the live AI tuning (ServerApp::new inserts the pinned default, so the
    // fallback only covers exotic test worlds).
    let mut ai_tun = world
        .get_resource::<AiTuning>()
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
    // R43: a quick weapon-equip request (a clicked weapon id) + the resolved player ship entity.
    // The click is captured in the read/UI phase and applied in the write phase below (where the world
    // is mutable). `equip_ship` is `None` until the player ship exists.
    let mut equip_weapon: Option<ModuleId> = None;
    let equip_ship = net
        .as_ref()
        .and_then(|n| host.server.ship_entity_for(n.local_id));
    // R99 Phase B — command/team requests captured in the UI phase, applied in the
    // write-back phase below (where the server world is mutable). `None` = no request.
    let mut command_request: Option<CommandRequest> = None;
    let mut team_request: Option<TeamJoinRequest> = None;

    // T038/T039 (TR-020): snapshot the AI inspection + runtime metrics. `world_mut()` is needed
    // only to build the read-only QueryStates (see `gather_ai` docs) — nothing in the sim world is
    // written, so viewing never perturbs decision order or determinism.
    let ai_data = gather_ai(
        host.server.world_mut(),
        equip_ship,
        &state.ai_selected,
        &ai_tun,
    );
    // T039 rolling rates (the Local-snapshot trick, kept in DevPanelState): thinks/s from the
    // Σ thinks_total delta between sampled ticks; tier transitions accumulated per sampled tick.
    // Sampled at panel (render) rate — ticks between frames are not observed.
    {
        let m = &ai_data.metrics;
        if state.ai_metrics_since.is_none() && m.brains > 0 {
            state.ai_metrics_since = Some(m.now);
        }
        match state.ai_rate_sample {
            Some((tick, thinks)) if m.now > tick => {
                state.ai_thinks_per_s = m.thinks_total.saturating_sub(thinks) as f32
                    / (m.now - tick) as f32
                    * m.tick_hz;
                state.ai_transitions_seen += m.tier_transitions_this_tick as u64;
                state.ai_rate_sample = Some((m.now, m.thinks_total));
            }
            None => state.ai_rate_sample = Some((m.now, m.thinks_total)),
            _ => {}
        }
    }

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
                        )
                        .on_hover_text("Read-only: emergent cruise speed (thrust / linear-drag) — the speed the loaded transport settles at.");
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

                // T038 (TR-020b) — live AiTuning: EVERY field group, mirroring the
                // SimTuning/MiningTuning pattern (read → sliders → write-phase insert_resource;
                // "Save dev settings" persists it in render_tuning.ron). All tick counts @30 Hz.
                egui::CollapsingHeader::new("AI tuning — AiTuning (live)").show(ui, |ui| {
                    // u32 fields: a plain int slider (the ricochet_min_neighbors pattern).
                    let int = |ui: &mut egui::Ui,
                               label: &str,
                               v: &mut u32,
                               range: std::ops::RangeInclusive<u32>,
                               tip: &str| {
                        ui.add(egui::Slider::new(v, range).text(label))
                            .on_hover_text(tip);
                    };
                    ui.label(egui::RichText::new("Think cadence (TR-005)").strong());
                    int(ui, "think ticks Active", &mut ai_tun.think_ticks_active, 1..=120,
                        "Active-tier FALLBACK think cadence, ticks. Events still trigger same-tick re-thinks.");
                    int(ui, "think ticks Mid", &mut ai_tun.think_ticks_mid, 1..=120,
                        "Mid-tier fallback think cadence, ticks.");
                    int(ui, "think ticks Dormant", &mut ai_tun.think_ticks_dormant, 1..=600,
                        "Dormant-tier fallback think cadence, ticks (the 2–5 s band; 90 = 3 s).");
                    int(ui, "fallback buckets", &mut ai_tun.fallback_bucket_count, 1..=64,
                        "Phase-bucket count spreading the fallback cadence — each tick services ≈ N/buckets brains.");
                    ui.separator();
                    ui.label(egui::RichText::new("AOI tiers (TR-007/TR-008)").strong());
                    slider(ui, "AOI radius Active", &mut ai_tun.aoi_radius_active, 5.0..=500.0)
                        .on_hover_text("Player-proximity radius of the Active tier (full per-ship AI), world units.");
                    slider(ui, "AOI radius Mid", &mut ai_tun.aoi_radius_mid, 20.0..=2000.0)
                        .on_hover_text("Player-proximity radius of the Mid tier (squad-driven AI); beyond = Dormant.");
                    int(ui, "tier hysteresis", &mut ai_tun.tier_hysteresis_ticks, 0..=300,
                        "Minimum ticks between tier changes per entity (no boundary thrash). Promotion is immediate.");
                    slider(ui, "promote nudge max", &mut ai_tun.promote_nudge_max, 0.0..=50.0)
                        .on_hover_text("TR-008 validity-nudge bound: max de-penetration distance applied at glide expansion, world units.");
                    ui.separator();
                    ui.label(egui::RichText::new("Squads (TR-010)").strong());
                    int(ui, "max squad size", &mut ai_tun.max_squad_size, 1..=32,
                        "Maximum members per squad; larger groups split.");
                    int(ui, "wing split threshold", &mut ai_tun.wing_split_threshold, 1..=64,
                        "Fleet size at/above which squads organize under a wing parent.");
                    ui.separator();
                    ui.label(egui::RichText::new("Utility scoring (TR-004)").strong());
                    slider(ui, "momentum bonus", &mut ai_tun.momentum_bonus, 0.0..=1.0)
                        .on_hover_text("Incumbent-behavior score multiplier bonus (~0.25 = +25%): hysteresis against behavior oscillation. Watch its effect in the inspection view's score breakdown.");
                    slider(ui, "compensation k", &mut ai_tun.compensation_k, 0.1..=4.0)
                        .on_hover_text("Mark's compensation factor for multiplied consideration curves (rescales so adding considerations doesn't starve a score).");
                    ui.separator();
                    ui.label(egui::RichText::new("Ramming (TR-012)").strong());
                    slider(ui, "ram target hull frac", &mut ai_tun.ram_target_hull_frac, 0.0..=1.0)
                        .on_hover_text("Target hull fraction at/below which it counts as near-dead/disabled for a ram.");
                    slider(ui, "ram self margin", &mut ai_tun.ram_self_margin, 1.0..=10.0)
                        .on_hover_text("Required ratio of projected dealt damage over projected self-damage (\"much weaker\").");
                    slider(ui, "ram min closing frac", &mut ai_tun.ram_min_closing_frac, 0.0..=1.0)
                        .on_hover_text("Minimum closing speed to commit, as a fraction of the attacker's top speed.");
                    ui.separator();
                    ui.label(egui::RichText::new("Fit archetypes (TR-006) ⟳ mass re-classify").strong());
                    slider(ui, "arch speed hi", &mut ai_tun.arch_speed_hi, 0.0..=300.0)
                        .on_hover_text("Top speed at/above which a fit reads as 'fast' (Kiter-leaning). Editing any cut re-classifies EVERY ship live (V-5; keeps damage).");
                    slider(ui, "arch dps hi", &mut ai_tun.arch_dps_hi, 0.0..=200.0)
                        .on_hover_text("Sustained DPS at/above which a fit reads as 'armed/heavy-hitting' (Brawler-leaning). Edit → live mass re-classification (V-5).");
                    slider(ui, "arch armor hi", &mut ai_tun.arch_armor_hi, 0.0..=500.0)
                        .on_hover_text("Armor/structure pool at/above which a fit reads as 'tanky' (Rammer/Brawler-leaning). Edit → live mass re-classification (V-5).");
                    ui.separator();
                    ui.label(egui::RichText::new("Sensors / perception (TR-013/TR-014)").strong());
                    slider(ui, "base sensor range", &mut ai_tun.base_sensor_range, 0.0..=2000.0)
                        .on_hover_text("Baseline own-ship sensor range, world units (v1 faction baseline).");
                    slider(ui, "datalink radius", &mut ai_tun.datalink_radius, 0.0..=2000.0)
                        .on_hover_text("Faction datalink connectivity radius for the sensor-network flood-fill.");
                    slider(ui, "sig threshold", &mut ai_tun.sig_threshold, 0.0..=10.0)
                        .on_hover_text("Minimum contact signature to detect (0 = everything visible in v1).");
                    int(ui, "scan ticks Mid", &mut ai_tun.scan_ticks_mid, 1..=300,
                        "Mid-tier scan cadence, ticks (~0.5 s fused query).");
                    int(ui, "scan ticks Far", &mut ai_tun.scan_ticks_far, 1..=600,
                        "Far-tier scan cadence, ticks (coarse-grid neighborhood; 90 = 3 s).");
                    int(ui, "max fused contacts", &mut ai_tun.max_fused_contacts, 1..=256,
                        "Cap per fused network picture (newest/highest-signature kept, deterministic cut).");
                    ui.separator();
                    ui.label(egui::RichText::new("Context steering (TR-002)").strong());
                    int(ui, "steering slots", &mut ai_tun.slot_count, 8..=16,
                        "Direction slots per context map (interest/danger), 8–16.");
                    slider(ui, "danger mask floor", &mut ai_tun.danger_mask_floor, 0.0..=1.0)
                        .on_hover_text("Floor a danger-masked slot is suppressed to (0 = fully blocked).");
                    ui.separator();
                    ui.label(egui::RichText::new("Movement profiles (R96)").strong());
                    slider(ui, "rush cap", &mut ai_tun.profile_rush_cap, 0.0..=1.5)
                        .on_hover_text("Forward throttle cap for the Rush profile (hot pace — full authority). Composed with the squad throttle_cap.");
                    slider(ui, "cruise cap", &mut ai_tun.profile_cruise_cap, 0.0..=1.5)
                        .on_hover_text("Forward throttle cap for the Cruise profile (PINNED 1.0 — the pre-R96 parity no-op).");
                    slider(ui, "leisurely cap", &mut ai_tun.profile_leisurely_cap, 0.0..=1.5)
                        .on_hover_text("Forward throttle cap for the Leisurely profile (lazy pace — capped to half by default).");
                    slider(ui, "brake aggr rush", &mut ai_tun.brake_aggression_rush, 0.0..=3.0)
                        .on_hover_text("Brake aggression for Rush — an earlier-braking multiplier on the arrive stopping distance.");
                    slider(ui, "brake aggr cruise", &mut ai_tun.brake_aggression_cruise, 0.0..=3.0)
                        .on_hover_text("Brake aggression for Cruise (PINNED 1.0 — unused on the no-brake parity path).");
                    slider(ui, "brake aggr leisurely", &mut ai_tun.brake_aggression_leisurely, 0.0..=3.0)
                        .on_hover_text("Brake aggression for Leisurely — brakes earliest for a long, gentle settle.");
                    slider(ui, "slow factor rush", &mut ai_tun.arrive_slow_factor_rush, 0.5..=12.0)
                        .on_hover_text("Arrive slow-radius factor for Rush (snug ramp — × ARRIVE_RADIUS).");
                    slider(ui, "slow factor cruise", &mut ai_tun.arrive_slow_factor_cruise, 0.5..=12.0)
                        .on_hover_text("Arrive slow-radius factor for Cruise (PINNED 4.0 — the WAYPOINT_SLOW_FACTOR).");
                    slider(ui, "slow factor leisurely", &mut ai_tun.arrive_slow_factor_leisurely, 0.5..=12.0)
                        .on_hover_text("Arrive slow-radius factor for Leisurely (wide ramp — eases in early).");
                    ui.separator();
                    ui.label(egui::RichText::new("Combat stances (R96)").strong());
                    slider(ui, "orbit radius frac", &mut ai_tun.orbit_radius_frac, 0.0..=2.0)
                        .on_hover_text("Orbit ring radius as a multiple of the archetype standoff_distance (1.0 = the standoff ring itself; >1 wider, <1 tighter).");
                    slider(ui, "orbit tangential weight", &mut ai_tun.orbit_tangential_weight, 0.0..=2.0)
                        .on_hover_text("Orbit tangential interest weight — scales the around-the-target term (dominates on-ring, yields to the radial correction off-ring).");
                    slider(ui, "kite range frac", &mut ai_tun.kite_range_frac, 0.5..=2.0)
                        .on_hover_text("Kite standoff target as a multiple of weapon_range (1.1 = hold just past the envelope edge so the gun still bears while the target chases).");
                    slider(ui, "strafe stance lateral", &mut ai_tun.strafe_stance_lateral, 0.0..=1.0)
                        .on_hover_text("Lateral strafe magnitude (0..1) a stance commands — applied ONLY when the hull can_strafe; a basic fighter orbits by turning alone.");
                    ui.separator();
                    ui.label(egui::RichText::new("Obstacle avoidance (R96)").strong());
                    slider(ui, "obstacle danger weight", &mut ai_tun.obstacle_danger_weight, 0.0..=3.0)
                        .on_hover_text("Danger weight written per in-range obstacle — scales how strongly an obstacle masks the headings into it.");
                    slider(ui, "obstacle lookahead s", &mut ai_tun.obstacle_lookahead_s, 0.0..=4.0)
                        .on_hover_text("Predictive lookahead (s) for the closeness test — a body is live if the ship's current OR pos+vel·this position is inside its avoid radius.");
                    slider(ui, "obstacle clearance pad", &mut ai_tun.obstacle_clearance_pad, 0.0..=40.0)
                        .on_hover_text("Extra clearance pad (world units) added to obstacle_radius + own_radius so the ship steers around with margin, not skimming the surface.");
                    slider(ui, "obstacle query radius", &mut ai_tun.obstacle_query_radius, 0.0..=500.0)
                        .on_hover_text("Scan radius (world units) around the ship — obstacles farther than this are ignored (a linear-scan gate over the tiny field).");
                    slider(ui, "obstacle min radius", &mut ai_tun.obstacle_min_radius, 0.0..=100.0)
                        .on_hover_text("Minimum body radius (world units) to enter the ObstacleField — only LARGE neutral bodies (asteroids/outposts/transports) are avoided.");
                    ui.separator();
                    ui.label(egui::RichText::new("Decision enrichment (R97)").strong());
                    // threat_recency_window_ticks is a u64 (a tick stamp horizon), so it needs its
                    // own slider — the shared `int` helper is u32-typed.
                    ui.add(egui::Slider::new(&mut ai_tun.threat_recency_window_ticks, 0..=600).text("threat recency window ticks"))
                        .on_hover_text("Recency window (ticks) over which a last_damaged_tick stamp counts as 'recently fired upon' — the survival-pressure horizon (now − last_damaged_tick < this). 90 ticks ≈ 3 s at 30 Hz.");
                    slider(ui, "threat proximity range", &mut ai_tun.threat_proximity_range, 0.0..=600.0)
                        .on_hover_text("Proximity range (world units) inside which a hostile contributes to the incoming-threat consideration — the spatial half of the threat scalar.");
                    slider(ui, "collision preempt gain", &mut ai_tun.collision_preempt_gain, 0.0..=10.0)
                        .on_hover_text("Gain applied to the collision-imminence consideration when sizing its preemptive avoidance response (Stage D) — higher brakes/turns earlier.");
                    slider(ui, "collision horizon s", &mut ai_tun.collision_horizon_s, 0.0..=5.0)
                        .on_hover_text("Collision look-ahead horizon (s): the window the time-to-collision is normalized over for con_collision_imminence (ttc / this, clamped) — a collision beyond it scores ~0 imminence.");
                    slider(ui, "move drive weight", &mut ai_tun.move_drive_weight, 0.0..=3.0)
                        .on_hover_text("Per-channel BASE weight for the MOVE-drive utility channel — Stage B scales its move-intent considerations by this. 1.0 = the neutral pass-through default.");
                    slider(ui, "aim drive weight", &mut ai_tun.aim_drive_weight, 0.0..=3.0)
                        .on_hover_text("Per-channel BASE weight for the AIM-drive utility channel — the twin of move drive weight for the aim/fire considerations (Stage C). 1.0 = neutral.");
                    slider(ui, "weapons free flee floor", &mut ai_tun.weapons_free_flee_floor, 0.0..=1.0)
                        .on_hover_text("Floor applied to the flee/evade desire while weapons-free: a non-zero floor keeps a minimum break-off willingness even mid-attack. 0.0 = no floor (flee is purely score-driven).");
                    ui.separator();
                    ui.label(egui::RichText::new("Debug (TR-020a)").strong());
                    int(ui, "debug history len", &mut ai_tun.debug_history_len, 1..=128,
                        "Per-brain behavior-transition ring length (capture is feature-gated OFF in headless/bench builds).");
                });

                // R99 Phase B (B5) — Team join: live re-faction + reposition the player ship to
                // a side's base (mirrors net.rs auto-join), and persist the preference so it is
                // honoured at the next join. Captured here, applied in the write-back phase.
                egui::CollapsingHeader::new("Team").show(ui, |ui| {
                    let cur = net
                        .as_ref()
                        .and_then(|n| host.server.ship_entity_for(n.local_id))
                        .and_then(|e| host.server.world().get::<Faction>(e).copied());
                    ui.label(
                        egui::RichText::new(format!(
                            "current: {}",
                            match cur {
                                Some(Faction::Red) => "Red",
                                Some(Faction::Blue) => "Blue",
                                None => "none (unfactioned)",
                            }
                        ))
                        .monospace(),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Join Red").on_hover_text("Re-faction the player ship to Red and reposition at Red's base. Persists the preference (sticky across loads).").clicked() {
                            team_request = Some(TeamJoinRequest { faction: Faction::Red });
                        }
                        if ui.button("Join Blue").on_hover_text("Re-faction the player ship to Blue and reposition at Blue's base. Persists the preference (sticky across loads).").clicked() {
                            team_request = Some(TeamJoinRequest { faction: Faction::Blue });
                        }
                    });
                    if !state.team_status.is_empty() {
                        ui.label(egui::RichText::new(&state.team_status).weak());
                    }
                });

                // T038 (TR-020a) — per-ship AI inspection: STRICTLY read-only (selection only
                // changes what the panel shows, never sim state).
                egui::CollapsingHeader::new("AI inspection (per-ship, read-only)").show(ui, |ui| {
                    if ai_data.rows.is_empty() {
                        // TR-020: a world with AI gated off renders an empty AI section.
                        ui.label(egui::RichText::new("no AI ships in this world").weak());
                        return;
                    }
                    // R99 Phase B (B1) — Command-Mode toggle. The in-world picker
                    // (crate::command_mode) reads this flag; selection is the SAME
                    // `ai_selected` the list below uses (one shared selection).
                    ui.checkbox(&mut state.command_mode, "Command mode (in-world clicking)")
                        .on_hover_text("LEFT-click an allied AI ship to select it; RIGHT-click an enemy to attack, or empty space to move there. The selected ship is highlighted by a cyan ground ring and shares this list's selection.");
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("stable id");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.ai_selected)
                                .hint_text("blank = nearest")
                                .desired_width(72.0),
                        )
                        .on_hover_text("Pin the inspected ship by its sim-stable id (the # in the list). Blank or unparseable = the ship nearest the player (the default).");
                        if ui.small_button("✕").clicked() {
                            state.ai_selected.clear();
                        }
                    });
                    for row in ai_data.rows.iter().take(12) {
                        let selected =
                            ai_data.detail.as_ref().is_some_and(|d| d.entity == row.entity);
                        let id_txt =
                            row.stable_id.map_or("#—".to_string(), |i| format!("#{i}"));
                        let dist_txt = if row.dist.is_finite() {
                            format!("{:.0}u", row.dist)
                        } else {
                            "—".to_string()
                        };
                        let text = egui::RichText::new(format!(
                            "{id_txt:>5} {dist_txt:>7}  {:<13} {:?}",
                            row.behavior, row.entity
                        ))
                        .monospace();
                        if ui.selectable_label(selected, text).clicked() {
                            if let Some(i) = row.stable_id {
                                state.ai_selected = i.to_string();
                            }
                        }
                    }
                    if ai_data.rows.len() > 12 {
                        ui.label(
                            egui::RichText::new(format!(
                                "… {} more (pin one via the stable-id field)",
                                ai_data.rows.len() - 12
                            ))
                            .weak(),
                        );
                    }
                    ui.separator();
                    if let Some(d) = &ai_data.detail {
                        render_ai_detail(ui, d, ai_data.metrics.now);
                    }
                    // R99 Phase B (B4) — command + settings controls for the SELECTED ship,
                    // shown only with command mode ON and a ship resolved. Each control captures
                    // a request applied in the write-back phase (the world is read-only here).
                    if state.command_mode {
                        if let Some(d) = &ai_data.detail {
                            let ship = d.entity;
                            ui.separator();
                            ui.label(egui::RichText::new("Command (selected ship)").strong());
                            ui.label(
                                egui::RichText::new(format!("order: {}", d.player_order)).monospace(),
                            )
                            .on_hover_text("The selected ship's live PlayerOrder (nav kind + the 3 style overrides). 'none' = no order; 'settings-only' = style overrides without a nav command.");
                            ui.horizontal(|ui| {
                                if ui.button("Hold here").on_hover_text("Command the ship to hold/defend at its current position.").clicked() {
                                    command_request = Some(CommandRequest::HoldHere(ship));
                                }
                                if ui.button("Clear command").on_hover_text("Remove the PlayerOrder entirely — the ship reverts to its scenario behaviour.").clicked() {
                                    command_request = Some(CommandRequest::Clear(ship));
                                }
                            });
                            // Movement profile dropdown ((inherit) + every variant).
                            let profiles = profile_choices();
                            egui::ComboBox::from_label("profile")
                                .selected_text(choice_label(&profiles, d.order_profile))
                                .show_ui(ui, |ui| {
                                    for (lbl, val) in profiles {
                                        if ui
                                            .selectable_label(d.order_profile == val, lbl)
                                            .clicked()
                                        {
                                            command_request = Some(CommandRequest::SetProfile(ship, val));
                                        }
                                    }
                                });
                            // Combat stance dropdown (Orbit split CW/CCW).
                            let stances = stance_choices();
                            egui::ComboBox::from_label("stance")
                                .selected_text(choice_label(&stances, d.order_stance))
                                .show_ui(ui, |ui| {
                                    for (lbl, val) in stances {
                                        if ui
                                            .selectable_label(d.order_stance == val, lbl)
                                            .clicked()
                                        {
                                            command_request = Some(CommandRequest::SetStance(ship, val));
                                        }
                                    }
                                });
                            // Posture dropdown.
                            let postures = posture_choices();
                            egui::ComboBox::from_label("posture")
                                .selected_text(choice_label(&postures, d.order_posture))
                                .show_ui(ui, |ui| {
                                    for (lbl, val) in postures {
                                        if ui
                                            .selectable_label(d.order_posture == val, lbl)
                                            .clicked()
                                        {
                                            command_request = Some(CommandRequest::SetPosture(ship, val));
                                        }
                                    }
                                });
                        } else {
                            ui.separator();
                            ui.label(
                                egui::RichText::new(
                                    "command mode on — left-click an allied AI ship to select one",
                                )
                                .weak(),
                            );
                        }
                    }
                });

                // T039 (TR-020c) — runtime AI metrics: read-only whole-world counts, sampled at
                // panel (render) rate over the embedded server world.
                egui::CollapsingHeader::new("AI metrics (runtime, read-only)").show(ui, |ui| {
                    let m = &ai_data.metrics;
                    if m.brains == 0 {
                        ui.label(egui::RichText::new("no AI ships in this world").weak());
                        return;
                    }
                    stat(ui, "tick", m.now)
                        .on_hover_text("Current sim tick of the embedded server (30 Hz fixed step).");
                    stat(
                        ui,
                        "ai ships",
                        format!(
                            "{} (Active {} / Mid {} / Dormant {})",
                            m.brains, m.tier_active, m.tier_mid, m.tier_dormant
                        ),
                    )
                    .on_hover_text("AiBrain carriers per AOI tier (a ship with no AoiTier counts as Active — the execution rule).");
                    stat(
                        ui,
                        "thinks Σ",
                        format!("{} (~{:.1}/s)", m.thinks_total, state.ai_thinks_per_s),
                    )
                    .on_hover_text("Σ thinks_total over all brains + the rate derived from the delta between sampled ticks.");
                    stat(
                        ui,
                        "thinks @tick",
                        format!(
                            "{} (cadence {} / event {})",
                            m.thinks_this_tick, m.thinks_cadence, m.thinks_event
                        ),
                    )
                    .on_hover_text("Brains that thought THIS tick, split by the scheduler's fallback-cadence test: an off-slot think can only be event-triggered. Sampled at panel rate.");
                    stat(
                        ui,
                        "squads",
                        format!("{} ({} gliding aggregates)", m.squads, m.gliding_aggregates),
                    )
                    .on_hover_text("Live squad entities; 'gliding' = collapsed to the cheap-glide aggregate (GlideState present).");
                    // R97 Phase 2 — the strategic picture at a glance: how many squads carry a
                    // SquadObjective, broken down by objective kind.
                    stat(
                        ui,
                        "strategy",
                        format!(
                            "{} with objective ({} defending, {} destroying, {} patrolling, {} holding, {} withdrawing/regrouping)",
                            m.squad_objectives,
                            m.obj_defending,
                            m.obj_destroying,
                            m.obj_patrolling,
                            m.obj_holding,
                            m.obj_withdrawing,
                        ),
                    )
                    .on_hover_text("R97 Phase 2 — squads carrying a SquadObjective (the strategic tier), broken down by objective kind. Withdraw + Regroup are reported together as the disengage states.");
                    stat(ui, "offscreen battles", m.offscreen_battles)
                        .on_hover_text("STF-001 live signal: EXPANDED (non-gliding) squads farther from the player than the Mid AOI radius — promoted off-screen engagements running full squad AI.");
                    let elapsed = m.now.saturating_sub(state.ai_metrics_since.unwrap_or(m.now));
                    let trans_rate = if elapsed > 0 {
                        state.ai_transitions_seen as f32 / elapsed as f32 * m.tick_hz
                    } else {
                        0.0
                    };
                    stat(
                        ui,
                        "tier transitions",
                        format!(
                            "{} this tick · ~{:.2}/s rolling",
                            m.tier_transitions_this_tick, trans_rate
                        ),
                    )
                    .on_hover_text("AoiTier carriers (ships + squads) whose since_tick == now: promotions + demotions landing this tick, plus a sampled rolling rate since the panel opened.");
                    stat(
                        ui,
                        "contacts Σ",
                        format!("{} ({} scans this tick)", m.contact_sum, m.scans_this_tick),
                    )
                    .on_hover_text("Σ ContactList lengths (per-ship perception memory) + ships whose own scan ran this tick.");
                    stat(
                        ui,
                        "networks",
                        format!(
                            "{} components · {} fused Σ",
                            m.network_components, m.fused_total
                        ),
                    )
                    .on_hover_text("Sensor-network connected components across all factions + the summed fused-picture contact totals.");
                });

                egui::CollapsingHeader::new(
                    "Sim consts — SimTuning (carve / mass / projectile / wreck / ram)",
                )
                .show(ui, |ui| {
                    slider(
                        ui,
                        &format!("{} ⟳", label(StatId::StructCellHp)),
                        &mut sim.struct_cell_hp,
                        0.5..=40.0,
                    )
                    .on_hover_text(StatId::StructCellHp.desc());
                    slider(
                        ui,
                        &format!("{} ⟳", label(StatId::StructCellMass)),
                        &mut sim.struct_cell_mass,
                        0.01..=2.0,
                    )
                    .on_hover_text(StatId::StructCellMass.desc());
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
                    )
                    .on_hover_text(StatId::RicochetMinNeighbors.desc());
                    ui.add(
                        egui::Slider::new(&mut sim.smooth_normal_radius, 0..=5)
                            .text(label(StatId::SmoothNormalRadius)),
                    )
                    .on_hover_text(StatId::SmoothNormalRadius.desc());
                    slider(
                        ui,
                        &format!("{} (unfitted)", label(StatId::ProjMass)),
                        &mut sim.projectile_mass,
                        0.001..=1.0,
                    )
                    .on_hover_text(StatId::ProjMass.desc());
                    slider(
                        ui,
                        &format!("{} (unfitted)", label(StatId::ProjDamage)),
                        &mut sim.projectile_damage,
                        1.0..=100.0,
                    )
                    .on_hover_text(StatId::ProjDamage.desc());
                    slider(
                        ui,
                        label(StatId::ProjLifetime),
                        &mut sim.projectile_lifetime,
                        0.2..=10.0,
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new("R42 weapon physics — caliber → size/rate/damage")
                            .strong(),
                    );
                    slider(ui, label(StatId::MmToWorld), &mut sim.mm_to_world, 0.001..=0.05);
                    slider(
                        ui,
                        label(StatId::VelocityScale),
                        &mut sim.velocity_scale,
                        0.05..=1.0,
                    );
                    slider(ui, label(StatId::RpmScale), &mut sim.rpm_scale, 0.002..=0.1);
                    slider(
                        ui,
                        label(StatId::ProjDensity),
                        &mut sim.projectile_density,
                        0.0..=0.00001,
                    );
                    slider(
                        ui,
                        label(StatId::DamagePerJoule),
                        &mut sim.damage_per_joule,
                        0.0..=0.01,
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "R92 rotation physics — jets resolve by placement + facing",
                        )
                        .strong(),
                    );
                    slider(ui, "lever scale", &mut sim.thruster_lever_scale, 0.0..=5.0)
                        .on_hover_text("R92 — torque per (jet force × world-unit lever arm from the CoM). 0 = placement doesn't matter; higher = extremity-mounted jets dominate turning.");
                    slider(ui, "inertia scale", &mut sim.thruster_inertia_scale, 0.0..=0.2)
                        .on_hover_text("R92 — angular inertia per unit of the layout's REAL moment (Σ m·r² about the CoM). Higher = spread-out/heavy designs turn sluggishly.");
                    slider(ui, "baseline turn", &mut sim.baseline_turn_torque, 0.0..=40.0)
                        .on_hover_text("R92 — the hull's built-in maneuvering-jet TURN authority (both directions); placed jets add on top.");
                    slider(ui, "baseline strafe", &mut sim.baseline_strafe_force, 0.0..=40.0)
                        .on_hover_text("R92 — built-in STRAFE authority (both sides); side-facing jets add on top.");
                    slider(ui, "baseline reverse", &mut sim.baseline_reverse_force, 0.0..=40.0)
                        .on_hover_text("R92 — built-in RETRO authority; without forward-facing jets this is your only brake (flip-and-burn!).");
                    ui.separator();
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
                        .on_hover_text(StatId::RicochetAngle.desc())
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

                // R66 — the typed per-cell HULL + ARMOR material catalog. Edits flow LIVE: HP/mass
                // via the `designs_changed` → force_rederive path; the gate reads it each tick. id 0
                // (Standard / None) is the byte-identical baseline (greyed); ids 1+ are editable.
                egui::CollapsingHeader::new("Cell materials (hull + armor) — R66").show(ui, |ui| {
                    ui.label("HULL materials (structural cells). id 0 = Standard (Sim consts):");
                    let mut remove_hull: Option<usize> = None;
                    for (i, h) in cell_materials.hull.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("{i}"));
                            ui.add_enabled(
                                i > 0,
                                egui::TextEdit::singleline(&mut h.name).desired_width(64.0),
                            );
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut h.cell_hp).speed(0.1).prefix("hp "),
                            );
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut h.mass).speed(0.02).prefix("mass "),
                            );
                            if i > 0 && ui.small_button("✕").clicked() {
                                remove_hull = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove_hull {
                        cell_materials.hull.remove(i);
                    }
                    if ui.button("+ hull material").clicked() {
                        cell_materials.hull.push(sim::fitting::HullMaterialDef {
                            name: "New".into(),
                            cell_hp: 4.0,
                            mass: 0.3,
                        });
                    }
                    ui.separator();
                    ui.label("ARMOR materials (plating). id 0 = None:");
                    let mut remove_armor: Option<usize> = None;
                    for (i, a) in cell_materials.armor.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("{i}"));
                            ui.add_enabled(
                                i > 0,
                                egui::TextEdit::singleline(&mut a.name).desired_width(56.0),
                            );
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut a.thickness)
                                    .speed(0.05)
                                    .prefix("th "),
                            )
                            .on_hover_text("plate thickness → the ricochet/penetration gate");
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut a.multiplier).speed(0.02).prefix("×"),
                            )
                            .on_hover_text("material hardness multiplier on thickness");
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut a.carve_hp).speed(0.5).prefix("hp "),
                            )
                            .on_hover_text("extra carve resistance per cell");
                            ui.add_enabled(
                                i > 0,
                                egui::DragValue::new(&mut a.mass).speed(0.05).prefix("m "),
                            )
                            .on_hover_text("extra mass per cell (agility tradeoff)");
                            if i > 0 && ui.small_button("✕").clicked() {
                                remove_armor = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove_armor {
                        cell_materials.armor.remove(i);
                    }
                    if ui.button("+ armor material").clicked() {
                        cell_materials.armor.push(sim::fitting::ArmorMaterialDef {
                            name: "New".into(),
                            thickness: 1.0,
                            multiplier: 1.0,
                            carve_hp: 20.0,
                            mass: 1.0,
                        });
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

                // R43: quick-equip a weapon onto the PLAYER ship's primary weapon slot (slot 3). The
                // click is applied to the live embedded-server ship in the write phase below — instant
                // test path: click a weapon, fly, fire. Validated (type/size/budget); rejections shown.
                egui::CollapsingHeader::new("Equip weapon → player ship (live)").show(ui, |ui| {
                    if equip_ship.is_none() {
                        ui.label(egui::RichText::new("no player ship yet").weak());
                    }
                    if let Some(cat) = modules.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            for (id, m) in cat.modules.iter() {
                                if m.kind == ModuleKind::Weapon
                                    && ui
                                        .button(&m.name)
                                        .on_hover_text(format!("{id:?} → slot 3"))
                                        .clicked()
                                {
                                    equip_weapon = Some(*id);
                                }
                            }
                        });
                    }
                    if !state.equip_status.is_empty() {
                        ui.label(&state.equip_status);
                    }
                });

                if let Some(modules) = modules.as_mut() {
                    // R39: "Module Designs" — one entry per DESIGN (catalog template), grouped by kind
                    // and labeled by name. Editing a design's stats applies to EVERY ship using it.
                    egui::CollapsingHeader::new("Module Designs ⟳").show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "A DESIGN's stats apply to every ship that uses it (live; keeps damage). Save writes modules.ron.",
                            )
                            .weak(),
                        );
                        const KINDS: [ModuleKind; 7] = [
                            ModuleKind::Reactor,
                            ModuleKind::Thruster,
                            ModuleKind::Weapon,
                            ModuleKind::Shield,
                            ModuleKind::Armor,
                            ModuleKind::Utility,
                            ModuleKind::Sensor,
                        ];
                        for (ki, kind) in KINDS.iter().enumerate() {
                            let ids: Vec<ModuleId> = modules
                                .modules
                                .iter()
                                .filter(|(_, m)| m.kind == *kind)
                                .map(|(id, _)| *id)
                                .collect();
                            if ids.is_empty() {
                                continue;
                            }
                            egui::CollapsingHeader::new(format!("{kind:?} ({})", ids.len()))
                                .id_salt(("modkind", ki))
                                .show(ui, |ui| {
                                    for id in ids {
                                        let Some(m) = modules.modules.get_mut(&id) else {
                                            continue;
                                        };
                                        egui::CollapsingHeader::new(format!("{} [{:?}]", m.name, id))
                                            .id_salt(("mod", id.0))
                                            .show(ui, |ui| {
                                                // Canonical order: mass → [thruster] → power gen/draw
                                                // → cpu → hp → [weapon | shield | armor].
                                                slider(ui, label(StatId::Mass), &mut m.mass, 0.0..=80.0);
                                                if let ModuleSpecifics::Thruster {
                                                    thrust_force, ..
                                                } = &mut m.specifics
                                                {
                                                    // R92 — ONE jet force; turn/strafe come from the
                                                    // slot's placement + facing (the flight computer).
                                                    slider(ui, label(StatId::Thrust), thrust_force, 0.0..=80.0);
                                                }
                                                slider(ui, label(StatId::PowerGen), &mut m.power_gen, 0.0..=100.0);
                                                slider(ui, label(StatId::PowerDraw), &mut m.power_draw, 0.0..=50.0);
                                                slider(ui, label(StatId::Cpu), &mut m.cpu_draw, 0.0..=50.0);
                                                slider(ui, label(StatId::Hp), &mut m.health_max, 1.0..=200.0);
                                                match &mut m.specifics {
                                                    ModuleSpecifics::Shield { shield_hp, regen } => {
                                                        slider(ui, label(StatId::ShieldHp), shield_hp, 0.0..=300.0);
                                                        slider(ui, label(StatId::ShieldRegen), regen, 0.0..=50.0);
                                                    }
                                                    ModuleSpecifics::Armor { armor_value } => {
                                                        slider(ui, label(StatId::Armor), armor_value, 0.0..=300.0);
                                                    }
                                                    ModuleSpecifics::Weapon {
                                                        caliber_mm,
                                                        muzzle_velocity_ms,
                                                        rpm,
                                                        spin_up_time,
                                                        dispersion_deg,
                                                        range_units,
                                                        ..
                                                    } => {
                                                        // R42: author the REAL specs; the game derives
                                                        // size/rate/damage/mass (read-only ↳ below).
                                                        slider(ui, label(StatId::CaliberMm), caliber_mm, 1.0..=120.0);
                                                        slider(ui, label(StatId::MuzzleVelocityMs), muzzle_velocity_ms, 100.0..=2000.0);
                                                        slider(ui, label(StatId::Rpm), rpm, 30.0..=8000.0);
                                                        slider(ui, label(StatId::SpinUp), spin_up_time, 0.0..=3.0);
                                                        slider(ui, label(StatId::DispersionDeg), dispersion_deg, 0.0..=5.0);
                                                        slider(ui, label(StatId::RangeUnits), range_units, 100.0..=3000.0);
                                                    }
                                                    // R92 — energy stores + cargo bays.
                                                    ModuleSpecifics::EnergyStore { capacity } => {
                                                        slider(ui, "energy capacity", capacity, 0.0..=300.0)
                                                            .on_hover_text("R92 — flat energy-pool capacity this store adds (health-scaled live; persists when the reactor dies).");
                                                    }
                                                    ModuleSpecifics::CargoBay { volume } => {
                                                        slider(ui, "cargo volume", volume, 0.0..=500.0)
                                                            .on_hover_text("R92 — cargo hold volume this bay adds (health-scaled live).");
                                                    }
                                                    // R93 — control modules.
                                                    ModuleSpecifics::ReactionWheel { torque } => {
                                                        slider(ui, "wheel torque", torque, 0.0..=40.0)
                                                            .on_hover_text("R93 — placement-free torque this CMG/reaction wheel adds to BOTH turn channels (health-scaled).");
                                                    }
                                                    ModuleSpecifics::FlightComputer { tier } => {
                                                        let mut t = *tier as f32;
                                                        slider(ui, "FC tier", &mut t, 1.0..=2.0)
                                                            .on_hover_text("R93 — Flight Computer tier: 1 = strafe authority, 2 = + diagonal keys.");
                                                        *tier = t.round() as u8;
                                                    }
                                                    ModuleSpecifics::Thruster { .. }
                                                    | ModuleSpecifics::Reactor
                                                    | ModuleSpecifics::Utility
                                                    | ModuleSpecifics::Cockpit
                                                    | ModuleSpecifics::ControlRelay
                                                    | ModuleSpecifics::Sensor { .. } => {}
                                                }
                                                // R42: read-only DERIVED game stats (the real specs
                                                // above × the live weapon-physics scales).
                                                if let Some(d) = derive_weapon(&m.specifics, &sim) {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "↳ derived: muzzle {:.0}  rof {:.1}/s  dmg {:.1}  slug {:.3}  radius {:.3}  life {:.1}s",
                                                            d.muzzle_speed,
                                                            d.fire_rate,
                                                            d.damage,
                                                            d.projectile_mass,
                                                            d.projectile_radius,
                                                            d.lifetime,
                                                        ))
                                                        .weak(),
                                                    );
                                                }
                                            });
                                    }
                                });
                        }
                    });
                }

                if let Some(hulls) = hulls.as_mut() {
                    egui::CollapsingHeader::new("Hull Designs ⟳").show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(
                                "A hull DESIGN's stats apply to every ship of that hull. Save writes ships.ron.",
                            )
                            .weak(),
                        );
                        for (id, h) in hulls.hulls.iter_mut() {
                            egui::CollapsingHeader::new(format!("{} [{:?}]", h.name, id))
                                .id_salt(("hull", id.0))
                                .show(ui, |ui| {
                                    ui.label(format!(
                                        "{} {:?} (read-only)",
                                        label(StatId::GridDims),
                                        h.grid_dims
                                    ))
                                    .on_hover_text(StatId::GridDims.desc());
                                    slider(
                                        ui,
                                        &format!("{} (budget axis)", label(StatId::BaseMass)),
                                        &mut h.hull_base_mass,
                                        0.1..=120.0,
                                    )
                                    .on_hover_text(StatId::BaseMass.desc());
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
                    let bar = |ui: &mut egui::Ui, name: &str, p: &mut crate::hud_bars::BarPos| {
                        slider(ui, &format!("{name} x"), &mut p.x_center, -8.0..=8.0)
                            .on_hover_text(TIP_BAR_X);
                        slider(ui, &format!("{name} y"), &mut p.y_base, -8.0..=8.0)
                            .on_hover_text(TIP_BAR_Y);
                        slider(ui, &format!("{name} extent"), &mut p.extent, 0.5..=8.0)
                            .on_hover_text(TIP_BAR_EXTENT);
                    };
                    bar(ui, "Energy", &mut hud_layout.energy);
                    bar(ui, "Heat", &mut hud_layout.heat);
                    bar(ui, "Afterburner", &mut hud_layout.afterburner);
                    bar(ui, "Shield", &mut hud_layout.shield);
                    bar(ui, "Armor", &mut hud_layout.armor);
                    bar(ui, "Hull", &mut hud_layout.hull);
                    ui.separator();
                    ui.label("Energy readout (viewport % / px).");
                    slider(ui, "readout left %", &mut hud_layout.readout_left_pct, 0.0..=100.0)
                        .on_hover_text("Energy numeric readout: left edge as % of the viewport width — line it up with the Energy bar's left edge.");
                    slider(ui, "readout width %", &mut hud_layout.readout_width_pct, 0.0..=100.0)
                        .on_hover_text("Energy readout row width as % of the viewport. With SpaceBetween, this sets how far right the rate sits vs the ENRG number — tune to span the bar.");
                    slider(ui, "readout bottom px", &mut hud_layout.readout_bottom_px, 0.0..=400.0)
                        .on_hover_text("Energy readout distance from the bottom of the screen, in pixels.");
                    ui.separator();
                    // R40: the bottom-right module-condition bar panel (Reactor/Thruster/Weapon/…).
                    ui.label("Module-condition bars (bottom-right).");
                    slider(ui, "module right px", &mut hud_layout.module_right_px, 0.0..=600.0)
                        .on_hover_text("Module-bar panel distance from the RIGHT screen edge, in pixels.");
                    slider(ui, "module bottom px", &mut hud_layout.module_bottom_px, 0.0..=600.0)
                        .on_hover_text("Module-bar panel distance from the BOTTOM screen edge, in pixels.");
                    slider(ui, "module bar width px", &mut hud_layout.module_bar_width_px, 30.0..=400.0)
                        .on_hover_text("Width of each per-type segmented bar track, in pixels.");
                    slider(ui, "module bar height px", &mut hud_layout.module_bar_height_px, 4.0..=40.0)
                        .on_hover_text("Height (thickness) of each per-type segmented bar track, in pixels.");
                    ui.separator();
                    // R44: the HUD layout has its OWN file + Save button now (it used to ride in the
                    // sim-tuning `render_tuning.ron`). Status shows by the Save buttons below.
                    if ui
                        .button("Save HUD layout → hud_layout.ron")
                        .on_hover_text("Persist the HUD bar/readout layout to its own hud_layout.ron, separate from the sim-tuning render_tuning.ron.")
                        .clicked()
                    {
                        state.save_status = match tuning_io::save_hud_layout(&hud_layout) {
                            Ok(m) => m,
                            Err(e) => format!("HUD save failed: {e}"),
                        };
                    }
                });

                // Refinement 34: bloom is a CAMERA post-process (the whole rendered image), NOT a
                // starfield knob — give it its own section. Editing mutates `StarfieldTuning` →
                // `update_starfield` applies `bloom_intensity` to the camera's `Bloom` next frame.
                egui::CollapsingHeader::new("Camera / Post-processing (client, live)").show(ui, |ui| {
                    slider(ui, "bloom intensity", &mut starfield.bloom_intensity, 0.0..=1.0)
                        .on_hover_text("Camera bloom strength — the glow on bright pixels across the WHOLE image (bright stars, emissive, ships). Higher = more glow; keep modest so ships stay readable against the field.");
                });

                // R49: live ship-visual tuning (glow / flame / nav / accent / fill / bloom / hull shader).
                egui::CollapsingHeader::new("Ship visuals (client, live)").show(ui, |ui| {
                    let sv = &mut *ship_visual;
                    ui.label("Engine / reactor glow (bloom halo):");
                    slider(ui, "glow intensity", &mut sv.glow_intensity, 0.0..=16.0)
                        .on_hover_text("Emissive brightness of the engine nozzles + reactor vents (HDR → bloom). Higher = brighter halo.");
                    slider(ui, "glow R", &mut sv.glow_color[0], 0.0..=1.0);
                    slider(ui, "glow G", &mut sv.glow_color[1], 0.0..=1.0);
                    slider(ui, "glow B", &mut sv.glow_color[2], 0.0..=1.0);
                    ui.label("Engine exhaust flame (throttle-driven):");
                    slider(ui, "flame length", &mut sv.flame_length, 0.0..=8.0)
                        .on_hover_text("Length of each thruster's exhaust flame at full throttle (× cell size).");
                    slider(ui, "flame width", &mut sv.flame_width, 0.1..=2.0);
                    ui.label("Lights / accents:");
                    slider(ui, "nav-light intensity", &mut sv.nav_intensity, 0.0..=8.0)
                        .on_hover_text("Nav/running lights (transports etc.; fighters have none). 0 = off.");
                    slider(ui, "accent intensity", &mut sv.accent_intensity, 0.0..=8.0)
                        .on_hover_text("Faction-colour accent spine strip + canopy cap. 0 = off.");
                    slider(ui, "fill light", &mut sv.fill_intensity, 0.0..=8000.0)
                        .on_hover_text("Cool fill DirectionalLight illuminance — softly lights the hull's shadowed sides. Subtle top-down. 0 = off.");
                    slider(ui, "ship bloom", &mut sv.bloom_intensity, 0.0..=1.0)
                        .on_hover_text("Camera bloom (shared with the starfield's bloom slider).");
                    ui.label("Hull shader (fresnel rim + panels + grime):");
                    slider(ui, "rim strength", &mut sv.rim_strength, 0.0..=4.0)
                        .on_hover_text("Faction fresnel RIM glow on the silhouette edge. 0 = off.");
                    slider(ui, "rim power", &mut sv.rim_power, 0.5..=8.0)
                        .on_hover_text("Rim falloff — higher = a thinner, sharper edge glow.");
                    slider(ui, "panel scale", &mut sv.panel_scale, 0.05..=2.0)
                        .on_hover_text("Spacing of the procedural panel-line grid (world units).");
                    slider(ui, "panel width", &mut sv.panel_width, 0.0..=0.2)
                        .on_hover_text("Width of the darkened panel-line grooves.");
                    slider(ui, "grime", &mut sv.grime, 0.0..=2.0)
                        .on_hover_text("Splotchy used-future wear/dirt across the hull.");
                    ui.label("Engine ion-trail:");
                    ui.checkbox(&mut sv.trail_on, "trail on");
                    slider(ui, "trail rate", &mut sv.trail_rate, 0.0..=200.0)
                        .on_hover_text("Particles/sec at full throttle streaming aft from each thruster.");
                    slider(ui, "trail size", &mut sv.trail_size, 0.0..=0.5);
                    slider(ui, "trail life", &mut sv.trail_life, 0.05..=1.5);
                    ui.label("Damage smoke / sparks:");
                    ui.checkbox(&mut sv.smoke_on, "smoke on (carve)");
                    slider(ui, "smoke amount", &mut sv.smoke_amount, 0.0..=30.0)
                        .on_hover_text("Smoke puffs emitted each time a hull cell is carved off.");
                    ui.checkbox(&mut sv.spark_on, "sparks on (hit)");

                    ui.separator();
                    ui.label("Camera & lighting (R53 — 3-D depth):");
                    slider(ui, "camera tilt°", &mut sv.camera_tilt_deg, 0.0..=45.0)
                        .on_hover_text("Camera PITCH off straight-down. 0 = pure top-down (relief invisible); a few degrees reveals the hull's depth. Aiming is heading-based, so the tilt never affects controls.");
                    ui.checkbox(&mut sv.shadows_on, "key-light shadows");
                    slider(ui, "shadow bias", &mut sv.shadow_normal_bias, 0.0..=4.0)
                        .on_hover_text("Shadow normal-bias — raise to kill acne/striping, lower if shadows detach from contact points.");
                    slider(ui, "key illuminance", &mut sv.key_illuminance, 0.0..=20000.0)
                        .on_hover_text("Key directional-light brightness (lux).");
                    slider(ui, "key azimuth°", &mut sv.key_azimuth_deg, 0.0..=360.0)
                        .on_hover_text("Compass direction the key light comes from (around +Z).");
                    slider(ui, "key elevation°", &mut sv.key_elevation_deg, 5.0..=90.0)
                        .on_hover_text("Key-light height: 90° = straight down (flat, no shadows); LOW = raking across the hull → long shadows that reveal the plate relief.");

                    ui.separator();
                    ui.label("Hull (beveled, R55 — live, rebuilds on edit):");
                    slider(ui, "hull thickness", &mut sv.hull_top, 0.03..=0.5)
                        .on_hover_text("Combat-hull thickness (top-face height). Modest — the bevel + tilt + shadows carry the 3-D read; too tall reads as a block.");
                    slider(ui, "hull bevel", &mut sv.hull_bevel, 0.0..=0.15)
                        .on_hover_text("Chamfer size on the silhouette edge: the top face insets by this and the edge slopes down to it → a beveled edge that catches the key light. 0 = a hard vertical wall.");
                    slider(ui, "hull roundness", &mut sv.hull_round, 0.0..=4.0)
                        .on_hover_text("Silhouette smoothing passes: 0 = hard/angular cells, 1 = lightly rounded, 2+ = rounder (and slightly smaller). Hard-surface look = low.");
                });

                // Refinement 25/35/36: live starfield — ONE unified galaxy model. `layers` = depth,
                // the spectral class table = star character, + the galaxy globals. Presets load a full
                // look (built-in buttons + drop-in RON files); Save persists the active config.
                egui::CollapsingHeader::new("Starfield (client, live)").show(ui, |ui| {
                    egui::CollapsingHeader::new("Presets").show(ui, |ui| {
                        ui.label("Load a full look, then tweak:");
                        ui.horizontal_wrapped(|ui| {
                            for (name, make) in crate::starfield::BUILTIN_STARFIELD_PRESETS {
                                if ui.button(*name).clicked() {
                                    *starfield = make();
                                    state.save_status = format!("loaded preset: {name}");
                                }
                            }
                        });
                        let files = tuning_io::list_starfield_presets();
                        if !files.is_empty() {
                            ui.label("Saved (.ron):");
                            ui.horizontal_wrapped(|ui| {
                                for (name, path) in &files {
                                    if ui.button(name).clicked() {
                                        state.save_status =
                                            match tuning_io::load_starfield_preset(path) {
                                                Ok(t) => {
                                                    *starfield = t;
                                                    format!("loaded preset: {name}")
                                                }
                                                Err(e) => format!("load failed: {e}"),
                                            };
                                    }
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            let name_id = ui.make_persistent_id("sf_preset_name");
                            let mut nm =
                                ui.data_mut(|d| d.get_temp::<String>(name_id).unwrap_or_default());
                            ui.add(
                                egui::TextEdit::singleline(&mut nm)
                                    .hint_text("preset name")
                                    .desired_width(120.0),
                            );
                            if ui.button("Save as preset").clicked() && !nm.trim().is_empty() {
                                state.save_status =
                                    match tuning_io::save_starfield_preset(&nm, &starfield) {
                                        Ok(msg) => msg,
                                        Err(e) => format!("save failed: {e}"),
                                    };
                            }
                            ui.data_mut(|d| d.insert_temp(name_id, nm));
                        });
                    });
                    ui.separator();
                    slider(ui, "layers (4-16)", &mut starfield.layer_count, 4.0..=16.0)
                        .on_hover_text("How many parallax DEPTH layers to draw (4–16). More = deeper field, slightly more GPU. The 'Layer N' rows below appear/disappear with this.");
                    slider(
                        ui,
                        "zoom size compensation",
                        &mut starfield.galaxy.zoom_compensation,
                        0.0..=1.0,
                    )
                    .on_hover_text("How star size tracks zoom. 0 = fixed PIXEL size (crisp, but the field gets brighter zoomed out / dimmer zoomed in). 1 = fixed APPARENT size (stars shrink/grow with zoom) so the overall brightness stays ~constant across zoom — zoom out = many tiny stars, zoom in = fewer bigger ones.");
                    ui.separator();
                    // Star CHARACTER: the spectral class table + the galaxy band/haze/dust/core/glare.
                    galaxy_controls(ui, &mut starfield.galaxy);
                    ui.separator();
                    // Per-layer DEPTH rows: parallax/spacing/density + brightness & size depth
                    // multipliers + an OPTIONAL per-layer tint overlay (off by default). Star
                    // character (color/twinkle/size base) is the spectral class table above.
                    let count = (starfield.layer_count.round() as usize).clamp(1, MAX_LAYERS);
                    for i in 0..count {
                        let l = &mut starfield.layers[i];
                        egui::CollapsingHeader::new(format!("Layer {i} (depth)")).show(ui, |ui| {
                            slider(ui, &format!("L{i} parallax"), &mut l.parallax, 0.0..=1.0)
                                .on_hover_text(TIP_LAYER_PARALLAX);
                            slider(ui, &format!("L{i} frequency"), &mut l.frequency, 0.05..=4.0)
                                .on_hover_text(TIP_LAYER_FREQUENCY);
                            slider(ui, &format!("L{i} density"), &mut l.density, 0.0..=1.0)
                                .on_hover_text(TIP_LAYER_DENSITY);
                            slider(ui, &format!("L{i} brightness"), &mut l.brightness, 0.0..=3.0)
                                .on_hover_text(TIP_LAYER_BRIGHTNESS);
                            slider(ui, &format!("L{i} size"), &mut l.size, 0.3..=4.0)
                                .on_hover_text(TIP_LAYER_SIZE);
                            // OPTIONAL per-layer tint overlay (off by default — strength 0 = no-op).
                            ui.horizontal(|ui| {
                                ui.label("layer tint").on_hover_text(TIP_LAYER_TINT);
                                ui.color_edit_button_rgb(&mut l.tint)
                                    .on_hover_text(TIP_LAYER_TINT);
                            });
                            slider(
                                ui,
                                &format!("L{i} tint strength"),
                                &mut l.tint_strength,
                                0.0..=1.0,
                            )
                            .on_hover_text(TIP_LAYER_TINT_STRENGTH);
                        });
                    }
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
            // Refinement 41: persist the dev-panel SIM TUNING + HUD + starfield to the windowed
            // `render_tuning.ron` override (loaded windowed-only — never `ServerApp::new` — so headless
            // determinism is untouched). Module/hull DESIGNS are NOT saved here; use the separate
            // "Save designs" button below. (Starfield presets save separately, in their section.)
            if ui.button("Save dev settings → RON").clicked() {
                let dev = tuning_io::DevSettings {
                    tuning,
                    sim_tuning: sim,
                    penetration: pen,
                    shield,
                    salvage,
                    stat_scaling: scaling,
                    resistance,
                    mining,
                    starfield: *starfield,
                    ship_visual: *ship_visual,
                    cell_materials: cell_materials.clone(),
                    ai: ai_tun,
                    // Preserve the persisted faction preference (not a server-world resource, so it
                    // isn't rebuilt from `host` like the rest) — a full save must not reset it.
                    preferred_faction: tuning_io::load_dev_settings().preferred_faction,
                };
                state.save_status = match tuning_io::save_dev_settings(&dev) {
                    Ok(m) => m,
                    Err(e) => format!("save failed: {e}"),
                };
            }
            // Refinement 41: write the edited module/hull DESIGNS back to the canonical
            // `assets/content/{modules,ships}.ron` (filtered to seed ids → no scenario-hull pollution).
            // Only rewrites a file when its design data actually changed, so a no-edit click leaves the
            // files (and their hand-authored comments) intact. A real rewrite drops that file's comments
            // + reorders to id order (RON has no comment-preserving writer) — these become the new
            // defaults that both the windowed game and the headless tests load.
            if ui
                .button("Save designs → modules.ron/ships.ron")
                .clicked()
            {
                state.save_status =
                    match tuning_io::save_catalogs(modules.as_ref(), hulls.as_ref()) {
                        Ok(m) => m,
                        Err(e) => format!("design save failed: {e}"),
                    };
            }
            if !state.save_status.is_empty() {
                ui.label(&state.save_status);
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
        // T038 (TR-020b): Reset-ALL also restores the pinned AiTuning defaults.
        world.insert_resource(AiTuning::default());
        let (m, h) = seed_catalogs();
        world.insert_resource(m);
        world.insert_resource(h);
        // Refinement 24/25: "Reset ALL" also restores the HUD layout + starfield/bloom defaults.
        *hud_layout = HudLayout::default();
        *starfield = StarfieldTuning::default();
        rederive = true;
    } else {
        // R39: a module/hull DESIGN edit (or struct-cell mass) needs the cached ship stats to
        // re-derive. Detect it BEFORE re-inserting (the world still holds the pre-edit values), then
        // re-derive WITHOUT healing (preserve battle damage). Flight/damage tuning is read live per
        // tick, so it needs no re-derive. Catalogs differing → live update for all ships.
        let designs_changed = world.get_resource::<ModuleCatalog>() != modules.as_ref()
            || world.get_resource::<HullCatalog>() != hulls.as_ref()
            || world.get_resource::<SimTuning>() != Some(&sim)
            // R66 — a hull/armor material HP or mass edit needs a re-derive (the gate reads live).
            || world.get_resource::<sim::fitting::CellMaterials>() != Some(&cell_materials);
        // T038 (TR-020b, V-5): an ARCHETYPE-CUT edit must mass re-classify every brain. The
        // classifier (`archetype_refresh_system`) keys off `Changed<ShipStats>`, so reuse the
        // keep-health re-derive below: `force_rederive_keep_health` marks every `FitLayout`
        // changed → `recompute_ship_stats_system` rebuilds `ShipStats` → `Changed<ShipStats>`
        // fires → every archetype re-classifies against the new cuts (damage preserved). The
        // other AiTuning fields are read live each tick and need no trigger.
        let arch_cuts_changed = world.get_resource::<AiTuning>().is_some_and(|t| {
            t.arch_speed_hi != ai_tun.arch_speed_hi
                || t.arch_dps_hi != ai_tun.arch_dps_hi
                || t.arch_armor_hi != ai_tun.arch_armor_hi
        });
        world.insert_resource(tuning);
        world.insert_resource(sim);
        world.insert_resource(pen);
        world.insert_resource(shield);
        world.insert_resource(salvage);
        world.insert_resource(scaling);
        world.insert_resource(mining);
        world.insert_resource(cell_materials);
        world.insert_resource(ai_tun);
        if let Some(m) = modules {
            world.insert_resource(m);
        }
        if let Some(h) = hulls {
            world.insert_resource(h);
        }
        if (designs_changed || arch_cuts_changed) && !rederive {
            force_rederive_keep_health(world);
        }
    }
    if rederive {
        force_rederive_all(world);
    }
    // R43: apply a quick weapon-equip to the player ship's primary weapon slot (slot 3). Validated
    // install against the live hull + catalog; writing the `Fit` triggers `recompute_ship_stats_system`
    // (full re-derive next tick). Windowed-only — the player ship exists only on this embedded path.
    if let (Some(weapon_id), Some(ship)) = (equip_weapon, equip_ship) {
        state.equip_status = equip_module(world, ship, SlotId(3), weapon_id);
    }
    // R99 Phase B (B4) — apply a captured command/settings request to the selected SERVER ship.
    if let Some(req) = command_request {
        apply_command_request(world, req);
    }
    // R99 Phase B (B5) — apply a captured Team-join request: re-faction + reposition the player
    // ship live (mirrors net.rs auto-join), then persist the preference so it sticks at next join.
    if let Some(req) = team_request {
        let ship = net
            .as_ref()
            .and_then(|n| host.server.ship_entity_for(n.local_id));
        let world = host.server.world_mut();
        state.team_status = apply_team_join(world, ship, req.faction);
    }
    state.tuning_open = tuning_open;
    state.stats_open = stats_open;
}

/// R99 Phase B (B4) — apply a [`CommandRequest`] to the selected SERVER ship,
/// MERGING with any existing [`PlayerOrder`] so unrelated overrides survive
/// (clone-or-`settings_only`, set the one field, re-insert). "Clear" removes the
/// component entirely so the ship reverts to its scenario behaviour.
fn apply_command_request(world: &mut World, req: CommandRequest) {
    // Read-clone the current order (or a fresh settings-only) — the merge base.
    let merge = |world: &mut World, ship: Entity, f: &dyn Fn(&mut PlayerOrder)| {
        let mut order = world
            .get::<PlayerOrder>(ship)
            .cloned()
            .unwrap_or_else(PlayerOrder::settings_only);
        f(&mut order);
        world.entity_mut(ship).insert(order);
    };
    match req {
        CommandRequest::HoldHere(ship) => {
            // Hold/defend at the ship's CURRENT position; merge so style overrides survive.
            let pos = world.get::<Position>(ship).map(|p| p.0).unwrap_or_default();
            merge(world, ship, &|o| {
                o.kind = Some(OrderKind::HoldAt {
                    anchor: pos,
                    radius: HOLD_RADIUS,
                });
            });
        }
        CommandRequest::Clear(ship) => {
            world.entity_mut(ship).remove::<PlayerOrder>();
        }
        CommandRequest::SetProfile(ship, profile) => {
            merge(world, ship, &|o| o.profile = profile);
        }
        CommandRequest::SetStance(ship, stance) => {
            merge(world, ship, &|o| o.stance = stance);
        }
        CommandRequest::SetPosture(ship, posture) => {
            merge(world, ship, &|o| o.posture = posture);
        }
    }
}

/// R99 Phase B (B5) — live re-faction + reposition the player ship to `faction`'s
/// base (mirrors the `net.rs` auto-join ops: insert `Faction` + `Position`), and
/// persist `preferred_faction` (by `tint_tag`) so the choice is honoured at the next
/// join. Returns a short status for the panel.
fn apply_team_join(world: &mut World, ship: Option<Entity>, faction: Faction) -> String {
    let Some(ship) = ship else {
        return "no player ship to re-faction".to_string();
    };
    let Some(spawns) = world.get_resource::<FactionSpawns>().copied() else {
        return "no FactionSpawns in this scenario".to_string();
    };
    let mut entity = world.entity_mut(ship);
    entity.insert(faction);
    entity.insert(Position(spawns.for_faction(faction)));

    // Persist the preference: load the on-disk settings, set the tag, save back. Loading
    // (vs. rebuilding from live world resources) preserves whatever else is on disk.
    let mut dev = tuning_io::load_dev_settings();
    dev.preferred_faction = Some(faction.tint_tag());
    let side = match faction {
        Faction::Red => "Red",
        Faction::Blue => "Blue",
    };
    match tuning_io::save_dev_settings(&dev) {
        Ok(_) => format!("joined {side} (preference saved)"),
        Err(e) => format!("joined {side}, but save failed: {e}"),
    }
}

/// Refinement 43 — install `module_id` into `slot` on the live `ship`'s [`Fit`] (validated against the
/// live hull + catalog: hardpoint type/size + budget) and write it back so
/// [`recompute_ship_stats_system`](sim::fitting::recompute_ship_stats_system) re-derives next tick.
/// Returns a short status string for the dev panel. Windowed-only (the embedded-server player ship).
fn equip_module(world: &mut World, ship: Entity, slot: SlotId, module_id: ModuleId) -> String {
    let Some(mut fit) = world.get::<Fit>(ship).cloned() else {
        return "no Fit on player ship".to_string();
    };
    let Some(hulls) = world.get_resource::<HullCatalog>().cloned() else {
        return "no hull catalog".to_string();
    };
    let Some(catalog) = world.get_resource::<ModuleCatalog>().cloned() else {
        return "no module catalog".to_string();
    };
    let Some(hull) = hulls.get(fit.hull) else {
        return "ship hull not in catalog".to_string();
    };
    match fit.install_module(slot, module_id, hull, &catalog) {
        Ok(()) => {
            let name = catalog
                .get(module_id)
                .map(|m| m.name.clone())
                .unwrap_or_default();
            world.entity_mut(ship).insert(fit);
            format!("equipped {name} → slot {}", slot.0)
        }
        Err(rej) => format!("rejected: {rej:?}"),
    }
}
