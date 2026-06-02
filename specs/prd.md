# Product Requirements Document: Dark Silence

> Date: 2026-06-01 | Status: Draft | Working title | Rev: refined to fold in cyberwarfare, empirical research, generative manufacturing, clearances/licenses, automation, and grounded-scaling

## Product Overview

Dark Silence is a top-down, physics-based space-warfare MMO set in a galaxy that has lost its connective tissue. Players pilot individually-fitted ships with real Newtonian movement and hit-location damage, fighting skill-driven battles inside a single, continuous, persistent universe. Three factions wage an endless, winnable war over a fractured frontier; players sustain that war through a player-driven economy of production, salvage, and trade, and survive it through information — because in a galaxy gone dark, knowing where the enemy is (and denying them the same) is the decisive advantage.

It serves players who want the depth, stakes, and emergent player-driven stories of a hardcore sandbox MMO, combined with combat skill that actually matters. Its value: a living, consequential world where what you fly, what you build, what you know, and how well you fly all matter — and where mastery, not grind, decides fights.

## Vision and Why Now

**Vision.** A persistent space frontier where every wreck, every blackout, and every supply line is the result of real players acting on imperfect information — a world that feels alive whether ten people are online or a thousand, and where a skilled newcomer can matter on day one.

**The Severing (premise).** The galaxy once shared an instantaneous communication and sensor network. Then came the Severing: the network collapsed, systems fell dark and isolated, and three successor powers now fight across the silent remnants. Information is the scarcest resource — you know only what your own sensors detect and your own rebuilt relays carry. This single premise makes fog-of-war, sensors, electronic warfare, and contested communications the *heart* of the game rather than incidental features.

**Why now.** Deep sandbox MMOs (EVE, Albion), persistent logistics wars (Foxhole), and physics-driven ship combat (Cosmoteer, Elite, Star Citizen) have each proven strong, loyal audiences — but no single title combines deep physics-based combat, a player-driven war economy, and information scarcity as its core tension. There is an underserved appetite for a sandbox where *skill expression* and *strategic depth* coexist.

## Problem Statement

Players who love deep, consequential multiplayer worlds face a split: titles with rich player-driven economies and warfare tend to have shallow, lock-and-fire combat where outcomes are dictated by stats and numbers, while titles with satisfying skill-based ship combat tend to lack a persistent, player-shaped world with real stakes. Meanwhile, "massive" worlds frequently feel *empty* and information feels free and total, removing tension and the value of scouting, secrecy, and surprise.

Left unsolved, players bounce between games that each satisfy only part of what they want, and niche worlds collapse when their populations thin and the world stops feeling alive. The cost is a persistent, engaged community that no current product fully captures.

## Background and Evidence

- **Genre appetite is proven at niche-but-sustainable scale.** EVE and Albion sustain dedicated communities for years at modest concurrency (tens of thousands of daily players, not AAA millions) on the strength of player-driven economies and stakes — evidence that a deep sandbox does not need mass scale to be viable.
- **Hardcore + supporting-cast symbiosis.** Successful sandboxes are sustained by a *partnership* between hardcore PvP players and safer, non-combat players: in EVE the safe-zone majority is the demand engine (markets, supply, targets) that powers the hardcore layer; in Foxhole logistics/industry is a near-equal, first-class career. A viable product must make non-combat play genuinely rewarding, not a chore.
- **Empty worlds kill niche MMOs.** Repeated cautionary cases show that once a sandbox dips below a "feels alive" threshold, even hardcore players leave — making population resilience a product-defining concern, not an afterthought.
- **Skill-based, physics-driven ship combat has a devoted audience** (Elite, Cosmoteer) that current persistent-war sandboxes do not serve.

## Target Users, Stakeholders, and Core Personas

### Target Users

- Players of hardcore/sandbox PvP MMOs (EVE, Albion, Foxhole) seeking deeper combat skill expression.
- Players of physics/ship-combat games (Elite, Cosmoteer, From the Depths) seeking a persistent world with stakes.
- Strategy/logistics-minded players who enjoy building, supplying, and coordinating more than (or alongside) fighting.
- Systems/optimization players drawn to deep, emergent tech, fitting, and "figure out how it works" gameplay.

### Stakeholders

