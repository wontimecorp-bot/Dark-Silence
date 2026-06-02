# Data Model: Ship Fitting & Modules (E006)

**Scope**: In-memory `bevy_ecs` domain model for the data-driven Module/Hull/Fit system. There is **NO database, NO SQL, NO persistence, and NO migrations this epic** — `Storage = N/A`. Nothing here is written to disk; fit presets live in-memory and their durable save/load is **E004 (persistence)**. This document models the `sim` **content tables**, **ECS components**, and **derived resources** (fields, relationships, validation rules, invariants) that the fitting UI and the runtime systems read and write. It realizes **ADR-0008** (the unified, data-driven domain model: uniform Module stat block, 2D cell-grid hull, fit-layout-IS-the-hitbox/armor-map, power+CPU+mass budgets).

**Serde note**: New domain types derive `Serialize`/`Deserialize` (matching E001 AD-002, the E002 pattern) so they reuse cleanly when persistence (E004) and replication (E003) consume them — but they are **not** serialized or stored this epic. The derive is a seam, not a feature. `ModuleId`/`HullId`/`SlotId` are stable **content** ids (data-authored, wire- and save-safe); raw `Entity` ids are runtime-local and never persisted.

**Principle II placement**: All domain logic (Module, Hull, Hardpoint/Slot, Fit, FitValidation, ShipStats, hit-map) lives in the shared **`crates/sim`** crate — the unified ADR-0008 model — so a future server validates fits and derives stats on the same code path. The Bevy **client** crate adds **only** the fitting screen (render/input/preview UI); it carries no authoritative fit truth and never re-derives stats with a forked formula.

**Cell-grid granularity note (ADR-0008)**: the hull cell-grid is authored at **section granularity** now (a cell maps to a slot/section; coarse module/section destruction). The grid is **cell-upgrade-ready**: fine cell-by-cell destruction (E007+) is a later content/resolution upgrade on this same structure, **not** a data-model refactor. Connectivity/severing are E007 concerns and out of scope here.

## Reused E001/E002 `sim` Types (do NOT redefine)

These already exist in `crates/sim/` and are reused as-is.

| Type | Definition | Reuse in E006 |
|------|------------|---------------|
| `Position(Vec2)` / `Velocity(Vec2)` | `crates/sim/src/components.rs` | World-space hit-line tracing maps to/from local hull-grid coordinates |
| `Heading(f32)` / `AngularVelocity(f32)` | `crates/sim/src/components.rs` | Read by flight using the fit-derived turn rate; hull facing for arc transform |
| `Health(f32)` | `crates/sim/src/components.rs` | Reused as the **per-installed-module** health scalar in the hit-map (per-cell health, FR-019) |
| `Weapon { cooldown, fire_rate, muzzle_speed }` | `crates/sim/src/components.rs` | Now **populated from the installed weapon module(s)** instead of `Tuning` (FR-016); cooldown gate INV-03 unchanged |
| `Ship` (marker) | `crates/sim/src/components.rs` | The ship entity gains `Fit` + derived `ShipStats` alongside the existing marker |
| `Tuning` (resource) | `crates/sim/src/tuning.rs` | **Superseded as the per-ship flight source** by `ShipStats` (FR-014); see Integration. The flight-model **formulae** (force vs drag, angular inertia, `turn_power_share`) are reused unchanged. |
| `integrate`/`analytic`, `Physics` | `crates/sim/src/motion.rs`, `physics.rs` | Untouched; the E001 motion-equivalence keystone is not modified |

## Content Tables (data-driven, authored — FR-025)

Module and Hull definitions are **content rows**, not code. They are loaded into `sim` resources at startup (`ModuleCatalog`, `HullCatalog`) and are extensible without code changes (FR-025, `NEW-CONFIG`). A `Fit` references content by id.

### Module — uniform data-driven stat block (FR-001)

The atom of fitting. Every installed device — reactor, thruster, weapon, shield, armor, utility — is **one** uniform record. Effective-stat contribution is selected by `kind` + `specifics`.

