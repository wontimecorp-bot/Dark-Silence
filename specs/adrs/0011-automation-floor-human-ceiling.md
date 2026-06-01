---
adr_id: ADR-0011
status: accepted
date: 2026-06-01
tags: [automation, ai, crewing, balance]
supersedes: []
superseded_by: ""
related_artifacts: [PRD CAP-008, PRD CAP-001, ADR-0008]
---

# ADR-0011: Automation floor, human ceiling (how ship roles are operated)

## Status

Accepted.

## Context

Solo pilots and under-crewed big ships must remain functional, but crewing and operator skill must be meaningfully rewarded — without forcing players into groups. A ship has multiple operable roles (piloting, weapons, electronic warfare, sensors, damage control), and the design must let a single player engage a large vessel while still preserving headroom for additional crew or skilled operators to add value. A decision is needed now to set the operating model for ship roles before the crewing, AI, and balance systems diverge.

## Decision Drivers

- Solo viability — never force crewing; a lone player must always be able to operate any ship.
- Make multi-crew and operator skill genuinely valuable, not merely cosmetic.
- Provide an accessible floor while preserving a high skill ceiling.
- Reuse the existing NPC AI system rather than building parallel automation.

## Considered Options

### Option A: Fully manual roles

- **Pros**: Maximal skill expression; every role is hand-flown.
- **Cons**: Punishes solo and under-crewed play; large ships become unplayable solo. Rejected.

### Option B: Fully automated roles

- **Pros**: Trivial to operate; no coordination overhead.
- **Cons**: Removes human value and skill expression entirely; nothing for a skilled operator or crew to improve upon. Rejected.

### Option C: Automation floor + human ceiling

- **Pros**: Every role has a competent AI baseline so solo and under-crewed play works; a skilled human operator (or hired NPC crew, or another player) outperforms that baseline, and the GAP between floor and ceiling is the reward. Supports a configurable delegation spectrum (full-auto ↔ assisted ↔ manual) plus dynamic takeover; unfilled seats run on AI; NPC-crew quality forms a middle tier bridging solo ↔ multiplayer. The floor↔ceiling gap becomes the central balance knob, widest where judgment matters most (EW timing, target prioritization, damage-control triage, reading deceptive sensor pictures).
- **Cons**: The floor↔ceiling gap requires careful per-role tuning, and building competent AI is significant work.

## Decision Outcome

Chosen option: **Option C: Automation floor + human ceiling** — adopt the automation-floor/human-ceiling model. Every ship role exposes a competent AI baseline (the floor) so solo and under-crewed play is always viable, while a skilled human operator, hired NPC crew, or another player can exceed that baseline (the ceiling), with the gap between them serving as the reward and the central balance lever. The automation IS the NPC AI system reused — one system doing three jobs: driving NPCs, filling empty crew seats, and piloting offline/logged-off ships. Human-uplift activities are opt-in attention that rewards focus rather than mandatory plate-spinning.

## Consequences

### Positive

- Solo-viable AND crew-rewarding: a lone player can operate any ship, while crew and skill still add measurable value.
- Accessible-yet-deep: a low floor welcomes newcomers while a high ceiling sustains mastery.
- One AI system serves many uses (NPCs, empty seats, offline ships), reducing duplicated systems.

### Negative

- The floor↔ceiling gap needs careful per-role tuning: too strong devalues crewing and multiplayer; too weak punishes solo play.
- Building competent role AI is significant engineering work.

### Neutral

- Ties into the NPC-crew economy and clearances (NPC-crew quality is the middle tier between solo and multiplayer).

## Links

- PRD CAP-008
- PRD CAP-001
- Related ADR: [ADR-0008](0008-unified-domain-data-model.md)
- docs/game-design.md §4
