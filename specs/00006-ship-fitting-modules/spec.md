---
feature_branch: "00006-ship-fitting-modules"
created: "2026-06-02"
input: "E006 — data-driven Module abstraction + positional-slot ship fitting bounded by power/CPU/mass budgets, where the fit layout is the damage hitbox/armor map and the fit drives the ship's flight & weapons"
spec_type: "product"
spec_maturity: "draft"
epic_id: "E006"
epic_sources: "{PRD:CAP-003}{SAD:ADR-0008}"
---

# Feature Specification: Ship Fitting & Modules

**Feature Branch**: `00006-ship-fitting-modules`  
**Created**: 2026-06-02  
**Status**: Draft  
**Spec Type**: product  
**Spec Maturity**: draft  
**Epic ID**: E006  
**Epic Sources**: {PRD:CAP-003}{SAD:ADR-0008}  
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

Right now a ship is a single hand-tuned object: its flight and weapon stats come from a global `Tuning` stand-in (E002), it has no customizable loadout, and there is no way for a player's choices to shape what their ship is or how it fights. Dark Silence's identity (PRD CAP-003) is that **loadout becomes identity** — players acquire ships and fit them within real tradeoffs, where *where* you place a module is a survivability choice and *what* you install changes how the ship flies. Without a fitting system there is no ship customization, no build diversity, and no positional foundation for the damage model (E007) to read — so combat stays a one-ship demo instead of a game of meaningful loadouts.

## Scope *(mandatory)*

### Included

- A data-driven **Module** abstraction: every installed device (reactor, thruster, weapon, shield, armor, utility, …) is one uniform stat block (power gen/draw, CPU/control draw, mass, heat, hitbox/health, hardpoint type + size, plus device-specific attributes).
- A **Hull + Fit** model: a hull authored as a **2D cell-grid** with positionally-placed, typed/sized slots; a **Fit** is the set of modules installed into those slots. The same grid is the fitting layout **and** the hitbox/armor map.
- **Positional-slot fitting** bounded by three competing budgets — **power, CPU/control, mass** — plus slot count/size; the system validates and blocks invalid or over-budget fits.
- An **interactive fitting screen**: place/remove modules into slots, live power/CPU/mass budget readouts, a before-commit preview of the resulting stats (positive/negative deltas), and save/name/load fit presets.
- **Fit-derived ship behavior**: the active fit determines the ship's effective flight stats (top speed/agility from installed thrust modules + total mass) and weapon capability (from installed weapon modules), **replacing the E002 global `Tuning` stand-in**.
- **Fit-layout-as-hit-map**: the realized layout is queryable as the hitbox/armor map (which module occupies each cell, outer modules shielding inner) and each weapon hardpoint carries a position-derived **firing arc** — the positional foundation the damage system (E007) reads.
- A **seed content ladder**: at least two hull sizes (e.g., fighter + corvette) with scaling budgets/slots, plus module archetypes spanning the tradeoff axes (reactor, thruster, weapon, shield, armor, utility).

### Excluded

- **Damage resolution itself** (penetration, defense-layer channels, destruction, severing, salvage) — that is E007; E006 only *produces* the hit-location/armor map and module health it reads.
- **Acquiring ships/modules** (markets, manufacturing, looting, the acquisition ladder) — economy is E013; E006 assumes modules are simply available to fit.
- **Exotic modules + side-effect/synergy engine and the materials/research chain** — E014/E016; E006 ships ordinary modules only.
- **The full hull-class ladder** (frigate→cruiser→capital→station) and faction/race reskins — content beyond the seed ladder is added later as data.
- **Firing-arc *enforcement* in combat** (turret auto-track/lock, can-this-weapon-hit) — E006 *defines* arcs as fit data; combat/E007 enforces them.
- **Fine cell-by-cell ("eaten-away") destruction** — deferred (E007+); E006 authors the grid so coarse→fine is a later data upgrade, not a refactor.

### Edge Cases & Boundaries

- **Empty hull**: a hull with no modules is a valid baseline fit (flies, but unarmed/under-powered).
- **Over-budget**: a fit exceeding power, CPU/control, **or** mass must be blocked and the offending axis named — never silently allowed to function.
- **Mismatched placement**: a module whose hardpoint **type** or **size** does not fit the target slot is rejected with the reason.
- **Removing a load-bearing module**: removing the reactor (power source) or the only weapon leaves a valid-but-crippled fit (no power budget / cannot fire), not an invalid one.
- **Stat-floor fits**: a fit with no thrust module is near-immobile; the derived stats must degrade gracefully (no divide-by-zero / infinite values) to a defined floor.
- **Occlusion ties**: when a hit line crosses multiple modules, resolution order is outermost-first along the line; fully-interior modules are reached only after their covers are gone.

