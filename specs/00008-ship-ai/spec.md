---
feature_branch: "00008-ship-ai"
created: "2026-06-10"
input: "AI for movement/waypoints/formation; combat/scouting/search&destroy; ramming-damage-aware; general + scenario; architecture that handles all physics/mechanics AND scales to many ships in big battles"
spec_type: "technical"
spec_maturity: "clarified"
epic_id: "E011"
epic_sources: "{PRD:CAP-008}{SAD:ADR-0011}"
---

# Feature Specification: Ship AI architecture — tiered autonomous behaviors at scale

**Feature Branch**: `00008-ship-ai`  
**Created**: 2026-06-10  
**Status**: Draft  
**Spec Type**: technical  
**Spec Maturity**: clarified  
**Epic ID**: E011  
**Epic Sources**: {PRD:CAP-008}{SAD:ADR-0011}  
**Product Document**: specs/prd.md

> Scope note: this is the **technical architecture core** of the E011 "NPC population & AI" epic — the deterministic, scalable AI *behavior engine* that ships drive through. The broader E011 product scope (NPC population director, automation-floor / offline role-filling) builds on this and is excluded here (see Scope/Excluded).

## Problem Statement *(mandatory)*

The game has rich physics + mechanics (facing-resolved flight, control modules, ramming-as-kinetic-damage, energy/heat gates, factions, sensors) but **no real AI**: the only autonomous code is three hardcoded systems — `seek_system` (a seeker glides straight at the player by mutating velocity), `turret_system` (mounted-gun aim+fire), and `mining_transport_system` (a 4-state nav loop) — and **enemy fighters have no AI at all** (they spawn as static carve-able dummies). There is no AI framework (no behavior selection, perception, steering library, or scheduler), and the current systems **bypass `ShipIntent`** (mutating physics directly), so they don't obey the flight model/modules and don't generalize. Without a real AI architecture there are no opponents to fight, no scenario set-pieces, and no path to the "big battles" the game is built for. The architecture must be deterministic (the project's whole safety story) and scale to many simultaneously-thinking ships per core.

## Scope *(mandatory)*

### Included

- A reusable, **intent-driven movement/steering substrate** (AI pilots via `ShipIntent`, full-physics-correct): seek, arrive, pursue/intercept, waypoint-follow, formation-keep, ramming-aware avoidance, via **context-steering** (interest/danger direction maps).
- A **deterministic AI "brain"** with an **event-driven scheduler** (+ fallback cadence) and **fit-derived tactics** (archetype from the ship's own `ShipStats`), emitting `ShipIntent` on the fixed-step schedule.
- **Behavior-LOD: hierarchical squad command + AOI-tiered sim-LOD** — individual brains near a player, squad brains mid, cheap aggregates far (expanding deterministically on approach), over a tiered build-once-read-many spatial index keyed on authoritative player proximity. The scale backbone.
- **Combat AI** (engage/pursue/position/aim+fire/evade/retreat) that respects energy+heat fire-gates, fire groups, and **ramming-as-kinetic-damage** (uses ramming deliberately only when advantageous).
- **Sensor-driven perception + a faction sensor NETWORK (datalink)** — per-ship sensing fused across a deterministic connected component; jamming/sever fragments it (local fallback).
- **Scenario scripting/orchestration** — designer-authored set-pieces (patrol routes, ambush, defend, retreat) composing/overriding the general brain.
- **Scouting** and **search & destroy** higher-order behaviors.
- An **AI-cost benchmark** (a real brain wired into `fleet_stress`) to set the scale target from data, not guesswork.

### Excluded

- The specific architecture choice (behavior tree vs utility AI vs hierarchical FSM vs GOAP) — that is the `/sddp-plan` decision; the spec only requires "deterministic + composable + time-sliceable." *(Resolved in plan: ADR-0015 chose the utility-scored state machine — TR-020's score-breakdown vocabulary reflects that decision back-propagated by the observability checklist, not a pre-emption of this deferral.)*
- NPC **population director / automation-floor / offline role-filling** (the broader E011 product scope) — this spec is the behavior+architecture engine those will build on.
- Player-ship autopilot / the FC assist features (auto-brake/match-velocity/etc.) — a separate flight-arc epic.
- Strategic / economy-level command AI — formations + squad tactics are in; multi-fleet strategy is later.
- Pathfinding around large concave obstacle fields — the arena is open space; local avoidance only for v1 (no nav-mesh need yet).
- Heat-signature mechanics themselves (a separate heat epic) — perception is built to consume a signature scalar that the heat work will feed.
- The **jamming mechanic** and the **electronic-warfare / sensor module taxonomy** — Active/Passive **Sensor** + Relay/Listener/Node **Datalink** modules (Sensor/Datalink × transmit/receive), plus "transmitting reveals you" — are the **CAP-007** EW epic (Clarifications Q2/Q3). E011 ships only a faction *baseline* Sensor + Datalink and the seam those modules plug into (a per-ship jammed/severed flag + connectivity over transmitters); the taxonomy is recorded as CAP-007's intended direction, not built here.

### Edge Cases & Boundaries

- A ship promoted from cheap-glide LOD (or aggregate) to full-physics as a player approaches must not "pop" — position must be continuous + deterministic at the boundary, and re-collapse cleanly when the player leaves.
- An AI with no live control source (derelict, per the control-modules work) or a dead reactor (no power) must degrade gracefully (drift / cease fire), not thrash.
- An AI that can't perceive any target (nothing in sensor range, or its sensor network is jammed) must have a sane default (patrol / hold / scout), not idle-spin.
- Determinism boundary: every activity decision + all AI math must be reproducible from sim state alone (no per-client camera, no RNG, no HashMap iteration, stable order).

## Technical Objectives *(mandatory for technical specs only)*

### Objective 1 - Intent-driven movement & steering substrate (Priority: P1)

A reusable layer of steering primitives that all AI ships drive through `ShipIntent` (the same surface a human uses), so AI motion obeys the full flight model, control modules, energy/heat, ramming, and determinism for free.

**Why this priority**: Everything else (combat, scouting, scenarios) is built on movement; it's the foundation the user named as the must-have-first milestone.

**Rationale**: Today's seek/mining systems bypass `ShipIntent` and don't generalize; a shared substrate that emits intents is the determinism-clean, physics-correct base.

**Deliverables**:
- Steering primitives (seek/arrive/pursue-intercept/waypoint/formation-keep/avoid) that output a target `ShipIntent`, **inertia-aware** (bias toward headings reachable given current velocity + turn-rate — ships are non-holonomic).
- A **layered** movement model (exact mix = plan): **flow-fields / influence-maps** for group-to-objective movement at scale (sample O(1)/ship; pairs with squads), **context-steering** (per-direction interest/danger maps, ~8–16 slots) for individual tactics (orbit-while-dodge/keep-range; no local-minima/jitter, ~constant cost; beats summed-force), and a **light mutual-avoidance** folded into the danger map (escalate to velocity-obstacle/RVO only if density demands).
- Integration with `ship_motion_system`; reuse of `turret::aim_angle` for intercept; the legacy `seek_system`/mining nav generalized onto (or reconciled with) it.

**Validation Criteria**:
1. **Given** an AI ship with a waypoint, **When** stepped, **Then** it flies there through the real flight model — verified by driving one ship with a recorded `ShipIntent` sequence injected as if from player input and a second ship by the AI emitting the SAME intent sequence, asserting bit-identical per-tick trajectories (position/velocity/heading), plus the static invariant that no AI system writes `Velocity`/`Heading`/`Position` on Active/Mid ships (TR-001/V-6).
2. **Given** two AI ships in a formation slot assignment, **When** the leader moves, **Then** followers hold their relative slots without oscillation — after a settle window of ≤ 10 s (300 ticks @30Hz), each follower's slot-position error stays ≤ 10% of its slot spacing and its turn intent does not flip sign on consecutive thinks (no sustained heading chatter).

### Objective 2 - Deterministic AI brain: event-driven scheduler + fit-derived tactics (Priority: P1)

A per-ship behavior-selection mechanism (composable; the concrete model chosen in `/sddp-plan`) that picks the active behavior and emits `ShipIntent`, on the fixed-step schedule. Re-evaluation is **event-driven** (re-decide on hit / target-lost / new-contact / waypoint-reached) with a slow fallback cadence + a per-tier think-cadence. Tactics **derive from the ship's OWN fit/stats**: an archetype (brawler / kiter / orbiter / rammer…) is classified ONCE when `ShipStats` change (cached on the brain), so behavior emerges from ship design and new modules/weapons auto-get sensible AI.

**Why this priority**: The substrate (OBJ1) needs a driver; without a brain there are no behaviors.

**Rationale**: Must be deterministic (no RNG / stable iteration) and cheap on many ships; event-driven re-decide (≈0 cost when nothing changed) + fit-archetype caching (per-think = read enum + branch, ~free) keep per-ship cost minimal while making behavior integrated with the ship's real capabilities.

**Deliverables**:
- An AI brain component (current behavior + blackboard + cached fit-archetype).
- The event-driven + cadence scheduler (gated, fixed-step).
- The P1 "simple" behavior set (idle/hold, patrol, waypoint, follow, formation).
- Fit→archetype classification on `Changed<ShipStats>`.

**Validation Criteria**:
1. **Given** identical sim state, **When** the brain runs twice, **Then** it selects identical behaviors + emits identical intents (deterministic).
2. **Given** a ship that takes a hit, **When** stepped, **Then** it re-evaluates that tick (event), not only on its cadence; an idle calm ship triggers no decision work — asserted via a think-invocation counter: zero brain evaluations between its fallback-cadence ticks absent events.
3. **Given** two ships with different fits (armor-brawler vs fast glass-cannon), **When** each engages, **Then** they adopt different archetype tactics from their own derived stats — assertable as range-band occupancy: the brawler spends the majority of the engagement inside its close-range band while the kiter holds beyond its keep-range band (bands = the named `AiTuning` archetype thresholds), and the two ships' median engagement ranges must not overlap.

### Objective 3 - Behavior-LOD: hierarchical squad command + AOI-tiered sim-LOD (Priority: P1)

Scale not just *motion* but the **unit of AI itself**, by authoritative player-ship proximity over a **tiered, shared, build-once-read-many spatial index**: **near → individual ship brains** (full physics, full think); **mid → one squad brain** steers a whole formation as a unit (full physics, cheap per-ship execution); **far → a cheap aggregate** (the group is one entity, cheap-glide) that **expands into individuals** deterministically when a player approaches (and collapses when they leave). Decision cost then scales with the number of **squads/groups**, not ships.

**Why this priority**: The "big battles" backbone. The fleet bench's caveat ("ships skip the Target AI path → this is combat-sim cost, not AI cost") means AI think-cost is the new variable to bound — and squad/aggregate LOD is what bounds it (decisions O(squads), per-ship execution O(1)).

**Rationale**: Two compounding levers. (a) **Build the spatial index once per tick; collision + sensors + AOI all READ it** (no per-system rebuilds; one deterministic structure) — with AOI/LOD on a **coarser interest tier** than the fine collision grid (a coarse query on the fine grid would scan thousands of cells), that coarse tier also seeding future **sector-sharding**. (b) **Squad command** collapses the expensive decision from O(ships) to O(squads) (~10–20× fewer reasoning passes) + yields coordinated tactics (focus-fire / pincers / screens) that N independent agents never produce; the aggregate far-tier drops cost further. Keying on authoritative ship positions (never a per-client camera) keeps it MP-safe + deterministic. Squads are composed by each member's **derived kinematics (`ShipStats` top-speed/agility) + fit-archetype role** (not a hardcoded ship type) and **pace to the slowest essential member (anchor)** — faster escorts spend their speed surplus on role behaviors (screen/flank/intercept) while holding station. An escort fleet is **one mixed squad** (heavies core, escorts screen) when small, or **a wing of role-coherent squads under a wing brain** when large (the ship→squad→wing→aggregate hierarchy). (Exact tier sizes + brain hierarchy + index structure = the `/sddp-plan` decision.)

**Deliverables**:
- The tiered AOI/LOD classifier on the shared index (coarse interest tier over the existing fine `sim::broadphase`).
- A **squad/group brain** that emits orders to member ships (which execute via cheap O(1) per-ship steering — formation slot / flow-field sample); composition-aware, pace-anchored squad formation (+ the wing tier for large mixed fleets). **Squad membership is scenario/spawner-authored; the squad brain re-derives pace/roles on a member-death EVENT; a squad of 1 degrades to an individual brain; no runtime cross-squad re-clustering in v1** (Clarification Q6).
- The cheap-glide aggregate far-tier + deterministic expand/collapse. **Promotion triggers: a player approaching, OR the far coarse scan detecting a HOSTILE group/player — two hostile aggregates that detect each other BOTH promote to the squad tier and fight at full physics (no combat occurs while dormant)** (Clarification Q1).
- An updated `fleet_stress` bench wiring a REAL brain in to **measure AI-thinking cost first**, so the scale target is data-driven.

**Validation Criteria**:
1. **Given** N AI ships in squads with one player, **When** stepped, **Then** decision work scales with squad count (not N) — asserted via per-tier think-invocation counters: individual-brain thinks ≈ Active-tier ship count, squad-brain thinks ≤ squad count, and doubling N at fixed squad count leaves the decision count ~flat — only near-player ships get individual brains, and AI overhead stays within the ≤30% relative budget at the pinned baseline N (TR-017/TR-018) — the absolute target N is an OUTPUT of the bench, not this criterion's gate.
2. **Given** an aggregate squad far off, **When** the player approaches, **Then** it expands into individual full-physics ships with continuous, deterministic positions (no pop), and re-collapses when the player leaves.
3. **Given** two hostile aggregates far from any player, **When** their far coarse scans detect each other, **Then** both promote to the squad tier and engage at full physics; while neither detects the other they pass without combat.
4. **Given** a squad whose pace-anchor is destroyed, **When** stepped, **Then** the squad re-derives its pace/roles that tick (member-death event) without dissolving, and a squad reduced to one ship continues as an individual brain.

### Objective 4 - Combat AI with ramming awareness (Priority: P2)

Autonomous fighters that engage, pursue, position for their weapons' arc, aim+fire (respecting the energy + heat fire-gates and fire groups), evade, and retreat — and that **understand ramming is `RAM_CARVE_K·closing²` kinetic damage**, choosing to ram only when advantageous (finishing a near-dead/disabled target, a desperate last resort, or vs. a much weaker hull), and otherwise avoiding self-destructive collisions.

**Why this priority**: The visible payoff (real opponents) — but it builds on the P1 substrate/brain/LOD, so it's P2.

**Rationale**: Combat must obey the same gates a human does (no firing while overheated/out of energy) and weigh ramming's cost/benefit, per the explicit requirement.

**Deliverables**:
- Combat behaviors (engage/pursue/position/strafe-run/evade/retreat/ram-decision).
- Energy+heat-aware fire control; fire-group selection.
- A ram cost/benefit evaluation using closing speed + relative hull/shield state.

**Validation Criteria**:
1. **Given** an AI fighter vs. a static target, **When** engaged, **Then** it closes to weapon range, aims (lead solver), and fires only when energy+heat permit, destroying the target within a bounded window (≤ 3600 ticks = 2 sim-minutes @30Hz; the test fails on timeout rather than running open-ended).
2. **Given** an AI fighter and a near-dead vs. a healthy stronger enemy, **When** deciding, **Then** it rams the near-dead/disabled one but does NOT ram the healthy stronger one.

### Objective 5 - Sensor-driven perception + faction sensor NETWORK (datalink) (Priority: P2)

AI targeting + awareness run on the **sensor/signature mechanics** (detection range, target signature — size now, heat-signature later) over the shared spatial index — so undetected / low-signature ships are harder to find and lock, exactly as for a human. On top of per-ship sensing, a **faction sensor NETWORK fuses connected members' detections into a shared situational picture**: connectivity = the deterministic connected component of linked ships + sensor stations (a flood-fill, like the hull-sever connectivity already in the engine); **jamming / a downed relay / range SEVERS links → the network FRAGMENTS**, and an isolated fleet falls back to its own component's fused picture (a fully-cut-off ship → its own local sensors only; link-jamming vs sensor-jamming are LAYERED degradations — v1 ships only the link layer: either `LinkState` flag, `jammed` or `severed`, removes the ship from network connectivity while its OWN local sensing stays unaffected; sensor-jamming is the CAP-007 layer).

**Why this priority**: Makes combat/scouting "integrated into the mechanics" (stealth/ECM/recon matter vs AI, not an omniscient targeter); combat (OBJ4) + squads (OBJ3) consume the shared picture; and it HELPS scale — a clustered fleet is ONE fused regional scan + O(1) reads instead of N overlapping per-ship scans.

**Rationale**: Perception (what the AI knows) is gameplay, separate from sim-LOD (what's simulated) — a nearby undetected ship is still simulated, just not yet *perceived* (avoids circular dormancy). Fusing per network-component is both cheaper in the clustered case and a deep electronic-warfare lever.

The model is **two systems, each with a transmit/receive nature: Sensor (detect) + Datalink (share)** (Clarification Q3). v1 ships a faction **baseline** of both (every faction ship detects within a base range and is on the network) so the AI works + is testable out of the box; the connectivity + fusion seam is built to ACCEPT the CAP-007 modules unchanged. **Perception runs at a tier-scaled cadence** (Clarification Q4): near ≈ every think; mid = one fused scan per squad (~0.5 s); far/aggregate = a coarse, signature-aware scan (~2–5 s) that doubles as the OBJ3 promotion trigger — signature/stealth is respected at every tier. **Jamming is consumed as a seam, not built here** (Clarification Q2): a deterministic per-ship jammed / link-severed flag (set by scenario/tests now; the CAP-007 EW epic feeds it later) drops a member from the connected component → it falls back to its own local picture.

**Deliverables**:
- A per-ship perception query (sensor range + signature threshold) over the shared index, run at the tier-scaled cadence above.
- A deterministic faction sensor-network connectivity (connected component over transmitting members) + per-component fusion (shared contact picture); baseline participation for every faction ship in v1.
- A per-ship jammed / link-severed seam flag → fragmentation + local fallback (mechanic = CAP-007).
- A signature input the heat work can later feed.

**Validation Criteria**:
1. **Given** an enemy outside sensor range / below the signature threshold, **When** the AI thinks at any tier, **Then** it does not target/detect it (perception is signature-aware near AND far).
2. **Given** a fleet sharing a sensor network and one member detecting a target, **When** the network is intact, **Then** all members see the contact; **When** a member's jammed/severed flag is set, **Then** it loses the shared contacts and falls back to its own local picture.

### Objective 6 - Scenario scripting / orchestration (Priority: P2)

A layer letting scenarios author set-pieces — patrol routes, ambushes, defend-the-outpost, scripted retreats — that compose or override the general brain, with the general AI as the default.

**Why this priority**: Enables the "special scenarios" the user wants; depends on the brain (OBJ2) existing.

**Rationale**: Designer intent (a scripted ambush) must direct the general autonomy without forking it.

**Deliverables**:
- A scenario-script/role mechanism (assign a ship a scripted goal/route/posture).
- Composition rules (script directs, general brain fills tactics).
- Examples (patrol, ambush, defend).

**Validation Criteria**:
1. **Given** a ship assigned a patrol route, **When** stepped, **Then** it follows the route and switches to combat behavior when a perceived threat appears, returning to patrol after.
2. **Given** an ambush script, **When** the trigger condition (a perceived player in range) fires, **Then** the assigned ships transition from hold to engage together — the trigger event forces a same-tick re-evaluation for every assigned ship, so ALL transition on the same tick (the coordinated-trigger assertion).

### Objective 7 - Scouting & Search-and-Destroy (Priority: P3)

Higher-order behaviors: scouting (explore an area, report/maintain contacts, avoid engagement) and search & destroy (sweep a region, hunt + kill perceived targets).

**Why this priority**: Richest behaviors; nice-to-have after the core fight works.

**Rationale**: Build on the brain + perception + combat; not needed for a viable opponent MVP.

**Deliverables**:
- Scout behavior (area coverage + contact reporting + disengage-on-threat).
- S&D behavior (region sweep + hunt + prosecute).

**Validation Criteria**:
1. **Given** a scout with no escort, **When** it perceives a stronger threat, **Then** it disengages and survives rather than fighting.
2. **Given** an S&D group + a hidden target in a region, **When** it sweeps, **Then** it covers the region — ≥ 90% of the region's coarse interest-tier cells sensor-swept (entered some member's sensor radius) within the scenario's time budget — and engages the target once perceived.

### Technical Constraints

- **Determinism (non-negotiable):** fixed-step schedule, no RNG (deterministic hashing only, like the turret jitter), no HashMap iteration, stable order; the golden determinism + botkit/harness + `demo_enemies_smoke` tests MUST stay bit-identical (new systems additive + `ScenarioActive`-gated; AI ships don't exist in those worlds).
- **Physics-correct seam:** full-physics AI ships drive exclusively through `ShipIntent` (no direct Velocity/Heading mutation); only dormant-LOD ships use the cheap-glide path.
- **MP-safe LOD:** the active/dormant trigger keys off authoritative ship positions via the shared spatial index — never a per-client camera. Render LOD (client-side, camera-based) is unchanged and separate.
- **Performance budget:** the AI think + steering + perception cost must fit within the 30Hz (33.3 ms) tick alongside the combat sim (the 60Hz / 16.7 ms figure is informational only — the acceptance gate is asserted @30Hz); scale is achieved by AOI/behavior tiers (squad/aggregate far) + event-driven thinking, no hard cap — **graceful degradation in a fixed order** when the tick budget is exceeded: (1) stretch fallback think cadences (Mid first, then Active), (2) demote Mid→Dormant at squad granularity from the AOI edge inward, (3) skip non-event perception scans; event-driven reactions of player-adjacent Active ships degrade last. **Acceptance floor (Clarification Q5):** the canonical gate is **TR-017** (≤ 30.0% mean overhead + the p99 budget, player-local, pinned N = 2000 @30Hz, all AI-attributable buckets, per the TR-018 protocol) — parameters are NOT restated here to keep one source of truth; the bench sets the absolute target as an OUTPUT (not a gate).
- **Windowed/scenario-gated:** AI runs only where scenarios place it; headless/golden worlds are untouched.

## Integration Points *(mandatory for technical and operational specs)*

- **IP-001**: AI movement depends on `ShipIntent` + `ship_motion_system` (`crates/sim/src/flight.rs`) — AI emits intents, flight consumes them.
- **IP-002**: AOI/LOD + sensor perception depend on a **tiered, build-once-read-many** spatial index — the existing fine `sim::broadphase` (collision + sensor radius-queries) plus a coarser interest tier for AOI/LOD (which also seeds future sector-sharding). All consumers read the same per-tick structure; no per-system rebuilds.
- **IP-003**: Combat gunnery reuses `turret::aim_angle` (lead/intercept solver) + the energy/heat gates (`energy_system`) + fire groups (`WeaponGroups`).
- **IP-004**: Targeting reuses `Faction`/`hostile()` (`crates/sim/src/components.rs`); ramming reasoning reads the `RAM_CARVE_K·closing²` model (`crates/sim/src/collision.rs`).
- **IP-005**: Perception depends on the `Sensor` module data + a target signature scalar (size now; the future heat-signature work feeds it).
- **IP-006**: Generalizes/subsumes the existing `seek_system`, `mining_transport_system` (`MiningState`), and turret targeting as precedents/consumers of the new substrate.
- **IP-007**: The faction sensor-network connectivity reuses the deterministic **connected-component flood-fill** already used for hull severing (`sim::damage` sever logic) — applied to "linked, un-jammed" ships+stations instead of cells.
- **IP-008**: The `fleet_stress` example (`crates/server/examples/`) is extended to run a real brain so AI-think cost is measured before the scale target/architecture is locked.

## Requirements *(mandatory)*

### Technical Requirements *(technical specs only)*

- **TR-001**: AI MUST control full-physics ships exclusively by writing `ShipIntent` (forward/strafe/turn/fire/…), never by directly mutating `Velocity`/`Heading`.
- **TR-002**: The substrate MUST provide composable steering primitives (seek, arrive, pursue/intercept, waypoint-follow, formation-keep, avoid) that output a `ShipIntent`.
- **TR-003**: Steering MUST be inertia-aware — it MUST account for the ship's turn-rate + momentum (non-holonomic) rather than assuming instantaneous heading change.
- **TR-004**: The AI brain MUST be deterministic — identical sim state MUST yield identical behavior selection and identical emitted intents (no RNG, no HashMap iteration, stable iteration order). Score math MUST be strict f32 — no fast-math compile flags and no platform-`libm` transcendentals (`sin`/`cos`/`exp`/`powf`) inside utility scoring — enforced as a verifiable rule: a CI lint/grep check over `sim::ai` scoring code plus the TR-019 checksum suite.
- **TR-005**: The brain MUST re-evaluate on events (damage taken, target lost, new contact, waypoint reached) with a bounded fallback cadence, so calm ships incur ≈0 decision cost.
- **TR-006**: The brain MUST classify a ship's tactic archetype from its derived `ShipStats`, recomputed only on `Changed<ShipStats>` (cached), so tactics emerge from the fit.
- **TR-007**: The system MUST classify each AI ship into a sim-LOD tier (active / mid / dormant) by authoritative player-ship proximity over a shared spatial index, never a per-client camera.
- **TR-008**: Dormant ships MUST use a cheap-glide motion path and MUST promote to full-physics with a continuous, deterministic position when a player nears (and demote when they leave) — no positional discontinuity. "No pop" is defined: the promote-tick position MUST equal the glide-extrapolated position bit-exactly; the ONLY permitted deviation is the deterministic validity nudge — a de-penetration applied along the reverse of the glide direction, at promotion, before the first full-physics tick, bounded by the named `AiTuning.promote_nudge_max` field (default in the data-model AiTuning table) — so the boundary test asserts the promoted position is within that bound of the glide position AND not inside geometry.
- **TR-009**: The system MUST support hierarchical squad command (a squad brain emits orders; members execute via O(1) local steering) so total decision cost scales with squad count, not ship count.
- **TR-010**: Squads MUST be composed from member kinematics (`ShipStats`) + fit-archetype and MUST pace to the slowest essential member; large mixed fleets MUST split into role-coherent squads under a wing brain. Membership is scenario/spawner-authored; on a member-death event the squad MUST re-derive pace/roles (not dissolve), and a squad reduced to one ship MUST degrade to an individual brain. No runtime cross-squad re-clustering in v1.
- **TR-011**: Combat AI MUST respect the energy + heat fire-gates (never fire while out of energy or overheated) and MUST select/use fire groups.
- **TR-012**: Combat AI MUST evaluate ramming via the `RAM_CARVE_K·closing²` model and ram only when advantageous (target near-dead/disabled or much weaker), avoiding self-destructive collisions otherwise. The decision thresholds are the named `AiTuning` fields `ram_target_hull_frac` / `ram_self_margin` / `ram_min_closing` (defaults in data-model), so both OBJ4-VC2 scenarios are constructible as fixtures against pinned values.
- **TR-013**: AI perception MUST be gated by the sensor/signature mechanics (detection range + signature threshold) over the shared index, at a tier-scaled cadence (near ≈ every think; mid = one fused scan per squad; far = a coarse signature-aware scan that is the OBJ3 promotion trigger — cadence defaults pinned in the data-model `AiTuning` table), respecting signature at every tier; and MUST be separate from the sim-LOD activity decision.
- **TR-014**: A faction sensor network MUST fuse connected members' detections into a shared picture via deterministic connectivity over transmitting members, with every faction ship participating at a v1 baseline; it MUST fragment when a member's jammed/link-severed seam flag is set (the flag is set by scenario/tests in E011; the jamming MECHANIC + the EW Sensor/Datalink module taxonomy are CAP-007), falling an isolated member back to its own local picture.
- **TR-015**: Scenarios MUST be able to assign scripted goals/roles (patrol, ambush, defend, retreat) that compose with or override the general brain.
- **TR-016**: All new AI systems MUST be additive and `ScenarioActive`-gated so the golden determinism / `demo_enemies_smoke` / harness/botkit tests stay bit-identical. The "golden trio" gate is exactly: `crates/server/tests/determinism.rs` (golden determinism), `crates/server/tests/demo_enemies_smoke.rs`, and the harness/botkit suite (`crates/server/tests/harness.rs` + `crates/server/tests/botkit/`) — all run via `cargo test -p server` in CI.
- **TR-017**: **The canonical acceptance gate** (all other sections cite this requirement rather than restating its parameters): an AI-cost benchmark (a real brain wired into `fleet_stress`) MUST measure per-tick AI cost as **all AI-attributable buckets per TR-018** (think, steering, perception scans, squad brains, LOD classify/promote, coarse-tier build); AI overhead MUST stay ≤ 30.0% (hard gate) of the no-AI baseline mean tick at the pinned baseline N = 2000 @30Hz, AND the `--ai` run's p99 tick MUST hold `max(33.3 ms, paired baseline p99)` — the absolute 33.3 ms budget when the baseline itself holds it, else AI must not WORSEN the tail *(p99 clause amended 2026-06-10 from the T024 bench measurement: the pre-existing no-AI baseline at the pinned N = 2000 has p99 ≈ 77 ms from mass-carve combat spikes — a known characteristic since R56/R57 — so a literal absolute p99 budget is unsatisfiable by any AI implementation; the relative form preserves TR-018's anti-burst intent)*; scoped player-local (off-screen promoted battles excluded from the numerator, measured + reported separately — the STF-001 signal); the absolute concurrent-ship scale target MUST be set from that measurement as a bench OUTPUT (not a gate) and recorded in the bench report + plan Performance Goals.
- **TR-018**: The TR-017 bench MUST follow a pinned, reproducible protocol: release build, one shard/core, the R57 pinned 2-gun `fleet_stress` configuration (R57 = the repo's existing pinned-formation bench methodology, documented in `crates/server/examples/fleet_stress.rs`) at N = 2000 @30Hz with one authoritative player ship at the engagement line (worst-case Active-tier mix, sustained combat so events fire continuously); ≥30 warmup ticks discarded + a ≥120-tick measured window; baseline and `--ai` runs paired on the same machine/session with identical composition and fight; gate statistic = mean tick (p50/p99 also reported — the `--ai` p99 must hold `max(33.3 ms, baseline p99)` per TR-017's amended p99 clause, so promotion/scan bursts can't hide in the mean); cost reported per bucket (think, steering, perception scans, squad brains, LOD classify/promote, coarse-tier build — counted INSIDE the AI numerator in v1 — and off-screen promoted battles, reported separately outside the numerator); plus a calm-fleet case (no hostiles → think-bucket cost ≈ 0, verifying event-driven idle savings) and a fixed-N squad-size sweep (decision cost tracks squad count; per-member execution stays ~constant). With `--ai` absent, `fleet_stress` MUST run byte-identical to today's no-AI path (the baseline cannot be contaminated). The `--ai` run MUST emit a machine-readable report (per-bucket means, p50/p99, overhead %) and exit non-zero on a ≤30%-gate or p99-budget breach, so CI can consume the result without parsing logs.
- **TR-019**: A NEW AI-populated determinism test MUST exist alongside the preserved golden trio: an AI scenario world (brains, squads, perception/network, and at least one aggregate expand/collapse round-trip) rebuilt fresh from the same scenario inputs in the same binary and re-run MUST be bit-identical, compared via a per-tick state checksum over `Position`/`Velocity`/`Heading`/`ShipIntent` plus AI behavior state — catching drift at the first divergent tick, not just at end state. (Cross-binary/cross-platform determinism is out of scope.)
- **TR-020**: Dev-time AI observability (AD-006 — the plan's score-debug architecture decision). The client dev panel MUST provide: **(a) a per-ship AI inspection view** — ship chosen from a list of AI ships sorted by distance to the player ship (default = nearest; stable-id entry override) — showing the current behavior; the utility score breakdown **captured at the last actual think** (per-candidate-behavior consideration values and the curve-multiplied score, the `momentum_bonus` applied to the incumbent, the priority bucket, and the winning score — never a live recompute), with staleness shown via `last_think_tick` + `commit_until_tick`; active target + `ContactList`; sensor-network component membership + `LinkState`; AOI tier; squad/wing + current `SquadOrder` (squad-driven ships show the squad/wing brain's order decision; dormant aggregates show tier + glide state); the active degraded-state cause when applicable (derelict/no-power `Hold`, no-perceived-target role fallback, missing-tuning defaults); and a bounded recent transition history (bound = the named `AiTuning.debug_history_len` field, default 16 entries; behavior from→to + triggering event/dominant consideration + tick, including squad lifecycle events: member-death re-derive, pace-anchor change, squad-of-1 degradation, aggregate collapse/expand). **(b) Live `AiTuning` editing** of ALL field groups, reusing the existing `SimTuning`/`MiningTuning` dev-panel slider + RON-save pattern (a requirement, not a hint) — closing the tuning feedback loop: an edit's effect on scores/behavior selection MUST be observable in (a). **(c) A runtime AI metrics readout**: per-tier ship counts + per-tier think time, think counts split event-triggered vs fallback-cadence, live squad/wing and dormant-aggregate counts, tier promotion + demotion rates, perception scan counts/costs per tier, fused-contact totals, and the live off-screen promoted-battle count (the STF-001 signal). The view MUST be strictly read-only — viewing never triggers thinks or perturbs sim state, decision order, or determinism — and ALL capture (scores, transitions, metrics) MUST be compile-time/feature-gated OFF in headless + bench builds (zero cost in the TR-017 measured path, per AD-006). Scope: client-embedded loopback ONLY (direct ECS reads of the embedded server world; no replication path; edits unavailable/unauthorized on a networked client); golden/bench runs use pinned `AiTuning` defaults, and a mid-run edit invalidates comparability with previously recorded runs/goldens; worlds with AI gated off render an empty AI section and write nothing (the golden/headless suites run without the client entirely). v1 dev-surface scope is exactly (a)–(c); world-space overlays (tier coloring, contact/sensor-network visualization) are post-E011. The SAD `tracing` + Prometheus baseline applies when the headless server grows runtime metrics (a later epic); v1 observability surfaces = this dev panel + the TR-018 machine-readable bench report. **Verification**: no SC covers this dev-time requirement by design — delivery is verified via the plan coverage map (T014 capture seam, T038 inspection view + live editing, T039 metrics readout).
- **TR-021**: The system MUST provide the OBJ7 higher-order behaviors: a **scout** MUST cover an assigned area, report/maintain contacts into its faction picture, and disengage (and survive) when it perceives a superior threat rather than fighting; a **search-and-destroy** group MUST sweep an assigned region (≥ 90% of its coarse interest-tier cells sensor-swept within the scenario's time budget) and engage targets once perceived. Verified by OBJ7 VC1/VC2 + SC-007.

### Key Entities *(include for product or technical specs if feature involves data)*

- **AI Brain**: a ship's autonomous controller — current behavior/goal, perception state, cached fit-archetype, think-tier; emits `ShipIntent`.
- **Behavior**: a selectable unit of conduct (idle/patrol/waypoint/formation/engage/evade/scout/sweep/ram) producing steering + fire intent.
- **Squad / Wing**: a group of ships under one brain (move/engage/form-up orders); a wing is a group of role-coherent squads.
- **Perception / Contacts**: the targets an AI currently knows about, from sensors + signatures over the spatial index (and the fused faction picture).
- **Sensor Network**: a faction's connected component of linked ships + stations sharing a fused contact picture; fragments under jamming/sever.
- **AOI Tier**: an AI ship's sim-cost classification (active / mid / dormant) from authoritative player proximity.
- **Scenario Script / Role**: a designer-authored goal/route/posture assigned to ships, directing the general brain.

## Assumptions & Risks *(mandatory)*

### Assumptions

- The existing deterministic spatial broadphase can be reused for AOI + sensor queries without a determinism regression.
- Driving AI through `ShipIntent` is fast enough at scale (the intent write is trivial; the flight-model cost is the same one combat sim already pays).
- The arena stays open-space (local avoidance suffices; no nav-mesh needed for v1).
- A target "signature" scalar is available (ship size today; heat-signature later).
- The hull-sever connectivity flood-fill is reusable for sensor-network connected components.
- Replay/desync diagnosis needs no AI-state serialization: all AI state is ephemeral and re-derivable from sim state and all decisions are deterministic (TR-004), so replaying recorded inputs reproduces AI decisions exactly — consistent with the SAD replay-recorder baseline; the TR-019 per-tick checksum is the desync/drift detector (first divergent tick), so AI is IN replay scope without serializing brain state.

### Risks

- **AI think-cost at scale** *(likelihood: medium, impact: high)*: per-ship behavior selection may dominate the tick; mitigate via squad/aggregate behavior-LOD + event-driven thinking + the new bench measuring AI cost specifically (TR-017).
- **Determinism regression** *(likelihood: medium, impact: high)*: AI is broad new sim code; mitigate by additive `ScenarioActive`-gating, no-RNG/no-HashMap discipline, and the golden tests as the gate (TR-016).
- **LOD-boundary artifacts** *(likelihood: low, impact: medium)*: position pop / divergence when an aggregate or glided ship promotes to full physics; mitigate with a continuous, deterministic glide that tracks the same waypoint the full path would, and deterministic expand/collapse.
- **Unbounded off-screen combat at MMO scale** *(likelihood: medium, impact: high — deferred)* (STF-001): Q1's auto-promotion of hostile far groups means off-screen battles run at full physics with no cap, so total cost can scale with world-wide combat activity, not just concurrent players — a galaxy-wide war could exceed the ≤30% budget. Accepted unbounded for v1 (the budget is asserted relative to player-local load); the MMO-scale mitigation (a global cap on concurrent live off-screen battles, or an abstract squad-attrition resolution for the overflow) is deferred until that load is real.

## Implementation Signals *(mandatory)*

- `NEW-ENTITY` — an AI brain component (+ cached fit-archetype + perception/contacts) on AI ships; a squad/group entity (membership + squad brain); a faction sensor-network component/picture.
- `NEW-WORKER` — the fixed-step AI systems (event-driven scheduler, squad brains, AOI/behavior-LOD classifier, cheap-glide aggregate + expand/collapse, sensor-network fusion) — all `ScenarioActive`-gated.
- `NEW-CONFIG` — AI tuning (think cadences/AOI radii per tier, squad sizes, behavior + ram-decision + archetype thresholds, sensor/jamming params), live-editable like the existing tuning resources.
- `NEW-API` — internal steering-primitive (context-steering) + behavior + squad-order interfaces (the substrate the brains compose).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [OBJ1]: AI shares the human control path — replaying an AI ship's emitted `ShipIntent` sequence as if from player input reproduces its per-tick trajectory (position/velocity/heading) bit-identically, AND no AI system writes `Velocity`/`Heading`/`Position` on Active/Mid ships (TR-001/V-6 — the physics-correct-seam invariant the test actually asserts).
- **SC-002** [OBJ2]: Re-running the AI brain on identical sim state — scope: a fresh world rebuilt from the same scenario inputs in the same binary (the existing determinism-test pattern; cross-binary out of scope) — yields identical behavior selection + intents, verified by byte-level per-tick state checksums (position/velocity/heading/intent + behavior state, TR-019); the golden determinism/demo tests stay bit-identical.
- **SC-003** [OBJ3]: AI overhead passes the canonical **TR-017** gate (≤ 30.0% mean + p99 budget, player-local, pinned N = 2000 @30Hz, all AI-attributable buckets per TR-018; the bench sets the absolute target as an output), decision work scales by squad count (not ship count), off-AOI groups run as cheap-glide aggregates, and a dormant aggregate expands to full-physics individuals with no positional pop (per TR-008's definition) on player approach.
- **SC-004** [OBJ4]: An AI fighter engages and destroys a target while never firing overheated/out-of-energy, chooses to ram a near-dead/disabled target but not a healthy stronger one, and ships with different fits adopt different archetype tactics (brawler rushes, glass-cannon kites — the OBJ2-VC3 outcome, asserted here in its combat context).
- **SC-005** [OBJ5]: An AI does not target an enemy outside its sensor range / below its signature threshold; a fleet on an intact sensor network all see a contact one member detects; a jammed/severed member loses the shared contacts and falls back to its own local picture.
- **SC-006** [OBJ6]: A patrol-scripted ship follows its route, breaks to engage a perceived threat, and resumes patrol after — and an ambush script triggers a coordinated transition on its condition.
- **SC-007** [OBJ7]: A scout disengages and survives a superior threat; an S&D group sweeps a region and kills a target once perceived.

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| AOI (Area of Interest) | The region around authoritative player ships within which entities are fully simulated/thought; drives sim-LOD tiers (not rendering). |
| Sim-LOD tier | An AI ship's cost class — active (full physics + full think), mid (full physics, squad-driven), dormant (cheap-glide aggregate). |
| Behavior-LOD | Scaling the *unit* of AI by proximity: individual ship brains near a player → one squad brain mid → a cheap aggregate far (expanding to individuals on approach). Decision cost scales with squads, not ships. |
| Squad brain | One AI controller that decides for a group of ships (move/engage/form-up); members execute with cheap O(1) local steering (formation slot / field sample). |
| Pace anchor / wing | A squad moves at its slowest essential member's speed (the anchor); large mixed fleets split into role-coherent squads under a wing (the ship→squad→wing→aggregate hierarchy). |
| Sensor network (datalink) | A faction's connected component of linked ships + sensor stations that fuses their detections into one shared situational picture; jamming/sever fragments it, isolating members to their local sensors. |
| Flow-field / influence-map | A precomputed per-region grid of "which way to go" / threat values that many ships sample O(1) each — the scalable method for group movement-to-objective. |
| Context-steering | Movement via per-direction interest/danger maps (~8–16 compass slots) — pick the best `interest − danger` heading; smooth orbit-while-dodging, no local-minima/jitter, constant cost. |
| Fit-archetype | A tactic class (brawler/kiter/orbiter/rammer…) classified once from a ship's own derived `ShipStats`, so behavior emerges from ship design. |
| Cheap-glide | The dormant-LOD motion approximation (move toward waypoint at ~cruise) used only for ships far from all players, off the authoritative flight model. |
| Intent-driven | AI controls a ship by writing `ShipIntent` (forward/strafe/turn/fire), the same surface a human uses, so it obeys the full flight model + determinism. |

## Clarifications

### Session 2026-06-10

- Q: Far-tier combat — when two HOSTILE squads/aggregates meet far from every player, does combat occur out there? -> A: They auto-promote. The far coarse scan (perception, below) detecting a hostile group/player promotes both groups to the mid (squad-brain) tier, and combat then runs at full physics. Dormant aggregates do NOT fight while dormant — they promote first.
- Q: Jamming for the sensor network (TR-014) — build the EW mechanic here, or only the seam? -> A: Seam only. E011 defines a deterministic per-ship jammed / link-severed state that the connectivity flood-fill consumes (set by scenario scripts/tests now); the actual jamming MECHANIC is the CAP-007 electronic-warfare epic.
- Q: Sensor-network link model + the Sensor/Datalink × TX/RX module taxonomy? -> A: Model = two systems — **Sensor** (detect; active/passive) + **Datalink** (share; relay/listener/node), each with a transmit/receive nature. E011 ships a faction **baseline** of both (so the AI works + is testable) and the connectivity seam ACCEPTS the modules. The full EW module taxonomy (Active/Passive Sensor; Relay/Listener/Node Datalink; emission-reveals-you) is the **CAP-007** epic — recorded here as the intended direction the seam targets, not built in E011.
- Q: Which LOD tiers perceive? -> A: All tiers, **tier-scaled cadence**: near ≈ every think; mid = one fused scan per squad (~0.5 s); far/aggregate = a coarse signature-aware scan (~2–5 s) that IS the hostile/player promotion trigger. Stealth/signature is respected at every tier (consistent near + far).
- Q: SC-003 has no failure condition until the bench exists — add a provisional acceptance floor? -> A: Relative budget — AI think + steering + perception MUST add ≤ ~30% to the per-tick time of the existing no-AI `fleet_stress` baseline at its sustainable N; the bench (TR-017) then refines the absolute target. *(Post-session: pinned to exactly 30.0% + a p99 budget — TR-017 is now the canonical gate.)*
- Q: Squad lifecycle — membership assignment + behavior under attrition? -> A: Scenario/spawner-authored membership; the squad brain re-derives pace/roles on a member-death event; a squad of 1 degrades to an individual brain; no runtime cross-squad re-clustering in v1.

## Stress-Test Findings

### Session 2026-06-10

- **STF-001** *(consistency, severity: HIGH)* — Q1's auto-promotion of hostile far-tier groups to full-physics combat tensions with Principle III (tiered-by-attention; cost scales with players, not world activity) and the Q5/SC-003/TR-017 ≤30% perf budget: off-screen battles consuming full simulation let total cost grow with world-wide combat activity, so at MMO scale a galaxy-wide war could exceed the budget even with few players online. *Affected*: OBJ3, SC-003, TR-017, Technical Constraints. *Given* a galaxy-wide war off-screen *When* many hostile aggregates mutually detect + promote *Then* full-physics combat for all of them can exceed the per-tick budget. **Resolution (accepted):** keep auto-promote unbounded in v1 (simplest, fully alive off-screen) and record it as a known MMO-scale risk to revisit (a global cap or abstract off-screen resolution) when concurrent-world-combat load becomes real — **measurable revisit trigger: the bench/telemetry's separately-reported off-screen promoted-battle bucket sustainedly exceeding 10% of the 33.3 ms tick budget** — the ≤30% budget is asserted relative to player-local load, not world-wide war (off-screen battles are excluded from the gate numerator but always measured + reported, so the trigger stays observable).

## Compliance Check

**Target**: specs/00008-ship-ai/spec.md
**Auditor**: PolicyAuditor (against project-instructions.md v1.1.0, AGENTS.md)
**Date**: 2026-06-10
**Verdict**: **PASS** — no violations. 0 CRITICAL / 0 MAJOR / 0 MINOR.

| Principle | Verdict | Evidence |
|-----------|---------|----------|
| I. Server-Authoritative Simulation | PASS | LOD/AOI triggers key off authoritative ship positions, never a per-client camera (Edge Cases; TR-007; "MP-safe LOD"). Render LOD kept client-side + separate. No client-authoritative path. |
| II. Shared Deterministic Sim Core | PASS | All AI drives `ShipIntent` through the shared `ship_motion_system` (IP-001, TR-001); SC-001 requires AI trajectory bit-identical to a human piloting the same intents — one code path, no fork. |
| III. Tiered Simulation by Attention | PASS | Behavior-LOD (active/mid/dormant) scales cost with player attention, not world size; dormant ships promote via continuous deterministic re-seed, not cross-boundary per-tick state (OBJ3, TR-008). |
| IV. Agent Output Style | PASS | Template sections only; within size limits; outcome-oriented. |
| V. Build the Seams, Defer Distribution | PASS | Coarse interest tier seeds future sector-sharding (OBJ3, IP-002) while single-node now; AI entities are ECS components (serializable seam). |
| VI. Bandwidth Is the Budget | PASS / N/A | Sim-side AI; no new replication path. Squad/aggregate LOD reduces simulated/replicable entity count. |
| VII. Playable Every Phase | PASS | P1→P3 prioritized, each objective independently demoable; P1 is a viable opponent MVP; TR-017 bench gates scale work on measured data. |
| Determinism Doctrine | PASS | Explicitly preserved (TR-004, TR-016, Constraints, SC-002): fixed-step, no RNG (deterministic hashing only), no HashMap iteration, stable order; new systems additive + `ScenarioActive`-gated; golden determinism / `demo_enemies_smoke` / harness/botkit stay bit-identical. Verified against actual `crates/server/tests/determinism.rs` (epsilon-0) + the real gate. |
| Testing & Quality Policy | PASS | Determinism guarded as a test gate; AI-cost benchmark required on a real brain in `fleet_stress` (TR-017, IP-008). |
| Governance / SDD lifecycle | PASS | Mandatory technical sections present; architecture choice deferred to `/sddp-plan` (Excluded), respecting phase ownership. |

**Notes (non-blocking):** the determinism claim is factually grounded (the cited golden test + `ScenarioActive` gate + `seek_system`'s direct-`Velocity` bypass are real in the tree). `/sddp-plan` should confirm the additive systems don't retroactively change the existing un-gated `seek_system` byte output, and tie the bench-derived scale target (TR-017) to a concrete pass/fail in tasks. **No action required — spec clears the Instructions Check gate.**

**Spec Validator**: PASS — 27/27 criteria (technical-spec ruleset: Technical Objectives, TR-### requirements, Validation Criteria, mandatory Integration Points; no `[NEEDS CLARIFICATION]`; every P1 objective has an SC).