| Field | Type | Constraints | Meaning |
|-------|------|-------------|---------|
| `id` | `ModuleId` (content id) | UNIQUE, stable | Catalog key referenced by a `Fit` |
| `kind` | `ModuleKind` (enum) | required | Selects which effective-stat this contributes (table below) |
| `power_gen` | `f32` | `>= 0.0` | Power **supplied** to the budget (reactors > 0; most modules 0) |
| `power_draw` | `f32` | `>= 0.0` | Power **consumed** from the budget |
| `cpu_draw` | `f32` | `>= 0.0` | CPU/control consumed from the budget |
| `mass` | `f32` | `> 0.0` | Contributes to total ship mass (∑ module mass → ship mass) |
| `heat` | `f32` | `>= 0.0` | Heat generated (authored now; thermal sim is E007/later — carried, not simulated) |
| `health_max` | `f32` | `> 0.0` | Max hit points of the installed module (seeds per-cell `Health` in the hit-map) |
| `hardpoint_type` | `HardpointType` (enum) | required | Gates which slot types accept this module (must match slot `slot_type`) |
| `hardpoint_size` | `SlotSize` (enum, ordered) | required | Must be `<= slot.size` (smaller fits a larger slot) |
| `specifics` | `ModuleSpecifics` (enum payload) | matches `kind` | Per-kind parameters (below) used to derive `ShipStats` |

Notes: `power_gen`/`power_draw` are separate so one reactor module supplies the budget all draws subtract from. `health_max` is content; the **live** per-module health lives in the hit-map (instance state), not in the catalog row.

#### `ModuleKind` and effective-stat contribution

| `ModuleKind` | `specifics` payload (fields) | Contributes to `ShipStats` |
|--------------|------------------------------|----------------------------|
| `Reactor` | *(none beyond `power_gen`)* | Adds to `power_supply` (the power budget capacity available to draws) |
| `Thruster` | `thrust_force: f32 >0`, `turn_torque: f32 >0`, `strafe_force: f32 >=0` | Sums into total thrust/torque → top speed & agility (against total mass) |
| `Weapon` | `muzzle_speed: f32 >0`, `fire_rate: f32 >0`, `damage: f32 >0` | Populates the `Weapon` component fire params (FR-016) |
| `Shield` | `shield_hp: f32 >=0`, `regen: f32 >=0` | Carried as defense data (consumed by E007 damage layers; not flight) |
| `Armor` | `armor_value: f32 >=0` | Carried as defense data (E007); its `mass` is the agility cost |
| `Utility` | `params: small typed map` | Generic; no flight/weapon contribution this epic (extensibility seam) |

`mass` is universal: **every** kind's `mass` sums into total ship mass regardless of `kind` (FR-015). `power_draw`/`cpu_draw` are likewise universal budget costs on every kind.

### Hull — 2D cell-grid chassis (FR-003, FR-004)

Designer-authored content (ADR-0008 neutral consequence: players fit modules, they do not build hull geometry).

| Field | Type | Constraints | Meaning |
|-------|------|-------------|---------|
| `id` | `HullId` (content id) | UNIQUE, stable | Catalog key referenced by a `Fit` |
| `name` | `String` | non-empty | Display name (e.g. "Fighter", "Corvette") |
| `grid_dims` | `(u16, u16)` `(cols, rows)` | both `> 0` | Cell-grid dimensions |
| `cells` | `[GridCell]` | each in-bounds, no dup coords | The authored set of occupiable cells (sparse: not every `cols×rows` need exist) |
| `power_capacity` | `f32` | `>= 0.0` | Power budget ceiling (base; reactor `power_gen` *supplies*, this is the structural cap) |
| `cpu_capacity` | `f32` | `> 0.0` | CPU/control budget ceiling |
| `mass_capacity` | `f32` | `> 0.0` | Max total fit mass the hull can carry |
| `hull_base_mass` | `f32` | `> 0.0` | Chassis mass added before modules (so empty hull mass > 0 — never zero) |
| `slots` | `[Slot]` | each at an in-bounds cell, ids unique within hull | Positional slot inventory (below) |

`GridCell`: `{ coord: (u16,u16) in-bounds, section: SectionId }` — cells group into **sections** (the coarse damage/occupancy unit). A `Slot` occupies one (later: a contiguous group of) cells in a section.

### Hardpoint / Slot — typed, sized, positioned mount (FR-004, FR-006/007, FR-020)

