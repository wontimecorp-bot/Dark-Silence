# Research: Ship Fitting & Modules (E006)

Domain best practices informing the PRODUCT spec for the data-driven Module abstraction + positional-slot fitting. Patterns only; the data model itself is a plan/ADR concern (ADR-0008).

## Positional-slot / spatial fitting

Make slot **position** on the hull determine both firing arc/coverage and which module is exposed to incoming fire, and treat the realized fit layout as the authoritative hitbox/armor map (outer mounts = more arc but more risk; inner = protected). In grid systems (Cosmoteer) the layout *is* the hit map — shots strike outer tiles first, so reactors/magazines must sit behind armor. EVE turret hardpoints fire within arcs; World of Warships ties survival to angling + a sectioned armor map (citadel hits hurt far more). **Validation signal**: a hit on a world point resolves to the module occupying that slot; outer modules shield inner; arc/facing gates whether a turret can engage. **Avoid**: cosmetic-only slots; uniform hull HP ignoring module position; hardpoints with no arc/facing.

## Resource-budget fitting

Three orthogonal budgets — **power, CPU/control, mass** — plus slot count/size; mass also feeds Newtonian agility so heavy fits trade flight feel. EVE forces tradeoffs via competing constraints (slots, powergrid, CPU, calibration): maxing tank starves damage/speed/utility, so you can't fit everything, and over-budget fits are rejected (can't undock). **Validation signal**: sum of module draw per axis ≤ budget; exceeding any axis blocks the fit; tension is good only when different fits bind on different axes (tank fit vs damage fit hit different ceilings). **Avoid**: one overpowered global budget; budgets that always bind together (no tension); silently allowing over-budget fits to function.

## Uniform module data model & emergent roles

One uniform module schema (power gen/draw, CPU draw, mass, heat, hitbox/health, hardpoint type+size); derive class/role from the installed set so roles/specialization/redundancy **emerge** from loadout rather than fixed classes. Give each module type a distinct strength + cost so tank/damage/speed/range/utility stay mutually exclusive at the margin. **Validation signal**: multiple viable distinct fits exist for one hull; no single module/fit strictly dominates across all metrics. **Avoid**: hardcoded ship classes bypassing the module model; near-identical modules; a stat curve with a strictly dominant module (degenerate optimal fit).

## Fitting UX & validation

Live budget bars (power/CPU/mass) recomputed per change; reject + clearly flag over-budget and type/size-mismatched placements; sandbox/preview + saved presets. EVE's simulator updates stats live, shows green (positive)/red (negative) deltas, filters the module browser by *remaining* budget so impossible modules vanish, and blocks over-budget fits with explicit warnings. **Acceptance edge cases the spec must cover**: empty fit (valid baseline), over-budget on **each** axis (power, CPU, mass), hardpoint **size** mismatch, hardpoint **type** mismatch. **Avoid**: silent overflow; commit-then-fail; obscured budget headroom; no preview before applying.

## Sources

- Cosmoteer Armor (layout-as-hitmap, layering) — cosmoteer.wiki.gg/wiki/Armor
- WoWS Gunnery & Armor Penetration (sectioned armor, angling, citadel) — wiki.wargaming.net
- EVE Fitting ships + Fitting Simulation (power/CPU/calibration + slots; live green/red; remaining-resource filter; hard rejection) — wiki.eveuniversity.org / eveonline.com
- Modular / data-oriented design (uniform records; homogenization risk) — gamesfromwithin.com
