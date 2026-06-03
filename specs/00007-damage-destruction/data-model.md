# Data Model: Damage & Destruction (E007)

**Scope**: In-memory `bevy_ecs` domain model for the unified typed-damage pipeline, hit-location penetration, sectioned destruction + connectivity severing, and clean-sever salvage. There is **NO database, NO SQL, NO persistence, and NO migrations this epic** — `Storage = N/A`. Nothing here is written to disk. Wrecks, chunks, and salvage are **in-memory world entities** (`bevy_ecs` entities/components) that live in the running `sim` world; their durable save/load and economic value are **E004 (persistence)** and **E013 (salvage economy)**, not E007. This document models the `sim` **content tables** (the resistance matrix + armor/shield/penetration constants), **ECS components** (`Shields`, `SectionArmor`, `Wreck`, …), the **value types** that flow through the pipeline (`DamageEvent`, `PenetrationResult`, …), and their fields/relationships/invariants. It realizes **ADR-0008** (the unified, data-driven domain model — typed-damage channels × ordered defense layers, the fit-layout-IS-the-armor-map, coarse-now/cell-ready destructible hulls) and **ADR-0012** (grounded-but-gameplay-scaled magnitudes).

**Reuse, don't redefine**: E007 damages the **per-cell module/section health the E006 `FitLayout` already exposes** (`CellOccupant.health`). It does **not** invent a parallel health store. The hit geometry is resolved by the existing E006 `resolve_hit(&Fit, p0, p1, …) -> HitResolution`; the swept ray is the existing E001 `Physics::swept_cast`. `derive_ship_stats` is **extended** (not forked) so a module's health fraction scales its `ShipStats` contribution. See "Reused `sim` Types" below.