A `Slot` (a.k.a. hardpoint) is a typed, sized position on the hull grid. Slot `slot_type` + `size` gate which modules may be installed; weapon slots additionally derive a firing arc from position/facing.

| Field | Type | Constraints | Meaning |
|-------|------|-------------|---------|
| `id` | `SlotId` (local to hull) | UNIQUE within hull | Key in the `Fit` slot→module map |
| `slot_type` | `HardpointType` (enum) | required | Module `hardpoint_type` must equal this (FR-006) |
| `size` | `SlotSize` (enum, ordered S<M<L<XL) | required | Module `hardpoint_size` must be `<= size` (FR-007) |
| `coord` | `(u16,u16)` | in `grid_dims`, on an authored cell | Grid position (drives occlusion depth + arc) |
| `facing` | `f32` (radians) | wrapped `[0, 2π)` | Mount facing on the hull (drives arc center) |
| `is_weapon_mount` | `bool` | — | If true, exposes a derived `FiringArc` |

`HardpointType` enum: `Reactor`, `Thruster`, `Weapon`, `Shield`, `Armor`, `Utility` (parallels `ModuleKind`; a slot type accepts modules whose `hardpoint_type` matches). `SlotSize` enum (ordered): `Small` < `Medium` < `Large` < `XLarge`.

`FiringArc` (derived, weapon mounts only): `{ center: f32 (radians) = hull_heading + slot.facing, half_angle: f32 (0, π] }` — the angular coverage the weapon can engage. **Derived from the slot's position/facing on the hull** (FR-020); `half_angle` is a function of mount position (edge mounts → wider arc, centerline → narrower) per hull authoring. E006 **defines** the arc as fit data; **enforcement** (turret track / can-this-hit) is E007. Invariant: never `0` or `> π` (no zero-width or wrap-around arc).

### Seed content (FR-022, the scaling ladder)

Two seed hulls with scaling budgets/slots/mass, plus the ~6 module archetypes. Concrete numbers are tuning (set in `plan.md`/content); the **shape** is the contract: larger hull = more slots/power but greater base mass → lower agility (SC-005).

| Hull | grid_dims | power_cap | cpu_cap | mass_cap | base_mass | slots (type×size) | Role |
|------|-----------|-----------|---------|----------|-----------|-------------------|------|
| `fighter` | small (e.g. 5×5) | low | low | low | low | ~1 Reactor·S, ~2 Thruster·S, ~2 Weapon·S, ~1 Armor·S, ~1 Utility·S | Agile, few slots |
| `corvette` | larger (e.g. 9×9) | high | high | high | higher | ~2 Reactor·M, ~3 Thruster·M, ~4 Weapon·M, ~3 Armor·M, ~2 Utility·M | Tankier/more firepower, less agile |

| Module archetype | kind | key contribution | dominant cost axis |
|------------------|------|------------------|--------------------|
| `reactor_basic` | Reactor | + power_supply | mass |
| `thruster_basic` | Thruster | + thrust/torque | power_draw + mass |
| `autocannon` | Weapon | fire params | power_draw + cpu_draw |
| `shield_basic` | Shield | shield_hp/regen | power_draw + cpu_draw |
| `armor_plate` | Armor | armor_value | **mass** |
| `utility_basic` | Utility | (seam) | cpu_draw |

Tradeoff contract (FR-023, SC-005): a tank fit binds **mass/power** (armor heavy), a damage fit binds **CPU/power** (weapons heavy) — different axes — so no single fit maxes tank+damage+speed at once.

## Runtime Components & Resources (entity table — primary artifact)