- **The developer** (solo): primary builder and operator; constrained time/budget.
- **The community**: an early, invested player base whose feedback shapes the product.
- **Future commercial/platform partners**: storefront and payment provider (for the eventual commercial model), ratings bodies, and any community moderators.

### Core Personas

Every persona has a real skill ceiling; non-combat roles are first-class, skill-based careers (not chores). Each ship role also has a competent automation baseline, so a solo player can function while a skilled human (or hired crew) unlocks the role's full ceiling.

- **The Warfighter** — lives at the front; values skill, stakes, and decisive battles. The sharp end of the war.
- **The Industrialist / Logistician** — builds ships and modules, runs supply lines and stockpiles, distributes technology. Wants a deep, first-class non-combat career (Foxhole "LOGI" / EVE industrialist).
- **The Scientist** — investigates unknown phenomena/materials by experiment to discover their properties and unlock emergent tech. Research is empirical, not a timer.
- **The Explorer / Scavenger** — scans the dark for wrecks, resources, anomalies, and rare finds; trades in information; low appetite for direct PvP, fully solo-viable.
- **The Trader / Economist** — plays the markets, moves goods, profits from scarcity; the demand engine that keeps the economy liquid.
- **The Operator (Sensors / EW)** — runs the information war: scanning, triangulation, jamming, spoofing — the human edge over sensor automation.
- **The Engineer (Damage Control)** — keeps a ship alive under fire: power rerouting, repair triage, breach/fire control — a tense, skilled crew role.
- **The Netrunner (Cyber-specialist)** — infiltrates enemy systems (hacking) or defends them (counter-hacking); a high-stakes specialist where the operator's own body is exposed.
- **The Commander / Org Leader** — earns standing, leads fleets, holds territory, and directs the strategic war. The engine of coordinated play.

## User Needs / Jobs To Be Done

- "When I fight, I want my skill — flying, positioning, fitting choices — to decide the outcome, not just my stats."
- "I want a world that remembers what I do and feels alive even when few players are online."
- "I want to choose how I matter — as a pilot, a builder, a scientist, a scout, a hacker, or a leader — and have that be a real, skill-rewarding path."
- "I want real stakes: things I can lose and fight to protect, without being permanently locked out by one bad day."
- "I want to outwit opponents I can't even see — to scout, hide, ambush, hack, and cut off the enemy's information and supply."
- "I want to discover and build genuinely novel tech that no designer hand-placed — and have those discoveries matter."
- "As a newcomer, I want to do something fun and meaningful quickly, and grow by getting better and richer — not by grinding levels."

## Product Principles or UX Principles

- **Skill over gear, in every role**: power comes from mastery — flying, positioning, fitting, scanning, hacking, experimenting, commanding — not from stat grind. Advanced equipment unlocks *options and specialization*, not raw dominance; a skilled newcomer can beat a mediocre veteran. This holds for combat *and* non-combat careers.
- **Automation floor, human ceiling**: every role has competent automation so solo and under-crewed play works, but a skilled human operator outperforms the AI — the gap is the reward for crewing and mastery, never a tax that forces grouping.
- **Fair by design — never pay-to-win**: real money will never buy in-game power, currency, or advantage. A non-negotiable trust commitment.
- **A living world**: the universe must feel inhabited and consequential at any population, carried by NPCs and by player action that visibly shapes the front and the economy.
- **Meaningful, recoverable stakes**: loss matters and drives the economy, but is gated so it concentrates where players *choose* danger; safe play near home is viable.
- **Many ways to matter**: combat is the spine, but production, research, trade, exploration, electronic warfare, hacking, and command are first-class, equally legitimate, skill-based careers.
- **Information is earned**: what you can see, know, and coordinate is a contested resource, not a free overlay — and because clients only ever receive what they have earned, information dominance cannot be cheated, only won in-game.
- **Emergent over authored**: systems (research → manufacturing) generate content; players discover and build novel, sometimes surprising tech, so depth scales far beyond hand-authored items.
- **Physically grounded, gameplay-scaled**: use real physics *relationships* for believable, consistent, emergent behavior, but tune *magnitudes* for playability and readability — a grounded feel, not a realism simulator.
- **Deliver value early and often**: each release stage must be genuinely playable and fun before the next layer of ambition is added.

## Scope Summary

