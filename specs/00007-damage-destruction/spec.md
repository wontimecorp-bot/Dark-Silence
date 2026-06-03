---
feature_branch: "00007-damage-destruction"
created: "2026-06-02"
input: "E007 — unified typed-damage pipeline (channels × ordered defense layers) with angle-based hit-location penetration reading the E006 fit layout, coarse module/section destruction with connectivity severing into wreckage, and clean-sever salvage"
spec_type: "product"
spec_maturity: "draft"
epic_id: "E007"
epic_sources: "{PRD:CAP-004}{SAD:ADR-0008}"
---

# Feature Specification: Damage & Destruction

**Feature Branch**: `00007-damage-destruction`  
**Created**: 2026-06-02  
**Status**: Draft  
**Spec Type**: product  
**Spec Maturity**: draft  
**Epic ID**: E007  
**Epic Sources**: {PRD:CAP-004}{SAD:ADR-0008}  
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

Combat today is a single whole-ship HP bar: a hit just subtracts health and a ship pops when it reaches zero. That throws away everything E006 built — the ship is a positioned grid of modules behind armor, but *where* a shot lands, *what kind* of damage it is, and *which module* sits behind the entry point currently mean nothing. Dark Silence's combat identity (PRD CAP-004) is **believable hit-location/armor combat where parts can be disabled, severed, and salvaged** — you angle to present armor, hide your reactor, watch a damaged thruster bleed your speed, blow a wing off, and scavenge an intact module from the wreck. Without a real damage-and-destruction model there is no reason to fit positionally, no skill in angling or target priority, no emergent "the ship gets worse as it's hit," and no wrecks to feed the salvage economy (E013).

## Scope *(mandatory)*

### Included

- A **unified typed-damage pipeline**: every hit is a `DamageEvent` with a **channel** (kinetic, thermal/energy, blast, EM, radiation), a magnitude, a penetration value, and a hit geometry (impact point + direction).
- **Hit-location resolution**: a swept-ray weapon hit resolves against the target's E006 **fit layout** (the hitbox/armor map) to the module/section at the entry point, outer-before-inner.
- **Ordered defense layers** — **Shields → Armor → Hull/Structure → Systems** — each absorbing/modifying a `DamageEvent` per a data-driven **(layer × channel) resistance matrix** (each channel strong vs a particular layer; no globally dominant channel/layer).
- **Angle-based penetration** (WoWs/WT core): effective armor = `thickness / cos(angle)`; **ricochet** at steep angles; **overmatch** (large hits ignore angle); **penetration vs over-penetration** damage tiers; surviving damage routed to the module behind the entry point.
- **Shields** as a basic regenerating, power-linked pool (strongest vs energy); depleted/unpowered shields expose the armor layer.
- **Emergent damage**: per-module/section health; a module's contribution to the ship's **effective stats (E006 `ShipStats`)** scales with its health — a damaged thruster lowers top speed/acceleration, a destroyed weapon can't fire, a destroyed reactor drops power (and power-linked shields).
- **Coarse module/section destruction** (cell-grid-ready for finer later) + **connectivity severing**: a flood-fill on the remaining hull grid (run only on destruction) splits disconnected regions into separate **wreck chunks** that drift off with inherited momentum.
- **Salvage**: a **clean sever** (module health intact, surrounding structure gone) yields an intact, scavengeable module; a destroyed/penetrated-through module yields **scrap**; destroyed ships + severed chunks persist as **lootable wrecks** (seeding E013).
- Authoritative, server-side resolution reusing the swept-ray CCD (E001/E002) so fast projectiles can't tunnel.

### Excluded