| Entity | Kind | Crate | Fields (name: type, constraints) | Relationships | State Transitions |
|--------|------|-------|----------------------------------|---------------|-------------------|
| **Fit** | Component | `sim` | `hull: HullId`; `assignments: Map<SlotId, ModuleId>` (a slot maps to 0 or 1 module) | on the Ship entity; refs one Hull + N Modules by content id | empty (baseline) → populated; recomputes `ShipStats` + hit-map on every change |
| **ShipStats** | Component | `sim` | `top_speed: f32 (>=floor)`; `thrust_force: f32 (>=floor)`; `reverse_force: f32`; `strafe_force: f32`; `turn_torque: f32 (>=floor)`; `angular_drag: f32 (>0)`; `angular_inertia: f32 (>0)`; `turn_power_share: f32 (0..=1)`; `linear_drag: f32 (>0)`; `total_mass: f32 (>0)`; `power_supply: f32 (>=0)`; `power_draw: f32 (>=0)`; `cpu_draw: f32 (>=0)`; `can_fire: bool` | on the Ship entity; **derived from `Fit`**; read by flight/weapon systems | recomputed whenever `Fit` changes (per-entity; replaces global `Tuning` read, FR-014) |
| **FitValidation** | Component / value | `sim` | `power: BudgetUsage`; `cpu: BudgetUsage`; `mass: BudgetUsage`; `violations: [Violation]`; `valid: bool` | derived from `Fit` + its `Hull`; surfaced live to the fitting UI | recomputed on every `Fit` change; `valid = violations.is_empty()` |
| **FitLayout** (hit-map) | Component | `sim` | `hull: HullId`; `cells: Map<(u16,u16), CellOccupant>`; depth/occlusion ordering helper | on the Ship entity; the queryable hitbox/armor map E007 reads (FR-019) | rebuilt on `Fit` change; per-cell `health` mutated by E007 damage (this epic only *exposes* it) |
| **ModuleCatalog** | Resource (singleton) | `sim` | `modules: Map<ModuleId, Module>` | loaded from content at startup (FR-025); read by validation + derivation | immutable at runtime (content reload only) |
| **HullCatalog** | Resource (singleton) | `sim` | `hulls: Map<HullId, Hull>` | loaded from content at startup (FR-025) | immutable at runtime |
| **FitPreset** | value (in-memory) | `sim` | `name: String (non-empty)`; `fit: Fit` | named saved fit; reloadable onto a compatible hull (FR-024) | in-memory only this epic; **durable save/load is E004** |
| **FittingPreview** | resource/value | `client` | candidate `Fit` + its derived `ShipStats`/`FitValidation` + per-axis **deltas vs current** | client-only; sandbox before commit (FR-013) | recomputed per prospective change; never applied until commit |

`BudgetUsage`: `{ used: f32 (>=0), capacity: f32 (>=0), over: bool = used > capacity }` — the live readout per axis (FR-009). For power, `capacity = hull.power_capacity + Σ reactor.power_gen` and `used = Σ power_draw`.

`CellOccupant`: `{ slot: SlotId, module: Option<ModuleId>, health: f32 (>=0), section: SectionId, depth: u16 }` — what occupies a cell, its live health, and its **occlusion depth** (smaller depth = outer, encountered first along a hit line). Empty cells (no module) are still part of the armor map at their section's structural value.

### Enum value sets

| Enum | Variants | Notes |
|------|----------|-------|
| `ModuleKind` | `Reactor`, `Thruster`, `Weapon`, `Shield`, `Armor`, `Utility` | Seed archetypes; selects `specifics` payload + effective-stat contribution |
| `HardpointType` | `Reactor`, `Thruster`, `Weapon`, `Shield`, `Armor`, `Utility` | Slot/module type gate (must match for install, FR-006) |
| `SlotSize` | `Small` < `Medium` < `Large` < `XLarge` | **Ordered**; module size must be `<=` slot size (FR-007) |
| `Violation` | `OverBudget(Axis)`, `SlotTypeMismatch{slot, module}`, `SlotSizeMismatch{slot, module}` | The named reasons an invalid fit reports (FR-011) |
| `Axis` | `Power`, `Cpu`, `Mass` | Which budget a violation is on |

### Entity → component composition (which components co-occur)

| Logical entity | Required components | Notes |
|----------------|---------------------|-------|
| Fitted ship | `Ship`, `Fit`, `ShipStats`, `FitLayout`, `Weapon` (when a weapon module is installed), `Position`, `Velocity`, `Heading`, `AngularVelocity`, `Health`, `FlightAssist` (+ client `RenderInterp`) | `ShipStats` replaces the per-ship `Tuning` read; `Weapon` params come from the installed weapon module. Empty-hull ship still has `Fit`(empty) + floored `ShipStats`. |

## Relationships

