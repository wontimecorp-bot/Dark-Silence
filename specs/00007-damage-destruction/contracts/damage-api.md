# Damage API Contract — `sim` crate (E007)

The Damage & Destruction domain lives in **`crates/sim`** (ADR-0008/0012, Principle II), so its public surface is an **internal Rust / cross-crate API**, not an HTTP/REST/GraphQL endpoint — there is no network or web surface here. This contract is therefore a **catalog of types + function/trait signatures** (the `protocol.md` / `fitting-api.md` pattern): signatures are illustrative, not final Rust, and types are named at the domain level. It documents only the **cross-crate / cross-epic surface** that consumers depend on; internal helpers and the data-driven balance schema (the resistance-matrix / penetration / shield content — a plan/data-model/`NEW-CONFIG` concern) are out of band.

Resolution is **server-authoritative** (FR-021, Principle I): the shared `sim` runs the whole pipeline inside the E003 single-node authoritative server; the client never decides an outcome (the legibility/feedback surface, FR-024, is presentation-only). Every signature below references only `sim`/`glam` domain types (`Vec2`, `f32`, `Entity`, the entities below) — no Bevy app/render type, no `renet`, no UI-toolkit type crosses this boundary, mirroring the `Physics`/`protocol`/`fitting` type-leak discipline.

What this surface **reuses vs. introduces**:

- **Reads (does not redefine) the E006 surface** — `resolve_hit` / `HitResolution`, `cell_map` / `CellOccupant`, `FitLayout`, `ShipStats` + `derive_ship_stats`, `module_at`, `hardpoint_arc`/`FiringArc` (contracts/fitting-api.md §2/§3). E006 *produces* the hit/armor map + per-cell `health`; E007 *resolves what the hit does* and *mutates* that health.
- **Reuses E001** — `Physics::swept_cast` / `SweptHit` (the projectile CCD primitive, FR-021) and `Physics::step` / `motion::BodyState` (chunk momentum, FR-016); no new geometry primitive is invented.
- **Reuses E002** — `Weapon` / `WeaponProfile`, `Projectile`, `Damage`, `ProjectileOwner`, `PrevPosition`→`Position` swept segment (the existing fixed-forward shot is the only delivery, spec Excluded).
- **Introduces (`NEW-ENTITY`)** — `DamageEvent`, `Channel`, `DefenseLayer`, `ResistanceMatrix`, `PenetrationResult`, `DamageOutcome`, `Shields`, `Wreck`/`WreckChunk`, `SalvageOutcome`.

Consumers of this surface:

- **Weapon / combat systems** (`sim`, E002 rewired) — build a `DamageEvent` from a `Projectile` hit and call `apply_damage` (the BREAKING-CHANGE path, §5/§1).
- **The fit-stats system** (`sim`, E006 `recompute_ship_stats_system`) — re-derives `ShipStats`/`FitLayout` after `apply_damage` mutates per-module health (§2).
- **The destruction worker** (`sim`, `NEW-WORKER`) — runs `on_section_destroyed` only at destruction events (§3).
- **The salvage / wreck system** (`sim`) and **E013** — consume `Wreck`/`SalvageOutcome` (§4); E013 prices them, E007 only emits them.
- **The HUD / client** (`crates/client`) — reads the `DamageOutcome` legibility tag for diegetic feedback (FR-024); read-only, never authoritative.

## Domain types (the data the surface speaks)

Named entities the signatures carry. Field-level schema and `serde`/`Component` derive discipline match the rest of `sim` (serde as the E003/E004 replication/persistence seam; value semantics) and are a data-model concern; this catalog fixes only the surface contract.