- **The Crew layer and the outer Avoidance / point-defense / ECM layer** — crew is a later embodiment epic; avoidance/PD/signature/ECM is E010 (sensors/EW). E007's innermost layer is module **Systems**; its outermost is **Shields**.
- **Deferred penetration mechanics** — per-shell normalization curves, multi-layer "armor cake" traversal, per-section damage saturation, fuzing, shatter, channel-specific penetration coefficients. (The pipeline is data-driven so these layer on without a rewrite.)
- **Damage-over-time analogs** (fire, breach/decompression, leak) and **cascade chains** beyond the direct "destroyed reactor → power loss" (ammo cook-off, chained reactor breach) — later.
- **New weapon delivery types** (missiles/torpedoes/mines/drones, the missile-guidance + explosion-fidelity model) — E007 damages via the existing E002 fixed-forward swept projectile; new weapons are later.
- **Fine per-cell ("eaten-away") destruction** — coarse module/section granularity now; the grid is cell-ready so it upgrades without a data-model refactor.
- **The salvage economy itself** (markets, refining, value) — E013; E007 only produces the wreck/salvage *entities* and the intact-vs-scrap outcome.
- **AOI/scaled networked replication of destruction at population** — E009; E007 resolves in the single-node authoritative server (E003).

### Edge Cases & Boundaries

- **Shields up vs down**: while powered, shields absorb first; depleted or unpowered (reactor lost) shields expose armor immediately.
- **Hit on an empty grid cell** (no module behind): the structural hull absorbs it (or the shot over-penetrates into space) — never a crash or a hit on "nothing".
- **Over-kill**: damage to an already-destroyed ship/section is bounded; an over-killed ship still leaves at least scrap (never zero loot).
- **Severing the core**: if the destroyed section disconnects the ship's core/command section, the ship is destroyed → a persistent wreck (no orphaned "ghost ship").
- **Orphan cells**: a single disconnected cell must sever cleanly as a chunk or be absorbed — no dangling, un-targetable fragments.
- **Boundary angles/health**: the ricochet angle threshold and "zero health = destroyed" are defined inclusively/exclusively and consistently.
- **Clean-sever vs through-kill boundary**: a module at the moment its surrounding structure severs — if its own health is above the intact threshold it salvages intact, otherwise scrap.
- **Friendly fire / friendly salvage**: damage applies regardless of source; wreck claiming is single-resolution (no double-claim).

## User Scenarios & Testing *(mandatory for product specs only)*

### User Story 1 - Hits land where you aim and armor matters (Priority: P1)

A shot is a *typed* hit that strikes a *specific spot*. It meets shields first (which shrug off energy but barely slow a kinetic slug), then armor — and a glancing hit off angled armor ricochets or barely scratches, while a square or oversized hit punches through to whatever module sits behind the entry point. The defender who angles their armor and buries the reactor survives; the attacker who brings the right damage type to the right spot wins.

**Why this priority**: It is the core of CAP-004 and the reason E006's positional fitting exists — without hit-location, typed, angle-aware damage, combat is still a single HP bar and the whole fitting layer is decorative.

**Independent Test**: Fire at a fitted target and verify a hit resolves to the entry-point module, is reduced by the layer/channel matrix, ricochets/over-armors at steep angles, overmatches when large, and on a clean penetration damages the module behind.

**Acceptance Scenarios**:

1. **Given** a fitted target, **When** a shot strikes it, **Then** it resolves to the module/section at the entry point and its damage is reduced as it passes Shields → Armor → Hull → Systems according to the (layer × channel) matrix.
2. **Given** an angled armor face, **When** a shot hits at a steep angle, **Then** it sees increased effective armor (`thickness/cos(angle)`) and, past the ricochet threshold, bounces off with little/no damage.
3. **Given** a sufficiently large hit, **When** it strikes thin plating, **Then** it overmatches (ignores the angle/ricochet rule) and penetrates; **and** a clean penetration applies its damage tier while an over-penetration applies a reduced tier.
4. **Given** a penetrating hit, **When** the shot passes the armor, **Then** the surviving damage is applied to the module occupying the cell behind the entry point (outer-before-inner), so a shielded/buried module is reached only after its covers.

### User Story 2 - The ship gets worse as it's hit (Priority: P1)