| From | To | Cardinality | Mechanism | Requirement |
|------|----|-------------|-----------|-------------|
| Fit | Hull | N:1 | `Fit.hull: HullId` → `HullCatalog` | FR-002, FR-003 |
| Fit | Module | 1:N | `Fit.assignments: SlotId → ModuleId` → `ModuleCatalog` | FR-001, FR-005 |
| Hull | Slot | 1:N | `Hull.slots` (positional inventory) | FR-004 |
| Hull | GridCell | 1:N | `Hull.cells`; cells group into Sections | FR-003 |
| Slot | GridCell | 1:1 (now) / 1:N (cell-upgrade) | `Slot.coord`; coarse = one cell per slot now | FR-003, ADR-0008 |
| Slot | Module | 0..1 | one module installed per slot (or empty) | FR-005 |
| Fit | ShipStats | 1:1 derive | recomputed from the module set on change | FR-014 |
| Fit | FitValidation | 1:1 derive | per-axis usage + violations from module set vs Hull | FR-008, FR-011 |
| Fit | FitLayout (hit-map) | 1:1 derive | slot→cell occupancy + per-module health + depth | FR-018, FR-019 |
| Slot (weapon) | FiringArc | 1:1 derive | center+half-angle from position/facing | FR-020 |
| ShipStats | flight/weapon systems | 1:N read | per-entity; replaces global `Tuning` for the ship's flight + `Weapon` params | FR-014, FR-015, FR-016 |
| FitLayout | E007 damage system | 1:N read | queryable hit/armor map (cross-epic dependency contract) | FR-019, FR-021 |
| FitPreset | Fit | 1:1 | named in-memory copy; reload onto compatible hull | FR-024 |

## Derivation Rules (Fit → ShipStats) and graceful floors

Recomputed whenever `Fit` changes (per-entity). Defines how each effective stat is derived and the floors for crippled fits (FR-017 — never NaN/inf/divide-by-zero).

| Derived field | Formula (conceptual) | Floor / crippled-fit behavior |
|---------------|----------------------|-------------------------------|
| `total_mass` | `hull.hull_base_mass + Σ module.mass` | `>= hull_base_mass > 0` — never zero (no divide-by-zero in `accel = force/mass`) |
| `thrust_force` | `Σ thruster.thrust_force` | No thruster → `THRUST_FLOOR` (small > 0), not 0 → ship is near-immobile but `top_speed = floor/drag` is finite |
| `turn_torque` | `Σ thruster.turn_torque` | No thruster → `TORQUE_FLOOR > 0` |
| `strafe_force` / `reverse_force` | `Σ thruster.strafe_force` / fraction of thrust | floor to small `>0` / `>=0` |
| `linear_drag`, `angular_drag`, `angular_inertia`, `turn_power_share` | hull/base constants (reuse `Tuning` defaults) | constants `>0` (share in `0..=1`) so denominators are never 0 |
| `top_speed` | `thrust_force / linear_drag` (emergent terminal velocity, reusing the E002 flight model) | finite because both floors hold; **no hard clamp**, matches existing model |
| `power_supply` | `hull.power_capacity + Σ reactor.power_gen` | `>= hull.power_capacity >= 0` |
| `power_draw` / `cpu_draw` | `Σ module.power_draw` / `Σ module.cpu_draw` | `>= 0` |
| `can_fire` | `true` iff ≥1 Weapon module installed | No weapon module → `can_fire = false`; `weapon_fire_system` spawns nothing (FR-016) |
| `Weapon{fire_rate, muzzle_speed}` | from the installed weapon module's `specifics` (first/primary if multiple this epic) | absent → `Weapon` component not present / `can_fire=false` |

No-power note (FR-017 edge): a fit whose reactor is removed has `power_supply = hull.power_capacity` only; if draws exceed it the fit is **invalid** (over-budget), but a *valid* zero-reactor fit (no powered modules) still flies on floored thrust — crippled, not broken.

## Invariants & Validation Rules

Runtime invariants enforced by `sim` validation/derivation systems (not DB constraints — there is no DB).