| Type | Shape (conceptual) | Notes |
|------|--------------------|-------|
| `DamageEvent` | `{ channel: Channel, magnitude: f32, penetration: f32, shot_size: f32, point: Vec2, dir: Vec2 }` | FR-001. The typed packet that flows through the layers. `point`/`dir` are the hit geometry (the entry ray); `shot_size` feeds overmatch. |
| `Channel` | enum: `Kinetic \| Thermal \| Blast \| Em \| Radiation` | FR-001/004. The 5 confirmed channels; each strong vs a particular layer (FR-023 non-degeneracy). Maps to a row of the matrix. |
| `DefenseLayer` | enum: `Shields \| Armor \| Hull \| Systems` | FR-003. The ordered absorbers (outer→inner); each is a matrix column. (Avoidance/crew layers deferred — see Out of scope.) |
| `ResistanceMatrix` | data-driven `(DefenseLayer × Channel) -> f32` mitigation table | FR-004/022/023. `NEW-CONFIG`; the table itself is content, not code. Each cell ∈ `[0, 1)`. |
| `PenetrationResult` | enum: `Ricochet \| NonPenetration \| Penetration \| OverPenetration`, each carrying its surviving-damage tier | FR-005/006/007/008. The outcome of the armor-angle calc; selects the post-armor damage tier. |
| `Shields` | `{ hp: f32, hp_max: f32, regen: f32, powered: bool }` | FR-010. The regenerating, power-linked outer pool; sourced from `ModuleSpecifics::Shield { shield_hp, regen }`. `powered` is gated on `ShipStats::power_supply >= power_draw`. |
| `DamageOutcome` | `{ struck: Option<ModuleRef>, applied: f32, layer_reached: DefenseLayer, result: HitKind, destroyed: bool }` | The result of `apply_damage`; `HitKind` is the legibility tag (FR-024). |
| `HitKind` | enum: `ShieldAbsorbed \| Ricochet \| Penetrated \| OverPenetrated \| NoModule` | FR-024 (SC-005). The diegetic "what happened" the HUD reads; never numeric spam. |
| `Wreck` | a destroyed ship or a severed region: `{ origin: Entity, body: BodyState, layout: FitLayout (residual), salvage: [SalvageOutcome] }` | FR-020. A persistent, lootable world entity; reuses `BodyState` for its drift. |
| `WreckChunk` | a severed-but-still-physical hull region split off a living ship: `{ body: BodyState, cells: [Cell], salvage: [SalvageOutcome] }` | FR-015/016. Spawned by the destruction worker; inherits parent COM momentum. |
| `SalvageOutcome` | enum: `IntactModule(ModuleId) \| Scrap(f32)` | FR-018/019/020. Clean-sever → intact; through-kill/over-kill → scrap (always ≥ a scrap floor). |

Reused E006/E001/E002 types named below (defined elsewhere, **not** redefined here): `ModuleRef`, `ModuleId`, `SlotId`, `SectionId`, `Cell`, `CellOccupant`, `FitLayout`, `HitResolution`, `ShipStats`, `BodyState`, `SweptHit`, `Health`, `Damage`, `ProjectileOwner`.

## 1. Damage-resolution pipeline (the core `NEW-API`)

Consumed by the **weapon / combat systems** inside `sim`, run **only** on the authoritative server (FR-021). `apply_damage` is the whole pipeline: resolve the entry point (E006 `resolve_hit`), traverse **Shields → Armor → Hull → Systems** applying `(layer × channel)` resistance and the penetration result, reduce the struck module's `health`, and flag destruction. It is the BREAKING-CHANGE successor to `combat::apply_damage(health, damage)` for fitted ships (§5).

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `apply_damage` | combat → `sim` (server only) | `apply_damage(&mut World, target: Entity, ev: DamageEvent) -> DamageOutcome` | FR-002/003/004/009/011/021. The full pipeline. Resolves `ev`'s ray to the entry-point occupant via E006 `resolve_hit`, runs the layer traversal, **mutates** the struck cell's `health` in the target's `FitLayout`, and returns the legible outcome. Destruction at `health <= 0` flags `destroyed` (handed to §2 re-derive + §3 worker). Idempotent on an already-dead target (over-kill is bounded, edge case). |
| `traverse_layers` | internal to `apply_damage` | `traverse_layers(&Shields, armor: &ArmorFace, ev: DamageEvent, matrix: &ResistanceMatrix) -> (DefenseLayer, f32, HitKind)` | FR-003/004. Walks Shields→Armor→Hull→Systems, each absorbing/mitigating before the next; returns the layer reached, surviving magnitude, and the legibility tag. Armor invokes `resolve_penetration`. |
| `resolve_penetration` | armor step → `sim` | `resolve_penetration(armor_thickness: f32, impact_angle: f32, shot_pen: f32, shot_size: f32) -> PenetrationResult` | FR-005/006/007/008. `effective = armor_thickness / cos(impact_angle)`; `Ricochet` past the angle threshold; **overmatch** (`shot_size` large vs `armor_thickness` ignores angle → forces `Penetration`); else `shot_pen` vs `effective` selects `Penetration` (full tier) / `OverPenetration` (reduced) / `NonPenetration` (little-none). Pure; angle/threshold/tier coefficients are `NEW-CONFIG`. |
| `layer_resist` | matrix lookup → `sim` | `layer_resist(&ResistanceMatrix, layer: DefenseLayer, channel: Channel) -> f32` | FR-004/022/023. The data-driven mitigation fraction `∈ [0, 1)` for one `(layer, channel)` cell. Total over the matrix; the non-degeneracy guard (FR-023, SC-005) is a test over this table, not a runtime branch. |