Damage isn't an abstract bar — it's felt. Shoot a ship's thruster and it loses top speed and acceleration; cripple its maneuvering and it turns to mush; destroy its weapon and it can't shoot back; pop its reactor and its power (and shields) collapse. A wounded ship is a degraded ship, which makes targeting *what* you hit a real decision.

**Why this priority**: "Damage is emergent" is the GDD's combat-feel cornerstone and the live tie-back to E006 — a module's effective contribution must scale with its health, or hit-location has no consequence beyond a kill.

**Independent Test**: Damage specific modules and verify the ship's derived stats degrade accordingly (thruster→lower speed/accel, weapon→can't fire, reactor→power/shield loss), reusing the E006 fit-derived stats.

**Acceptance Scenarios**:

1. **Given** a flying ship, **When** its thrust module is damaged (not destroyed), **Then** its top speed and acceleration drop proportionally to the module's lost health.
2. **Given** an armed ship, **When** its weapon module is destroyed, **Then** it can no longer fire.
3. **Given** a powered ship, **When** its reactor is destroyed, **Then** its power budget collapses and power-linked shields drop.
4. **Given** any module, **When** its health changes, **Then** the ship's effective stats re-derive from the current module set (a healthy ship and a battered one of the same fit fly/fight measurably differently).

### User Story 3 - Blow it apart: sections destroyed and severed (Priority: P1)

When a section takes enough damage it's gone — and if blowing it off disconnects a wing or a pod, that chunk physically breaks away and drifts off on its own momentum. Ships come apart in pieces along their structure, not as a single explosion.

**Why this priority**: Destruction + severing is the visible, physical payoff of the destructible-hull model and a project-plan acceptance criterion; it also produces the chunks/wrecks salvage depends on.

**Independent Test**: Destroy a connecting section and verify the hull splits into separate drifting bodies with inherited momentum; verify no split while the hull stays connected and that connectivity is checked only on destruction.

**Acceptance Scenarios**:

1. **Given** a multi-section hull, **When** a section reaches zero health, **Then** it is removed from the layout.
2. **Given** a section whose removal disconnects part of the hull, **When** it is destroyed, **Then** a connectivity (flood-fill) check splits the disconnected region into a separate physical chunk.
3. **Given** a severed chunk, **When** it breaks away, **Then** it inherits the parent ship's linear + angular velocity at its center of mass (it drifts, it doesn't pop in place or freeze).
4. **Given** a hull that stays connected after a section is destroyed, **When** the connectivity check runs, **Then** no split occurs; **and** the check runs only at destruction events, not every frame.

### User Story 4 - Salvage: cut it clean for an intact part (Priority: P2)

A precise attacker disables and cleanly severs a module to scavenge it intact; a brute who blasts straight through it gets only scrap. Destroyed ships and severed chunks linger as wrecks you can pick over — feeding the loop where combat creates the materials the economy runs on.

**Why this priority**: Salvage seeds the economy (E013 consumes it) and rewards the precision/harvester playstyle, but the core combat loop (US1–US3) is demonstrable without it, so it follows the P1 destruction work.

**Independent Test**: Cleanly sever a module (its health intact, surrounding structure gone) and verify it detaches as an intact, re-equippable module; destroy a module through and verify it yields scrap; confirm a destroyed ship leaves a persistent, lootable wreck.

**Acceptance Scenarios**:

1. **Given** a module whose own health is intact, **When** its surrounding structure is severed away, **Then** it detaches as an **intact, operational** module that can be scavenged/re-equipped.
2. **Given** a module that is destroyed or penetrated-through (its own health depleted), **When** it is salvaged, **Then** it yields **scrap**, not an intact module.
3. **Given** a destroyed ship, **When** it dies, **Then** it leaves a persistent, lootable wreck entity; **and** an over-killed ship still leaves at least scrap (never nothing).

### User Story 5 - The defense matrix is a real, readable choice (Priority: P2)