## User Scenarios & Testing *(mandatory for product specs only)*

### User Story 1 - Fit a ship within its budgets (Priority: P1)

A player opens a hull in the fitting screen, drags modules into its positional slots, and watches the power, CPU/control, and mass budgets fill as they go. The screen lets valid fits through and clearly blocks invalid ones — over-budget on any axis, or a module that doesn't match a slot's type/size — explaining why, so the player shapes a loadout that is theirs.

**Why this priority**: Core value of the epic and of CAP-003 — without the ability to fit a ship within enforced tradeoffs there is no customization and nothing for the rest of the epic to build on.

**Independent Test**: Open a hull, place and remove modules, observe live budget bars, get a clear rejection when exceeding a budget or mismatching a slot, and save a valid fit.

**Acceptance Scenarios**:

1. **Given** a hull in the fitting screen, **When** the player installs a module into a compatible slot, **Then** the power/CPU/mass budget readouts update to reflect its draw and the fit remains valid if all budgets are within capacity.
2. **Given** a fit near a budget ceiling, **When** the player adds a module that would exceed power (or CPU, or mass), **Then** the screen blocks/flags the change and names the violated budget rather than allowing it.
3. **Given** a module of a given hardpoint type/size, **When** the player tries to place it in a slot of the wrong type or too-small size, **Then** the placement is rejected with the mismatch reason.
4. **Given** a hull with no modules, **When** the player views it, **Then** it is a valid (baseline) fit; **and When** the player removes a module from a valid fit, **Then** that module's budget is freed.

### User Story 2 - The fit changes how the ship flies and fights (Priority: P1)

A player who installs heavier armor and more weapons feels the ship become sluggish; one who strips to light thrusters feels it dart. A ship with no weapon module simply cannot fire. The loadout is not cosmetic — the active fit *is* the ship's flight and weapon profile, replacing the old global stand-in.

**Why this priority**: This is the payoff that makes fitting matter in play; it realizes the GDD intent that flight stats come from installed equipment, and it is the integration that retires E002's `Tuning` stand-in.

**Independent Test**: Fly two different fits of the same hull and observe a measurable difference in agility/top speed; confirm a fit with no weapon module cannot fire and a better thrust module raises top speed.

**Acceptance Scenarios**:

1. **Given** two valid fits of one hull — one heavy/armored, one light/high-thrust — **When** each is flown, **Then** the heavier fit is measurably less agile and slower-topping than the lighter one.
2. **Given** a fit with no weapon module, **When** the player fires, **Then** nothing is launched; **and Given** a fit with a weapon module, **When** the player fires, **Then** a projectile launches with that module's parameters.
3. **Given** a fit, **When** it is the active loadout on a running ship, **Then** the ship's flight and combat behavior is driven by the fit's derived stats (not by a hand-set global).
4. **Given** a fit whose only thrust source is removed, **When** it is flown, **Then** the ship degrades to a defined low-mobility floor without invalid (infinite/NaN) behavior.

### User Story 3 - Where you put a module is a survivability choice (Priority: P1)

The hull is a grid of cells, and modules sit in real positions on it. A reactor tucked centrally behind armor is hard to reach; the same reactor mounted on the edge is exposed. Weapon hardpoints cover different arcs depending on where they sit. The realized layout is the ship's hitbox/armor map — the foundation the damage model will read.

**Why this priority**: It is the distinctive positional depth of the system and the explicit dependency contract for E007 (the fit layout IS the damage hitbox/armor map, ADR-0008) — deferring it would make E007 unbuildable on this foundation.

**Independent Test**: Query which module a world-space hit point/line resolves to and confirm an outer module is encountered before the inner module it covers; confirm each weapon hardpoint exposes a firing arc derived from its position.

**Acceptance Scenarios**:

1. **Given** a fit, **When** a hit line crosses the layout, **Then** it resolves to the module occupying the struck cell(s), encountering an outer module before any inner module it covers.
2. **Given** two fits that differ only in reactor placement (central-behind-armor vs edge-exposed), **When** the same shot is traced, **Then** the exposed reactor is reached sooner than the protected one.
3. **Given** a weapon installed at a hardpoint, **When** its firing arc is queried, **Then** the arc is determined by the hardpoint's position/facing on the hull.
4. **Given** any valid fit, **When** the damage system requests the hitbox/armor map, **Then** the layout reports which module/section occupies each cell and that module's health.

