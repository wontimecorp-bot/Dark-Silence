---
feature_branch: "00003-authoritative-networking"
created: "2026-06-02"
input: "E003 — authoritative server/client networking with prediction, reconciliation, interpolation, and input validation"
spec_type: "technical"
spec_maturity: "draft"
epic_id: "E003"
epic_sources: "{SAD:ADR-0002}{SAD:ADR-0007}"
---

# Feature Specification: Authoritative Networking

**Feature Branch**: `00003-authoritative-networking`  
**Created**: 2026-06-02  
**Status**: Draft  
**Spec Type**: technical  
**Spec Maturity**: draft  
**Epic ID**: E003  
**Epic Sources**: {SAD:ADR-0002}{SAD:ADR-0007}  
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

E002 proved the flight-and-combat loop is fun, but it runs entirely in one client process — there is no shared world and no authority. Every later world system (shared world E005, tiered sim E008, interest management E009, sensors E010, war E011) depends on an authoritative server that clients connect to, predict against, and cannot cheat. Without this networking foundation the game stays single-player. This epic splits the running slice into a server-authoritative architecture — the server is the single source of truth, clients predict their own ship and interpolate everyone else, and the server validates every input — established at a small, measurable baseline (two clients + bots) before scale (AOI/bandwidth budget) and persistence are layered on.

## Scope *(mandatory)*

### Included

- A headless **authoritative `server`** that runs the shared E001 `sim` at the fixed tick as the single source of truth, with an in-process **loopback** mode (server + client in one process) for solo play, development, and headless tests.
- A **`protocol` crate** defining the wire messages (numbered client input, delta snapshot, connect/session handshake) and isolating the netcode library behind an adapter (swappable).
- **UDP** transport with delta-vs-last-acked snapshots and redundant input sends; a small **session** of two or more connected clients sharing one server world.
- **Client-side prediction** of the local ship (run `sim` locally on numbered inputs) + **server reconciliation** (re-seed to authoritative state and deterministically replay unacknowledged inputs, with smoothed corrections).
- **Remote-entity interpolation** at a fixed delay (~100 ms) from a snapshot buffer.
- **Server-side input validation**: accept only inputs (never client positions/hits), clamp thrust/turn, rate-limit fire, resolve hits authoritatively (baseline lag-compensated rewind).
- A **bandwidth baseline** (bytes/client/sec) and a **headless bot harness** driving ≥2 networked clients for integration tests.
- **Secure transport**: sessions use an authenticated + encrypted channel (renet_netcode secure mode, connect-token connections); an unauthenticated/unsecure connect is rejected. The connect-token **issuer** is a stub (local signing key) in E003 — E004 replaces it with account-backed issuance; the secure-connection wiring is reused.
- Networkizing the E002 client: input becomes `ClientInput` sent to the server; rendering is driven by the predicted local state + interpolated remote snapshots.

### Excluded

- **Area-of-interest / interest-management filtering and the per-client bandwidth budget** — that is E009; E003 replicates the whole small session and only *measures* the baseline.
- **Persistence, accounts, login, and the shared persistent world** — E004/E005.
- **Tiered simulation (Tier-0 bubbles / Tier-1 transit) and time dilation** — E008/E009.
- **Multi-node server meshing** — deferred (ADR-0005); E003 is single-node.
- **Full anti-cheat heuristics and WAN lag-compensation tuning** — baseline validation only; hardening is later.
- **New gameplay** — E003 networkizes the *existing* E002 flight/combat entities; no new ships, weapons, or damage systems.

### Edge Cases & Boundaries

