# Fitting API Contract — `sim` crate (E006)

The Ship Fitting & Modules domain lives in **`crates/sim`** (ADR-0008, Principle II), so its public surface is an **internal Rust / cross-crate API**, not an HTTP/REST/GraphQL endpoint — there is no network or web surface here. This contract is therefore a **catalog of types + function/trait signatures** (the protocol.md pattern), described conceptually: signatures are illustrative, not final Rust, and types are named at the domain level. It documents only the **cross-crate / cross-epic surface** that consumers depend on; internal helpers and the data-driven content schema (the plan/data-model concern) are out of band.

Consumers of this surface:

- **E007 damage system** (`sim` combat) — reads the hit-map / layout query API and the per-cell health map; E006 *produces* the map, E007 *resolves damage* against it.
- **Client fitting UI** (`crates/client`, NEW-UI) — drives `validate_fit` / install / remove / `budget_usage` and the preset API; renders live budget bars and before-commit deltas.
- **Running flight & weapon systems** (`sim`, E002→E006) — read `ShipStats` (the fit-derived effective-stats surface) in place of the global `Tuning` stand-in (BREAKING-CHANGE).
- **Future authoritative server** (E003+, networked integration) — owns Fit **validation** (`validate_fit`) and effective-stats **application** (`derive_ship_stats`); the client surface is identical, but at networked integration the server is authoritative and the client copy is advisory/predictive (Principle I; spec Compliance advisory).

All signatures reference only `sim`/`glam` domain types (`Vec2`, `f32`, the entities below) — no Bevy, no renet, no UI-toolkit type crosses this boundary, mirroring the `Physics`/`protocol` type-leak discipline.

## Domain types (the data the surface speaks)

These are the named entities the signatures below carry. Field-level schema and `serde`/`Component` derive discipline are a data-model concern; this catalog fixes only the surface contract.

| Type | Shape (conceptual) | Notes |
|------|--------------------|-------|
| `Module` | uniform stat block: `power` (gen/draw), `cpu` (control draw), `mass`, `heat`, `health`, `hardpoint: HardpointType`, `size: SlotSize`, device-specific attrs | FR-001; data-driven (FR-025); the atom of fitting |
| `HardpointType` | enum: `Reactor \| Thruster \| Weapon \| Shield \| Armor \| Utility \| …` | FR-006 type-match key; extensible as data |
| `SlotSize` | ordered scale (e.g. `Small < Medium < Large`) | FR-007 size-fit key (module size ≤ slot size) |
| `Hull` | 2D `CellGrid` + `Budgets` + `[Slot]` (slot inventory) | FR-003/004; designer-authored geometry (ADR-0008 neutral) |
| `Slot` | `{ id: SlotId, hardpoint: HardpointType, size: SlotSize, cells: [Cell], facing }` | FR-004; positional + typed + sized; `facing` drives weapon arc |
| `Fit` | `{ hull: HullId, installed: Map<SlotId, ModuleRef> }` | FR-002; the validated/saved/derived unit |
| `ModuleRef` | stable handle to an installed module (slot id + module id) | identity used by hit-map + UI; runtime-local like `ProjectileOwner` |
| `Budgets` | `{ power, cpu, mass }` capacities | FR-004; the per-hull tradeoff source |
| `Cell` | grid coordinate `(u, v)` on the hull | FR-003; the shared fitting/hit-map unit |
| `Arc` | `{ center_facing, half_angle }` (angular coverage) | FR-020; position-derived; defined here, enforced by E007 |

## 1. Fitting / validation API

Consumed by the **client fitting UI** and the **future server** (server-authoritative at networked integration). Pure, side-effect-free over a `Fit` value — the UI calls these per change for live readouts and before-commit previews; the server calls them as the authority gate.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `validate_fit` | UI + server → `sim` | `validate_fit(&Hull, &Fit) -> FitValidation` | FR-008/010/011. Per-axis budget usage (power/CPU/mass) **plus** the list of violations. Empty hull (no modules) ⇒ `valid` baseline. Server-authoritative at networked integration. |
| `install_module` | UI + server → `sim` | `install_module(&Hull, &mut Fit, SlotId, &Module) -> Result<(), FitRejection>` | FR-005/006/007. Places a module into a slot; rejects on type mismatch, size mismatch, or would-exceed-budget — never silently applied. |
| `remove_module` | UI + server → `sim` | `remove_module(&mut Fit, SlotId) -> Option<ModuleRef>` | FR-005. Removes the fitted module; frees its budget (SC-002). Removing a load-bearing module (reactor / only weapon) leaves a **valid-but-crippled** fit, not an invalid one. |
| `budget_usage` | UI + server → `sim` | `budget_usage(&Hull, &Fit) -> BudgetUsage` | FR-009. `{ power, cpu, mass }` each `{ used, capacity }` for live bars. For a before-commit delta, the UI calls it on the candidate fit (or a prospective copy) and diffs against the current (FR-013). |

