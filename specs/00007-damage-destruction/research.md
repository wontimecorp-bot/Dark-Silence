# Research: Damage & Destruction (E007)

Domain best practices informing the PRODUCT spec for the unified typed-damage pipeline, hit-location penetration, sectioned destruction + severing, and salvage. Patterns only; the pipeline data model is a plan/ADR concern (ADR-0008).

## Typed damage channels × layered defenses

Make each defense layer (shields/armor/hull/systems) resist channels **asymmetrically** so each channel has a "preferred" target — e.g. EM/energy strong vs shields, kinetic/penetration strong vs armor, thermal/blast vs hull/systems. Use a small **flat-percentage (layer × channel) resistance matrix**; it reads intuitively and lets players reason about loadouts. Ensure every channel and every layer has at least one regime where it wins, so no single cell dominates. **Validation signal**: no channel is globally optimal vs all layers, no layer is bypassable by one channel; effective-HP curves cross across ≥2 channels. **Avoid**: pure multiplicative stacking (unintuitive); resist/HP redundancy where the matrix collapses to one dimension.

## Angle-based penetration / hit-location armor

**CORE for MVP**: effective armor = `thickness / cos(impact angle)`; ricochet above an angle band (guaranteed >60°, chance 45–60°); overmatch (penetration size ≥ ~1.3× plate thickness ignores angle); pen vs over-penetration damage tiers (full pen ~33%, overpen ~10%); post-penetration damage applies to whatever module/section sits behind the entry point (the E006 fit layout as armor map / hitbox). **DEFER**: per-shell normalization curves, multi-layer "armor cake" traversal, per-section damage saturation, channel-specific penetration coefficients. **Validation signal**: angling a ship measurably raises effective armor + ricochet rate; overmatch defeats thin plating; a full pen routes damage to the correct module behind the hit. **Avoid**: making every shot binary (one-shot or nothing); always-100% citadel zones that make positioning irrelevant.

## Sectioned destructible hulls + connectivity severing

Run **flood-fill island detection on the hull grid ONLY at destruction events** (a section reaches zero health), never per frame. Disconnected regions spawn as independent physics bodies inheriting the parent's velocity + angular velocity at their centre of mass. Coarse section granularity now; keep the grid cell-addressable so fine per-cell can replace sections later (ADR-0008) without changing the connectivity contract. **Validation signal**: removing a connecting section splits the hull into N drifting chunks, each conserving inherited momentum; no split while the grid stays connected. **Avoid**: per-cell collider rebuild + continuous connectivity scans (the perf trap); orphaned single cells; chunks spawning with zero/incorrect inherited velocity (visual "pop").

## Salvage / wreckage from combat

**Two-tier outcome**: a **clean sever** (module detached intact via connectivity, never damaged below threshold) yields a scavengeable intact module; a module **destroyed/penetrated through** yields scrap/resources only. Persist wrecks as **lootable world entities** that survive owner logout and feed the economy (E013); selective salvage rewards exposing high-value modules — scavenging (cut armor → expose → claim), not mining. **Validation signal**: a clean-severed module is re-equippable at full identity; a through-killed module yields only scrap; the wreck persists + is claimable; an over-killed ship still leaves at least scrap (never nothing). **Avoid**: over-kill yielding zero loot (frustration); double-claiming a wreck; partial wrecks with dangling/un-targetable modules; "blast it apart" strictly beating careful salvage (kills the clean-sever path).

## Sources

- World of Warships — Ship Armor & Penetration (effective armor, ricochet, overmatch, pen/overpen tiers) — wiki.wargaming.net
- War Thunder — shell normalization vs angled armor (deferred mechanics) — wiki.warthunder.com
- Voxel physics — island/region scan + split-into-bodies (Iolite) — docs.iolite-engine.com; Space Engineers disconnect failure modes — keenswh
- Avorion — wreckage as entity, salvage-vs-destroy loot split, persistent shared-wreck economy — avorion.fandom.com
- Damage-matrix readability (flat % vs multiplicative) — jmargaris.substack.com / gamedev.net