Picking a weapon, choosing a facing, and prioritizing a target are genuine decisions: energy melts shields but plinks off armor; kinetic chews armor but wastes on shields; EM ignores plating to fry systems. No single damage type wins everywhere and no defense is a free pass — and you can tell, in the moment, whether a hit ricocheted, penetrated, or was eaten by shields.

**Why this priority**: It's the tactical depth and legibility that make CAP-004 *fun* and non-degenerate, but it's an emergent property tuned/validated on top of the US1 pipeline rather than a separate system.

**Independent Test**: Compare each channel against each layer and confirm no channel dominates all layers and no layer is bypassed by one channel (effective-HP curves cross); confirm shields regenerate while powered and drop on power loss; confirm a hit's outcome (ricochet/penetrate/absorbed) is distinguishable.

**Acceptance Scenarios**:

1. **Given** the channel × layer matrix, **When** each channel is evaluated against each layer, **Then** every channel has a layer it beats and every layer has a channel it resists — no globally dominant channel, no single-channel-bypassed layer.
2. **Given** a powered ship, **When** time passes without hits, **Then** its shields regenerate; **and When** its reactor/power is lost, **Then** shields drop and the armor layer is exposed.
3. **Given** a hit, **When** it resolves, **Then** its outcome is legibly distinguishable (ricochet vs penetration vs shield-absorb) and identifies the affected layer/module — without numeric spam.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST represent each damage instance as a typed **`DamageEvent`** carrying a **channel** (one of kinetic, thermal/energy, blast, EM, radiation), a magnitude, a penetration value, and a hit geometry (impact point + direction).
- **FR-002**: System MUST resolve a swept-ray weapon hit against the target's **E006 fit layout** to the module/section at the entry point (outer-before-inner) — the fit layout IS the hitbox/armor map.
- **FR-003**: System MUST pass a `DamageEvent` through the ordered defense layers **Shields → Armor → Hull/Structure → Systems**, each absorbing/modifying the event before the next.
- **FR-004**: System MUST apply a data-driven **(layer × channel) resistance matrix** in which each channel is strong against a particular layer (energy→shields, kinetic→armor, blast→hull, EM→systems, radiation→systems/electronics).
- **FR-005**: System MUST compute **effective armor** as nominal thickness divided by the cosine of the impact angle, so a steeper hit presents more armor.
- **FR-006**: System MUST **ricochet** a hit whose impact angle exceeds the ricochet threshold (a steep glancing hit bounces with little/no damage).
- **FR-007**: System MUST **overmatch** — a hit whose penetration size is large relative to the plate thickness ignores the angle/ricochet rule and penetrates.
- **FR-008**: System MUST apply **penetration vs over-penetration** damage tiers: a clean penetration applies its full damage tier, an over-penetration a reduced tier, a non-penetration little/none.
- **FR-009**: System MUST route surviving post-penetration damage to the module/section occupying the cell **behind** the entry point, so placement (covered vs exposed) determines whether a module is reached.
- **FR-010**: System MUST model **Shields** as a regenerating, power-linked pool (strongest vs energy) that absorbs damage first and regenerates over time while powered; a depleted or unpowered shield exposes the armor layer.
- **FR-011**: System MUST track **per-module/section health**; damage reduces it and a section/module reaching zero health is destroyed.
- **FR-012**: System MUST scale a module's contribution to the ship's **effective stats (E006 `ShipStats`)** with its health — a damaged thrust module yields less thrust (lower top speed + acceleration), damaged maneuvering turns more sluggishly — so the ship degrades as it is hit.
- **FR-013**: System MUST disable a destroyed module's function — a destroyed weapon cannot fire; a destroyed reactor stops generating power (collapsing the power budget and power-linked shields).
- **FR-014**: System MUST remove a destroyed section/module from the hull layout at coarse module/section granularity (cell-grid-ready for finer destruction later).
- **FR-015**: System MUST run a **connectivity (flood-fill) check** on the remaining hull grid when a section is destroyed, splitting any region no longer connected to the ship's core into a separate physical **wreck chunk**.
- **FR-016**: System MUST give a severed chunk **inherited momentum** — it drifts with the parent's linear + angular velocity at its center of mass (no zero-velocity pop).
- **FR-017**: System MUST run the connectivity check **only at destruction events**, not per frame.
- **FR-018**: System MUST yield an **intact, scavengeable module** on a clean sever — a module whose own health survives but whose surrounding structure is severed detaches operational.
- **FR-019**: System MUST yield **scrap** (not an intact module) when a module is destroyed or penetrated-through (its own health depleted).
- **FR-020**: System MUST persist destroyed-ship **wrecks** and severed chunks as lootable world entities; an over-killed ship still leaves at least scrap.
- **FR-021**: System MUST resolve all damage, penetration, destruction, severing, and salvage **authoritatively in the shared `sim`/server**, reusing the swept-ray CCD so fast projectiles cannot tunnel.
- **FR-022**: The channels, the resistance matrix, and the armor/penetration/layer values MUST be **data-driven** (tunable as content without code changes).
- **FR-023**: The channel × layer matrix MUST be **non-degenerate** — every channel has a layer it beats and every layer has a channel it resists, with no globally dominant channel or single-channel-bypassed layer (a tunable, test-guarded property).
- **FR-024**: System SHOULD surface a hit's outcome **legibly** — ricochet vs penetration vs shield-absorb is distinguishable and identifies the affected layer/module — without numeric spam (diegetic feedback; the GDD's "audio is the informative channel").