**Serde note**: New domain types derive `Serialize`/`Deserialize` (matching E001 AD-002, the E002/E006 pattern) so they reuse cleanly when persistence (E004) and replication (E003/E009) consume them — but they are **not** serialized or stored this epic. The derive is a seam, not a feature. `Channel`, the matrix, and the armor/shield constants are stable **content** (data-authored, tunable, wire/save-safe); raw `Entity` ids (a wreck's runtime entity, the `damaged_by` source) are runtime-local and never persisted.

**Principle II placement**: All damage/penetration/destruction/severing/salvage logic lives in the shared **`crates/sim`** crate — the unified ADR-0008 model — so the authoritative server resolves combat on the same code path the client predicts on. The Bevy **client** crate adds **only** the diegetic hit feedback (ricochet/penetrate/shield-absorb audio-visual cue, FR-024); it carries no authoritative damage truth and never re-resolves a hit with a forked formula. Resolution is server-authoritative (Principle I, FR-021), reusing the swept-ray CCD so fast projectiles cannot tunnel.

**Coarse-now / cell-ready note (ADR-0008)**: destruction is at **section / module granularity** now (a section reaches zero → removed; connectivity flood-fills the remaining section grid). The hull cell-grid is **cell-upgrade-ready**: fine per-cell ("eaten-away") destruction is a later content/resolution upgrade on this same `FitLayout`/`Hull` structure — **not** a data-model refactor.

## Reused E001/E002/E006 `sim` Types (do NOT redefine)

These already exist in `crates/sim/` and are reused as-is; E007 reads/mutates them, it does not duplicate them.

| Type | Definition | Reuse in E007 |
|------|------------|---------------|
| `FitLayout { hull, cells: CellMap }` | `sim/fitting/layout.rs` | **The damage target.** The queryable hitbox/armor map; E007 mutates per-cell `CellOccupant.health` as it applies post-penetration damage (FR-009/011) |
| `CellOccupant { slot, module, health, depth }` | `sim/fitting/layout.rs` | The per-cell live module health + occlusion depth; `health` is what E007 reduces (`0.0` = destroyed). `depth` already orders outer-before-inner (INV-F10) |
| `resolve_hit(&Fit, p0, p1, &Hull, &ModuleCatalog) -> Option<HitResolution>` | `sim/fitting/layout.rs` | Resolves the swept ray to the FIRST module struck (outer-before-inner); E007 routes surviving post-penetration damage to `HitResolution.module` (FR-002/009) |
| `cell_map(&Fit, &Hull, &ModuleCatalog) -> CellMap` / `module_at(…)` | `sim/fitting/layout.rs` | Per-cell occupant lookups the severing flood-fill + salvage walk over |
| `HitResolution { module: ModuleRef, toi, cell }` | `sim/fitting/layout.rs` | The entry-point module + cell a `DamageEvent` lands on |
| `Hull { id, grid_dims, cells: [GridCell], slots, … }`, `GridCell { coord, section }`, `SectionId` | `sim/fitting/hull.rs` | The section-grouped cell grid the connectivity flood-fill runs on; a section is the coarse destruction unit |
| `Module { kind, health_max, mass, specifics, … }`, `ModuleKind`, `ModuleSpecifics`, `ModuleId`, `ModuleRef` | `sim/fitting/{module,fit}.rs` | `health_max` seeds per-cell health; the salvaged-module identity is `ModuleRef`/`ModuleId`; `kind` selects which defense role a hit cell plays (Armor cell vs Systems cell) |
| `ShipStats` + `derive_ship_stats(&Hull, &Fit, &ModuleCatalog)` | `sim/fitting/stats.rs` | **Extended** so each module's `health/health_max` fraction scales its contribution (FR-012); `power_supply` drives `Shields.power_linked` (FR-013). Flight-model formulae unchanged |
| `Health(f32)` | `sim/components.rs` | **Degenerate-case** single-layer HP — kept on unfitted practice targets (dummies/asteroids) so E002/E003 targets still work without a `FitLayout` |
| `Damage(f32)`, `Weapon`, `Projectile`, `ProjectileOwner`, `Lifetime`, `PrevPosition` | `sim/components.rs` | The projectile carrying the hit; E007 builds a `DamageEvent` from the projectile + the `WeaponProfile` channel/pen instead of subtracting flat `Damage` |
| `Position`/`Velocity`/`Heading`/`AngularVelocity` | `sim/components.rs` | A `Wreck`/`Chunk` carries these (it is a physical body); a severed chunk **inherits** the parent's linear + angular velocity at its COM (FR-016) |
| `Physics::swept_cast`, `SweptHit`, `segment_circle_toi` | `sim/{physics,collision}.rs` | The swept-ray CCD the hit reuses (no tunnelling, FR-021); the COM/momentum math reuses the same glam-only deterministic style |
| `destruction_system`, `apply_damage`, `is_destroyed`, `HitFeedback` | `sim/combat.rs` | **`BREAKING-CHANGE`**: the whole-ship `Health`→despawn path is **replaced/extended** by per-module hit-location damage + emergent degradation; `destruction_system` becomes the fitted-ship destroy→wreck handler (degenerate single-`Health` targets keep the old despawn) |

## Content Tables (data-driven, authored — FR-022)

The damage-tuning data is **content**, not code: loaded into `sim` resources at startup, tunable without code changes (FR-022, `NEW-CONFIG`), gameplay-scaled per ADR-0012. The non-degenerate property (FR-023) is a **test-guarded** constraint on this content, not a code structure.

### Channel — the damage type (FR-001)

| Variant | Strong against (preferred target layer) | Notes |
|---------|------------------------------------------|-------|
| `Kinetic` | **Armor** | Slugs/penetrators; carries the penetration value + size (overmatch) |
| `ThermalEnergy` | **Shields** | Energy/laser; melts shields, plinks off armor |
| `Blast` | **Hull/Structure** | Explosive concussion; chews structural HP |
| `Em` | **Systems** (modules) | Ignores plating to disrupt the device behind |
| `Radiation` | **Systems / electronics** | Like `Em`; degrades the module/electronics behind cover |

`Channel` is a `Copy` enum (5 variants). Each channel has at least one layer it beats and each layer a channel it resists — the non-degenerate guard (FR-023, INV-D11).

### ResistanceMatrix — the (DefenseLayer × Channel) mitigation table (FR-004)

A flat-% mitigation lookup: `mitigation(layer, channel) -> f32 ∈ [0, 1)`. Strong-vs pairings are **low** mitigation (the channel gets through); resisted pairings are **high**. A `Resource` (const-seeded, tunable).

| Field | Type | Constraints | Meaning |
|-------|------|-------------|---------|
| `table` | `[[f32; CHANNELS=5]; LAYERS=4]` | each `∈ [0.0, MAX_MITIGATION < 1.0]` | Flat fraction of magnitude the layer removes for that channel (`1.0` is forbidden — no total immunity, INV-D02) |

`mitigation(layer, channel)` indexes `table[layer as usize][channel as usize]`. `surviving = magnitude * (1.0 - mitigation)`. Capped strictly `< 1.0` so no (layer, channel) cell is a free pass (FR-023). Shape (the contract; numbers are ADR-0012 tuning):

| Layer ↓ \ Channel → | Kinetic | ThermalEnergy | Blast | Em | Radiation |
|---------------------|---------|---------------|-------|----|-----------|
| **Shields** | high | **low** | mid | mid | mid |
| **Armor** | **low** | high | mid | mid | mid |
| **Hull/Structure** | mid | mid | **low** | mid | mid |
| **Systems** | mid | mid | mid | **low** | **low** |

### Penetration / armor / shield constants (FR-005/006/007/008/010)

A `Resource` of grounded-but-scaled (ADR-0012) tuning constants; data-driven (FR-022).

| Constant | Type | Constraints | Meaning |
|----------|------|-------------|---------|
| `ricochet_angle` | `f32` (radians) | `∈ (0, π/2)` | Impact angle past which a non-overmatching hit ricochets (FR-006; guaranteed-bounce band) |
| `overmatch_ratio` | `f32` | `> 0` | A hit overmatches when `pen_size >= overmatch_ratio * thickness` (FR-007; ignores angle) |
| `effective_armor_cap` | `f32` | `> 0`, finite | Upper clamp on `thickness / cos(angle)` so a near-grazing `cos→0` stays **finite** (INV-D03) |
| `pen_tier_full` | `f32` | `∈ (0, 1]` | Fraction of surviving damage a clean **Penetration** applies (e.g. ~0.33) |
| `pen_tier_over` | `f32` | `∈ (0, pen_tier_full)` | Fraction an **OverPenetration** applies (pass-through, reduced — e.g. ~0.1) |
| `pen_tier_non` | `f32` | `∈ [0, pen_tier_over)` | Fraction a **NonPenetration** applies to the armor only (little/none) |
| `material` (per `ArmorMaterial`) | small typed map | — | Per-material multiplier on nominal thickness (steel/composite/…); content seam |

`ArmorMaterial`: an enum (`Steel`, `Composite`, …) authored per section; the angle math reads `thickness * material_multiplier`.

### Shield tuning (FR-010)

| Constant | Type | Constraints | Meaning |
|----------|------|-------------|---------|
| `shield_regen_default` | `f32` | `>= 0` | Default regen/sec applied while powered (a fitted shield module's `regen` overrides) |
| `unpowered_decay` | `f32` | `>= 0` | Rate a shield depletes when `power_linked && !powered` (reactor lost → shields drop, FR-013) |

### Stat-scaling tuning (FR-012, emergent damage)

| Constant | Type | Constraints | Meaning |
|----------|------|-------------|---------|
| `stat_health_floor` | `f32` | `∈ [0, 1)` | The minimum contribution fraction a *damaged-but-alive* module keeps; `health_frac` is clamped to `[stat_health_floor, 1]` before scaling so a barely-alive thruster still gives *some* thrust (not a cliff). A **destroyed** (health `0`) module contributes nothing (binary off, FR-013) |

## The DefenseLayer model — the ordered stack (FR-003)

The ordered absorber stack a `DamageEvent` traverses: **`Shields → Armor → Hull/Structure → Systems`**. Each layer has per-target state and a per-channel resistance row in the matrix. The order is the in-scope subset of ADR-0008's full stack (the outer Avoidance/PD/ECM layer is E010; Crew is later).

| `DefenseLayer` variant | Per-target state lives in | Role |
|------------------------|---------------------------|------|
| `Shields` | `Shields` component (one per ship) | Regenerating power-linked pool; absorbs first; strong vs `ThermalEnergy`. Depleted/unpowered → exposes Armor |
| `Armor` | `SectionArmor` per hull section (`thickness`/`material`) | Angle-based penetration gate; strong vs `Kinetic` |
| `HullStructure` | `HullStructure` aggregate component (one per ship) | Structural HP backstop; strong vs `Blast` |
| `Systems` | E006 `FitLayout` / `CellOccupant.health` (the module behind) | The module struck; strong-resisted by `Em`/`Radiation` |

A `DamageEvent` is mitigated by Shields first; on shield depletion/penetration it meets Armor (angle math → `PenetrationResult`); a penetrating remainder hits Hull/Structure and the `Systems` module behind the entry point. The matrix mitigates the magnitude at **each** layer it passes.

## Pipeline value types (flow through the layers — not ECS components)

These are the packets/results computed during resolution; they are `Copy` values, not stored components.

### DamageEvent — the typed damage packet (FR-001)

The unit that flows the layers. Built from a projectile hit + its `WeaponProfile` channel/pen.

| Field | Type | Constraints | Meaning |
|-------|------|-------------|---------|
| `channel` | `Channel` | required | Damage type; selects the matrix row (FR-001) |
| `magnitude` | `f32` | `>= 0` | Base damage before any layer mitigation (FR-001) |
| `penetration` | `f32` | `>= 0` | Penetration value vs effective armor (FR-005/008) |
| `pen_size` | `f32` | `>= 0` | Penetrator size for the overmatch test vs plate thickness (FR-007) |
| `impact_point` | `Vec2` | finite | Where it struck (hull-local; the `resolve_hit` entry geometry) |
| `direction` | `Vec2` | finite, ~unit | Incoming direction; with the surface normal gives the impact angle (FR-005) |
| `source` | `Option<Entity>` | runtime-local (not serde) | The firing ship (`ProjectileOwner`); for single-resolution wreck claiming. Damage applies regardless of source (friendly fire) |

### PenetrationResult — the angle/armor calc outcome (FR-005/006/007/008)

The outcome of the armor gate; carries the effective armor and the surviving damage routed to the module behind.

| Variant | Carries | Meaning |
|---------|---------|---------|
| `Ricochet` | `effective_armor: f32` | Steep glancing hit past `ricochet_angle` (and not overmatched) → bounces, little/no damage (FR-006) |
| `NonPenetration` | `effective_armor: f32` | Hit the plate but `penetration < effective_armor` → only `pen_tier_non` applied to the armor; module behind untouched (FR-008) |
| `Penetration(tier)` | `effective_armor: f32`, `surviving: f32` | Clean pass-into → `surviving = pen_tier_full * post-matrix magnitude` routed to the module behind (FR-008/009) |
| `OverPenetration(tier)` | `effective_armor: f32`, `surviving: f32` | Pass-through → reduced `pen_tier_over` tier; shot exits, module behind takes the reduced remainder (FR-008) |

`effective_armor = clamp(thickness * material / cos(angle), 0, effective_armor_cap)` — finite even as `cos(angle) → 0` (INV-D03). Overmatch (`pen_size >= overmatch_ratio * thickness`) **bypasses** the angle/ricochet test and forces at least `Penetration` (FR-007).

## Runtime Components & Resources (entity table — primary artifact)

Downstream agents consume this table. `Crate` is `sim` unless noted; `client` types carry no authoritative truth.

| Entity | Kind | Crate | Fields (name: type, constraints) | Relationships | State Transitions |
|--------|------|-------|----------------------------------|---------------|-------------------|
| **Shields** | Component | `sim` | `current: f32 (0..=max)`; `max: f32 (>=0)`; `regen_rate: f32 (>=0)`; `power_linked: bool` | on the Ship entity; `max`/`regen` seeded from fitted Shield module(s); `powered` read from `ShipStats.power_supply >= power_draw` | regenerates while powered (`current → max`); depletes at `unpowered_decay` while `power_linked && !powered`; `current == 0` → armor exposed |
| **SectionArmor** | Component (map) | `sim` | `sections: Map<SectionId, ArmorFacet>` | on the Ship entity; keyed by the hull's `SectionId`s; the angle math reads this | per-section; a section's armor is consumed/removed when its section is destroyed (severing) |
| **HullStructure** | Component | `sim` | `current: f32 (0..=max)`; `max: f32 (>0)` | on the Ship entity; aggregate structural HP backstop | `current` reduced by Hull-routed `Blast`/spillover; `current == 0` contributes to ship-destroy (with core-sever, FR-015 edge) |
| **DamageContext** | Component | `sim` | the per-ship handle bundling `Shields` + `SectionArmor` + `HullStructure` + `FitLayout` for one resolution | on the Ship entity; the query the damage system reads | rebuilt/attached when the ship gains a `Fit` (alongside E006's `FitLayout`) |
| **Wreck** | Component | `sim` | `origin: WreckOrigin {DestroyedShip \| SeveredChunk}`; `contents: [SalvageItem]`; `claimed: bool` | a **persistent physical world entity** (carries `Position`/`Velocity`/`Heading`/`AngularVelocity` + a `FitLayout`/cell-grid); lootable; refs the salvageable `ModuleRef`s | spawned on ship-destroy or sever; `claimed` flips once (single-resolution, INV-D10); contents removed as looted (E013 consumes) |
| **Chunk** | (Wreck w/ `origin = SeveredChunk`) | `sim` | the severed-region cells + their `CellOccupant`s; inherited COM kinematics | a `Wreck` whose `origin` is a severed region; a sub-grid of the parent's cells | spawned by the severing flood-fill; drifts on inherited momentum (FR-016); is itself a lootable wreck |
| **ResistanceMatrix** | Resource (singleton) | `sim` | `table: [[f32;5];4]` (each `∈ [0, <1)`) | loaded from content (FR-022); read by every `DamageEvent` resolution | immutable at runtime (content reload only); test-guarded non-degenerate (FR-023) |
| **PenetrationConfig** | Resource (singleton) | `sim` | `ricochet_angle`, `overmatch_ratio`, `effective_armor_cap`, `pen_tier_{full,over,non}`, `material map` | loaded from content (FR-022); read by the armor gate | immutable at runtime |
| **ShieldConfig / StatScalingConfig** | Resource (singleton) | `sim` | `shield_regen_default`, `unpowered_decay`; `stat_health_floor` | loaded from content (FR-022) | immutable at runtime |
| **HitOutcome** | value / `HitFeedback` ext | `sim`→`client` | the legible result tag: `Ricochet \| Penetrated \| Absorbed \| Destroyed` + affected `ModuleRef`/layer | produced per hit; surfaced to the client cue (FR-024) | transient per resolution; decays like `HitFeedback` |

`ArmorFacet`: `{ thickness: f32 (>0), material: ArmorMaterial, normal: Vec2 (unit) }` — a section's nominal plate thickness, material, and outward face normal (the angle is `acos(direction · normal)`). Seeded from fitted Armor module(s) + the hull section authoring.

`SalvageItem`: the per-module loot a wreck yields (below). `WreckOrigin`: `DestroyedShip` (whole ship died) vs `SeveredChunk` (a disconnected region).

### Section / Module health — the E006 health, reduced by E007 (FR-011/012)

This is **not a new store** — it is the E006 `FitLayout.cells[coord].health` (`CellOccupant.health`), plus the per-section structural HP. E007 reduces it; `0.0` = destroyed.

| Datum | Where it lives | E007 action |
|-------|----------------|-------------|
| Per-module live health | `CellOccupant.health` (E006 `FitLayout`) | Reduced by post-penetration `surviving` damage routed to `HitResolution.module` (`apply_damage` clamp ≥ 0); `0.0` = module destroyed (FR-011) |
| Per-section structural HP | `SectionHealth { section: SectionId, current, max }` (new, small) | Aggregates a section's structural integrity (empty/structural cells); `0.0` → section destroyed → removed from layout → triggers connectivity check (FR-014/015/017) |

#### Emergent-damage link — health → `ShipStats` contribution (FR-012)

`derive_ship_stats` is **extended**: when summing a module's contribution it scales by its live health fraction.

```
health_frac = clamp(occupant.health / module.health_max, 0.0, 1.0)
scaled      = if health_frac == 0.0 { 0.0 }                         // destroyed = off (FR-013)
              else { contribution * clamp(health_frac, stat_health_floor, 1.0) }  // damaged = linear w/ floor
```

| Module kind | Healthy contribution | Damaged (health_frac) | Destroyed (health 0) |
|-------------|----------------------|------------------------|----------------------|
| `Thruster` | `thrust/torque/strafe` | scaled linearly (floored at `stat_health_floor`) → lower top speed/accel/agility (FR-012) | floor only / near-immobile, never NaN (reuses INV-F07 floors) |
| `Weapon` | populates `WeaponProfile`, `can_fire=true` | (this epic: binary — alive fires, destroyed cannot) | `can_fire=false`; profile dropped (FR-013) |
| `Reactor` | `+power_supply` | scaled `power_gen` (less power) | `power_gen=0` → power budget collapses → `Shields.powered=false` (FR-013) |
| `Shield`/`Armor` | defense data | reduced shield max / armor as section damaged | removed; layer exposed |

The contribution-scaling is **linear with a floor** (`stat_health_floor`) so degradation is felt continuously, not a cliff; a **destroyed** module is a hard `0` (binary disable). All existing `ShipStats` floors (INV-F07) still hold — a fully-crippled fit stays finite, never NaN/inf/divide-by-zero.

### Wreck / Chunk — the persistent physical world entity (FR-015/016/020)

A `Wreck` is a destroyed ship **or** a severed disconnected hull region: a persistent, lootable physical body in the `sim` world, **not** a DB row.

| Aspect | Detail |
|--------|--------|
| Physical | Carries `Position`/`Velocity`/`Heading`/`AngularVelocity` — a real body in the sim. A severed `Chunk` **inherits** the parent ship's linear + angular velocity evaluated at the chunk's center of mass (FR-016) — it drifts, never pops at zero velocity |
| Cell-grid-ready | Carries its slice of the `FitLayout` cell-grid (the severed cells + their `CellOccupant`s) so a chunk is re-targetable/re-salvageable, and a destroyed-ship wreck keeps its module layout |
| Salvageable contents | `contents: [SalvageItem]` — the modules that were on the destroyed/severed region + their intact-vs-scrap state (below) |
| Lootable + claim | `claimed: bool`; claiming is single-resolution — no double-claim (INV-D10). An over-killed ship still leaves ≥ scrap (never zero loot, INV-D09) |
| Connectivity | The flood-fill that produces chunks runs **only on a destruction event** (a section reached `0`), never per frame (FR-017, INV-D08) |

### Salvage outcome — intact module vs scrap (FR-018/019)

Per module on a wreck/chunk: the loot it yields, decided at the moment its surrounding structure severs.

| `SalvageItem` variant | Carries | When (boundary) |
|-----------------------|---------|------------------|
| `IntactModule(ModuleRef)` | the module's content id + identity | **Clean sever**: the module's own `health >= INTACT_THRESHOLD` and its surrounding structure was severed away (not penetrated through). Re-equippable, operational (FR-018) |
| `Scrap(amount: f32)` | a scalar scrap quantity (`> 0`) | The module was **destroyed or penetrated-through** (own `health < INTACT_THRESHOLD`, in the limit `0`). Yields scrap, not an intact module (FR-019). An over-kill still yields `Scrap` ≥ a minimum (never nothing, INV-D09) |

`INTACT_THRESHOLD`: a content constant (`∈ (0, health_max]`) — the clean-sever vs scrap boundary (FR-018 edge). At/above → intact; below → scrap. "Blast it apart" (through-kill) must not strictly beat careful clean-sever (it yields only scrap), preserving the precision playstyle.

### Enum value sets

| Enum | Variants | Notes |
|------|----------|-------|
| `Channel` | `Kinetic`, `ThermalEnergy`, `Blast`, `Em`, `Radiation` | The 5-channel matrix axis (FR-001); each strong vs one layer (INV-D11) |
| `DefenseLayer` | `Shields`, `Armor`, `HullStructure`, `Systems` | The ordered stack (FR-003); the matrix's other axis (4 layers) |
| `PenetrationResult` | `Ricochet`, `NonPenetration`, `Penetration(tier)`, `OverPenetration(tier)` | The armor-gate outcome (FR-008); tiers carry `surviving` damage |
| `WreckOrigin` | `DestroyedShip`, `SeveredChunk` | Why the wreck exists (FR-020) |
| `SalvageItem` | `IntactModule(ModuleRef)`, `Scrap(f32)` | The two-tier loot outcome (FR-018/019) |
| `ArmorMaterial` | `Steel`, `Composite`, … | Per-section plate material; multiplier on thickness (content seam) |
| `HitOutcome` | `Ricochet`, `Penetrated`, `Absorbed`, `Destroyed` | The legible diegetic cue tag (FR-024) |

### Entity → component composition (which components co-occur)

| Logical entity | Required components | Notes |
|----------------|---------------------|-------|
| Fitted combat ship | E006: `Ship`, `Fit`, `ShipStats`, `FitLayout`, `Position`, `Velocity`, `Heading`, `AngularVelocity`; **+ E007**: `Shields`, `SectionArmor`, `HullStructure`, `SectionHealth`(per section), `DamageContext` | The E006 fitted ship **gains** the defense-layer state. `FitLayout` health is the damage target. `ShipStats` is re-derived from health (FR-012). The whole-ship `Health` is **dropped** for fitted ships (BREAKING-CHANGE) |
| Unfitted practice target (degenerate) | E002: `Target`, `TargetKind`, `Health`, `CollisionRadius`, `Position`, `Velocity` | **No `FitLayout`** → falls back to the **single-layer `Health`** path (one degenerate layer): a `DamageEvent` collapses to flat `apply_damage` on `Health`; dummies/asteroids keep working so E002/E003 are not broken |
| Wreck / Chunk | `Wreck`, `Position`, `Velocity`, `Heading`, `AngularVelocity`, (slice of) `FitLayout` | Persistent physical lootable body; chunk inherits COM momentum (FR-016) |

## Relationships

| From | To | Cardinality | Mechanism | Requirement |
|------|----|-------------|-----------|-------------|
| DamageEvent | Channel | N:1 | `DamageEvent.channel` selects the matrix row | FR-001, FR-004 |
| DamageEvent | DefenseLayer stack | 1:N traversal | Shields → Armor → Hull → Systems, mitigated at each | FR-003 |
| ResistanceMatrix | (DefenseLayer × Channel) | 1:1 lookup | `table[layer][channel]` flat-% | FR-004, FR-023 |
| Armor gate | PenetrationResult | 1:1 | angle/thickness/overmatch → one outcome | FR-005/006/007/008 |
| PenetrationResult(Penetration/OverPen) | CellOccupant (module behind) | 1:1 route | `surviving` damage → `resolve_hit(...).module` cell health | FR-009, FR-011 |
| Ship | Shields | 1:1 | `Shields` component; `max`/`regen` from fitted Shield modules | FR-010 |
| Ship | SectionArmor | 1:1 (N facets) | `SectionArmor.sections` keyed by hull `SectionId` | FR-005 |
| Ship | HullStructure | 1:1 | aggregate structural HP backstop | FR-003 |
| Shields | ShipStats.power_supply | 1:1 read | `power_linked` shields powered iff reactor supplies power | FR-010, FR-013 |
| CellOccupant.health | ShipStats contribution | 1:1 scale | `derive_ship_stats` extended: `health_frac` scales the module's stat | FR-012 |
| SectionHealth (=0) | connectivity flood-fill | 1:N | destruction event → flood-fill on remaining section grid | FR-014/015/017 |
| flood-fill | Chunk(s) | 1:N | disconnected region(s) → severed wreck chunk(s) | FR-015/016 |
| Chunk | parent COM kinematics | 1:1 inherit | linear + angular velocity at the chunk's center of mass | FR-016 |
| Ship-destroy / Chunk | Wreck | 1:1 | persistent lootable physical entity | FR-020 |
| Wreck | SalvageItem | 1:N | per-module intact vs scrap contents | FR-018/019 |
| SalvageItem(Intact) | ModuleRef | 1:1 | the re-equippable module identity (E006 `ModuleRef`) | FR-018 |
| DamageEvent | HitOutcome | 1:1 | the legible resolution tag for the client cue | FR-024 |
| Unfitted Target | Health | 1:1 (degenerate) | no `FitLayout` → single-layer flat damage | spec edge (E002 compat) |

## Resolution Order (how a hit traverses — for the implementer)

The damage system, given a projectile hit on a fitted ship, runs (server-authoritative, FR-021):

1. **Geometry** — reuse `resolve_hit(&Fit, prev_pos, pos, &Hull, &ModuleCatalog)` (E006 + E001 swept ray) → `HitResolution { module, toi, cell }` (entry point, outer-before-inner).
2. **Build `DamageEvent`** — channel/magnitude/pen/pen_size from the `WeaponProfile`; `impact_point`/`direction` from the hit geometry; `source = ProjectileOwner`.
3. **Shields** — if `Shields.current > 0` (powered or residual): mitigate `magnitude` by `matrix(Shields, channel)`, subtract from `current`; if shields absorb it all → `HitOutcome::Absorbed`, stop. Depleted/unpowered shields are skipped (armor exposed).
4. **Armor** — read the entry section's `ArmorFacet`; compute angle from `direction · normal`; `effective_armor = clamp(thickness·material / cos(angle), 0, cap)`; apply matrix(Armor, channel) to the magnitude; run the gate → `PenetrationResult` (Ricochet/Non/Pen/OverPen, with overmatch bypass).
5. **Hull/Systems** — a penetrating `surviving` is mitigated by matrix(Hull, channel) then matrix(Systems, channel) and routed to `CellOccupant.health` of `HitResolution.module` (`apply_damage`); spillover/`NonPenetration` damages `HullStructure`/`SectionHealth`.
6. **Destruction** — if a module/section hits `0`: mark destroyed, remove from layout, and (section case) run the connectivity flood-fill (FR-015/017); re-derive `ShipStats` (FR-012). Ship core severed or destroyed → spawn `Wreck` with `SalvageItem` contents.
7. **Feedback** — emit `HitOutcome` for the client cue (FR-024).

## Invariants & Validation Rules

Runtime invariants enforced by `sim` damage/destruction systems (not DB constraints — there is no DB).

| ID | Invariant | Where enforced | Requirement |
|----|-----------|----------------|-------------|
| INV-D01 | **Health bounds**: every health (`CellOccupant.health`, `Shields.current`, `HullStructure.current`, `SectionHealth.current`) is clamped `>= 0` (reuses `apply_damage`); `0.0` = destroyed (the `<= 0` destroy boundary, reused from E002) | damage application | FR-011 |
| INV-D02 | **Bounded resistance**: every `matrix(layer, channel) ∈ [0.0, MAX_MITIGATION < 1.0]` — no cell is `1.0` (total immunity) or `< 0` (amplification); a layer always lets *some* damage through | matrix load/validate | FR-004, FR-023 |
| INV-D03 | **Finite effective armor**: `effective_armor = clamp(thickness·material / cos(angle), 0, effective_armor_cap)` — never `inf`/NaN as `cos(angle) → 0` (grazing hit) (cap finite, divisor floored) | armor gate | FR-005 |
| INV-D04 | **Overmatch bypasses angle**: `pen_size >= overmatch_ratio · thickness` forces at least `Penetration` and skips the ricochet/angle test (a large hit on thin plate cannot ricochet) | armor gate | FR-007 |
| INV-D05 | **Penetration tier ordering**: `pen_tier_non < pen_tier_over < pen_tier_full <= 1.0` — overpen is strictly weaker than full pen, non-pen weakest (no binary one-shot/nothing) | tier apply | FR-008 |
| INV-D06 | **Surviving damage routes behind**: post-penetration `surviving` damage is applied to the module occupying the cell **behind** the entry point (`resolve_hit` outer-before-inner, INV-F10) — a buried module is reached only after its covers | resolution step 5 | FR-009 |
| INV-D07 | **Momentum conservation on sever**: a severed chunk inherits the parent's linear + angular velocity evaluated **at the chunk's center of mass** — total linear (and angular about the parent COM) momentum is conserved across the split (no zero-velocity pop, no energy injection) | severing | FR-016 |
| INV-D08 | **Connectivity only on destruction**: the flood-fill island detection runs **only** when a section reaches `0` health, never per frame | destruction handler | FR-017 |
| INV-D09 | **Over-kill leaves ≥ scrap**: a destroyed/over-killed ship or section always yields at least one `Scrap` item (`amount > 0`) — never zero loot; no dangling un-targetable orphan cell (a lone disconnected cell severs as a chunk or is absorbed) | wreck spawn / sever | FR-020, spec edge |
| INV-D10 | **Single-resolution claim**: a wreck is claimed exactly once (`claimed` flips once); no double-claim across sources; damage applies regardless of source (friendly fire) but loot is single-resolution | wreck claim | spec edge (friendly salvage) |
| INV-D11 | **Non-degenerate matrix**: every `Channel` has a layer it beats (low mitigation) and every `DefenseLayer` has a channel it resists (high mitigation) — no globally dominant channel, no single-channel-bypassed layer (effective-HP curves cross); a **test-guarded** property on the content matrix | matrix test (CI) | FR-023 |
| INV-D12 | **Clean-sever vs scrap boundary**: at the moment its surrounding structure severs, a module yields `IntactModule` iff its own `health >= INTACT_THRESHOLD`, else `Scrap`; a through-killed (penetrated, health below threshold) module never yields an intact module | salvage decision | FR-018, FR-019 |
| INV-D13 | **Emergent-stat floor & binary-disable**: a *damaged-but-alive* module scales its `ShipStats` contribution linearly, clamped to `[stat_health_floor, 1]` (never NaN/inf, reuses INV-F07 floors); a *destroyed* module (health `0`) contributes exactly `0` (binary off) | `derive_ship_stats` (extended) | FR-012, FR-013 |
| INV-D14 | **Shield power-link**: `power_linked` shields regenerate (`current → max` at `regen_rate`) only while powered (`ShipStats.power_supply >= power_draw`); on power loss they deplete at `unpowered_decay` and, at `0`, expose Armor | shield system | FR-010, FR-013 |
| INV-D15 | **Core-sever destroys the ship**: if a destroyed section disconnects the ship's core/command section, the ship is destroyed → a single persistent `Wreck` (no orphaned ghost ship) | connectivity handler | FR-015, spec edge |
| INV-D16 | **Server-authoritative + no-tunnel**: all damage/penetration/destruction/severing/salvage resolves in the shared `sim`/server, reusing the swept-ray CCD (`Physics::swept_cast`) so a fast projectile cannot tunnel; the client never re-resolves authoritatively | resolution (all) | FR-021 |
| INV-D17 | **Degenerate single-layer target**: an entity **without** a `FitLayout` (unfitted dummy/asteroid) resolves a `DamageEvent` as flat `apply_damage` on its single `Health` — E002/E003 targets keep working; this is the one-layer degenerate case of the stack | resolution dispatch | spec edge (E002 compat) |

## Integration Points with existing `sim` (the combat rewire — `BREAKING-CHANGE`)

| Integration | Before (E002/E006) | After (E007) |
|-------------|---------------------|--------------|
| Hit resolution | `collision_detect_system` swept-casts the projectile vs each `Target` `CollisionRadius`, then flat `apply_damage` on whole-ship `Health` | For a **fitted** ship: swept-cast → `resolve_hit` (E006) → build a `DamageEvent` → traverse Shields→Armor→Hull→Systems → per-module `CellOccupant.health`. For an **unfitted** target: the old flat-`Health` path (degenerate single layer, INV-D17) |
| Health model | whole-ship `Health(f32)` → despawn at `<= 0` | **Per-module/section health** on the `FitLayout` (`BREAKING-CHANGE`). `Health` retained only on unfitted degenerate targets |
| Destruction | `destruction_system` despawns any `Health <= 0` entity | Extended: a destroyed **module/section** is removed from the layout + triggers connectivity (FR-014/015/017); a destroyed **ship** (core severed/structure gone) spawns a `Wreck` with salvage. Degenerate targets keep the despawn |
| Ship stats | `derive_ship_stats` sums each module's full contribution | **Extended**: scales each contribution by `health_frac` (FR-012); destroyed module = `0` (FR-013). Flight-model formulae + all INV-F07 floors unchanged |
| Power → shields | n/a | `Shields.power_linked` reads `ShipStats.power_supply`; a destroyed reactor drops power → shields decay/drop (FR-013/INV-D14) |
| Weapon fire-gate | `ShipStats.can_fire` from "≥1 weapon module installed" | Now also "≥1 weapon module **alive**"; a destroyed weapon module drops `can_fire`/`WeaponProfile` (FR-013) |
| Physics / severing | `Physics`/`swept_cast`, `elastic_velocities` for ship↔asteroid rams | Reused: the swept ray for the hit (no tunnel); the COM-momentum inheritance for severed chunks reuses the same glam-only deterministic style (FR-016) |
| Feedback | `HitFeedback { hit_flash, destroy_flash }` resource | Extended with a `HitOutcome` tag (ricochet/penetrate/absorb/destroy + affected module/layer) for the diegetic cue (FR-024) — presentation only, no authority |

## Out of Scope (explicit non-data this epic)

| Excluded | Reason |
|----------|--------|
| SQL DDL / ER diagram / migrations / persistence | **No database this epic** (`Storage = N/A`). Wrecks/chunks/salvage are in-memory `sim` world entities; durable save/load is **E004** |
| Salvage **economy** (markets, refining, value, currency) | **E013**; E007 only emits the wreck/salvage *entities* + the intact-vs-scrap outcome |
| Crew layer + the outer Avoidance / point-defense / signature / ECM layer | Crew is a later embodiment epic; avoidance/PD/ECM is **E010**. E007's outermost layer is `Shields`, innermost is `Systems` |
| Deferred penetration mechanics (per-shell normalization, multi-layer "armor cake", per-section saturation, fuzing, shatter, channel-specific pen coefficients) | The pipeline is data-driven so these layer on without a rewrite (FR-022) |
| Damage-over-time (fire/breach/leak) + cascade chains beyond "destroyed reactor → power loss" (ammo cook-off, chained reactor breach) | Later epics |
| New weapon delivery types (missiles/torpedoes/mines/drones, guidance + explosion fidelity) | Later; E007 damages via the existing E002 fixed-forward swept projectile |
| Fine per-cell ("eaten-away") destruction | Coarse module/section granularity now; the grid is cell-ready, so finer is a content/resolution upgrade, **not** a data-model refactor (ADR-0008) |
| AOI/scaled networked replication of destruction at population | **E009**; E007 resolves in the single-node authoritative server (E003) |
| Replication serialization of the new types | Serde derives present as a seam (E003/E009); not exercised this epic |

## Data Model Summary (for plan.md)

Compact entity overview suitable for the plan's `## Data Model Summary` table:

| Entity | Key fields | Relationships | Notes |
|--------|-----------|---------------|-------|
| DamageEvent (value) | `channel:Channel`, `magnitude>=0`, `penetration>=0`, `pen_size>=0`, `impact_point`, `direction`, `source` | flows Shields→Armor→Hull→Systems; selects matrix row | `sim`; the typed packet (FR-001); built from a projectile + `WeaponProfile` |
| Channel (enum) | `Kinetic`/`ThermalEnergy`/`Blast`/`Em`/`Radiation` | matrix axis; each strong vs one layer | `sim`; non-degenerate (FR-023, INV-D11) |
| ResistanceMatrix (resource) | `table:[[f32;5];4]`, each `∈[0,<1)` | (DefenseLayer × Channel) lookup | `sim`; data-driven (FR-022); test-guarded crossing curves (FR-023) |
| PenetrationResult (enum) | `Ricochet`/`NonPenetration`/`Penetration(tier,surviving)`/`OverPenetration(tier,surviving)` + `effective_armor` | armor gate → routes `surviving` to module behind | `sim`; angle/overmatch outcome (FR-005/006/007/008); finite eff-armor (INV-D03) |
| Shields (component) | `current(0..=max)`, `max>=0`, `regen_rate>=0`, `power_linked` | on Ship; powered from `ShipStats.power_supply` | `sim`; regen while powered, decay/expose-armor unpowered (FR-010/INV-D14) |
| SectionArmor (component) | `sections:Map<SectionId,ArmorFacet{thickness,material,normal}>` | on Ship; angle math reads this | `sim`; per-section plate (FR-005); `Kinetic`-resistant |
| HullStructure (component) | `current(0..=max)`, `max>0` | on Ship; structural backstop | `sim`; `Blast`-resistant; aggregate HP (FR-003) |
| Section/Module health | **E006 `CellOccupant.health`** (+ new `SectionHealth`) | reduced by E007; scales `ShipStats` | `sim`; **NOT a new store** — the E006 hit-map health; `0`=destroyed (FR-011/012) |
| Wreck / Chunk (component+entity) | `origin:WreckOrigin`, `contents:[SalvageItem]`, `claimed`; +`Position/Velocity/Heading/AngularVelocity`+cell-grid | persistent physical body; chunk inherits COM momentum | `sim`; in-memory world entity (not DB); lootable (FR-015/016/020) |
| SalvageItem (enum) | `IntactModule(ModuleRef)` \| `Scrap(f32>0)` | per module on a wreck | `sim`; clean-sever (≥`INTACT_THRESHOLD`) vs through-kill; over-kill ≥ scrap (FR-018/019, INV-D09/D12) |

**Reused (E001/E002/E006, do not redefine)**: `FitLayout`/`CellOccupant.health` (the damage **target**), `resolve_hit`/`cell_map`/`HitResolution`/`ModuleRef`, `Hull`/`GridCell`/`SectionId`, `Module`/`ModuleKind`/`health_max`, `ShipStats`/`derive_ship_stats` (**extended** for emergent damage), `Health` (kept only on degenerate unfitted targets), `Damage`/`Weapon`/`Projectile`, `Position`/`Velocity`/`Heading`/`AngularVelocity`, `Physics::swept_cast`/`SweptHit`, `destruction_system`/`apply_damage`/`HitFeedback` (**replaced/extended**, `BREAKING-CHANGE`).