The product is a persistent, online, physics-based space-warfare sandbox. The boundary for early releases is a **fun, lasting core loop at modest scale**: skill-based ship combat, ship fitting, meaningful damage, and a shared persistent world — proven enjoyable with a small, lively community before the deeper war, economy, information, research, and cyber systems are layered on. Massive concurrency, full multi-region seamless scale, deep cyberwarfare, and on-foot/avatar play are explicitly later horizons.

### In-Scope Capabilities

- Physics-based piloting and skill-driven combat in a shared, persistent online world.
- Ship acquisition, fitting, and meaningful hit-location damage, destruction, and salvage.
- Three factions and a winnable, ebbing-and-flowing territorial war.
- A player-driven hybrid economy (production, salvage, trade) with replacement-by-logistics and loss as its engine.
- **Generative manufacturing** — modules built parametrically from researched materials, yielding emergent variants.
- Information warfare: sensors, electronic warfare, and contested communications.
- **Cyberwarfare** — a simulated hacking / counter-hacking discipline (infiltrate, seize, sabotage, steal; detect, trace, neutralize).
- A living, NPC-populated world that sustains activity and an evolving front.
- Long-range strategic weapons and messages that travel across the galaxy over time.
- **Research as empirical discovery** — experiment on unknown phenomena/materials to discover properties; technology is contested knowledge (stolen/intercepted/denied).
- **Progression via earned clearances/licenses** that gate requisition/production, never the use of found gear.
- **Automation floor with a human-skill ceiling** across ship roles (solo-viable, crew-rewarding).
- Reputation, prestige, and lightweight player organizations; exploration; and a strategic command experience (later-priority).

### Out-of-Scope Items

- Pay-to-win or any real-money path to in-game power, currency, or gear.
- Any hacking system that touches real infrastructure, real exploits, or real accounts — cyberwarfare is a fully simulated, sandboxed, in-fiction model only.
- On-foot / walking avatars and ship/station interiors (a future horizon).
- Player free-form construction of ship geometry (hulls are designer-authored; *fitting* is generative, hull *geometry* is not).
- Massive single-battle concurrency (thousands co-located) and full seamless multi-region scale as launch requirements.
- Character stat XP (HP/damage/speed bonuses) — progression is access/economy via clearances/reputation, never combat-power stats.
- Real-world physical accuracy as a goal (grounded relationships, gameplay-scaled magnitudes).
- Under-13 audience (a 13+ minimum is assumed).
- VR, mobile, and console platforms (not initial targets).

## Product Capability Map

Project-level execution anchors used by the project plan. Capability clusters, not feature-level stories. Priority reflects MVP viability: **P1 clusters alone form a playable, fun, demonstrable product.**

| Capability ID | Capability | Priority | Outcome |
|---------------|------------|----------|---------|
| CAP-001 | Physics-based piloting & combat | P1 | Weighty Newtonian flight and skill-driven, fair combat that is fun moment-to-moment. |
| CAP-002 | Shared persistent universe | P1 | A single continuous online world players inhabit, leave, and return to; the world and assets persist. |
| CAP-003 | Ship customization & fitting | P1 | Acquire and fit ships within real tradeoffs and a meaningful layout; loadout becomes identity. |
| CAP-004 | Damage, armor & destruction | P1 | Believable hit-location/armor combat where parts can be disabled, severed, and salvaged. |
| CAP-005 | Factions & territorial war | P2 | Pick a faction, contest a moving front, capture/hold facilities; winnable war-cycles keep it dynamic. |
| CAP-006 | Player-driven hybrid economy & generative manufacturing | P2 | Production, salvage, and trade form a closed loop; loss creates demand; recover via supply/logistics. Manufacturing is generative — parametric modules built from researched materials yield emergent variants. |
| CAP-007 | Information warfare (sensors / EW / comms) | P2 | Seeing and denying sight is power; destructible communications make awareness a contested resource; information dominance is earned, not cheatable. |
| CAP-008 | Living NPC-populated world | P2 | NPC traders, pirates, and faction fleets keep the world alive and the front moving at any population; NPC AI also fills empty crew seats and offline assets. |
| CAP-009 | Long-range strategic weapons & messaging | P2 | Weapons and messages that travel vast distances over time, striking enemies who may never see the sender. |
| CAP-010 | Empirical research & contested technology | P2 | Research is empirical discovery of generated phenomena (experiment → infer → apply); technology is contested knowledge (blueprints/data stolen, intercepted, denied); applied discoveries yield emergent, sometimes surprising tech. |
| CAP-011 | Reputation, clearances & player organizations | P3 | Earned standing + clearances/licenses drive *access/requisition rights* and command authority (never the use of found gear); fleets and groups coordinate the war. |
| CAP-012 | Exploration & discovery | P3 | Scanning the dark reveals wrecks, resources, anomalies, and rare finds; supplies research subjects; a viable non-combat career. |
| CAP-013 | Strategic command layer | P3 | A higher-resolution command and coordination experience for leaders directing the faction war. |
| CAP-014 | Cyberwarfare (hacking & counter-hacking) | P3 | Infiltrate, seize, sabotage, or steal from enemy systems via a simulated, non-repetitive hacking discipline; counter-hackers detect, trace, and physically neutralize intruders. A deep specialist layer with a simple baseline. |