### Key Entities

- **DamageEvent**: a typed damage packet — channel, magnitude, penetration value, impact point + direction; the unit that flows through the defense layers.
- **Channel**: the damage type (kinetic, thermal/energy, blast, EM, radiation); resisted differently per layer.
- **Defense layer**: an ordered absorber (Shields, Armor, Hull/Structure, Systems) with per-channel resistance; Shields additionally regenerate and are power-linked; Armor carries angle/thickness.
- **Resistance matrix**: the data-driven (layer × channel) table governing how much each layer mitigates each channel.
- **Module / Section health**: per-module/section hit points (from the E006 fit layout), depleted by post-penetration damage; zero = destroyed.
- **Wreck / Chunk**: a severed disconnected hull region or a destroyed ship — a persistent physical, lootable world entity with inherited momentum.
- **Salvage outcome**: intact module (clean sever) vs scrap (destroyed/through-killed) — the loot a wreck yields.

## Assumptions & Risks *(mandatory)*

### Assumptions

- E006's `FitLayout` / `resolve_hit` / `CellOccupant{module, health, depth}` provides the hit-location/armor map E007 reads (the dependency contract); E007 damages the per-cell module health that layout already exposes.
- E001/E002 swept projectiles + the E003 server-authoritative sim provide the combat substrate; damage resolves server-side and reuses the swept-ray CCD.
- The E006 `derive_ship_stats` path is extended so a module's health scales its stat contribution (emergent damage reuses fit-derived stats, not a parallel model).
- The Crew layer and the Avoidance/point-defense/ECM layer are deferred to later epics; coarse section granularity is sufficient (cell-ready for finer later).
- Wrecks/salvage persist as in-world entities; their economic value/markets are E013.

### Risks

- **Balance complexity / degenerate matrix** *(likelihood: medium, impact: high)*: 5 channels × 4 layers × penetration mechanics could yield a dominant channel/layer or a one-shot/no-damage binary. Mitigation: a flat-% data-driven matrix, the non-degenerate guard (FR-023/SC-005), penetration tiers (not binary), and grounded-scaled tuning (ADR-0012) verified in playtest.
- **Severing physics/perf** *(likelihood: medium, impact: medium)*: flood-fill + spawning chunks as physics bodies with inherited momentum could be costly or pop visually. Mitigation: connectivity only on destruction (FR-017), coarse granularity, reuse the `sim::Physics` trait, inherit COM velocity (FR-016).
- **Lethality feel** *(likelihood: medium, impact: medium)*: grounded-but-scaled lethality is hard to tune — too lethal frustrates, too spongy bores. Mitigation: ADR-0012 grounded-gameplay-scaled magnitudes + a feel playtest gate; values are data-driven for fast iteration.

