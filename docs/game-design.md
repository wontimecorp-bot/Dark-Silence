# Dark Silence — Game Design Document

> **Status:** consolidated design (v0.2, 2026-06-01). Working title.
> This document organizes the confirmed design by system. It is **consolidation only** — every pillar here was decided collaboratively; open/undesigned items are listed in [§14 Open Questions & Gaps](#14-open-questions--gaps). It is the intended source for `/sddp-prd` (capabilities) and `/sddp-systemdesign` (ADRs).
> Technical/architecture detail lives in [project-instructions.md](../project-instructions.md) and the plan/memory; this doc focuses on *what the game is*.

---

## 1. Vision & Pillars

A **top-down 2D-gameplay, 3D-rendered, physics-based space-action MMO**: a single seamless persistent universe of three warring factions, fought at every scale from a one-on-one dogfight to fleet sieges to strategic long-range strikes against enemies you may never see.

**Design pillars** (mirrors `project-instructions.md`):
1. **Server-authoritative** — the server is the single source of truth; clients predict, never dictate.
2. **Shared deterministic sim core** — client and server run the same `sim` crate; one code path, no desync.
3. **Tiered simulation by attention** — cost scales with where players are looking, not world size.
4. **As physics-based as possible** — movement, projectiles, damage, recoil, debris are all physical.
5. **Build the seams, defer distribution** — single-node now, seamless multi-node reachable later.
6. **Bandwidth is the budget** — interest management, not physics CPU, is the scaling wall.
7. **Playable every phase** — verify core fun before scaling.

**Core fantasy:** in an information-starved galaxy, *seeing is power and being seen is death*. You build, fit, fly, and fight physical ships; you sustain a war through logistics; and what you own is only ever as safe as the territory holding it.

**Touchstones:** EVE Online (economy, single-shard, persistence), Foxhole (logistics war, replacement-by-supply), Cosmoteer (modular physical ships), World of Warships/War Thunder (penetration damage, angling), Elite Dangerous (Newtonian flight + flight-assist), Star Citizen (server meshing, systemic facilities).

---

## 2. Setting & Factions

### Premise — information scarcity
The setting's job is to make fog-of-war, sensors, EW, and comms the world's **baseline state** rather than arbitrary mechanics. The working frame ("Dark Silence") is that a galactic communication network has failed/contracted, so there is no omniscient grid — you know only what *your* sensors detect and *your* rebuilt relays carry. This is *why* destroying a comms array genuinely isolates a region, why long-range fire arrives anonymously, and why sensors/EW are a central pillar rather than a side feature.

> **Open:** the exact **tone** of that premise is not yet chosen — see [§14](#14-open-questions--gaps).

### Three factions (Set C — societal roles)
Faction = your team in the war + a distinct **systemic edge**. All three can field every combat archetype, but each is **best-in-class at one signature** (lean, not lock):

| Faction | Societal role | Systemic edge | Signature archetype | Aesthetic |
|---|---|---|---|---|
| **Foundry Compact** | war-industry | production / economy / logistics — cheap, fast, mass-produced | **Armor / mass** | riveted, utilitarian |
| **Custodians** | guardians of surviving high-tech | defense + sensors/comms/repair | **Energy / defense** | sleek, pristine, high-tech |
| **Shrike Syndicate** | privateer crime | stealth / EW / salvage / black-market | **Speed / stealth** | jury-rigged, stolen-tech |

### Races — the 3×3 = 9 matrix
Each faction is a coalition of **3 races, one per combat archetype**:
- **Color = faction** (team); **shape = race = archetype**: cube/sphere/cone → armor/energy/speed (and the same square/circle/triangle strategic icons, tinted by faction).
- Player identity = **faction (team + systemic playstyle) × race (combat role)** = 9 distinct starts. The same archetype is culture-flavored per faction (a Foundry armor-brute vs. a Shrike scavenged bruiser vs. a Custodian bastion).
- **Visuals need zero modeling**: Bevy primitives, optionally Kenney/Quaternius CC0 low-poly kits.
- **Scope:** design 9 slots, **ship one race per faction first** (its signature archetype); races stay a light flavor+bonus layer over the faction's deep systems.

### The war's stakes
Factions fight over resource-rich remnants, surviving infrastructure, and the secret behind the Silence (the endgame/exotic-tech lore hook). Three factions self-balance (the weaker two tend to gang the leader).

### Faction model & allegiance
- **Three symmetric-ruleset factions** — they differ in *flavor, systemic edge, and signature archetype*, but play by the **same rules**. (Structural asymmetry was rejected: it's a solo-dev balance trap and would break the 3-way war-cycle.) **Three is the deliberate optimum** — DAoC/Planetside 3-realm RvR self-balances and concentrates a scarce population; 4+ dilutes population and worsens balance.
- **Piracy / outlaw = an emergent reputation status, not a faction.** Any player can go outlaw (prey on their own faction → standing tanks → hunted, black-market-dependent, locked out of friendly facilities). The **Shrike Syndicate is the cultural home** for that playstyle but is still a normal territory-holding team. This delivers pirates, mercenaries, freelancers, and "pirate the other faction" at zero asymmetric-balance cost.

---

## 3. World Structure & Tiered Scale

### Tiered simulation (the core architectural idea)
Cost scales with player attention, not world size:
- **Tier 0 — combat bubbles:** real-time authoritative physics where players are (Rapier2D, ~30 Hz, AOI-limited, prediction/reconciliation).
- **Tier 1 — transit layer:** long-range projectiles, "messages in a bottle," ships crossing the void — stored as **closed-form analytic trajectories** (a DB row + a scheduled event), evaluated on demand, **promoted** to Tier 0 only when they approach something that can perceive them.
- **Tier 2 — persistent universe:** accounts, inventory, territory, economy, the galaxy map — event-driven, durable (Postgres).

Entities move between tiers by **re-seeding from the analytic form** (the integrator and closed-form evaluator must agree — the `sim` crate's load-bearing invariant). A galaxy full of in-transit objects and offline assets costs almost nothing until someone interacts with them.

### The strategic graph (one structure, four jobs)
A node-and-lane graph of strategic locations (homeworlds, stations, facilities, resource nodes, relays) simultaneously serves as the **AOI sector index, the comms network, the supply network, and the territory map**.

### Starting map (MVP)
Each faction: **1 homeworld** (manufacturing + science + population core; Tier-2 anchor) + **1 far frontline station** (combat hub + respawn). 3 cores + 3 frontlines + travel lanes = the first persistent content *and* comms-graph v1 / supply-network v1. **Build one matchup's two stations + one contested zone first.**

### Seamlessness
Single-node: there are no seams (sectors are just an index). Multi-node seamlessness (boundary ghosting + mid-flight authority handoff) is the deferred Phase-5 work; the architecture keeps it reachable without paying for it early.

---

## 4. Core Gameplay Loop

### Flight
**Newtonian planar flight** (3DOF: x, y, heading; thrust/mass/momentum/torque) with a **flight-assist toggle**: ON = drift-dampened/accessible; OFF = decoupled full-momentum/high-skill (Elite-style).

### Combat feel & controls (scales by ship class)
- **Fighters/small (visceral):** direct **thrust + rotate** keyboard piloting; **fixed forward weapons fire where the nose points** (manual aim by pointing). Momentum dogfights.
- **Capitals (tactical):** command / point-to-move + **lock a target; turrets auto-track within their firing arcs**. Naval angling.
- Emergent combat = **bring weapon arcs to bear while presenting strong armor and hiding fragile modules** × Newtonian momentum.

### Damage pipeline (unified: "fast object meets armor")
All damage is a typed packet with channels **{Kinetic/penetration, Thermal/Energy, Blast, EM, Radiation}** that flows through ordered defense layers, each absorbing/modifying per channel. Projectiles are **swept rays (CCD)** so they can't tunnel. Mechanics (WoWs/WT-style, adopt incrementally): angle → effective armor (thickness/cos θ), normalization, ricochet, overmatch, shatter, overpenetration, fuzing, post-pen module/crew damage, **cascades** (ammo cook-off / reactor breach), DoT analogs (breach/decompression/fire/leak), saturation/compartmentalization.

### Defense layers (outer → inner) & the type matrix
Avoidance (evasion/signature/point-defense/ECM) → **Shields** (regen, power-hungry) → **Armor** (ablative, angle/material) → **Hull/Structure** (HP) → **Systems/Crew**.

| Damage ↓ \ best vs → | counter |
|---|---|
| Directed energy | **Shields** |
| Kinetic | **Armor** |
| Explosive/Blast | **Hull / soft targets** |
| EM/EMP | **Systems** (bypasses physical) |
| Radiation/Particle | **Crew / electronics** |

A shared **power/heat economy** (reactor generation vs. module draw) is itself a defense/resource layer — raise shields vs. fire DEW vs. run cold.

### Weapons taxonomy (all categories)
Kinetic · Explosive · Directed-energy · Electronic/soft-kill (EMP/jam/cyber) · Area-denial (mines/drones/decoys) · optional exotic. **Delivery (missile/torpedo/bomb) is a separate axis from damage type** — a missile carries any payload. Missiles use proportional-navigation guidance; explosions scale by fidelity (radius+impulse default → real shrapnel projectiles for big/near warheads, LOD-gated).

### Game feel (subtle-realistic + diegetic-informative)
Restrained, simulation-forward; **physics + audio carry the feel** (recoil = reverse impulse, impact shove, drifting debris → salvage). Minimal screen-shake/hit-stop, small precise VFX, **space-realistic explosions** (brief flash + expanding debris/gas + light; no persistent fire/smoke in vacuum). **Audio is the informative channel** — distinct ricochet "spang" / penetration "thunk" / shield "whump" + layer-specific visual cues; light readouts, no number spam. *Risk:* this is the hardest style to make satisfying → high bar on audio, tight responsive controls, low input latency, physical motion feedback.

### Skill ceilings beyond combat (skill > gear, everywhere)
"Skill > gear" applies to **every career, not just combat** — which is also how non-combat players get *engaging* careers (it solves the PRD's "casuals need a real, fun career" goal). Skilled disciplines: **piloting** (flight-assist-off mastery, momentum/drift, manual aim with projectile lead); **sensors/EW/ISR** (signal triangulation, sensor tuning vs. signatures, jamming/spoofing/lock-break timing); **damage control** (in-combat power-reroute, repair triage, breach/fire fighting — a tense crew role, Barotrauma-style); **hacking** (the cyber kill-chain); **research** (experimental design + inference); **salvage** (surgical disable-then-clean-sever for intact loot); **logistics** (route planning + evasion + escort); **stealth** (signature management / running silent); **trading** (arbitrage/prediction); **command** (reading the picture, timing engagements, managing intel/comms). Mix mechanical/real-time skill (piloting, aim, damage-control, EW timing) with cognitive/strategic skill (fitting, trading, logistics, command, research) — not everything a twitch minigame, none mandatory busywork. Principle: **mastery of *any* role is rewarded** → every persona has a real ceiling to climb.

### Automation floor, human ceiling (how roles are operated)
Every role has **competent automation** (so a solo pilot or under-crewed capital still functions), but a **skilled human operator outperforms the AI** — and that **gap is the reward** for crewing + mastery. Mechanics:
- **Configurable delegation spectrum** per subsystem (full-auto ↔ assisted ↔ manual) with **dynamic takeover**; unfilled seats on big ships run on AI until a human/NPC occupies them.
- **The AI-floor ↔ human-ceiling gap is the central balance knob** — competent baseline (never punish solo/under-crewed) + meaningful human uplift (crewing/skill clearly worth it). Biggest gap where judgment matters most (EW timing, target prioritization, damage-control triage, reading a *deceptive* sensor picture); near-full automation for routine stabilization/point-defense/power-balancing.
- **NPC-crew quality = the middle tier** (AI-baseline free → hired/trained NPC crew via economy/clearance → skilled player), bridging solo and multiplayer without forcing either.
- **Automation prevents the busywork trap**: human-uplift is *opt-in attention that rewards focus*, never mandatory plate-spinning. Solo fighter = focus on fly+shoot (rest auto), deep activities out of combat.
- **Reuse:** this automation IS the NPC AI system (which also drives NPCs and offline-asset behavior). One AI system, three jobs.

### Physical grounding vs. gameplay scaling
Rule: **physically grounded, gameplay-scaled.** Keep real physics *models/relationships* (Newtonian motion, momentum, KE = ½mv², penetration ∝ energy/area + angle, inverse-square falloff, more-explosive → bigger-radius by a plausible curve) — they give consistency, emergent depth, believable feel, and make the emergent research/manufacturing learnable. But **choose the magnitudes for playability, readability, and tier/scale limits, kept internally self-consistent.** Real space distances/speeds/lethality are unplayable (invisible instant death from off-screen, or hours of waiting; real warheads one-shot everything), so distances/speeds/timescales are compressed (often orders of magnitude) and damage/armor/TTK are gameplay-tuned. Real specs are **inspiration & texture, not binding numbers**; internal consistency (a self-consistent unit system) is what sells "realism," not real-world accuracy. Chasing real accuracy is a large time-sink that usually *worsens* fun for a solo dev (cf. Children of a Dead Earth = the niche realism extreme; KSP shrinks its system ~10×; Elite/EVE abstract heavily). Matches the subtle-realistic feel pillar.

---

## 5. Ships, Modules & Fitting

### Module abstraction
Every installed device (reactor, turret, shield gen, sensor, engine, manufacturing bay, research lab, comms array) is a data-driven **Module** with `power_gen/draw, heat, mass, hitbox/health, hardpoint type/size` + specifics. A ship = **hull + hardpoints + modules**; roles/specialization/backups/redundancy emerge from loadout. (Refs: Cosmoteer, EVE, FTL.)

### Ship class ladder
fighter → corvette → frigate → cruiser → capital/carrier → mobile station (+ hauler/industrial/science). Scaling axes (mass, power, slots, crew, cost, spawn-facility-gate, persistent-vulnerability, combat-tier) all reinforce other systems; faction races reskin/retune the same ladder.

### Fitting — positional slots + 3 budgets
- **Positional slots:** hulls have fixed, meaningfully-placed slots (weapon hardpoints with position-based **firing arcs**, module bays, armor sections, engine mounts). **The fit layout IS the damage hitbox/armor map** — where you put the reactor (central behind armor vs. exposed) is a real survivability choice the penetration model reads directly.
- **Three competing budgets:** **power + CPU/control + mass** (+ slot count/size). Mass → Newtonian agility, so fitting changes how the ship flies. You can't max everything: tank vs. damage vs. speed vs. range vs. utility.

### Exotic gear = capability + liability
Power via tradeoff/synergy, not raw stats. An exotic module is an extreme Module config + a **side-effect emitter** (heavy power/heat, fragile, big signature, material-filtered DoT auras) reusing the damage/sensor pipelines. A **tag-based synergy rules engine** ("if A+B → C") drives emergent combinations. Tied to a **materials + science chain** (research → materials → exotic module → side-effects → lore). Balanced by being situational and counterable, so casuals stay relevant. (Refs: EVE, Highfleet, FTL, Stellaris dangerous tech.)

### Destructible hulls (coarse now, grid-designed for later)
Hulls are authored as a **2D cell-grid** — the *same grid as the fitting layout* (cells grouped into sections/modules) — so the representation is destruction-ready from day one ("build the seams").
- **First implementation = coarse module/section destruction:** sections/modules have aggregate health; the penetration pipeline (§4) decides what's hit; a destroyed section is removed.
- **Severing:** when sections are destroyed, a connectivity check (flood-fill) on the remaining grid splits any disconnected region into a separate physical body that drifts off with inherited momentum + impulse → wreckage and **salvage** (feeds §7).
- **Clean-sever for loot:** if a module's own health survives but its *surrounding structure* is severed, it detaches **intact and operational** → scavenge a working part; damage *through* the module → scrap. Rewards precision (disable-then-sever — a harvester playstyle).
- **Fine cell-by-cell "eaten-away" destruction is deferred** — the grid data model upgrades from whole-section to per-cell removal *without a refactor*. **Simulate at cell granularity, render fine** (client makes it look pixel/voxel; sim stays coarse).
- **Cost controls:** connectivity only on destruction; projectiles raycast vs. the grid (no per-cell collider rebuild); coarse/lazy colliders; AOI+delta+LOD networking; coarser cells for big stations; only Tier-0 ships actively simulate it. Decoupled from *constructibility* (hulls stay designer-authored slot-based). (Refs: Cosmoteer, Avorion / Space Engineers cut-and-claim, Barotrauma.)

---

## 6. Sensors, EW, Comms & C2

> **Key unifying idea:** sensors/EW are **interest management surfaced as gameplay** — a client only receives a contact at the fidelity its sensors have earned. Building sensors well *also* solves bandwidth, and drives Tier-1 promotion.

- **Detection model:** entities have signatures; sensors have range/sensitivity/cone, optional LOS occlusion. **Passive vs. active** (listen-stealthy vs. ping-but-reveal-yourself). Contact information states: undetected → blip/bearing → tracked → identified → full telemetry (= the replication-LOD ladder).
- **Electronic warfare:** jamming, spoofing/decoys, ECM/ECCM, sensor-lock breaking — modifiers on the information layer. Physically interacts with missiles (a spoofed missile flies at a decoy).
- **Multi-scale zoom = LOD in render AND replication, gated by sensors/entitlements:** concentric AOI rings (near = full fidelity, far = aggregated blips on a low-rate strategic channel). Strategic zoom-out is bounded by what you're entitled to see, so it can't blow up bandwidth.
- **Comms = destructible graph infrastructure** (relays/arrays/beacons as nodes). Sensor contacts, orders, and supply requests **propagate through the graph with latency**; destroy nodes → the graph partitions → regions go dark (can't warn of attacks, can't coordinate supply). Long-range comms packets reuse the Tier-1 analytic-transit model.
- **Tactical → strategic C2 (sense → relay → command):** fighters get a tight raw picture; command echelons get a wide, aggregated, **delayed, comms-dependent** picture. Counter-intel/SIGINT/spoofing operate on the relayed picture. All cheap (event/graph), no real-time physics load.

### Information dominance as legitimate anti-cheat
Because the server only ever sends a client what its sensors/comms have *earned*, the classic maphack is **structurally impossible** — the unseen data never reaches the client. So the legitimate sensor system and the anti-cheat are the same mechanism, and the *only* way to "see more than you should" is to win the information war in-game. Information is an earned, graded (bearing→track→ID→telemetry), costed (active sensing reveals you), counterable (EW/spoofing/decoys can *falsify* it), and contested resource.

### External meta-gaming (streams, Discord, alts, third-party tools)
Posture: **embrace + structurally-defang**, not police. The architecture already caps external info to "what was legitimately scouted." Embrace spying/intel as an on-theme meta-layer (EVE model) and let **disinformation** punish reliance on stolen intel. Add a **streamer mode** (hide own coords/fleet; optional spectator delay) and **intel time-decay** (last-known, not live) to defang stream-sniping. **Canary/honeypot intel** doubles as leak detection AND a counter-intel mechanic. Hard collusion/alt detection is a moderation + anomaly-analytics + reporting problem; out-of-game voice coordination is healthy and expected. **Make in-game ISR strictly better than snooping:** legitimate intel is live, integrated (feeds targeting/command map), fleet-shareable, and queryable — a stream is one person's delayed, un-integrated view. Combined with intel **staleness/decay** (timestamped, growing uncertainty radius; only active sensing/live relay is current), external snapshots are stale-on-arrival, so the meta-incentive flips toward investing in in-game scouting.

### Cyberwarfare — hacking & counter-hacking (later pillar)
A **simulated, sandboxed, server-authoritative** model of real hacking (inspired by Uplink/Hacknet/Grey Hack/Exapunks) — faithful to the *concepts/feel*, NEVER literal exploits against real infrastructure. The cyber-specialist infiltrates an enemy facility, reaches a terminal, and runs the real cyber kill-chain: **Recon → gain access → escalate privilege → act (take over / use systems / cause malfunctions / install a virus / open security holes / steal intel-blueprints) → persist (backdoor) → cover tracks.** The **counter-hacker** detects (IDS), **traces back** (exposes the attacker), patches, sets honeypots, and ejects intruders. Two tempos: fast combat-hacking vs. slow async network infiltration. This is the cyber face of the EW class + the systemic-capture verbs + contested-knowledge + social espionage — opt-in specialist depth with a simple "hack vs. security rating + detect/trace/patch" version for everyone. **Non-repetition is the core design constraint:** no hand-authored puzzles or fixed vuln catalog. Instead — (a) **procedural attack surface** generated from the target's *actual* module/firmware/config; (b) **defender-authored defenses** (the humans are the content); (c) **emergent vulnerability *chains*** from a property/rule system, not a fixed list; (d) a **tools × config meta** (loadouts of exploits/zero-days vs. configurations, evolving as both sides patch/trade); (e) **live contested duels** + **persistent history** (backdoors, patches, rivalries). Constant *grammar* (kill-chain), variable *content* — chess, not a fixed puzzle. **Candidate ADR.**
- **Hacking has a physical source (cyber ↔ physical interlock):** the hacker is physically present (a terminal / a ship in range / an infiltrator), so hacking **exposes your body**. The counter-hacker's trace **localizes the source** → security/forces are sent to **kill or capture the hacker at that location**. A hack is a raid with a soft, exposed operator.
- **Proxy/relay chaining (Uplink-style):** the hacker bounces the connection through multiple compromised terminals/relays to lengthen/obscure the trace; the counter-hacker traces **hop-by-hop** (time/effort per hop), buying the hacker time. Arms-race axis (longer/hidden chains vs. faster tracing). Chains route through comms relays → **cutting a relay can break a chain**, and holding relays aids tracing. Risk/reward: long chain = safer but slower setup; direct = fast but easily traced.
- **Entry = an easy-ish foothold, not full control.** Gaining terminal/user access is *meant* to be achievable (the easy first rung); the real difficulty is in **privilege escalation** (foothold → use systems → malfunction → virus → take over → deep data), each layer a fresh contest giving repeated detection windows. Breach-time is the **detection window**, but it is a *contest of attacker tooling/skill/method vs. defender-set security strength*, NOT a literal fixed digit-code (a digit-lock is script-brute-forceable and repetitive — avoid as the mechanic; keep only as cosmetic "cracking…" flavor). Multiple **situational entry methods**: brute/crack (slow + **noisy** → big detection window), stolen/cracked credentials or keys (fast + quiet, but must be obtained first), matching exploit/zero-day for *this* terminal's software version (tools×config), or physical infiltration. Key decision = **speed vs. stealth**. Guardrail: any code/credential check is server-authoritative with lockout+alert after repeated failures (no script-spam).

---

## 7. Economy, Tech & Logistics

### Hybrid economy (production + scavenging + trading)
A closed loop where activities feed each other: combat → wrecks → salvage → materials → manufacture → combat; piracy → risk → escorts/insurance.

- **Wide & shallow power band:** rares unlock tactics/specializations, **not raw power**; skill > gear; cheap gear stays viable.
- **Acquisition ladder = activity map:** baseline (NPC price-floor) → manufactured/researched → crafted-special → scavenged → pirated/looted → unique. Each rung is a playstyle and a livelihood (a casual can have a full career without ever winning a fight).
- **Quality/origin axis** separate from role: the same module exists as standard / refined / salvaged / prototype / faction-looted.
- **Market:** **NPC price-floor + player market** on top.
- **Sinks:** ships, ammo, modules, upkeep, fuel, insurance — loss is the economic engine; severity scales by distance from home.

### Recovery = replacement economy (not insurance)
Lose a ship → re-ship at a facility from a **stockpile** (instant), **self-build** (blueprint+materials+production; cheapest), **buy with credits** (fastest/priciest, frontier markup), or salvage/source — any mix. A **time ↔ credits ↔ effort** triangle. Timers are supply-chain reality (stockpile = 0, build = build-time, pay = expedite), **not** arbitrary cooldowns. "Instant" = a ready hull *at the station*, **not** instant return to the fight (you still travel back — a kill always removes you from the engagement; blocks buyback abuse). Credits are an **in-game sink, not real money** (no P2W). Self-build/salvage stays meaningfully cheaper than buyback so loss keeps biting. Stockpiles are facility-vulnerable → forward-deploying spares is a logistics game; raiding stockpiles is strategic. This *is* the respawn system, facility-gated. (Ref: Foxhole.)

### Tech as physical, contested knowledge
tech → parts → modules → ships is a dependency **DAG** (Tier-2 data). **Blueprints are transportable data-cargo**; a facility can't build until a blueprint is **delivered** there. **Tiered transfer:** courier (slow, lootable) / comms transmission (fast, interceptable via SIGINT-EW, needs relays) / secure facility link (fast, infra-gated). This unifies research with logistics + comms + piracy + counter-intel (tech can be stolen, intercepted, raided, denied). Baseline blueprints free at home; advanced ones must be researched **and** distributed.

### Resources
Start as a per-tick faction trickle scaled by owned territory/nodes (counters); later gate on physical logistics/transport (gather + move).

### Exploration, anomalies & discovery
A non-combat pillar that reuses existing systems:
- **Discovery via scanning** turns the sensor system into an explorer loop (probes/sweeps/triangulation resolve a faint signal → a known site) — a viable solo/explorer career.
- **Site types:** derelict wrecks (salvage), resource fields, unknown signals, hazards (gravity wells/nebulae/radiation), and rare **special sites** (exotic materials, prototype tech, lore caches) that feed the research → materials → exotic-gear chain ("special random occurrences").
- **Procedural + handcrafted hybrid:** procedural common sites = the grind-free faucet; handcrafted rare/lore sites (the secret-of-the-Silence chain) = the memorable chase.
- **Risk/reward by depth** (deeper/darker = rarer finds); sites can be contested or hazardous.
- **Living-world ties:** transient signals reuse the Tier-1 scheduler; player/NPC battle wrecks (and severed hull chunks, §5) become explorable salvage; world events drift derelicts in.
- **Information as a tradeable good:** scan-data/maps can be sold — and intercepted/contested via the comms/info systems.

### Research pacing
Throughput is bound by **infrastructure + scarce inputs**, not a daily clock: more labs/research divisions = parallel slots; each experiment consumes a scarce **research sample / data fragment / recovered artifact** (from exploration, salvage, or hacked data — ties research to those loops). **Puzzles are an optional, skill-based, anytime accelerator** with **diminishing returns per project**, framed diegetically as *breakthrough → lab-work* (the insight is the scarce part). Optional **risk** for rushing experimental tech (instability → wasted samples / lab damage / material-DoT side-effects) is a self-balancing throttle. Team research (more scientists/labs) scales it and ties to the social/org layer. No daily ration, no abuse, never pointless. (Progression here is role + facility capability, not a personal stat — see §11.)

### Research as empirical science (replaces puzzles as the core)
The science twin of the hacking design: make research the **real scientific method against procedurally-generated unknown phenomena**, not a puzzle minigame. Each researchable thing (new material, alien artifact, captured enemy module, physical anomaly) has **hidden, generated properties + interactions** (density, conductivity, thermal/channel reactivity, decay/instability, exotic resonances, "combines with X → Y") that you do NOT know. Research = **designing and running experiments to measure them** (subject the sample to chosen conditions → observe → infer), with **measurement uncertainty** you reduce by repetition — hypothesize → experiment → measure → infer → apply. **Discovery unlocks and de-risks application**: characterize a phenomenon enough to build modules exploiting it; **incomplete knowledge = unknown failure modes** (rush a half-understood super-material → undiscovered instability — ties to capability+liability + rush-risk). **Non-repetitive by construction** (constant grammar = scientific method; variable content = generated phenomena). Unifies with **exploration** (supplies samples/subjects), **espionage/hacking** (research data is contested/stealable — hack an enemy's findings to skip experiments), **exotic gear** (applied discoveries, liabilities = discovered properties), and the **economy** (characterized materials feed recipes). Pacing falls out diegetically: you can only study what you have a **sample** of; experiments need **labs/time**; reducing uncertainty needs **repeats** → throughput = samples × labs × scientist-effort. Phase it (simple "analyze → properties reveal with uncertainty" first → true experimental design later). Strong lab-notebook UI (known vs. unknown). Touchstones: Noita, Kerbal Space Program, Subnautica, real materials science. Lightweight puzzle-like steps may survive *inside* an experiment, but the soul is investigation. **Candidate ADR.**

### Generative manufacturing & emergent tech (research's property-space, applied)
Research and manufacturing are **two ends of one shared property pipeline**: research *discovers* properties; manufacturing *composes* them; module behavior *emerges* from the composition. **Modules are parametric/generative templates, not fixed items** — slots for researched materials/components, and the instance's stats + quirks are **computed from what you put in** (the same emitter built from different materials yields a spectrum of variants). So **manufacturing's variation-space = research's discovery-space, automatically**, with zero extra authoring. Discovered properties propagate as emergent behavior + side-effects (resonance/emission/instability express through the module — capability+liability); **combination synergies** ("A+B → C") live here. **Weirdness/"emergent usefulness never thought possible" comes from cross-domain coupling**: because materials, damage channels, sensors, comms, and physics share one property/channel model, a weird material ripples across systems in unforeseen ways (a passive EM-emitter accidentally beacons your hull *or* becomes a jammer *or*, combined, a decoy). Generator design for surprise: wide ranges + rare extremes, composable side-effects, surprise-in-discovery / reliability-in-use, plus a few **authored iconic phenomena** (Severing-secret tech) as guaranteed peaks atop the emergent long tail. **Strategic payoff (solo dev):** you author *systems & rules*; the *content* (weird materials, novel modules, surprising metas) emerges and is **player-discovered** — effectively infinite content; players become content generators (a genius variant's blueprint is worth stealing). Guardrails: instability/risk throttle, counterability + wide-shallow band + skill>gear (no autowin), rarity gating, clear lab-notebook feedback ("not useful" is valid science). Touchstone: Noita / Dwarf Fortress (emergent from rules). **Candidate ADR.**

---

## 8. Persistence, Death & Stakes

- **Everything persists vulnerable; ships stay as-left.** All assets are durable world entities. Even stored assets are lost if their **facility falls** (facility = inventory container; destruction cascades to loot/destroy). An offline ship sits exactly as left → rewards pre-logout **low-signature/positioning** (reuses the sensor model). Cheap via tiering (offline asset = dormant Tier-1 until attacked).
- **Reconciled with casual-friendliness:** catastrophic loss is gated behind **territorial defeat**, not daily risk. A well-defended home core, far from the front, is safe-in-practice; the hardcore bite is where you *choose* to operate (frontier). **Insurance/replacement** softens individual losses.
- **Home core: capturable but epic** — cores fall only via a massive, multi-stage, telegraphed campaign (rare, server-shaping). Near-wiped-faction handling (refugee/fallback/regeneration) is a future consideration.
- **Comms siege-alerts** ("your station is under attack") are critical — and cuttable (cut comms first and the defender may never get the warning).

---

## 9. Population & NPCs

A **NPC-populated living world** (the population insurance for a solo-dev MMO — it must feel alive at any player count):
- **Economic NPCs:** haulers (raid targets / loot), traders (the physical price floor), miners (faucet).
- **Combat NPCs:** pirates, patrols, roaming fleets — difficulty scales by region (weak near home = the onboarding PvE).
- **Faction NPCs wage the baseline war** so the front moves even with few players online; the world **evolves between sessions**.
- A **director/spawn system** rides interest management (spawn life where attention is).
- **NPC AI doubles as offline-asset behavior** (a logged-off ship can use the same brains).

Players are the *decisive spikes*; NPCs are the substrate.

---

## 10. Territory & War

- **Territory = ownership state on the strategic node-lane graph** (Tier-2).
- **Capture = systemic & function-dependent** (NOT an abstract meter): a facility is a standard **module assembly + power-dependency graph + control core**. Reusable verbs against modules: **Hack/Seize** (EW vs. control core; access + interruptible channel-time → capture intact), **Disable** (cut/overload power — energy failure cascades to all powered modules; **power = the universal lever** → reactors are the linchpin and need redundancy), **Destroy** (reactor breach/critical kill), **Raid** (extract stockpiles/blueprints). Core tactical choice: **capture-intact (harder, keep systems alive) vs. deny/destroy (easier, get rubble)**. Reuses Module + power + EW/damage systems — content/UX work, not new tech. (Refs: immersive-sim — Star Citizen, Deus Ex.)
- **Capture flow:** **contiguous-front** (attack only adjacent nodes → emergent front line; cores are graph-deep) + supply/comms interdiction softens targets + **multi-stage sieges** scaling with importance. **Supply attrition** penalizes deep offensives (defender edge near core → "capturable but epic" emerges naturally).
- **Winnable war-cycles:** a faction reaching total dominance / holding objectives triggers a victory (rewards/lore/prestige) + a **soft-reset of the contested frontier**; cores, characters, tech, and blueprints persist. Anti-snowball, gives climaxes, and is the faction-wipe safety net. (Foxhole/Planetside model.)

---

## 11. Players, Embodiment & Onboarding

- **Embodiment:** pilot + hangar (you own ships and fly one at a time; losing a ship ≠ losing your character). **Facility-gated spawning** — capitals/big ships only spawn where supporting infrastructure is held, tying big-ship power to territory.
- **Crewing:** small ships single-pilot (the core loop); capitals/stations multi-crew (later pillar). Walking top-down **avatar** on stations is a later pillar.
- **Onboarding = the risk gradient:** new players spawn safe at the home core, cut their teeth on weak near-home NPCs and low-stakes economy/PvE, and *choose* when to push frontward. Cheap replacement makes early loss painless. Little bespoke tutorial content needed — the world structure teaches itself.

### Progression — external / sandbox only (no character XP)
There is **no character stat/XP progression.** You grow entirely through **external things earned by active play**: assets, tech/blueprints, reputation/standing, territory/influence, and **player mastery**. **Access-gating is economic/territorial** (acquire + build + hold the facility), *not* a personal grind — no time-walls or skill-timers; pacing comes from the economy, the risk gradient, and facility-gating. Consequence: the **combat/piloting skill ceiling carries all differentiation** (validating the deep flight/fitting/EW systems). (Refs: EVE without skill-training; Albion "you are what you wear".)
- **Recognition = reputation + prestige:** faction/NPC standing functionally gates access (contracts, faction tech, regions, markets); social recognition (ranks, war-cycle victory prestige, killboards, titles) provides the visible "growth arc" without stat power. Reputation/prestige is also the **earned path to command authority**.

### Social & organization
- **Lightweight first:** fleets/party-grouping now; persistent corps/guilds (shared wallet, industry, blueprint library, stockpiles — all facility-vulnerable) later; alliances and faction-command much later. Nested target: squad/fleet → corp → alliance → faction-command.
- **Command authority is earned** (reputation/prestige + org rank), not bought. Orgs are the engine of the strategic systems (hold territory, run logistics/replacement, own and distribute blueprints, wield C2/comms, set objectives).
- **Social-layer espionage:** corp infiltration can leak intel, steal blueprints, or sabotage — tying counter-intelligence into the social game (trust + permissions matter).
- **Orgs are powerful but optional** — solo play stays viable via freelance activities, the NPC-populated world, and the faction as a default team.

### Character progression — clearances & licenses (not stat XP)
Evolves the earlier "no character XP" stance: there *is* character progression, but it gates **access to the supply chain, never capability**. Earned ranks / clearances / licenses / reputation (via activity + contribution) unlock the *right to requisition, manufacture, or spawn* a ship/equipment class, plus facility access and command authority. **Found / stolen / salvaged gear is always usable** — gated only by physical access (unlock / power-on / **hack** it) — so a skilled newcomer who captures a capital can fly it. Acquiring-above-your-license (steal/salvage/hack) is a *feature*, and pulls in the hacking system. **Guardrail:** XP perks are access/convenience/economy only (faster requisition, more contracts, cheaper insurance, broader blueprint access) — **never combat stats** (no +HP/+damage/+speed), preserving skill > gear.

---

## 12. Technical Architecture (summary)

Full detail in [project-instructions.md](../project-instructions.md) and the plan/memory. Headlines:
- **Stack:** Rust; **Bevy** client + custom **authoritative server** sharing a `sim` crate; `bevy_ecs` in `sim`; **lightyear** netcode; **Rapier2D** behind a swappable `Physics` trait; **bitcode** snapshots; **Postgres + Redis** persistence; `glam` math.
- **Netcode/lag stack:** client prediction (own ship) + server reconciliation (input-ring replay) + entity interpolation (~100 ms) + sparing extrapolation + lag compensation. UDP reliable/unreliable, delta vs. last-acked, redundant input sends. Inter-player collisions are server-resolved (can't be predicted) → keep hard coupling bounded.
- **Sim:** fixed-timestep + **velocity-Verlet** (exact vs. the closed form under constant accel — the Tier-0↔Tier-1 invariant, already implemented and tested in `crates/sim`). `dt` is a runtime variable → time dilation.
- **Partitioning by tier:** Tier 0 spatial (adaptive authority regions + fine AOI grid); Tier 1 by time-bucket/entity hash; Tier 2 by entity/ID hash. Avoid entity-hashing the physics world (destroys locality).
- **Scaling levers:** AOI, quantization + delta compression, per-client bandwidth budget + priority function; time dilation for overload. **Bandwidth/AOI is the wall, not physics CPU.**
- **Build the seams, run single-node;** multi-node meshing (boundary ghosting + handoff) is optional Phase 5. Prior art: HLA/DIS RTI, HPC domain-decomposition + MPI halo exchange.
- **Build tailor-made, not a general framework** (reusability emerges from clean crate boundaries; extract on the rule-of-three).

---

## 13. Phased Roadmap

| Phase | Goal | Gate |
|---|---|---|
| **0** | Single-player vertical slice | momentum flight + shooting *feels good* |
| **1** | Authoritative netcode, one bubble | prediction/reconciliation/interpolation; 2 clients + bots; bandwidth baselined |
| **2** | Sectors, AOI, persistence, accounts | log out / back in to the same world; AOI proven; 100 bots within budget |
| **3** | Tier-1 transit + message-in-a-bottle | strategic missile persists across restart, promotes into another player's bubble |
| **4** | Scale hardening (bandwidth budget, time dilation, custom physics) | 1,000 bots co-located on one node, graceful degradation — a legit ship target |
| **5** | Multi-node seamless meshing (optional, defer) | a battle straddling a node boundary, no visible seam |

### Phase 0 spec (next build)
Single-player, Bevy, reusing `sim`: a tinted-primitive fighter (faction color + cube/sphere/cone) on the 2D plane rendered in 3D; **thrust/rotate/(strafe) + flight-assist toggle** driving `sim` velocity-Verlet; a **fixed forward gun firing swept (CCD) projectiles** where the nose points; **asteroids / a dummy** to shoot (hit-detect + destroy); top-down **zoomable camera**; subtle-realistic feel. New work beyond `sim`: Bevy app, input→thrust mapping, primitive rendering, projectile + swept collision, targets, camera. Already done: `crates/sim` velocity-Verlet integrator + closed-form analytic + equivalence tests (passing).

---

## 14. Open Questions & Gaps

Known unresolved items (surfaced, not silently resolved):

- **Setting tone NOT chosen.** The information-scarcity premise is agreed, but the flavor — *post-Severing* (grim) / *dark-frontier expansion* (hopeful) / *ancient cold war* (political) — was never selected. All are mechanically equivalent; pure tone/naming. *(Pick before `/sddp-prd` lore.)*
- **Strategic / command UI** — the command layer is conceptually present (earned command authority, C2 tools, objective-setting), but the actual *interface/role* — how a commander plays, directs fleets/NPCs — is undefined. Best designed against running code.
- **Monetization / business model** — undecided. Credits are confirmed *in-game* (not real money / no P2W); the revenue model is open.
- **Near-wiped-faction handling** — winnable war-cycles soft-reset the frontier (the safety net), but detailed refugee/fallback/regeneration rules for a faction pushed to its core are a future consideration.
- **Tuning unknowns (by nature):** all game-feel values (projectile speed, recoil, shake, hit-stop), damage/armor/EW balance, and economy tuning are deferred to in-engine iteration.
- **Project config TODOs:** MSRV not pinned; no global test-coverage target (only `sim`/`transit` invariants mandated).

**Resolved since v0.1** (now in the sections above): player progression (§11) · social/organization (§11) · faction model & allegiance (§2) · exploration/anomalies (§7) · destructible hulls (§5). The earlier "faction = archetype / Ironhold-Concord-Talon" mapping is **superseded** by Set C societal-role factions + the 9-race matrix.

---

*Source: consolidated from the design pillars in `~/.claude/plans/...` and `project-instructions.md`. No new design decisions were introduced in this document.*