### User Story 4 - Real tradeoffs and a hull ladder (Priority: P2)

On one hull, a player can build a tank, a glass cannon, or a fast skirmisher — but not all at once; each build hits a different budget ceiling. Stepping up to a larger hull trades agility for more slots and power. Distinct, viable roles emerge from the loadout rather than from fixed ship classes.

**Why this priority**: It is the depth/identity payoff (loadout = identity) and balance guard, but the MVP fitting loop (US1–US3) is demonstrable without proving the full tradeoff space.

**Independent Test**: Build a tank fit and a damage fit on the same hull, confirm each is constrained by a different budget and neither dominates, and confirm a larger hull offers more slots/power at the cost of agility.

**Acceptance Scenarios**:

1. **Given** one hull, **When** a tank-oriented and a damage-oriented fit are each built, **Then** both are valid and each is bound by a *different* budget ceiling (e.g., tank binds mass/power, damage binds CPU/power).
2. **Given** any single fit, **When** its stats are evaluated, **Then** it cannot simultaneously maximize tank, damage, and speed.
3. **Given** two seed hulls of different size, **When** their budgets/slots/mass are compared, **Then** the larger hull has more slots/power but greater mass (lower agility).

### User Story 5 - Save, preview, and reuse fits (Priority: P3)

A player experiments with a loadout, previews its resulting stats before committing, and saves it under a name to reload later — without having to re-place every module each time.

**Why this priority**: Convenience and iteration speed; the fitting system is fully usable without presets, so this is polish.

**Independent Test**: Save a named fit, reload it onto a hull, and preview its derived stats before applying.

**Acceptance Scenarios**:

1. **Given** a valid fit, **When** the player saves it with a name, **Then** it can be reloaded onto a compatible hull later.
2. **Given** a candidate fit, **When** the player previews it, **Then** the resulting flight/weapon/budget stats are shown before it is committed to a live ship.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST represent every installed device as a data-driven **Module** with one uniform stat block: power generation/draw, CPU/control draw, mass, heat, hitbox/health, hardpoint type + size, plus device-specific attributes.
- **FR-002**: System MUST represent a ship as a **hull + positionally-placed hardpoints/slots + the modules fitted into them** (a **Fit**), with role/specialization emerging from the installed set rather than a fixed class.
- **FR-003**: System MUST author each hull as a **2D cell-grid** whose cells group into slots/sections, so that the one grid is both the fitting layout and the hitbox/armor map.
- **FR-004**: System MUST define, per hull, the fitting **budgets** (power capacity, CPU/control capacity, mass capacity) and a **slot inventory** (each slot's type, size, and grid position/facing).
- **FR-005**: System MUST let a player install a module into a slot and remove a fitted module.
- **FR-006**: System MUST reject (or flag and block commit of) a placement whose module hardpoint **type** does not match the target slot's type.
- **FR-007**: System MUST reject/block a placement whose module **size** exceeds the target slot's size.
- **FR-008**: System MUST compute a fit's aggregate power draw vs generation, CPU/control draw vs capacity, and total mass vs the hull's mass capacity, and MUST block any fit that exceeds **any one** budget.
- **FR-009**: System MUST surface each budget's usage vs capacity **live** as the fit changes, so the player sees remaining headroom and the cost of a prospective change before committing it.
- **FR-010**: System MUST treat an empty hull (no modules) as a valid baseline fit.
- **FR-011**: System MUST report, for an invalid fit, which rule(s) it violates (the over-budget axis; the mismatched slot and reason).
- **FR-012**: System MUST provide an interactive fitting interface to place/remove modules into a hull's positional slots, showing live power/CPU/mass budget readouts.
- **FR-013**: System MUST preview the stat/budget effect of a prospective change (e.g., positive/negative deltas) before it is applied.
- **FR-014**: System MUST derive the ship's effective capabilities from its active fit — at minimum its flight stats (achievable top speed and agility/turn) and its weapon capability — and apply them to the running ship, **replacing the E002 global `Tuning` stand-in**.
- **FR-015**: System MUST make greater total fit mass reduce agility/acceleration, and more/better installed thrust modules increase achievable speed.
- **FR-016**: System MUST make weapon capability depend on installed weapon modules: a ship with no weapon module cannot fire, and a firing weapon's parameters come from its module.
- **FR-017**: System MUST degrade derived stats gracefully to defined floors for crippled fits (e.g., no thrust, no power) — never producing invalid (infinite/NaN) behavior.
- **FR-018**: System MUST resolve a world-space hit point/line against the fit layout to the module(s) occupying the struck cells, encountering an outer module before any inner module it covers.
- **FR-019**: System MUST expose the realized fit layout as a **hitbox/armor map** — which module/section occupies each cell, and that module's health — for the damage system (E007) to read.
- **FR-020**: System MUST derive each weapon hardpoint's **firing arc** from its mount position/facing on the hull, and expose it as fit data.
- **FR-021**: System MUST make module placement a survivability choice: a module covered by other modules/armor is reached by a hit only after its covers, so central placement protects and edge placement exposes.
- **FR-022**: System MUST ship a seed content set of at least **two hull sizes** with scaling budgets/slots/mass, plus module archetypes spanning the tradeoff axes (at minimum reactor, thruster, weapon, shield, armor, utility).
- **FR-023**: System MUST ensure the budgets bind such that no single fit can simultaneously maximize tank, damage, and speed on a hull (maxing one starves the others).
- **FR-024**: System MUST let a player save and name a fit, reload it onto a compatible hull, and preview a fit's derived stats without committing it to a live ship.
- **FR-025**: Module and hull definitions MUST be **data-driven** (content added/edited as data without code changes), so the seed ladder can grow without reworking the system.

### Key Entities

- **Module**: a data-driven installable device with a uniform stat block (power gen/draw, CPU/control draw, mass, heat, hitbox/health, hardpoint type + size) plus device-specific attributes (e.g., a thruster's thrust, a weapon's fire parameters). The atom of fitting.
- **Hull**: a ship chassis authored as a 2D cell-grid, carrying the fitting budgets (power/CPU/mass capacity) and a positional slot inventory; designer-authored geometry (players fit modules, not build hull shape).
- **Hardpoint / Slot**: a typed, sized position on the hull grid that accepts a matching module; weapon hardpoints additionally carry a position-derived firing arc.
- **Fit**: the assignment of modules to a hull's slots; the unit that is validated, saved, and from which the ship's effective stats and hit/armor map are derived.
- **Budget**: a per-hull capacity (power, CPU/control, mass) that the summed module draw must not exceed; the source of fitting tradeoffs.
- **Fit layout / hit-map**: the realized grid occupancy (which module/section + health per cell) exposed for combat/damage to resolve hits and arcs against.
- **Effective stats**: the flight + weapon profile derived from a fit, applied to the running ship in place of the global stand-in.

## Assumptions & Risks *(mandatory)*

### Assumptions

- The shared `sim` crate (E001) hosts the Module/Hull/Fit domain types as the unified data-driven model (ADR-0008); E006 extends it, consistent with Principle II (gameplay logic in `sim`).
- The E002 client provides the ship plus the flight and weapon systems that the fit's derived stats will drive; replacing the global `Tuning` stand-in with fit-derived stats reuses the existing flight model (so feel is preserved for an equivalent baseline fit).
- Modules are simply available to fit in this epic; how they are acquired (markets/manufacture/loot) is out of scope (E013/E014).
- Hull geometry is designer-authored content; players customize the *fit*, not the hull shape (ADR-0008 neutral consequence).
- A small seed ladder (2–3 hulls + ~6 module archetypes) is sufficient to validate budgets, tradeoffs, and the hit-map; the full class ladder and exotics are later data/epics.

### Risks

- **Degenerate dominant fit** *(likelihood: medium, impact: high)*: if module costs/benefits aren't tuned, one fit strictly dominates and the tradeoff space collapses. Mitigation: distinct per-module strength+cost, budgets that bind on different axes, and SC-005 as a guard (tank vs damage bind differently; no fit maxes all).
- **Flight-feel regression from the `Tuning` rewire** *(likelihood: medium, impact: medium)*: making flight fit-derived could change the E002 feel the user signed off on. Mitigation: derive the same flight-model inputs from the fit and tune the baseline seed fit to reproduce the current feel; verify in playtest.
- **Scope of the cell-grid hit-map + interactive UI** *(likelihood: medium, impact: medium)*: positional grid + arcs + a full fitting screen is large. Mitigation: minimal seed content, a coarse grid (section granularity, cell-upgrade-ready per ADR-0008), and an MVP-then-polish UI.

## Implementation Signals *(mandatory)*

- `NEW-ENTITY` — The unified domain model: `Module`, `Hull` (cell-grid), `Hardpoint/Slot`, `Fit`, budgets, and the layout/hit-map (ADR-0008; consumed by E007/E013/E014).
- `NEW-UI` — The interactive fitting screen: positional slot placement, live power/CPU/mass budget bars, before-commit stat preview (positive/negative deltas), and save/load presets.
- `BREAKING-CHANGE` — The ship's flight and weapon stats become **fit-derived**, replacing the E002 global `Tuning` stand-in; the flight/weapon systems consume fit-derived effective stats.
- `NEW-API` — A queryable surface for the fit's **effective stats** and **hit-location/armor map + firing arcs**, read by the running ship and by the damage system (E007).
- `NEW-CONFIG` — Data-driven hull and module definitions (the seed ladder + module archetypes), authored as content rather than code.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [US1]: A player can assemble a loadout by placing modules into a hull's slots, and the system blocks every fit that exceeds the power, CPU, or mass budget or places a module in a type/size-mismatched slot — naming the violated rule each time.
- **SC-002** [US1]: Power/CPU/mass usage updates live as modules are added or removed, an empty hull is a valid fit, and removing a module frees its budget.
- **SC-003** [US2]: Two valid fits of one hull fly measurably differently (a heavier fit is less agile and slower-topping than a lighter, higher-thrust one), and a fit with no weapon module cannot fire — with the running ship driven by fit-derived stats rather than the global stand-in.
- **SC-004** [US3]: A traced hit resolves through the fit layout to the correct module with outer-before-inner ordering, two fits differing only in reactor placement expose the reactor to a shot at different depths, and each weapon hardpoint reports a position-derived firing arc; the layout is queryable as the hit/armor map E007 consumes.
- **SC-005** [US4]: On one hull a tank-oriented fit and a damage-oriented fit are both viable and bound by different budget ceilings, no single fit maxes tank+damage+speed at once, and a larger seed hull offers more slots/power at the cost of agility.
- **SC-006** [US5]: A player can save, name, reload, and preview a fit's derived flight/weapon/budget stats before committing it to a ship.

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| Module | A data-driven installable device with a uniform stat block (power, CPU, mass, heat, health, hardpoint type/size) + device-specific attributes; the atom of fitting. |
| Hull | A ship chassis authored as a 2D cell-grid, carrying fitting budgets and a positional slot inventory; designer-authored geometry. |
| Hardpoint / Slot | A typed, sized position on the hull grid that accepts a matching module; weapon hardpoints carry a position-derived firing arc. |
| Fit | The assignment of modules to a hull's slots — validated, saved, and the source of the ship's effective stats and hit-map. |
| Budget | A per-hull capacity (power, CPU/control, mass) the summed module draw must not exceed; the source of fitting tradeoffs. |
| Firing arc | The angular coverage a weapon can engage, derived from its hardpoint's position/facing on the hull. |
| Fit layout / hit-map | The realized grid occupancy (which module/section + health per cell) the damage system resolves hits and arcs against. |
| Effective stats | The flight + weapon profile derived from a fit and applied to the running ship, replacing the global `Tuning` stand-in. |

## Compliance Check

**Verdict**: PASS — no `project-instructions.md` (v1.1.0) violations; no CRITICAL findings.

- **Principle II (Shared Sim Core)**: PASS — Module/Hull/Fit domain in `crates/sim` as the unified ADR-0008 model; fit-derived stats drive the same shared flight/weapon systems, replacing the E002 `Tuning` stand-in (no forked client model). (Assumptions; FR-014; NEW-ENTITY)
- **Principle I (Server-Authoritative)**: PASS — pre-networked scope; nothing implies client authority over fit-derived stats. *Advisory (Plan/networked-integration)*: state server ownership of Fit validation (FR-008/FR-011) and effective-stats application (FR-014) when E006 integrates with the networked server, so it does not inherit a client-authoritative assumption.
- **ADR-0008**: PASS — uniform Module stat block, 2D cell-grid hull, fit-layout-as-hit-map, power/CPU/mass budgets, coarse→fine deferral, and designer-authored hull geometry all consistent. (FR-001/003/008/019; Hull Key Entity)
- **Tech-stack / Source layout**: PASS — domain as `sim` data, fitting UI as `client` (NEW-UI), data-driven content (FR-025). *Advisory (Plan)*: bind crate placement normatively in `plan.md` so domain logic cannot land in `client`.
- **No P2W / economy**: PASS — acquisition and economy excluded to E013/E014; no real-money or pay-to-win surface. (Scope/Excluded)

Advisories are SHOULD-level (Plan-phase), not blocking.