## Implementation Signals *(mandatory)*

- `NEW-ENTITY` — `DamageEvent`, `Channel`, the defense-layer model + `ResistanceMatrix`, `Wreck`/`Chunk`, salvage outcomes (the typed-damage + destruction domain, ADR-0008).
- `NEW-API` — the damage-resolution surface (apply a `DamageEvent` → layer traversal → penetration → per-module damage → destruction/severing/salvage), consumed by the weapon/combat systems and reading the E006 hit-map.
- `BREAKING-CHANGE` — combat shifts from whole-ship `Health` / `Target` destruction to **per-module hit-location damage + emergent stat degradation**; the E002/E003 projectile-hit path is replaced/extended.
- `NEW-WORKER` — the destruction-event handler: connectivity severing + wreck/chunk spawning (runs only on destruction).
- `NEW-CONFIG` — data-driven channels, the resistance matrix, and armor/penetration/layer/shield values (balance content).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [US1]: A weapon hit resolves to the entry-point module and its damage is reduced as it passes the defense layers per the channel matrix; a steep-angle hit shows increased effective armor and ricochets past the threshold; a large hit overmatches; and a penetrating hit applies its tiered damage to the module behind the entry point.
- **SC-002** [US2]: As a ship takes module damage it measurably degrades — a damaged thruster lowers its top speed/acceleration, a destroyed weapon cannot fire, a destroyed reactor drops power (and power-linked shields) — i.e., capability scales with module health, not a single HP bar (re-derived from the E006 fit).
- **SC-003** [US3]: Destroying a connecting section severs the hull into separate drifting chunks that inherit the parent's linear + angular momentum; while the hull stays connected no split occurs; and the connectivity check runs only at destruction events.
- **SC-004** [US4]: A cleanly severed module (own health intact, surrounding structure gone) detaches as an intact, re-equippable module; a destroyed/penetrated-through module yields only scrap; a destroyed ship leaves a persistent, lootable wreck and an over-kill still leaves at least scrap.
- **SC-005** [US5]: Across the channel × layer matrix no channel beats every layer and no layer is bypassed by a single channel (effective-HP curves cross); shields regenerate while powered and drop on power loss; and a hit's outcome (ricochet / penetration / shield-absorb) and the affected layer/module are legibly distinguishable.

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| DamageEvent | A typed damage packet (channel, magnitude, penetration, impact geometry) that flows through the ordered defense layers. |
| Channel | The damage type — kinetic, thermal/energy, blast, EM, or radiation — resisted differently per defense layer. |
| Defense layer | An ordered absorber: Shields → Armor → Hull/Structure → Systems; each mitigates per channel. |
| Resistance matrix | The data-driven (layer × channel) table of how much each layer mitigates each channel. |
| Effective armor | Nominal armor thickness ÷ cos(impact angle) — angling presents more armor. |
| Ricochet | A steep glancing hit (past the angle threshold) bouncing off with little/no damage. |
| Overmatch | A hit large relative to the plate ignoring the angle/ricochet rule and penetrating. |
| Penetration / over-penetration | A clean pass-into (full damage tier) vs a pass-through (reduced tier) of the armor. |
| Severing | A connectivity (flood-fill) split of a hull region no longer connected to the core into a separate drifting chunk. |
| Clean sever | Severing a module whose own health is intact, detaching it operational (vs scrap from a through-kill). |
| Wreck / chunk | A persistent, lootable physical body left by a destroyed ship or a severed hull region. |
| Emergent damage | A module's effective-stat contribution scaling with its health, so a damaged ship flies/fights worse. |