- **Packet loss / jitter**: a dropped snapshot must be ridden out by the interpolation buffer (no teleport); lost inputs self-heal via redundant sends.
- **Prediction mismatch**: when the client mispredicts (e.g., an inter-player interaction it cannot foresee), reconciliation MUST converge to the authoritative state with a smoothed (non-teleporting) correction.
- **Malicious / impossible input**: out-of-bounds thrust/turn, excessive fire rate, replayed/old sequence numbers, or client-asserted positions/hits MUST be rejected or ignored — the server state is unaffected.
- **Client disconnect mid-session**: the server drops the session cleanly; remaining clients continue (the ship's coast-on-disconnect behavior is a later epic, not required here).
- **Late/duplicate/out-of-order packets**: stale snapshots and duplicate inputs are discarded by sequence/ack tracking.
- **Clock/tick alignment**: client prediction and server authority share the fixed logical tick (E002 FR-016); snapshot send rate (15–20 Hz) is below the tick rate (30 Hz).
- **Determinism boundary**: reconciliation assumes the shared `sim` is deterministic for identical inputs; floating-point drift across machines is absorbed by re-seeding from the authoritative snapshot each correction.

## Technical Objectives *(mandatory for technical specs only)*

### Objective 1 - Authoritative server & session (Priority: P1)

A headless `server` that owns the world: it runs the shared `sim` at the fixed tick, accepts client connections into a session, and is the single source of truth for all entity state. An in-process loopback mode runs server + client together for dev/solo/tests.

**Why this priority**: Core of the epic and of the whole MMO — without an authoritative server there is no shared world; every later world epic builds on it (E005/E008/E009/E010/E011).

**Rationale**: ADR-0002 (server-authoritative) and ADR-0005 (single-node first); the server reuses the E001 `sim` so client and server never diverge.

**Deliverables**: `server` crate (authoritative tick loop + session manager); loopback mode; client↔server connection lifecycle.

**Validation Criteria**:
1. **Given** a running server, **When** two clients connect, **Then** they share one authoritative world and see each other's entities.
2. **Given** loopback mode, **When** a single process starts, **Then** the embedded server drives the local client identically to a remote connection.

### Objective 2 - Protocol crate & transport isolation (Priority: P1)

A `protocol` crate defining the wire message set and isolating the netcode library behind an adapter, over UDP with delta snapshots and redundant input sends.

**Why this priority**: All client/server communication and the later replication/AOI work (E009) depend on a stable, library-agnostic protocol surface; isolation lets the netcode library be upgraded/swapped (ADR-0007 fallback) without touching gameplay.

**Rationale**: ADR-0007 (tech stack; netcode behind `protocol` + adapters); keeps `sim` and gameplay free of library types.

**Deliverables**: `protocol` crate (message types: `ClientInput`, `Snapshot`, handshake/connect); netcode-library adapter; UDP send/receive path.

**Validation Criteria**:
1. **Given** the protocol types, **When** a message is encoded and decoded, **Then** it round-trips to an equal value.
2. **Given** the adapter boundary, **When** code outside the adapter is inspected, **Then** it names no netcode-library type (gameplay/`sim`/`protocol`-consumer surface is library-agnostic).

### Objective 3 - Prediction & reconciliation (Priority: P1)

The client predicts its own ship by running `sim` locally on numbered inputs, and reconciles to the authoritative state by re-seeding and deterministically replaying unacknowledged inputs, smoothing the correction.

**Why this priority**: Responsiveness under latency is non-negotiable for the action combat E002 proved; without prediction the game feels laggy, without reconciliation it desyncs.

**Rationale**: ADR-0002; reuses the deterministic E001 `sim` (the same code on both ends) so replay is exact.

**Deliverables**: input sequence numbering + buffer; local prediction step; reconciliation (re-seed + replay) with correction smoothing.

**Validation Criteria**:
1. **Given** normal play, **When** the client acts, **Then** its own ship responds immediately (predicted), with no perceptible input delay.
2. **Given** a forced prediction mismatch, **When** the next authoritative snapshot arrives, **Then** the client converges to the server state with a smoothed (non-teleporting) correction.

### Objective 4 - Remote-entity interpolation (Priority: P1)

Clients render remote entities at a fixed interpolation delay (~100 ms) from a snapshot buffer, interpolating between snapshots to ride out jitter and loss.

**Why this priority**: Other ships/projectiles/targets must move smoothly despite a 15–20 Hz lossy snapshot stream; raw snapshots stutter.

**Rationale**: ADR-0013 (the E002 client already interpolates render between fixed steps — extended here to remote snapshots).

**Deliverables**: per-client snapshot buffer; interpolation of remote entity transforms at the delay window.

**Validation Criteria**:
1. **Given** a 15–20 Hz snapshot stream, **When** remotes are rendered, **Then** their motion is smooth (interpolated), not stepped.
2. **Given** simulated packet loss/jitter, **When** a snapshot is dropped or late, **Then** remotes keep moving smoothly within the buffer window (no teleport).

### Objective 5 - Server-side input validation (Priority: P1)

The server trusts only inputs: it clamps thrust/turn to bounds, rate-limits fire, ignores client-asserted positions/hits, and resolves hits authoritatively (baseline lag-compensated rewind).

**Why this priority**: Server authority is the structural anti-cheat the whole design rests on (PRD/SAD); a client must never be able to dictate state.

**Rationale**: ADR-0002; SAD Security (one trust boundary at the server).

**Deliverables**: input bounds/rate-limit validation; rejection of impossible/stale inputs; server-authoritative hit resolution with target rewind.

**Validation Criteria**:
1. **Given** an out-of-bounds or excessive-rate input, **When** the server processes it, **Then** it is clamped/rejected and the authoritative state is unaffected.
2. **Given** a client that asserts a position or a hit, **When** the server receives it, **Then** the assertion is ignored — only server-`sim`-derived state and server-resolved hits count.

### Objective 6 - Bandwidth baseline & bot harness (Priority: P2)

Delta-vs-last-acked quantized snapshots with measured bytes/client/sec, and a headless bot harness driving ≥2 networked clients for automated integration tests.

**Why this priority**: The netcode functions without a formal byte measurement, but the baseline number is the input E009 optimizes against, and the bot harness is how the P1 objectives are verified repeatably; valuable but not blocking the core loop.

**Rationale**: SAD performance attribute (bytes/client/sec); project testing policy (headless-bot integration harness).

**Deliverables**: snapshot delta+quantization; bytes/client/sec instrumentation; headless bot/test harness.

**Validation Criteria**:
1. **Given** a bot session of two clients, **When** it runs, **Then** baseline bytes/client/sec is measured and recorded.
2. **Given** the bot harness, **When** integration tests run headlessly, **Then** prediction/reconciliation/interpolation/validation are exercised without rendering.

### Technical Constraints

- **Server-authoritative** (ADR-0002): clients never dictate state; the server validates all input.
- **Library isolated** behind the `protocol` crate + adapter (ADR-0007); no netcode-library type in gameplay/`sim`/protocol-consumer surfaces (swappable; lightyear planned, `bevy_replicon` fallback).
- **UDP** delta snapshots; reference rates from the SAD: ~30 Hz authoritative tick, 15–20 Hz snapshot send, ~100 ms client interpolation.
- **Shared deterministic `sim`** (E001) runs identically on both ends; reconciliation depends on it.
- **Single-node** (ADR-0005); **no AOI/budget** (E009), **no persistence** (E004), **no tiering/dilation** (E008/E009) in this epic.
- Clippy `-D warnings` + rustfmt; the headless-bot harness drives networked integration tests (project Testing Policy).

## Integration Points *(mandatory for technical and operational specs)*

- **IP-001**: Server and client both depend on the E001 `sim` crate (`BodyState`, components, fixed-step systems) — the shared deterministic code path.
- **IP-002**: Networkizes the E002 client — keyboard input becomes a numbered `ClientInput` to the server; rendering is driven by predicted local state + interpolated remote snapshots (the thin-client seam, ADR-0013).
- **IP-003**: Produces the `protocol` crate + net adapters consumed by both client and server, and later by E009 (interest-management filters/encodes this replication stream).
- **IP-004**: Uses the netcode library named in ADR-0007 (lightyear planned), confined to the adapter; the transport is UDP.

## Requirements *(mandatory)*

### Technical Requirements *(technical specs only)*

- **TR-001**: System MUST provide a headless authoritative `server` that runs the shared E001 `sim` at the fixed logical tick as the single source of truth for all entity state.
- **TR-002**: System MUST support a session of two or more clients connected to one server, sharing a single authoritative world.
- **TR-003**: System MUST provide an in-process loopback mode (embedded server + client in one process) for solo play, development, and headless tests, behaviorally equivalent to a remote connection.
- **TR-004**: System MUST define a `protocol` crate with the wire message set — at minimum a numbered `ClientInput`, a `Snapshot` (entity state + per-client input ack), and a connect/session handshake.
- **TR-005**: System MUST isolate the netcode library behind an adapter such that no library type appears in the `sim`, gameplay, or `protocol`-consumer public surfaces (the library is swappable).
- **TR-006**: System MUST transport over UDP, sending delta snapshots and redundant recent inputs so a single lost packet self-heals.
- **TR-007**: System MUST number client inputs with a monotonic per-client sequence and apply them locally on the client (prediction of the local ship) by running the shared `sim`.
- **TR-008**: The server MUST acknowledge each client's last-processed input sequence in its snapshots.
- **TR-009**: The client MUST reconcile by re-seeding the local ship to the authoritative snapshot state and deterministically replaying all inputs after the acknowledged sequence through the shared `sim`, smoothing the resulting correction (no teleport).
- **TR-010**: The client MUST render remote entities at a fixed interpolation delay (~100 ms) by interpolating between buffered snapshots, riding out single-packet loss/jitter.
- **TR-011**: The server MUST validate inputs — clamping thrust/turn to physical bounds and rate-limiting fire — and MUST reject impossible, replayed, or stale inputs without affecting authoritative state.
- **TR-012**: The server MUST treat client-reported positions and hit claims as non-authoritative — all motion comes from the server `sim`, and hits are resolved server-side (baseline lag-compensated rewind of targets to the firer's viewed time).
- **TR-013**: Snapshots MUST be delta-encoded against each client's last-acknowledged snapshot, with quantized fields, sent at 15–20 Hz (below the 30 Hz tick).
- **TR-014**: System MUST measure and record baseline bytes/client/sec for a test session.
- **TR-015**: System MUST provide a headless bot harness driving two or more networked clients (no rendering) for integration tests of prediction, reconciliation, interpolation, and validation.
- **TR-016**: The shared `sim` MUST run identically on server and client; given identical inputs the server state and a client's prediction MUST agree within the reconciliation tolerance. The **reconciliation tolerance is environment-scoped** (TR-032): in the deterministic in-memory loopback harness it is **exact (bit-identical, epsilon = 0)**; across a live cross-machine run it is the convergence epsilon of TR-033/SC-002 (small f32 drift absorbed by re-seeding each correction).
- **TR-017**: Server-side hit resolution MUST rewind candidate targets to the firer's viewed time, defined as a bounded interval equal to the fixed interpolation delay (~100 ms, TR-010) plus that client's most recently measured round-trip latency; the rewindable interval MUST be capped at a maximum of 500 ms, and a fire whose viewed time falls outside the retained target-history window MUST be resolved against the oldest retained state rather than extrapolated. (Baseline lag-compensation; full WAN tuning deferred.)
- **TR-018**: Loopback mode MUST route client inputs through the identical server input-validation path (TR-011/TR-012) as a networked connection — loopback MUST NOT be an authority or validation bypass; the only permitted difference is the in-memory transport.
- **TR-019**: When a *validated, in-bounds* input would nonetheless drive the authoritative `sim` toward a physically impossible result (e.g., it contradicts a `sim` constraint such as a collision or a fixed kinematic limit), the authoritative outcome MUST be the `sim`-constrained result — the server applies the input through the `sim` and the `sim`'s own constraint resolution governs the result. This case is distinct from an out-of-bounds input (TR-011), which is clamped or rejected before reaching the `sim`.
- **TR-020**: For each `ClientInput` field the validation behavior MUST be defined per field as either *clamp* (silently bound to range, then apply) or *reject* (discard the whole input, apply nothing): the analog `forward`/`strafe`/`turn` fields are **clamped** to the quantized −1..=1 range; the `fire` boolean is **rate-limit gated** (TR-021); `toggle_assist` is accepted as-is (any boolean is in-bounds); and the `EntityKind` enum and any field that fails to decode to a known value cause the input to be **rejected** (TR-030). Every `ClientInput` field MUST have exactly one defined behavior.
- **TR-021**: The fire rate limit MUST be expressed as a measurable threshold: the server MUST reject (not apply) a `fire` intent that arrives before the firing entity's authoritative weapon cooldown (defined by the E002 `sim`) has elapsed for that entity. A test MAY assert this by submitting fire inputs faster than the `sim` cooldown and observing that excess fires produce no projectile in the authoritative state. (No new gameplay number is introduced; the bound is the existing `sim` cooldown.)
- **TR-022**: "Replayed" and "stale" inputs MUST be defined in terms of the per-client monotonic `seq` and the `tick` fields: an input whose `seq` is less than or equal to the client's last-processed `seq` is a **replay/duplicate** and MUST be discarded without mutating authoritative state; an input whose `tick` is older than the bounded acceptance window (the server's current tick minus the unacknowledged-input buffer bound, TR-027) is **stale** and MUST be discarded. Both cases MUST be discard-only, never partial-apply.
- **TR-023**: An **out-of-order** input (a `seq` greater than the last-processed `seq` but received after a higher `seq` was already processed — i.e., not stale and not a duplicate) MUST be applied if and only if its `seq` has not already been processed; an already-superseded `seq` MUST be treated as a replay (TR-022). Because inputs carry redundant recent history (TR-006), the server MUST process each `seq` at most once and MUST NOT reorder already-applied authoritative state.
- **TR-024**: The handshake protocol-version check MUST use **exact-match** comparison of `Connect.protocol_version` against the server's protocol version; a mismatch MUST yield `ConnectRejected{reason: version}` and MUST NOT establish a session. (No version-range negotiation in E003.)
- **TR-025**: The session MUST enforce a configurable maximum connected-client capacity with a baseline default of **8** clients; a `Connect` received while at capacity MUST yield `ConnectRejected{reason: full}` and MUST NOT allocate a session slot. (The E003 validation target remains two clients + bots; 8 is the baseline ceiling, not the AOI-scaled figure of E009.)
- **TR-026**: The `ConnectRejected{reason: banned}` outcome MUST close the connection without establishing a session and MUST be testable by a server configured to reject a given `client_token`/endpoint; the **source and lifecycle** of bans (issuance, persistence, expiry) are explicitly **deferred to a later epic** (E004 identity / a later hardening epic) — E003 only reserves the reason code and the reject-and-close behavior.
- **TR-027**: All client-growable buffers MUST be bounded to prevent memory exhaustion: the per-client **unacknowledged-input buffer** MUST be capped (baseline 64 inputs, ~2 s at 30 Hz) with the oldest dropped on overflow; the client **snapshot/interpolation buffer** MUST be capped (baseline 32 snapshots) with the oldest dropped; and the **redundant-input tail** length carried in each `ClientInput` MUST be a fixed small bound (baseline 8). Inputs or snapshots beyond these bounds MUST be dropped, never grow the buffer unboundedly.
- **TR-028**: The server MUST enforce a per-client **inbound packet/message rate limit** (baseline default: 4× the client send rate, i.e. ≤ ~120 messages/sec given 30 Hz input) independent of the fire rate limit (TR-021); a client exceeding it MUST be rate-limited (excess packets dropped, offender flagged per the plan Error Handling) so a single connected client cannot flood the server. (Baseline DoS bound; finer anti-flood heuristics deferred.)
- **TR-029**: Each encoded `Snapshot` MUST fit within a single transport datagram bounded by the path MTU (baseline ≤ 1200 bytes payload) without IP-level fragmentation; an encoder that would exceed the bound MUST split across snapshots or drop lowest-priority entities for that tick rather than emit an oversize datagram, and a received payload exceeding the bound MUST be treated as malformed (TR-030). (AOI-driven prioritization is E009; this is the baseline malformed-oversize guard.)
- **TR-030**: A malformed or undecodable inbound packet — a failed `bitcode` decode, an unknown/unsupported message type, a truncated payload, or a payload exceeding the size bound (TR-029) — MUST be dropped without mutating authoritative state and MUST be logged with offending-connection context (TR-031 logging); it MUST NOT be clamped (clamping applies only to decodable out-of-range analog fields, TR-020).
- **TR-031**: The server MUST log every rejected, invalid, or malformed input with sufficient context for later anti-cheat — at minimum the offending `ConnectionId`/`client_id`, the rejection reason category, and the server tick — while NOT logging exploitable internal detail (e.g., exact validation thresholds or full raw payloads at default log level). Client disconnect/timeout MUST use a defined idle timeout (baseline 10 s with no received packet) after which the server cleanly drops only that session, leaving remaining clients' sessions and authoritative state unaffected (no slot leak from a half-open connection).
- **TR-032**: Determinism verification MUST be performed in two explicitly distinct environments with distinct tolerances and ownership: (a) the **deterministic in-memory loopback harness** — the server `sim` and the client predicted `sim` are the *same compiled code* stepped with the same `FixedDt` (TR-016) on one host — where the guarantee is **bit-identical** (epsilon = 0) and is asserted automatically (TR-034, SC-007); and (b) a **live cross-machine run**, where exact f32 reproduction across hosts is NOT guaranteed and convergence is the bounded reconciliation behavior of TR-033/SC-002, verified by manual play-feel rather than a bit-identical assertion. The bit-identical primitive (a single deterministic `sim` advancing identically for identical inputs) is **owned and asserted by the E001 like-target determinism test and is re-exercised at the E003 split** by the loopback determinism test (TR-034) — E003 does not re-prove f32 reproducibility, it asserts that the *same* `sim` code path drives both server and client.
- **TR-033**: "Reconciles cleanly" / "smoothed (non-teleporting) correction" (TR-009, SC-002) MUST be expressed as two asserted bounds a test can check: a **convergence bound** — after a forced mismatch the client's predicted local-ship state MUST reach the authoritative state within the reconciliation epsilon `RECON_EPS` (position and velocity; see open decision OD-001) within a bounded **convergence window** of **≤ 5 snapshots (≈ 250–333 ms at 15–20 Hz)**, and MUST NOT oscillate (each successive correction's residual error MUST be non-increasing) — a non-converging or oscillating reconciliation is a test failure (SC-002 edge); and a **no-teleport bound** — no single applied correction may move the rendered local ship by more than `MAX_SNAP` in one tick (see OD-001), the correction being blended/smoothed across ticks rather than hard-snapped.
- **TR-034**: A determinism test MUST drive both the authoritative server `sim` and the client predicted `sim` from a **known, fixed initial state (seed) and an identical numbered `ClientInput` stream** (the same `seq`-ordered inputs fed to both), step both for a fixed number of ticks in the deterministic loopback harness, and assert the two resulting `sim` states are **bit-identical** (SC-007). The test inputs (seed, initial entity set, the numbered input stream, and tick count) MUST be fixed so the comparison is reproducible run-to-run.
- **TR-035**: The forced prediction mismatch used to verify reconciliation (SC-002, TR-009/033) MUST be produced by a **reproducible, deterministic injection** in the loopback harness — the baseline method is to step the authoritative `sim` with an input the client did not predict (an injected divergence on the local ship, e.g., a one-tick authoritative state override or a scripted inter-player ram resolved server-side) so the next snapshot disagrees with the client prediction by a known amount. The injection MUST be scripted (fixed seed/inputs) so the mismatch magnitude and the resulting convergence are repeatable run-to-run.
- **TR-036**: The interpolation loss/jitter test conditions MUST be quantified as fixed harness parameters: the in-memory loopback transport MUST be driven with a **baseline of 5% uniform-random single-packet snapshot loss and ±50 ms uniform jitter** applied to snapshot delivery, plus an explicit **consecutive-drop case** (a deterministically scripted burst of dropped snapshots). The snapshot send rate (15–20 Hz, baseline 20 Hz) and interpolation delay (~100 ms) are **fixed test parameters** so the buffer-window assertion is deterministic (TR-010/013). "Remote motion stays smooth" (SC-004) MUST be asserted as an **objective signal**: between consecutive rendered frames no interpolated remote entity may jump more than `MAX_INTERP_DELTA` of position within the interpolation window (see OD-001) — i.e., no teleport — for the single-drop + 5%/±50 ms case; the buffer (≈ 2 snapshots at 100 ms / 20 Hz) MUST ride out a **single** dropped snapshot with no visible jump, and the consecutive-drop case bounds how many drops (baseline: the buffer absorbs up to the buffered-snapshot count before a stall/extrapolation is acceptable) before a stall is an accepted outcome rather than a failure.
- **TR-037**: Late/duplicate/out-of-order *snapshots* (distinct from pure packet loss, TR-036, and from input replay, TR-022/023) MUST be handled by sequence: a snapshot whose `server_tick` is older than the newest already-applied snapshot MUST be **discarded as stale** and MUST NOT regress the interpolation buffer; a duplicate snapshot MUST be ignored. A test MUST drive deliberately reordered/duplicated snapshot delivery in the loopback harness and assert the interpolation buffer advances monotonically (no backward jump).
- **TR-038**: Each invalid-input class in SC-003 MUST have its **own enumerated rejection test case**, not a single blanket case: (1) out-of-bounds analog `forward`/`strafe`/`turn` → clamp (TR-011/020); (2) excessive `fire` rate → rate-limit gate (TR-021); (3) replayed/duplicate `seq` and stale `tick` → discard (TR-022); (4) client-asserted position/hit → ignored, only server-`sim`-derived state counts (TR-012/021). Each case MUST assert the rejection's observable signal of TR-039.
- **TR-039**: "Leaves the authoritative state unaffected" (SC-003) MUST be asserted as a **state-equality check**: the server `sim` state (the set of authoritative entity transforms/velocities/weapon-cooldown bookkeeping that the snapshot encodes) MUST be byte-for-byte identical immediately before and immediately after the rejected/ignored input is processed, except for the input-ack bookkeeping (the rejected `seq` may still be recorded as seen to enforce TR-022/023). A clamp case (TR-011) instead asserts the applied value equals the clamped bound, not the asserted out-of-range value.
- **TR-040**: Loopback behavioral equivalence (SC-008) MUST be defined as a **named set of paths that MUST match** the transport-backed connection: (a) authoritative-state delivery (the client receives the same authoritative entity state via `Snapshot`), (b) client-side prediction of the local ship (same `seq`-numbered input → predicted state path), (c) reconciliation (re-seed + replay), and (d) remote-entity interpolation (same snapshot-buffer path). Equivalence is asserted across these four paths. The behaviors **allowed to differ** are transport-only properties — added latency, packet loss, jitter, and per-datagram MTU framing/fragmentation — which loopback's in-memory channel does not exhibit; an equivalence test MUST therefore compare logical state/path behavior under matched (loss-free, zero-added-latency) conditions and MUST NOT fail on transport-only differences.
- **TR-041**: The renet UDP adapter (`RenetTransport`) MUST be exercised by a **separate integration test bound to the loopback network address** (e.g., `127.0.0.1`), distinct from the in-memory `NetTransport` used by the deterministic harness, so both transport implementations have stated coverage (the in-memory path for deterministic logic tests, the real-UDP path for the adapter wiring). {AD-004}
- **TR-042**: The bandwidth baseline (bytes/client/sec, SC-005, TR-014) MUST be measured against a **fixed, repeatable baseline session**: **2 networked bot clients + 4 server-controlled bot ships** (≈ 6 ships plus their projectiles), each bot driving a **fixed scripted input loop**, run for a **fixed 30 s window** at the baseline 20 Hz snapshot rate from a fixed initial world seed. The metric source MUST be `NetTransport::stats` (bytes out per connection), aggregated as **mean bytes/client/sec over the 30 s window** (also reporting peak per-second). The figure is **recorded-only** in E003 — there is **no pass/fail budget gate** (the per-client byte budget is E009); a run is valid as long as the figure is captured and the scenario parameters above are held fixed so two runs are comparable.
- **TR-043**: TR-015's headless bot harness MUST exercise an **enumerated, traceable scenario set**: (1) prediction responsiveness (SC-001), (2) forced-mismatch reconciliation convergence (SC-002, TR-033/035), (3) per-class invalid-input rejection (SC-003, TR-038/039), (4) smooth interpolation under the loss/jitter conditions of TR-036 (SC-004), (5) bytes/client/sec baseline over the TR-042 session (SC-005), and (6) client-disconnect-mid-session — a client drops and the server cleanly frees only that slot while remaining clients continue (TR-031, Edge Cases). The harness MUST drive **≥ 2 networked clients headlessly with no rendering**, so every "smooth"/"responsive" assertion uses a non-visual numeric signal (state deltas, sequence/ack bookkeeping, `stats`), never a visual judgment. Each P1 success criterion (SC-001..SC-004, SC-007, SC-008) MUST trace to at least one harness scenario or unit-test tier (SC-007 → TR-034 unit/loopback determinism test; SC-008 → TR-040 equivalence test), and the protocol round-trip (OBJ2 VC-1, encode/decode equality) MUST be covered by a **unit test independent of the bot harness**.
- **TR-044**: The three reference rates MUST be **normative single/bounded values that the server announces as session defaults**: authoritative tick = **30 Hz** (single value), snapshot send = **15–20 Hz** (bounded range, baseline default 20 Hz, TR-036/042), client interpolation delay = **100 ms** (single default; the "~" denotes the value MAY be tuned within ±1 snapshot interval for feel but is otherwise fixed, not free-form). These defaults are the values the server emits in `ConnectAccepted{tick_rate_hz, snapshot_rate_hz, interp_delay_ms}` (contracts/protocol.md): the contract's session parameters are **server-announced defaults the client adopts**, not client-proposed values — E003 performs **no rate negotiation** (the client aligns its fixed step to the announced rates; a client cannot request a different rate). The relationship **snapshot rate < tick rate** is an **invariant the implementation MUST hold**: a configuration in which the announced snapshot rate is ≥ the tick rate is a requirement violation (rejected/asserted at server start), not merely a note.
- **TR-045**: The delta-snapshot encoding MUST have these **assertable** properties, distinct from E009's quantization-budget tuning (TR-021, Compliance Check): (a) **delta baseline** — each snapshot is encoded against the client's last-acknowledged snapshot identified by `baseline_id`/`acked_input_seq` (TR-013, contracts/protocol.md); (b) **unchanged-entity cost** — an entity unchanged since the baseline MUST encode to a single presence/change bit (the "~1 bit" expectation is a **checkable encoding property**: a unit test encoding a snapshot in which N of M entities are unchanged MUST observe the unchanged entities contribute ≤ 1 bit each, not an aspiration); (c) **deterministic field widths** — the quantized field bit-widths and value ranges (`QVec2` position/velocity to sector-relative bounds, `QAngle` heading) MUST be **fixed at implementation in the `protocol::quantize` module so encoded size is deterministic per build** — the *concrete* widths/ranges are an implementation-pinned constant set, recorded with the baseline (TR-046), with precision/budget *tuning* deferred to E009; (d) **lost-ack graceful degradation** — if a client's `SnapshotAck` is lost, the server MUST continue delta-encoding against that client's **last acknowledged** `baseline_id` (never an unacked one) and, when no baseline is yet acknowledged, MUST send a **full keyframe** (delta-from-nothing); snapshot growth from stale baselines is bounded by the MTU guard (TR-029) so a lost ack degrades gracefully (re-baseline) without unbounded growth.
- **TR-046**: The bandwidth baseline figure (SC-005, TR-014/042) MUST be **fully specified for reproducibility**: (a) **transport measured over** — the baseline is measured over the **renet UDP adapter path** (the networked figure E009 optimizes against), and the in-memory loopback figure MAY also be recorded for comparison but is explicitly **lower** because loopback exhibits no per-datagram framing (TR-040); the two MUST NOT be conflated; (b) **framing inclusion** — the recorded figure is the application-payload bytes/connection reported by `NetTransport::stats` (the encoded `protocol` message bytes), **excluding** lower-level UDP/IP and renet transport headers/acks (the figure is the replication payload E009 filters, not raw wire bytes); if transport-level overhead is also captured it MUST be reported as a **separate** line, not folded into the payload figure; (c) **recording/output** — the figure (mean + peak bytes/client/sec, per direction = **out**) MUST be **emitted by the baseline test as structured output** (test log line / recorded artifact under the harness's output) so SC-005 is asserted-as-satisfied by the presence of the recorded figure, not left implicit.
- **TR-047**: Per-client **snapshot encode cost** (CPU) MUST be treated as an **observed, recorded-only** replication-hot-path attribute at the baseline scale (project Testing Policy: benchmarks on replication hot paths): because each client deltas against its own last-acked baseline, encode cost is inherently per-client. E003 MUST make encode cost **observable** — the snapshot encoder is structured as a benchmarkable unit (a `cargo bench`/timed path over the TR-042 baseline world) and its per-client cost at baseline scale (≈ 6 ships + projectiles, ≤ 8 clients) is **recorded alongside the bandwidth baseline** (TR-046) — but, exactly like bytes/client/sec, there is **no per-client encode-cost budget/pass-fail gate in E003**: cost scaling is **in-scope to observe, out-of-scope to optimize** (optimization, like AOI and the byte budget, is E009). Absence of a budget is by design and stated, not implied.

- **TR-048**: Sessions MUST be established over an **authenticated + encrypted** transport (renet_netcode secure mode, connect-token connections); an unauthenticated/unsecure connect attempt MUST be **rejected**. The connect-token **issuer** is a stub (local signing key) in E003, isolated so E004 replaces it with account-backed issuance without touching the secure-connection path; netcode-library specifics stay confined to the `renet_adapter` (TR-005). (netcode.io secure mode is authenticated *and* encrypted together — there is no auth-without-encryption split.)

### Key Entities *(include for product or technical specs if feature involves data)*

- **ClientInput**: a client→server message carrying a monotonic sequence number and the per-tick pilot intents (the networked form of E002's `ShipIntent`: forward/strafe/turn/fire/assist-toggle). Buffered client-side until acknowledged.
- **Snapshot**: a server→client message carrying authoritative entity state, delta-encoded vs the client's last acknowledged snapshot and quantized, plus that client's last-processed input sequence (the reconciliation anchor).
- **Protocol message**: the union of wire messages — connect/handshake, `ClientInput`, `Snapshot`, disconnect — defined in the `protocol` crate, library-agnostic.
- **Session**: a server-side record of the connected clients sharing one authoritative world; tracks per-client connection state and input-ack/sequence bookkeeping.

## Assumptions & Risks *(mandatory)*

### Assumptions

- The E001 `sim` is deterministic under identical inputs and its components are serde-derivable (verified in E001/E002), making replay-based reconciliation and snapshot encoding viable.
- The E002 client is a thin render/input shell (ADR-0013) that can be refactored to send inputs and render from prediction + interpolated snapshots without reworking gameplay.
- The netcode library named in ADR-0007 (or its fallback) provides a usable UDP transport behind the adapter.
- A two-client + bots session over local/LAN latency is a sufficient baseline to validate the architecture; AOI and WAN tuning are deferred.

### Risks

- **Netcode-library maturity / API churn** *(likelihood: medium, impact: medium)*: a young, fast-moving dependency. Mitigation: isolate behind the `protocol` adapter, keep a fallback library, pin the version.
- **Cross-machine determinism** *(likelihood: medium, impact: high)*: f32 results may differ across hosts, breaking exact replay. Mitigation: one shared fixed-step `sim`, re-seed from the authoritative snapshot each correction (tolerate small drift), lean on the E001 bit-identical determinism test on like targets.
- **Inter-player interaction misprediction** *(likelihood: high, impact: medium)*: a client cannot predict another ship's actions (e.g., a ram), so corrections will occur. Mitigation: predict only the local ship, resolve all interactions server-side, smooth corrections — accept visible-but-graceful reconciliation (a known hard truth in the SAD).

## Implementation Signals *(mandatory)*

- `NEW-API` — The `protocol` wire message set (`ClientInput`, `Snapshot`, handshake) — the client/server contract, later consumed by E009.
- `NEW-WORKER` — The authoritative server tick loop + session manager (per-client connection, input-ack, snapshot send).
- `NEW-CONFIG` — New `server` and `protocol` crates added to the Cargo workspace; the netcode library + UDP transport added to `[workspace.dependencies]`, confined to the adapter.
- `BREAKING-CHANGE` — The E002 client changes from a local-only sim to a networked client (sends `ClientInput`, renders from prediction + interpolated snapshots); loopback preserves the single-player experience.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [OBJ1, OBJ3, OBJ4]: Two clients plus bots share one authoritative session — each client's own ship is predicted (responds immediately) and all remote entities are interpolated (smooth).
- **SC-002** [OBJ3]: A forced prediction mismatch (deterministically injected, TR-035) reconciles cleanly — the client converges to the authoritative state within the reconciliation epsilon (OD-001) within ≤ 5 snapshots without oscillating (residual error non-increasing), and no single applied correction moves the rendered local ship by more than `MAX_SNAP` in one tick (no visible teleport/snap) (TR-033).
- **SC-003** [OBJ5]: Each invalid-input class — out-of-bounds thrust/turn, excessive fire rate, replayed/stale sequence, asserted position/hit — is rejected in its own enumerated test (TR-038) and leaves the authoritative `sim` state byte-for-byte unaffected aside from input-ack bookkeeping (TR-039).
- **SC-004** [OBJ4]: Remote-entity motion stays smooth across the fixed simulated loss/jitter conditions (baseline 5% single-packet loss + ±50 ms jitter, plus a scripted consecutive-drop case, TR-036) — no interpolated remote jumps more than `MAX_INTERP_DELTA` between rendered frames for the single-drop case, the ~100 ms buffer riding out a single dropped snapshot; the consecutive-drop case bounds how many drops are absorbed before a stall/extrapolation is an accepted outcome.
- **SC-005** [OBJ6]: Baseline bytes/client/sec is measured and recorded over the fixed, repeatable baseline session (2 bot clients + 4 bot ships, fixed scripted inputs, 30 s window at 20 Hz, fixed seed; mean + peak via `NetTransport::stats`, TR-042) — recorded-only, no pass/fail budget gate in E003 (the figure E009 will optimize against).
- **SC-006** [OBJ2]: The netcode library is isolated — swapping the transport adapter requires no change to `sim`/gameplay code, and the protocol-consumer surface names no library type.
- **SC-007** [OBJ1]: The same shared `sim` runs on server and client; in the deterministic in-memory loopback harness, server state and client prediction driven from a fixed seed + identical numbered input stream are **bit-identical** (epsilon = 0) after a fixed tick count (TR-032/034) — an observable state-divergence assertion, not absence of visible desync. (Live cross-machine reproduction is not bit-asserted; it is the bounded reconciliation of SC-002.)
- **SC-008** [OBJ1]: Loopback mode is behaviorally equivalent to a networked connection across four named paths — authoritative-state delivery, local-ship prediction, reconciliation, and remote interpolation (TR-040); the equivalence test compares logical path behavior under matched loss-free/zero-added-latency conditions and does not fail on transport-only differences (latency/loss/jitter/MTU framing).
- **SC-009** [OBJ5]: An input submitted over loopback exercises the identical validation path as one submitted over the transport — an out-of-bounds or excessive-rate input is clamped/rejected in loopback exactly as over UDP, demonstrating loopback is not an authority bypass (TR-018).
- **SC-010** [OBJ5]: A malformed/undecodable packet, a replayed/stale/out-of-order input, and an input that exceeds the per-client buffer or packet-rate bound are each discarded with the authoritative state unchanged and the offending connection logged, with no unbounded buffer growth (TR-022/023/027/028/030/031).
- **SC-011** [OBJ2, OBJ6]: Each emitted `Snapshot` fits within the MTU payload bound with no IP fragmentation, and a connected client at the session-capacity ceiling receives `ConnectRejected{reason: full}` rather than a leaked slot (TR-025/029).
- **SC-012** [OBJ4, OBJ6]: Late/duplicate/out-of-order *snapshots* are discarded by sequence so the interpolation buffer advances monotonically (TR-037), and the headless bot harness exercises the full enumerated scenario set (prediction, forced-mismatch reconciliation, per-class invalid-input rejection, loss/jitter interpolation, bytes/client/sec baseline, client-disconnect-mid-session) with ≥ 2 clients and no rendering, with every P1 criterion (SC-001..SC-004, SC-007, SC-008) and the protocol round-trip (OBJ2 VC-1) tracing to a named test tier (TR-043).

- **SC-013** [OBJ2, OBJ5]: Sessions are authenticated + encrypted (secure transport) — an unauthenticated/unsecure connect attempt is rejected, and an established session's channel is secure; the connect-token issuer is a swappable stub this epic (TR-048).

### Open Decisions *(product-judgment values pending confirmation)*

- **OD-001** — Concrete reconciliation/interpolation feel constants: the reconciliation epsilon `RECON_EPS` (max position/velocity error at which predicted state counts as "converged" to authoritative), the per-tick no-teleport cap `MAX_SNAP` (max world-units the smoothed correction may move the rendered local ship in one tick), and the interpolation no-jump cap `MAX_INTERP_DELTA` (TR-033/036, SC-002/004). These are play-feel product calls, not derivable from the artifacts. **Recommended baseline defaults** (apply if confirmed): `RECON_EPS` = 0.05 m position / 0.05 m·s⁻¹ velocity; `MAX_SNAP` ≈ blend the residual over ≤ 5 ticks with no single tick exceeding 25% of the residual (i.e., no instantaneous full-magnitude snap); `MAX_INTERP_DELTA` = the distance the fastest entity travels in one interpolation step at the 20 Hz send rate (one-frame motion bound), exceeding which is a teleport. The structural bounds (≤ 5-snapshot convergence window, non-increasing residual, single-drop ride-out) are fixed in TR-033/036 regardless of the chosen constants. **Accepted as provisional (2026-06-02):** these defaults stand for E003; tune in networked playtest (like the flight `Tuning` values).

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| Authoritative server | The headless process that owns the single source of truth for world state; clients predict against it and cannot override it. |
| Prediction | The client running the shared `sim` locally on its own inputs so its ship responds immediately, before server confirmation. |
| Reconciliation | Re-seeding the local ship to the latest authoritative snapshot and replaying unacknowledged inputs to correct mispredictions. |
| Interpolation delay | A fixed lag (~100 ms) at which remote entities are rendered, so motion can be interpolated smoothly between buffered snapshots. |
| Snapshot | A server→client message of authoritative entity state, delta-encoded vs the client's last ack and quantized, with the client's last-processed input sequence. |
| ClientInput | A numbered client→server message of per-tick pilot intents; the networked form of E002's `ShipIntent`. |
| Loopback mode | Server + client running in one process (no real network) for solo play, dev, and headless tests. |
| Session | The set of clients connected to one authoritative server world. |

## Compliance Check

**Status**: PASS — no `project-instructions.md` violations (0 CRITICAL); 7/7 Core Principles satisfied (III N/A — tiering/dilation deferred to E008/E009).

This epic **realizes Principle I (Server-Authoritative Simulation)**: the server is the single source of truth; it accepts inputs only (clamped/rate-limited), treats client positions/hits as non-authoritative, and resolves hits server-side (OBJ1/OBJ5, TR-001/011/012). **II (Shared Deterministic Sim Core)** — the same E001 `sim` runs on both ends; reconciliation depends on it (TR-001/007/009/016, SC-007). **V (Build the Seams)** — single-node (ADR-0005); netcode isolated behind the `protocol` adapter, swappable (TR-005, SC-006). **VI (Bandwidth Is the Budget)** is **staged**: E003 measures baseline bytes/client/sec with delta + quantized snapshots (TR-013/014), while AOI-filtering, the per-client budget, and priority are deferred to E009 (ADR-0006) — consistent with the project plan, not a violation. **VII** — loopback preserves runnable solo play. Technology Stack (Rust + UDP + netcode behind `protocol` per ADR-0007), Testing Policy (headless-bot harness, clippy `-D warnings`, rustfmt), and Source Layout (new `server` + `protocol` crates) all aligned.

**Advisory (LOW, non-blocking)**: the body also relies on ADR-0005 / ADR-0006 / ADR-0013 beyond the frontmatter's canonical `{ADR-0002}{ADR-0007}` — fold these into plan traceability. TR-013 "quantized fields" is the baseline encoding (the figure E009 optimizes against), distinct from E009's deferred quantization-budget.