| ID | Invariant | Where enforced | Requirement |
|----|-----------|----------------|-------------|
| INV-F01 | **Type match**: a module installs into a slot only if `module.hardpoint_type == slot.slot_type`; else `SlotTypeMismatch` | install / validation | FR-006 |
| INV-F02 | **Size fit**: install allowed only if `module.hardpoint_size <= slot.size`; else `SlotSizeMismatch` | install / validation | FR-007 |
| INV-F03 | **Budget non-exceedance (all three)**: `Σ power_draw <= power_supply`, `Σ cpu_draw <= cpu_capacity`, `total_mass <= mass_capacity`; exceeding **any one** axis → `OverBudget(axis)` and `valid=false` (commit blocked) | validation | FR-008, SC-001 |
| INV-F04 | **One module per slot**; a slot holds 0 or 1 module (no double-occupancy) | install | FR-005 |
| INV-F05 | **Empty hull = valid baseline** (`assignments` empty → `violations` empty → `valid=true`); flies on floors, unarmed/under-powered | validation init | FR-010, SC-002 |
| INV-F06 | **Remove frees budget**: removing a module subtracts its draws/mass; removing the reactor or only weapon yields a valid-but-crippled fit, not an invalid one | install/remove + derive | FR-005, SC-002, spec edge |
| INV-F07 | **No NaN/inf in ShipStats**: every denominator (`total_mass`, `linear_drag`, `angular_drag`, `angular_inertia`) is `> 0`; thrust/torque floored `> 0` (FR-017) | derivation | FR-017, SC-003 |
| INV-F08 | **Live recompute**: `FitValidation` + `ShipStats` + `FitLayout` are recomputed on every fit change so UI readouts and running-ship behavior reflect the current fit | fit-change system | FR-009, FR-014 |
| INV-F09 | **`valid == violations.is_empty()`**; an invalid fit names every violated rule (over-budget axis; mismatched slot + reason) | validation | FR-011, SC-001 |
| INV-F10 | **Occlusion order**: tracing a hit line yields cells in increasing `depth` (outer before inner); a fully-interior module is reached only after the covering cells along the line | FitLayout query | FR-018, FR-021, SC-004 |
| INV-F11 | **Hit-map completeness**: every authored cell reports its occupant (`CellOccupant`) and the installed module's live `health` for E007 to read | FitLayout build | FR-019, SC-004 |
| INV-F12 | **Firing arc derived + bounded**: each weapon mount exposes `FiringArc{center, half_angle}` from its position/facing; `half_angle ∈ (0, π]` (never zero-width or wrap-around) | arc derivation | FR-020, SC-004 |
| INV-F13 | **Catalog-id integrity**: every `ModuleId`/`HullId`/`SlotId` referenced by a `Fit` resolves in the catalogs / its hull; a dangling ref is a rejected fit | validation | FR-002, FR-025 |
| INV-F14 | **Total mass ≥ hull base mass > 0**; ship mass is the **sum of all module masses plus hull base** (ADR-0008) | derivation | FR-015 |

## Integration Points with existing `sim` (the `Tuning` rewire — `BREAKING-CHANGE`)

| Integration | Before (E002) | After (E006) |
|-------------|---------------|--------------|
| Flight source | `ship_motion_system` reads the **global `Tuning`** resource for `thrust_force`/`mass`/`turn_torque`/etc. | Reads the **per-entity `ShipStats`** component derived from the ship's `Fit` (FR-014); the flight-model formulae (force vs drag, `turn_power_factor`, `step_angular`, emergent top speed) are **unchanged** — only the input source moves from global → per-ship |
| Weapon params | `Weapon{fire_rate, muzzle_speed}` hand-set / from `Tuning` | Populated from the installed **weapon module**; `can_fire=false` (no weapon module) → `weapon_fire_system` spawns nothing (FR-016); cooldown gate INV-03 unchanged |
| Ship mass | `Tuning.mass` (global constant) | `ShipStats.total_mass = Σ module.mass + hull_base_mass` (FR-015); heavier fit → lower agility/accel, emergently |
| Hitbox | E002 single `CollisionRadius` / scalar `Health` | `FitLayout` is the **positional hitbox/armor map** (per-cell occupant + health + occlusion depth); coarse `CollisionRadius` may remain as a cheap broad-phase, but per-hit resolution reads `FitLayout` (FR-018/019) |
| Recompute trigger | n/a | A `sim` fit-change system re-derives `ShipStats`/`FitValidation`/`FitLayout` when `Fit` mutates (INV-F08) |
| `Tuning` role | Per-ship flight magnitudes | **Demoted** to the source of base/constant defaults (`linear_drag`, `angular_drag`, `angular_inertia`, `turn_power_share`) and the seed-fit baseline tuned to reproduce the E002 feel for an equivalent fit (risk mitigation: flight-feel regression) |