Supporting result types:

- `FitValidation` — `{ valid: bool, usage: BudgetUsage, violations: [FitViolation] }`. `valid == violations.is_empty()`.
- `FitViolation` — `OverBudget { axis: BudgetAxis, used, capacity }` \| `SlotTypeMismatch { slot: SlotId, expected: HardpointType, got: HardpointType }` \| `SlotSizeMismatch { slot: SlotId, slot_size: SlotSize, module_size: SlotSize }`. Each names the offending rule (FR-011, SC-001).
- `FitRejection` — the install-time reason: same variants as `FitViolation` (`SlotTypeMismatch` \| `SlotSizeMismatch` \| `WouldExceedBudget { axis }`).
- `BudgetAxis` — `Power \| Cpu \| Mass`.
- `BudgetUsage` — `{ power: AxisUsage, cpu: AxisUsage, mass: AxisUsage }`; `AxisUsage = { used: f32, capacity: f32 }`.

**Invariants**: validation is total (every fit, including empty, yields a defined `FitValidation`); a fit that exceeds **any one** axis is invalid (FR-008); install is all-or-nothing (a rejected install leaves the `Fit` unchanged). **Ownership**: at networked integration the **server** is the validation authority — the client runs the identical functions for prediction/UX, but the server's `FitValidation` is canonical (Principle I).

## 2. Effective-stats API

Consumed by the **running ship's flight + weapon systems** inside `sim`. This is the BREAKING-CHANGE surface: fit-derived `ShipStats` **replaces** the global `Tuning` resource (FR-014).

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `derive_ship_stats` | flight/weapon systems + UI preview → `sim` | `derive_ship_stats(&Hull, &Fit) -> ShipStats` | FR-014/015/016/017. The fit-derived flight + weapon profile. Pure; also used by the UI to preview a candidate fit's stats without committing (FR-024). |

`ShipStats` (conceptual) carries the same magnitudes the flight model already consumes from `Tuning`, now *derived* from the fit:

- **Flight** — `thrust_force`, `reverse_force`, `strafe_force`, `mass` (total fit mass), `linear_drag`, `turn_torque`, `angular_drag`, `angular_inertia`, `turn_power_share`. Derived: thrust magnitudes sum from installed thruster modules; `mass` is the summed module mass over the hull base (FR-015 — more mass ⇒ lower agility/accel, more/better thrust ⇒ higher emergent top speed `thrust_force / linear_drag`).
- **Weapon** — `Option<WeaponProfile { muzzle_speed, fire_rate, … }>`. `None` when no weapon module is fitted ⇒ the ship cannot fire (FR-016). With a weapon module the profile is that module's fire parameters.

**Consumer rewire (BREAKING-CHANGE, FR-014)** — these existing `sim` systems read `ShipStats` instead of `Tuning`:

