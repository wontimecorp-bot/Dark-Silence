---
adr_id: ADR-0009
status: accepted
date: 2026-06-01
tags: [cyberwarfare, hacking, gameplay-system, security]
supersedes: []
superseded_by: ""
related_artifacts: [PRD:CAP-014, PRD:CAP-007, ADR-0002, ADR-0006, ADR-0008]
---

# ADR-0009: Cyberwarfare — simulated, sandboxed hacking & counter-hacking

## Status

Accepted.

## Context

The design wants in-game hacking and counter-hacking that feels like real hacking and is non-repetitive, integrating with the systemic-capture verbs, electronic warfare (EW), and contested-knowledge systems — without ever exposing real infrastructure. A decision is needed now to fix the model (constant grammar, variable content) and the security boundary before any cyber depth is built, so that later phases extend one foundation rather than reconcile divergent prototypes.

## Decision Drivers

- Faithful hacking feel.
- Non-repetition.
- Integration with existing systems (capture, EW, contested-knowledge).
- Security: never real exploits, infrastructure, or accounts.
- Server-authoritative resolution.
- Phaseable for a solo dev.

## Considered Options

### Option A: Abstract stat-check hack

Tool rating vs. security rating plus a channel-time, resolved as a single check.

- **Pros**: Cheap; low friction. Kept as the early baseline.
- **Cons**: Shallow; does not meet the "feels like real hacking" goal.

### Option B: Fixed puzzle minigame

A fixed puzzle minigame (e.g. EVE-style).

- **Pros**: Gamey; simple to build.
- **Cons**: Becomes repetitive (same puzzle every time). Rejected as the core.

### Option C: Simulated-systems model

A simulated-systems model (inspired by Uplink/Hacknet/Grey Hack/Exapunks) of the real cyber kill-chain: recon → gain access → escalate privilege → act (take over / use systems / cause malfunctions / install a virus / open security holes / steal intel-blueprints) → persist → cover tracks; the counter-hacker detects (IDS), traces back, patches, sets honeypots, and ejects. Non-repetition by construction: a procedural attack surface generated from the target's actual module/firmware/config; defender-authored defenses (humans are the content); emergent vulnerability CHAINS from a property/rule system (not a fixed vuln catalog); a tools×config meta (loadouts of exploits/zero-days vs. configurations, evolving as both sides patch and trade); and live contested duels plus persistent history. Cyber↔physical interlock: a successful counter-trace LOCALIZES the hacker's terminal/ship so security/forces can physically neutralize them. Proxy/relay chaining: hop-by-hop trace, where cutting comms relays can break a chain.

- **Pros**: Deep and non-repetitive; faithful to real hacking; integrates with EW, capture, contested-knowledge, and social espionage; exposes the hacker's body, creating tension. Chosen.
- **Cons**: Significant scope (later pillar; must be phased); balance depends on the counter-hacker arms-race; the sandboxing / no-real-infra constraint is mandatory.

## Decision Outcome

Chosen option: **Option C: Simulated-systems model** — adopt the simulated, sandboxed, server-authoritative cyberwarfare model, with the abstract baseline (Option A) as the early phase. Provide opt-in specialist depth (a "netrunner" role) alongside a simple version for everyone. The grammar (kill-chain) is constant; the content (configs, defenders, tools, situation) is variable — chess, not a fixed puzzle. NEVER literal exploits against real infrastructure.

## Consequences

### Positive

- Deep, fresh, and non-repetitive.
- Integrates EW + capture + contested-knowledge + social espionage.
- Exposes the hacker's body, creating strong tension.

### Negative

- Significant scope — a later pillar that must be phased.
- Balance must come from the counter-hacker arms-race.
- The sandboxing / no-real-infrastructure constraint is mandatory.

### Neutral

- Resolves entirely inside the authoritative server.

## Links

- PRD: CAP-014, CAP-007
- Related ADRs: [ADR-0002](0002-server-authoritative-netcode.md), [ADR-0006](0006-interest-management-and-bandwidth-scaling.md), [ADR-0008](0008-unified-domain-data-model.md)
- docs/game-design.md §6