## Success Metrics / KPIs / Desired Outcomes

North star: a **fun, retained core community and a healthy, living economy** — explicitly NOT peak concurrent-user count. Targets are early hypotheses to calibrate in testing, set deliberately at niche-indie (not AAA) levels.

| Metric | Target | Why It Matters | Measurement Window |
|--------|--------|----------------|--------------------|
| Core retention (D30+) | 5–10% reach engaged "core" | The true north star for a deep sandbox; the sticky core sustains the world | 30+ days from first session |
| Early retention (D1 / D7) | ~25–40% / ~10–20% | Onboarding/first-impression health; secondary to core retention | 1 / 7 days |
| Average session length | 60–120 min | Sandbox sessions are "operations," not snacks; signals depth of engagement | Per session |
| Sessions per week (engaged) | 3–5 | Habit formation among the retained core | Weekly |
| Economic health (faucet/sink balance) | Within ~±10–15%; stable price indices | A balanced, non-inflating player economy is the sign of a living world | Monthly economy review |
| Market activity | Active listings + daily transaction volume above a floor | Confirms the trade/economy loop is functioning, not dead | Weekly |
| Contested-zone liveness | Players + NPCs above a per-zone "feels alive" floor | Directly measures the anti-empty-world goal where it matters | Continuous, per contested zone |
| Role diversity | Meaningful share of active players in non-combat careers | Confirms "many ways to matter" is real, not decorative | Monthly |
| Core-loop fun (qualitative) | Positive playtest sentiment on flight + combat feel | Principle: verify fun before scaling; gates further investment | Each alpha/playtest |

## Assumptions

- A small but dedicated community is achievable and sufficient for the product to be considered successful.
- Players will accept and even embrace hardcore stakes *if* loss is gated and recovery is fair.
- Non-combat careers (industry, research, trade, exploration, EW, hacking) will attract and retain a meaningful share of players, especially when made skill-based.
- NPC activity (and AI-filled roles) can credibly make the world feel alive at low player counts and let solo players field capable ships.
- The audience will reward fairness (no pay-to-win) and skill expression with loyalty.
- Emergent, systemic content (research → manufacturing) can substitute for large volumes of hand-authored content.

## Constraints

- **Solo developer, limited budget and time** — scope and ongoing operational burden must stay within one person's sustainable capacity.
- **Modest-scale-first** — the product is intended to run affordably for a modest community before any large-scale ambitions; early releases concentrate population rather than spreading it.
- **Never pay-to-win** — no real-money path may ever feed in-game currency, gear, or the replacement/expedite mechanic. This constrains the eventual monetization model to cosmetics, buy-to-play, and/or subscription.
- **Automation floor required** — every ship role must function on competent AI so solo/under-crewed play is viable; the design must never *force* crewing.
- **Physically grounded, gameplay-scaled** — model real physics relationships, but tune magnitudes for playability; not a realism simulator.
- **Cyberwarfare is simulated and sandboxed** — never real exploits, code, or infrastructure.
- **13+ minimum age** — to keep account/data obligations manageable and avoid child-directed regulatory burden.
- **Hardcore persistence** — assets persist in the world and can be lost, gated behind territorial defeat; a deliberate constraint on the safety model.

## Dependencies