- `flight::ship_motion_system` — reads the flight magnitudes from `ShipStats` (today `Res<Tuning>`). Per-entity: each piloted ship's `ShipStats` is derived from *its* active fit (a component/lookup), not a single global resource — the server drives N independently-fitted ships in one shared step.
- `weapon::weapon_fire_system` — reads `WeaponProfile` (today the `Weapon` component's `fire_rate`/`muzzle_speed`); when the profile is `None`, the system fires nothing (FR-016).

**Invariants (graceful floors, FR-017, SC-003)**: `mass > 0` always (hull base mass is non-zero), so `accel = force / mass` is finite; a fit with no thruster degrades `thrust_force` toward a defined low floor (near-immobile, never `0`-divide or `NaN`/`inf`); `linear_drag`/`angular_drag` stay strictly positive so emergent top speed and turn rate are bounded. **Ownership**: the **server** derives and applies `ShipStats` to the authoritative ship at networked integration; the client derives the same for prediction.

## 3. Hit-map / layout query API (the E007 dependency contract)

The fit-layout **IS** the hitbox/armor map. Consumed by the **E007 damage system** (and combat). E006 *exposes* the map and resolves a hit to the first module struck; E007's penetration / defense-layer model reads through this surface. Pure queries over a realized `Fit`/`Hull`.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `module_at` | E007 → `sim` | `module_at(&Fit, Cell) -> Option<ModuleRef>` | FR-019. Per-cell occupant lookup; `None` for an empty cell. |
| `cell_map` | E007 → `sim` | `cell_map(&Fit) -> CellMap` | FR-019. The full occupant + health map: per occupied cell `{ module: ModuleRef, health: f32 }` (mirrors the `Health` component). The hitbox/armor map E007 consumes (SC-004). |
| `resolve_hit` | E007 → `sim` | `resolve_hit(&Fit, p0: Vec2, p1: Vec2) -> Option<HitResolution>` | FR-018/021. Resolves the FIRST module struck along the segment/ray, **outer-before-inner** along the line. Returns `{ module: ModuleRef, toi: f32, cell: Cell }` (`toi` mirrors `SweptHit`/`Physics::swept_cast`). |
| `hardpoint_arc` | E007/combat → `sim` | `hardpoint_arc(&Hull, SlotId) -> Option<Arc>` | FR-020. Position/facing-derived firing arc for a weapon hardpoint; `None` for a non-weapon slot. **Defined here as fit data; enforced by combat/E007**, not E006. |

Supporting types:

- `CellMap` — `Map<Cell, CellOccupant>`; `CellOccupant = { module: ModuleRef, health: f32 }`.
- `HitResolution` — `{ module: ModuleRef, toi: f32, cell: Cell }`. `toi ∈ [0, 1]` along `p0→p1`, consistent with `physics::SweptHit`.

**Invariants**: `resolve_hit` returns the module whose occupied cells the line enters **first** (outer modules shield inner — central placement protects, edge placement exposes, FR-021/SC-004); two fits differing only in module placement resolve the same shot to different depths (SC-004). `cell_map` health reflects the current module health (the value E007 mutates as it applies damage). **Ownership**: E006 owns map *construction* and *first-hit geometry*; E007 owns *what the hit does* (penetration, channels, destruction). E006 does not mutate health — it exposes it.

## 4. Presets API (client convenience)

Consumed by the **client fitting UI** only (FR-024, US5). Save/name/load a `Fit` and preview its derived stats without committing.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `save_preset` | UI → `sim` | `save_preset(name: &str, &Fit) -> PresetId` | FR-024. Persist a named fit for later reuse. |
| `load_preset` | UI → `sim` | `load_preset(PresetId, &Hull) -> Result<Fit, FitRejection>` | FR-024. Reload onto a **compatible** hull; rejects (type/size/budget) if the saved modules do not fit the target hull. |
| `preview_stats` | UI → `sim` | `preview_stats(&Hull, &Fit) -> (ShipStats, BudgetUsage)` | FR-024/SC-006. Show derived flight/weapon + budget stats **before** committing to a live ship — thin composition of `derive_ship_stats` + `budget_usage`, no live-ship mutation. |

**Invariant**: preview and preset load never touch a running ship's authoritative state — they operate on `Fit` values only. Preset *storage* backend (in-memory vs persisted) is a plan concern; the surface above is storage-agnostic.

## Out of scope (later epics)

- **Damage resolution & penetration itself** — typed-damage channels, defense-layer pipeline (avoidance→shields→armor→hull→systems), destruction, severing, salvage — **E007**. E006 only produces the hit-location/armor map + module health it reads, and *defines* (does not enforce) firing arcs.
- **Acquisition & markets** — how ships/modules are obtained (markets, manufacture, loot, the acquisition ladder) — **E013**. E006 assumes modules are simply available to fit.
- **Exotic modules & side-effect/synergy engine + materials/research chain** — **E014/E016**. E006 ships ordinary modules only.
- **Interest-management / replication of fit state** over the wire and the authoritative server process itself — **E003/E009**; this contract names server *ownership* of validation/derivation but not the transport.