Supporting type:

- `ArmorFace` — the per-cell armor datum the armor step reads: `{ thickness: f32, normal: Vec2 }` derived from the struck `CellOccupant`'s module (`ModuleSpecifics::Armor { armor_value }`) + the hull face geometry; `impact_angle = angle(ev.dir, normal)`. Armor geometry authoring is E006/content; E007 only reads it.

**Invariants**: `apply_damage` is total — a hit on an empty/structural cell (`resolve_hit` → no module, or the struck cell's `module == None`) yields `DamageOutcome { struck: None, result: NoModule, .. }`, never a panic (edge case "hit on nothing"). Surviving post-penetration damage routes to the cell **behind** the entry point (FR-009) by E006 depth ordering — central modules are reached only after their covers. Health is clamped at `0` (reuses the `combat::apply_damage` clamp semantics, INV-01). `layer_resist ∈ [0, 1)` so no layer is a hard wall and none is bypassed (FR-023). **Ownership**: the **server** owns every mutation; the returned `DamageOutcome.result`/`layer_reached` is the only thing the client reads (FR-024), and it is advisory feedback, never authority (Principle I).

### Shields handling (FR-010)

`Shields` is the outermost layer in the traversal and additionally regenerates over time while powered.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `shield_absorb` | Shields step in `traverse_layers` | `shield_absorb(&mut Shields, ev: DamageEvent, matrix: &ResistanceMatrix) -> (f32, bool)` | FR-010. Absorbs first (strongest vs `Thermal`); returns the **surviving** magnitude passed to Armor and whether the shield is now depleted. A depleted or `!powered` shield passes the event through **untouched** (armor exposed immediately, edge case). |
| `shield_regen_system` | fixed-step system → `sim` (server) | `shield_regen_system(dt, Query<(&mut Shields, &ShipStats)>)` | FR-010 (SC-005). Regenerates `hp` toward `hp_max` at `regen·dt` **only while `powered`**; sets `powered = stats.power_supply >= stats.power_draw` (a destroyed reactor drops `power_supply`, so shields go unpowered and stop regening — the §2 power→shield link). Pure per-entity; mirrors the `feedback_decay_system` / `projectile_step_system` fixed-step shape. |

**Invariant**: `0 <= hp <= hp_max`; an unpowered shield neither absorbs nor regenerates (it exposes armor). `powered` is derived from `ShipStats`, never set by the client.

## 2. Emergent-damage hook (extends E006 `derive_ship_stats`)

After `apply_damage` mutates a cell's `health`, the ship's effective stats must re-derive so a damaged module contributes less. This **extends** the existing E006 derivation rather than forking it — `derive_ship_stats` now folds per-module health into each module's contribution.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `derive_ship_stats` (extended) | fit-stats system + preview → `sim` | `derive_ship_stats(&Hull, &Fit, &ModuleCatalog, &FitLayout) -> ShipStats` | FR-012/013. **BREAKING signature extension**: now also threads the live `FitLayout` (the per-cell health source) so each module scales its `ModuleSpecifics` contribution by `health / health_max`. Still pure; the formulae are unchanged, only weighted by health. |
| `health_factor` | internal to derivation | `health_factor(occupant: &CellOccupant, module: &Module) -> f32` | FR-012. `(health / health_max).clamp(0, 1)`; a destroyed module (`health == 0`) contributes `0` to its stat (thrust/torque/weapon/power) — i.e. a destroyed thruster adds no thrust, a destroyed weapon yields `can_fire = false`, a destroyed reactor adds no `power_gen` (collapsing `power_supply`, which un-powers shields via §1). |

**Re-derive trigger (the rewire)**: the existing `recompute_ship_stats_system` (E006, gated on `Changed<Fit>`) is extended to also re-run on a **health change** — i.e. after any `apply_damage` that mutated the `FitLayout`. Conceptually the trigger becomes `Changed<Fit>` **OR** `Changed<FitLayout>`, so a battered ship and a healthy ship of the same fit derive measurably different `ShipStats` (SC-002). The systems that read `ShipStats` are unchanged (E006 §2: `flight::ship_motion_system`, `weapon::weapon_fire_system`) — they simply see degraded numbers.

**Invariants**: derivation stays **total and floored** — `health_factor ∈ [0, 1]`, and the E006 graceful floors (`THRUST_FLOOR`/`TORQUE_FLOOR`/`STRAFE_FLOOR`, `total_mass >= hull_base_mass > 0`) still hold, so a ship with every drive destroyed is near-immobile-but-finite, never `NaN`/`inf`/divide-by-zero (E006 INV-F07 preserved). Mass is **not** scaled by health (a damaged module still has mass). **Ownership**: server re-derives and applies; the client re-derives identically for prediction (Principle I).

## 3. Destruction + severing (`NEW-WORKER`, runs ONLY on destruction)

When `apply_damage` flags a section/module destroyed, the destruction worker runs the coarse removal + connectivity check. It is **event-driven, not per-frame** (FR-017) — the whole reason connectivity is a flood-fill only at destruction.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `on_section_destroyed` | destruction worker → `sim` (server) | `on_section_destroyed(&mut World, ship: Entity, section: SectionId)` | FR-014/015/016/017. Removes `section`'s cells from the ship's `FitLayout`/grid (coarse granularity, cell-ready), then runs `connected_region`. For each region disconnected from the core, calls `sever_chunk`. Runs **once per destruction event**, never on a tick where nothing was destroyed. |
| `connected_region` | internal to the worker | `connected_region(&FitLayout, core: Cell) -> HashSet<Cell>` | FR-015. Flood-fill over the **remaining** hull grid from the ship's core/command cell; the returned set is what stays attached. Cells outside it are disconnected regions to sever. The core-severing edge case: if the core itself is gone, the **whole ship** dies → a `Wreck` (no orphaned ghost ship). |
| `sever_chunk` | internal to the worker | `sever_chunk(&mut World, ship: Entity, cells: &HashSet<Cell>) -> WreckChunk` | FR-016. Splits a disconnected region into a separate physics body. Inherits the parent's linear + angular velocity **evaluated at the chunk's center of mass** (reuses `BodyState` + the `Physics` trait — `chunk.body.vel = parent.vel + parent.angvel × (com_chunk − com_parent)`), so it drifts, never zero-velocity-pops (edge case). A single orphan cell severs cleanly as a (small) chunk or is absorbed — no dangling fragment. |

**Invariants**: the connectivity check touches only the **remaining** grid after the destroyed section is removed; it runs strictly at destruction events (FR-017, SC-003) — a hull that stays connected produces **no** split. Momentum is conserved at the COM (FR-016). Severing reuses the `sim::Physics` trait and `BodyState`; no new physics engine is introduced. **Ownership**: server-only; chunk/wreck spawns are authoritative ECS spawns (the client renders the replicated result, never spawns its own).

## 4. Wreck / salvage (feeds E013)

A severed chunk or a destroyed ship persists as a lootable `Wreck`; salvaging it yields intact modules (clean sever) or scrap (through-kill / over-kill). E007 emits these entities + the intact-vs-scrap split; **E013 owns their economic value** (markets/refining/price are out of scope).

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `salvage` | salvage system / E013 → `sim` | `salvage(&Wreck) -> Vec<SalvageOutcome>` | FR-018/019/020. Per residual module: a **clean sever** (own `health` above the intact threshold, structure severed) → `IntactModule(ModuleId)`; a **destroyed / penetrated-through** module (`health == 0` or below threshold) → `Scrap(amount)`. An **over-killed** ship still yields ≥ a `Scrap` floor (never an empty `Vec`, edge case). |
| `intact_threshold` | internal to `salvage` | `intact_threshold(occupant: &CellOccupant, module: &Module) -> bool` | FR-018. The clean-sever vs through-kill boundary: `health >= INTACT_FRACTION · health_max` at the moment its surrounding structure severs. `INTACT_FRACTION` is `NEW-CONFIG`. |

**Invariants**: `salvage` is **single-resolution** — a wreck is claimed once (no double-claim; friendly/hostile source is irrelevant to the outcome, edge case). Over-kill is bounded to never yield zero loot (≥ scrap floor, FR-020/SC-004). Salvage **reads** module `health`/`health_max` (E006 data) but does not run combat. **Ownership**: server emits and resolves wrecks; E013 consumes the `SalvageOutcome` list for pricing — that pricing is **not** this surface.

## 5. Weapon → `DamageEvent` construction (the BREAKING-CHANGE path)

How an E002 `Projectile` hit becomes a `DamageEvent`, and the replacement of the whole-ship `Health` damage path for fitted ships.

| Operation | Direction / consumer | Conceptual signature | Notes |
|-----------|----------------------|----------------------|-------|
| `damage_event_from_hit` | weapon/collision system → `sim` | `damage_event_from_hit(projectile: Entity, hit: &SweptHit, src: &WeaponSource) -> DamageEvent` | FR-001. Builds the event from the shot: `channel` from the firing module/weapon (`WeaponSource`), `magnitude` from the E002 `Damage` / `WeaponProfile::damage`, `penetration` + `shot_size` from the weapon's data-driven stats, and the hit geometry (`point`/`dir`) from the reused `Physics::swept_cast` `SweptHit` + the projectile's `Velocity`. |
| weapon-hit resolution (rewired) | combat collision system → `sim` (server) | *fitted:* `apply_damage(world, target, ev)` (§1); *unfitted:* `Health(apply_damage(h.0, dmg))` (E002, unchanged) | **BREAKING-CHANGE**. A hit on a **fitted** target (has `FitLayout`/`ShipStats`) routes through §1's per-module pipeline, **replacing** the E002/E003 whole-ship `combat::apply_damage(health, damage)` → `destruction_system` path. A hit on an **unfitted** target (no `FitLayout`) keeps the simplified whole-ship `Health` path verbatim (mirrors the E006 fitted/unfitted weapon-fire split). |

Supporting type:

- `WeaponSource` — the channel + penetration + `shot_size` carrier for the firing weapon, sourced from the weapon `Module`'s data (a `ModuleSpecifics::Weapon` extension or adjacent content row, `NEW-CONFIG`). The E002 `Weapon` component / `WeaponProfile` stays the fire-timing source; `WeaponSource` adds the damage-typing the new pipeline needs.

**Invariants**: the **fitted vs unfitted** branch mirrors E006 exactly — `apply_damage` (§1) for fitted ships, the legacy `Health` clamp for unfitted fixtures/bots (so E001/E002/E003 tests and server bots are untouched). The CCD is the **same** swept-ray primitive (`Physics::swept_cast`) so fast projectiles still cannot tunnel (FR-021). `magnitude > 0` (INV-04 preserved). **Ownership**: the server resolves the hit and constructs the event; `ProjectileOwner` still prevents self-hits (E002).

## Out of scope (later epics)

- **DoT analogs & cascade chains** — fire / breach / decompression / leak damage-over-time, and cascades beyond the direct "destroyed reactor → power loss" (ammo cook-off, chained reactor breach) — **later** (spec Excluded). The pipeline is data-driven so these layer on without a rewrite.
- **New weapon delivery types** — missiles / torpedoes / mines / drones, missile-guidance + explosion-fidelity — **later**. E007 damages via the existing E002 fixed-forward swept `Projectile` only.
- **Fine per-cell ("eaten-away") destruction** — coarse module/section granularity now (`SectionId`); the grid is cell-addressable so finer destruction is a content upgrade, not a refactor of this connectivity contract (HINT-004).
- **The salvage economy itself** — markets, refining, value, the acquisition ladder — **E013**. E007 only emits `Wreck`/`SalvageOutcome` entities + the intact-vs-scrap split; pricing is not this surface.
- **AOI-scaled replication of destruction** at population — **E009**. E007 resolves in the single-node authoritative server (E003); this contract names server *ownership*, not the wire transport of wrecks/chunks.
- **Crew layer + the outer Avoidance / point-defense / ECM layer** — crew is a later embodiment epic; avoidance/PD/signature/ECM is **E010** (sensors/EW). E007's innermost layer is `Systems`, its outermost is `Shields`; `DefenseLayer` is the in-scope subset of ADR-0008's full stack.
- **Deferred penetration fidelity** — per-shell normalization curves, multi-layer "armor cake" traversal, per-section damage saturation, fuzing/shatter, channel-specific penetration coefficients (spec Excluded). The single-armor-face `resolve_penetration` here is the MVP core; the data-driven shape admits these later.