- **Game hosting / server operation** for a persistent, always-on online world (and its ongoing cost).
- **Population substrate** — the NPC-populated world (and AI-filled roles) is a dependency for the world feeling alive at launch and at low concurrency.
- **Payment provider / merchant-of-record** (e.g., a storefront or payment platform) for the eventual commercial model, to offload payment-security, tax, and chargeback handling.
- **Ratings and legal baseline** — age rating (self-serve/digital), Terms of Service, EULA, and privacy policy aligned to GDPR/CCPA before any online or commercial launch.
- **Moderation & community support** — a reporting/moderation pipeline and a published code of conduct for chat, names, and player-created content.
- **Anti-cheat / economy-integrity measures** — protections against cheating, botting, and real-money trading; note the architecture (clients receive only entitled data) structurally prevents the most damaging info-cheats (maphacks).

## Risks

- **Scope creep / solo-developer burnout (existential).** The single most common killer of indie MMOs. *Mitigation:* deliver-value-early discipline and a deliberately minimal first release (one faction matchup, one contested area); validate fun at modest scale before scaling work; treat cyberwarfare/empirical-research/generative-manufacturing as later, phased pillars.
- **Cold-start / empty-world death spiral.** Below a liveness floor, even hardcore players leave. *Mitigation:* NPC-populated world as population insurance; concentrate launch geography; track contested-zone liveness as a real metric.
- **Hardcore-loss churn.** Full-stakes loss can repel the casual demand base. *Mitigation:* loss gated behind territorial defeat; safe home regions; fair, multi-path recovery so a loss is a setback, not an exit.
- **Accidental pay-to-win.** Even indirect money→power coupling triggers strong backlash. *Mitigation:* hard rule that real money never touches currency, gear, or the expedite/replacement path; cosmetic/B2P-first model.
- **Emergent-systems balance.** Generative research/manufacturing could produce accidentally over-powered or merely-noisy combos. *Mitigation:* instability/risk throttle, counterability + wide-shallow power band + skill>gear (no autowin), rarity gating, and active meta monitoring; "not useful" outcomes are acceptable and discoverable.
- **Automation tuning.** An AI floor too strong devalues crewing/multiplayer; too weak punishes solo. *Mitigation:* tune the floor↔ceiling gap per role; provide an NPC-crew middle tier.
- **Cyberwarfare scope/balance.** A deep hacking layer risks becoming an "I-win" button or a niche time-sink. *Mitigation:* phase it (abstract → deep), opt-in specialist depth with a simple baseline, counter-hacker arms-race balance, and the simulated/sandboxed constraint.
- **Toxicity & cheating** in a competitive sandbox. *Mitigation:* server-authoritative outcomes (info-cheats structurally limited), moderation pipeline, and anti-cheat/anti-RMT planning.
- **Operational sustainability** — 24/7 hosting, support, and economy monitoring as a solo operator. *Mitigation:* affordable modest-scale operation; lean live-ops; monetization to eventually cover costs.

## Open Questions

- **Exact monetization model.** "Eventually commercial, never pay-to-win" is decided; the specific mix — buy-to-play, optional subscription, and/or cosmetic-only — is not. *Recommendation: buy-to-play and/or cosmetics first; subscription only once population is proven.*
- **Near-wiped-faction handling.** Winnable war-cycles reset the contested frontier, but the detailed treatment of a faction pushed to the brink (refugee/fallback/regeneration) is undecided.
- **Strategic command experience (CAP-013).** The intent (earned authority directing the war) is clear; the actual player experience/interface is best defined against a working game.
- **Cyberwarfare depth & phasing (CAP-014).** How deep the simulated-hacking discipline goes, and when, vs. the simple baseline.
- **Emergent-tech balance.** How to keep generative research/manufacturing fun and bounded (neither OP nor noisy).
- **Automation gap tuning.** The right AI-floor↔human-ceiling gap per role.
- **Physics scale factors.** The chosen compression of distance/speed/lethality/time (grounded relationships, scaled magnitudes).
- **Liveness-floor and economic-health thresholds.** Numeric floors for "world feels alive" and faucet/sink tolerance, to calibrate through testing.

## Release or Validation Approach

A staged, feedback-driven rollout that validates the core loop before scaling:

1. **Core-loop validation** — prove that physics flight + skill-based combat is *fun* (the principle-zero gate) before building outward.
2. **Closed alpha** — invite-limited, iterative; validate fitting, damage, persistence, and the shared world with a small group; dev streams/Q&A tied to each test for direct feedback.
3. **Time-boxed stress tests** — scheduled, bounded sessions to validate capacity and the "feels alive" floor before widening access.
4. **Early Access** — incremental updates layering the war, economy, information, research, and cyber systems; validate economy health (faucet/sink, market activity) and retention at small scale.

**Cold-start mitigation at launch:** seed the world with the NPC substrate so it feels alive at low concurrency, and concentrate the opening on a single contested area / one faction matchup so the early population meets and fights rather than scattering.

## Domain Glossary / Terminology

- **The Severing**: the cataclysm that collapsed the galactic communication network, plunging systems into isolation and "dark silence."
- **Faction**: one of three warring successor powers a player belongs to; the player's team in the war.
- **Frontline / contested zone**: a region where factions actively fight for control; higher risk and higher reward, farther from safe home space.
- **Home core**: a faction's heavily-defended heartland and safe respawn/storage; only lost via a rare, epic campaign.
- **Outlaw status**: a reputation state any player can enter by preying on their own faction — hunted, dependent on the black market, locked out of friendly facilities.
- **Blueprint**: a transportable piece of technical knowledge required to manufacture an item; can be moved, stolen, intercepted, or denied.
- **Stockpile**: stored ships/parts at a facility, used for fast replacement; vulnerable if the facility falls.
- **War-cycle**: a bounded arc of the persistent war that a faction can "win," triggering rewards and a reset of the contested frontier while home cores and player progress persist.
- **Clearance / License**: an earned credential gating the *right to requisition or manufacture* a ship/equipment class — never the right to *use* found/stolen/salvaged gear.
- **Research sample**: scarce material or data analyzed to discover hidden properties; the input that paces empirical research.
- **Counter-hacker**: a defender who detects, traces back, and neutralizes (including physically) intruders in their systems.
- **Proxy chain**: a hacker's connection relayed through multiple systems to slow and obscure trace-back.

## Handoff Guidance

Context that downstream architecture design or governance work must preserve.

- **Product intent to preserve**: skill-decides-fights in *every* role; automation-floor/human-ceiling; a world alive at modest scale; information scarcity as the core tension (and as the anti-cheat); emergent/systemic content over authored; physically-grounded-but-gameplay-scaled; many equally-valid careers; deliver-value-early.
- **Scope boundaries to respect**: modest-scale-first; no on-foot/avatar or free-form hull construction early; cyberwarfare is simulated/sandboxed and a later pillar; first release is a single faction matchup + one contested zone.
- **Critical constraints**: **never pay-to-win** (real money never reaches currency, gear, or replacement/expedite); progression gates access/requisition, never combat stats or usage of found gear; hardcore persistence gated behind territorial defeat; 13+ audience; solo-sustainable footprint.
- **Open decisions needing technical input**: per-zone liveness floor and economic-health thresholds; the command-layer experience; cyberwarfare depth; emergent-tech balance; automation gap tuning; physics scale factors; how recovery/expedite stays provably non-pay-to-win.

## Project Context Baseline Updates

- **2026-06-01 (refine):** Folded in cyberwarfare (CAP-014), empirical research + generative/emergent manufacturing (CAP-006/CAP-010), and clearances/licenses progression (CAP-011), plus new principles (skill-in-every-role, automation floor/human ceiling, emergent-over-authored, physically-grounded-gameplay-scaled) and personas (Scientist, Operator, Engineer, Netrunner). **Supersession:** the earlier "no character XP" framing is replaced by clearances/licenses that gate requisition (not usage) and never grant combat stats. Source: `docs/game-design.md`. Capability IDs CAP-001…013 preserved; CAP-014 added.
- **2026-06-01 (E002 spec):** Cross-cutting piloting decision for CAP-001 — flight uses a **flight-assist toggle** as the standard control paradigm: assist ON = drift-damped/accessible (ship trends toward heading); assist OFF = decoupled full-momentum (heading independent of velocity). All future piloting/combat epics inherit this model. CAP-001 is validated by a hands-on principle-VII "feels good" playtest gate before networked scaling. Source: `specs/00002-single-player-flight-combat/spec.md`.