## Out of Scope (explicit non-data this epic)

| Excluded | Reason |
|----------|--------|
| SQL DDL / ER diagram / migrations / persistence | No database this epic; `Fit`/`FitPreset` are in-memory. Durable save/load is **E004**. |
| Damage resolution (penetration, defense-layer channels, destruction, severing, salvage) | **E007**; E006 only *produces* the hit/armor map + module health + arcs E007 reads. |
| Fine cell-by-cell ("eaten-away") destruction + grid connectivity/severing | Deferred (E007+); grid authored at section granularity, **cell-upgrade-ready**, not a refactor (ADR-0008). |
| Firing-arc **enforcement** (turret track, can-this-weapon-hit) | E006 *defines* arcs as data; combat/E007 enforces. |
| Acquiring ships/modules (markets, manufacture, loot) | **E013/E014**; modules are simply available to fit. |
| Exotic modules + synergy/side-effect engine + materials/research | **E014/E016**; ordinary modules only. |
| Full hull-class ladder + faction/race reskins | Content beyond the seed ladder; added later as data (FR-025). |
| Replication serialization of Fit/stats | Serde derives present as a seam (E003); not exercised this epic. |

## Data Model Summary (for plan.md)

Compact entity overview suitable for the plan's `## Data Model Summary` table:

| Entity | Key fields | Relationships | Notes |
|--------|-----------|---------------|-------|
| Module (content) | `id:ModuleId`, `kind:ModuleKind`, `power_gen/draw`, `cpu_draw`, `mass>0`, `health_max>0`, `hardpoint_type`, `hardpoint_size`, `specifics` | in `ModuleCatalog`; referenced by `Fit` | `sim`; uniform stat block (FR-001); data-driven (FR-025) |
| Hull (content) | `id:HullId`, `grid_dims`, `cells`, `power/cpu/mass_capacity`, `hull_base_mass>0`, `slots[]` | in `HullCatalog`; has N Slots/Cells | `sim`; 2D cell-grid; designer-authored; 2 seed hulls (fighter/corvette) |
| Slot/Hardpoint | `id:SlotId`, `slot_type:HardpointType`, `size:SlotSize`, `coord`, `facing`, `is_weapon_mount` | on Hull; accepts 0..1 Module; weapon → `FiringArc` | `sim`; type+size gate (INV-F01/02); arc derived from position (FR-020) |
| Fit | `hull:HullId`, `assignments:Map<SlotId,ModuleId>` | on Ship entity; refs 1 Hull + N Modules; derives ShipStats/Validation/Layout | `sim`; empty=valid baseline (INV-F05); the validated/saved unit |
| ShipStats | `top_speed`, `thrust_force`, `turn_torque`, `total_mass>0`, `power_supply/draw`, `cpu_draw`, `can_fire` | on Ship; derived from Fit; read by flight/weapon | `sim`; **replaces global `Tuning`** per-ship (FR-014); floored, never NaN (FR-017) |
| FitValidation | `power/cpu/mass:BudgetUsage`, `violations:[Violation]`, `valid:bool` | derived from Fit + Hull; surfaced live to UI | `sim`; budget non-exceedance (INV-F03); names violations (FR-011) |
| FitLayout (hit-map) | `cells:Map<coord,CellOccupant{slot,module,health,depth}>` | on Ship; read by E007 damage | `sim`; fit-layout-IS-hitbox/armor map (ADR-0008); occlusion order (INV-F10) |
| FitPreset | `name:String`, `fit:Fit` | named copy of a Fit | `sim`; in-memory only; durable save = E004 (FR-024) |
| FittingPreview (client) | candidate Fit + derived stats + per-axis deltas | client-only sandbox | `client`; before-commit preview (FR-013); no authoritative truth |

**Reused (E001/E002, do not redefine)**: `Position`/`Velocity`/`Heading`/`AngularVelocity`, `Health` (now per-module in hit-map), `Weapon` (now fit-populated), `Ship`, `integrate`/`analytic`, `Physics`; `Tuning` **demoted** to base-constant source + seed baseline.
