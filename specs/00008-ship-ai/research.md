# Research: Ship AI Architecture
> 00008-ship-ai | 2026-06-10 | Ground the utility-FSM brain ADR + scheduler/steering/LOD plan

## Utility-FSM Hybrid
- **Decision**: Score enum states via multiplied consideration response curves (linear/quadratic/logistic over normalized inputs); give the incumbent state a ~25% momentum bonus; group behaviors into priority buckets evaluated highest-first.
- **Rationale**: Dave Mark's Infinite Axis Utility AI is the shipped-game canon; the momentum bonus is the standard oscillation fix.
- **Rejected**: Weighted-sum scoring (loses the zero-score veto) and pure FSM transition tables (edge explosion).
- **Pitfalls**: Multiplication shrinks scores as consideration count grows (apply Mark's compensation factor); without a per-ship score-breakdown debug view, tuning is blind.
- **Sources**: [https://www.gdcvault.com/play/1018040/Architecture-Tricks-Managing-Behaviors-in], [https://media.gdcvault.com/gdc10/slides/MarkDill_ImprovingAIUtilityTheory.pdf]

## AI Determinism
- **Decision**: Fixed-step thinks; strict-f32 scoring (no fast-math, no libm transcendentals in scores); seeded hash of (entity-id, tick) instead of RNG; sorted iteration with entity-id tiebreak on equal scores.
- **Rationale**: Per-binary float determinism holds when transcendentals, iteration order, and randomness are controlled.
- **Rejected**: Full fixed-point math — the sim is server-authoritative single-binary, not cross-platform lockstep.
- **Pitfalls**: Equal scores without a stable tiebreak silently break replay; checksum sim state in golden tests to catch drift early.
- **Sources**: [https://gafferongames.com/post/floating_point_determinism/], [https://www.gamedeveloper.com/programming/cross-platform-rts-synchronization-and-floating-point-indeterminism]

## Context Steering
- **Decision**: 8–16-slot interest + danger maps; behaviors combine per-slot with max; mask slots whose danger exceeds the minimum, pick the highest-interest unmasked slot, sub-slot interpolate.
- **Rationale**: Fray's masking (not subtraction) keeps hard avoidance at constant cost, jitter-free — shipped in F1 2011.
- **Rejected**: Subtracting danger from interest — high interest can override lethal headings.
- **Pitfalls**: Non-holonomic ships need slots pre-biased by turn-rate/velocity reachability or the chosen heading chatters against the flight model.
- **Sources**: [https://www.gameaipro.com/GameAIPro2/GameAIPro2_Chapter18_Context_Steering_Behavior-Driven_Steering_at_the_Macro_Scale.pdf], [https://andrewfray.wordpress.com/2013/03/26/context-behaviours-know-how-to-share/]

## Flow Fields for Group Movement
- **Decision**: Compute fields per squad objective over coarse tiles, on demand and cached, only for tiles the group crosses; members sample O(1) into their interest map.
- **Rationale**: SupCom2 shipped flow-field tiles for thousands of units; AoE4 confirms the field-to-goal + local-steering hybrid.
- **Rejected**: Per-ship repathing and whole-map field rebuilds (cost scales with map, not groups).
- **Pitfalls**: Field integration is the costly step — amortize across ticks and invalidate deterministically; open space means fields pay off only near obstacles/contested zones.
- **Sources**: [https://www.gameaipro.com/GameAIPro/GameAIPro_Chapter23_Crowd_Pathfinding_and_Steering_Using_Flow_Field_Tiles.pdf], [https://media.gdcvault.com/GDC+2022/Speaker+Slides/Pathing+In+Age_Cheng_Frank+2022-03-29+00.16.38.pdf]

## AI LOD / Hierarchical Command
- **Decision**: Hard-cap full brains near players (AC Unity: 40 real AIs in a 10k crowd); a hierarchical group tree expands/collapses nodes by player proximity; time-slice mid-tier thinks across buckets.
- **Rationale**: Shipped crowd titles get 100×+ scale from fixed full-fidelity budgets plus group-node abstraction.
- **Rejected**: Uniform per-ship think throttling — degrades ships in front of the player too.
- **Pitfalls**: Promote/demote must synthesize plausible individual state (position continuity, in-flight orders) and use boundary hysteresis, or tiers thrash and pop.
- **Sources**: [https://gdcvault.com/play/1022411/Massive-Crowd-on-Assassin-s], [https://www.researchgate.net/publication/221252089_Level_of_Detail_AI_for_Virtual_Characters_in_Games_and_Simulation]

## ECS AI Scheduling (bevy_ecs)
- **Decision**: One brain component holding the behavior enum + blackboard (mutate the field, never add/remove state components); event/Changed-driven re-evaluation; fallback cadence via stable-id hash % N phase buckets.
- **Rationale**: Per-state marker components force a table move per transition and explode archetype count; enum-in-component keeps iteration dense and order stable.
- **Rejected**: Marker-component-per-state and per-tick polling of all brains.
- **Pitfalls**: Entity-index buckets are deterministic only if spawn order is — derive buckets from a sim-stable id.
- **Sources**: [https://taintedcoders.com/bevy/archetypes], [https://github.com/bevyengine/bevy/discussions/6493]

## Summary
| Topic | Decision | Rationale |
|---|---|---|
| Utility-FSM | Curves ×, momentum bonus, buckets | IAUS canon; fixes oscillation |
| Determinism | Strict-f32, seeded hash, tiebreaks | Per-binary float determinism is controllable |
| Context steering | 8–16 slots, max-combine, danger masking | Hard avoidance, constant cost, shipped |
| Flow fields | Per-squad-goal tiles, cached, O(1) sample | SupCom2/AoE4 proven at scale |
| AI LOD | Capped full brains + collapsible group tree | Fixed budgets give 100×+ scale |
| ECS scheduling | Enum-in-component + event-driven + hash buckets | Avoids archetype thrash; ≈0 idle cost |

## Sources Index
| URL | Topic | Fetched |
|---|---|---|
| gdcvault.com/play/1018040 | Utility-FSM | 2026-06-10 |
| media.gdcvault.com/gdc10/slides/MarkDill_ImprovingAIUtilityTheory.pdf | Utility-FSM | 2026-06-10 |
| gafferongames.com/post/floating_point_determinism/ | Determinism | 2026-06-10 |
| gamedeveloper.com/programming/cross-platform-rts-synchronization-and-floating-point-indeterminism | Determinism | 2026-06-10 |
| gameaipro.com/GameAIPro2/GameAIPro2_Chapter18 (PDF) | Context steering | 2026-06-10 |
| andrewfray.wordpress.com/2013/03/26/context-behaviours-know-how-to-share/ | Context steering | 2026-06-10 |
| gameaipro.com/GameAIPro/GameAIPro_Chapter23 (PDF) | Flow fields | 2026-06-10 |
| media.gdcvault.com/GDC+2022 Pathing In Age (PDF) | Flow fields | 2026-06-10 |
| gdcvault.com/play/1022411 | AI LOD | 2026-06-10 |
| researchgate.net/publication/221252089 | AI LOD | 2026-06-10 |
| taintedcoders.com/bevy/archetypes | ECS scheduling | 2026-06-10 |
| github.com/bevyengine/bevy/discussions/6493 | ECS scheduling | 2026-06-10 |
