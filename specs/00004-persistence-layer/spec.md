---
feature_branch: "00004-persistence-layer"
created: "2026-06-04"
input: "E004 — durable + hot persistence with accounts (PostgreSQL + Redis, sqlx repositories, migrations, account auth)"
spec_type: "technical"
spec_maturity: "draft"
epic_id: "E004"
epic_sources: "{SAD:ADR-0007}"
---

# Feature Specification: Persistence Layer

**Feature Branch**: `00004-persistence-layer`  
**Created**: 2026-06-04  
**Status**: Draft  
**Spec Type**: technical  
**Spec Maturity**: draft  
**Epic ID**: E004  
**Epic Sources**: {SAD:ADR-0007}  
**Product Document**: specs/prd.md

## Problem Statement *(mandatory)*

The authoritative server holds all game state in memory and loses it on restart or crash; there is no account system, and the E003 secure-connection path issues connect tokens from a stub issuer with no real identity behind them. Without durable persistence, hot caching, and accounts, there is no shared persistent universe (CAP-002): players cannot log in, the world cannot survive a restart, and the shared-world epic (E005) is blocked. This epic establishes the durable + hot persistence substrate, account storage and authentication, and account-backed connect-token issuance so state survives, identity is real, and connections are authenticated — without perturbing the deterministic authoritative tick.

## Scope *(mandatory)*

### Included

- A new `crates/persistence` crate: repository abstractions over PostgreSQL (durable source of truth) + Redis (hot/ephemeral cache), with managed connection pooling and transactions.
- Versioned schema via `sqlx` migrations (apply to a fresh database, support rollback) and a versioned persisted-entity format with backup-before-migrate discipline.
- Serialize → store → load round-trip for the shared `serde`-derived `sim`/domain entities.
- Account storage + credential authentication (salted Argon2id hashes), account create + authenticate.
- Account-backed connect-token issuance that replaces E003's `StubTokenIssuer` behind the existing `TokenIssuer` trait (verify credentials → mint a `renet_netcode` `ConnectToken`).
- A determinism-preserving async IO bridge: an off-thread async runtime reached by non-blocking channels so persistence IO never blocks or perturbs the fixed-step tick.
- Graceful degradation when PostgreSQL is unavailable (serve reads from Redis, queue durable writes, refuse risky state-mutating actions).

### Excluded

- The shared-world login/session lifecycle, logout/in cross-session persistence, and spawn/respawn flow — owned by E005 (this epic provides the substrate, not the world flow).
- Domain-specific schemas for economy, inventory, territory, reputation, and research — owned by E013/E012/E017/E016 (they build on this epic's repository + migration substrate, which does not define their tables).
- Tier-1 long-range / "message in a bottle" trajectory persistence — lands with its owning epic (E015/transit); the `persistence` crate is its future home, not part of this epic.
- Interest management, tiering, and time dilation — E008/E009.
- Multi-node sharding, read replicas, and cross-region storage — later (ADR-0005).
- Operational provisioning of PostgreSQL/Redis and TLS termination for token delivery — deployment/ops concern integrated at E005; this epic defines the issuer and the connection contract, not the hosting.

### Edge Cases & Boundaries

- PostgreSQL unavailable at startup vs. mid-session; Redis unavailable (cache-miss path must still serve from PostgreSQL).
- Migration failure or partial apply; rollback; schema/format-version mismatch on load.
- Duplicate account creation; authentication against a non-existent account or wrong password; a credential must never appear in logs or error payloads.
- Async durable-write-queue backpressure and bounded-queue overflow.
- A persistence stall (slow query, lock, disconnect) MUST NOT delay or perturb the deterministic fixed-step tick (the E001/E003 bit-identical invariant).

## Technical Objectives *(mandatory for technical specs only)*

### Objective 1 - Durable + hot persistence substrate (Priority: P1)

The `crates/persistence` crate providing repository abstractions over PostgreSQL (durable) and Redis (hot), with `sqlx` connection pooling, transactions, versioned migrations, a versioned persisted format, and a serialize→store→load round-trip for `serde`-derived domain entities, plus a basic Redis hot-path read/write.

**Why this priority**: Foundation for all durable state — accounts, the shared world, and every later durable system build on it; nothing persists without it.

**Rationale**: Server state is in-memory-only and lost on restart; the SAD/ADR-0007 require a durable PostgreSQL truth plus a Redis hot layer, decoupled from the authority model, in a dedicated `persistence` crate.

**Deliverables**:
- `crates/persistence` crate with a repository trait + at least one concrete repository.
- Managed `PgPool` + Redis client/connection with configurable endpoints.
- `sqlx` migration set + the embedded `migrate!` apply path; backup-before-migrate convention.
- Versioned persisted-entity format + serialize→store→load for serde-derived `sim` entities.
- Transaction support for multi-step durable mutations.

**Validation Criteria**:
1. **Given** a fresh database and the migration set, **When** migrations are applied then rolled back, **Then** the schema round-trips cleanly with no residue.
2. **Given** a serde-derived domain entity, **When** it is stored to PostgreSQL and reloaded, **Then** the loaded value equals the original under the versioned format.
3. **Given** a multi-step durable repository operation, **When** a step fails mid-operation, **Then** the enclosing transaction rolls back atomically (no partial write).

### Objective 2 - Accounts + credential authentication (Priority: P1)

Account records with create + authenticate operations, credentials stored as salted Argon2id PHC hashes, never plaintext.

**Why this priority**: Identity is the precondition for a shared persistent world and the real token issuer; credential handling is security-critical.

**Rationale**: No account system exists; secure credential storage and verification are required before authenticated connections (OBJ3) and before E005's login flow.

**Deliverables**:
- An `Account` entity + repository (durable).
- Account creation (rejecting duplicates) and authentication (password-vs-hash verification).
- Argon2id hashing with per-account salt (PHC string); credential-handling that never logs or returns secrets.

**Validation Criteria**:
1. **Given** a newly created account, **When** authenticated with the correct password, **Then** authentication succeeds; with the wrong password it fails.
2. **Given** the account store and logs, **When** inspected, **Then** no plaintext or reversibly-encoded credential is present anywhere.
3. **Given** an existing username, **When** a second account is created with the same username, **Then** creation is rejected.

### Objective 3 - Account-backed connect-token issuance (Priority: P1)

An account-backed `TokenIssuer` implementation that mints a `renet_netcode` connect token only after verified authentication, replacing E003's `StubTokenIssuer` behind the unchanged `TokenIssuer` trait and secure-connection path.

**Why this priority**: Closes the E003 deferral; authenticated connections are a precondition for the shared world (E005) and a security requirement (no unauthenticated connect).

**Rationale**: E003 shipped a stub issuer with a swap seam; real accounts must now gate who can obtain a connect token.

**Deliverables**:
- An account-backed `TokenIssuer` behind E003's existing trait.
- A verify-credentials-then-mint flow producing a `ConnectToken` keyed to the authenticated account's id (the connect-token client id).
- The E003 `secure_server`/`secure_client` handshake left unchanged (drop-in replacement).

**Validation Criteria**:
1. **Given** valid credentials, **When** a connect token is requested, **Then** a token bound to the account's id is issued.
2. **Given** invalid credentials, **When** a connect token is requested, **Then** NO token is issued.
3. **Given** the E003 secure handshake, **When** fed an account-backed token, **Then** the client connects exactly as it did with the stub issuer.

### Objective 4 - Determinism-preserving async IO bridge (Priority: P1)

An off-thread async runtime/actor reached by non-blocking channels so all persistence IO runs off the authoritative tick thread, keeping the fixed-step tick bit-identical and unstalled.

**Why this priority**: Server determinism (bit-identical fixed-step + bot-harness equivalence) is a hard invariant from E001/E003; introducing the server's first async/IO must not break it.

**Rationale**: The headless server is a synchronous 30 Hz deterministic loop with no async runtime; database IO is inherently async/blocking and must be isolated so a slow query or disconnect cannot delay or perturb a tick.

**Deliverables**:
- An off-thread async runtime (the server's first) owning the connection pool, reached from the tick via command/result channels (non-blocking send + try-receive).
- Load/save at session boundaries; mid-session durable writes go to a bounded async write queue.
- The `sim` crate kept free of any database or async-runtime dependency.

**Validation Criteria**:
1. **Given** the persistence layer active, **When** the determinism and bot-harness/botkit equivalence suites run, **Then** they stay bit-identical.
2. **Given** an induced persistence IO stall (slow/blocked query), **When** the server ticks, **Then** the fixed-step cadence is not delayed and no IO runs inside a `sim` fixed-step system.
3. **Given** a saturated durable-write queue, **When** the bound is reached, **Then** the queue applies backpressure rather than blocking the tick or growing unbounded.

### Objective 5 - Graceful degradation under storage failure (Priority: P2)

Cache-aside Redis hot-layer consistency plus a PostgreSQL-unavailable degradation mode: serve reads from Redis, queue durable writes for replay, and refuse risky state-mutating actions until storage recovers.

**Why this priority**: Significant resilience value, but the durable persistence path (OBJ1–4) functions correctly without it; degradation hardens operation rather than enabling the MVP.

**Rationale**: A single-host PostgreSQL is a real availability bound (SAD); the documented degradation rule keeps the server serving safely through a database outage.

**Deliverables**:
- A cache-aside read/write pattern over Redis with TTLs (read Redis → miss → load PostgreSQL → backfill; write → update PostgreSQL → invalidate).
- A PostgreSQL-down mode: serve cached reads, enqueue durable writes for later replay, and refuse risky state-mutating actions.

**Validation Criteria**:
1. **Given** PostgreSQL induced-unavailable, **When** a read is requested, **Then** it is served from the Redis hot layer where the key is present.
2. **Given** PostgreSQL induced-unavailable, **When** a durable write is attempted, **Then** it is queued for replay and a risky state-mutating action is refused until recovery.
3. **Given** PostgreSQL recovers, **When** the queued writes are drained, **Then** durable state converges with no lost or duplicated writes.

### Technical Constraints

- **Determinism**: persistence IO MUST NOT run inside `sim` fixed-step systems nor block the authoritative tick; server determinism + bot-harness/botkit equivalence MUST stay bit-identical (E001/E003 invariant, ADR-0003).
- The `sim` crate stays IO-free with no database or async-runtime dependency; the async-runtime feature MUST NOT leak into `sim`.
- PostgreSQL is the durable source of truth; Redis is a cache only, never the sole truth.
- Versioned persisted formats; `sqlx` migrations; backup-before-migrate (SAD/ADR-0007).
- Credentials: salted Argon2id PHC hashes only; never plaintext, never logged. Store minimal personal data (handle + hash) consistent with the PRD 13+/data-minimization posture.
- Compile-time-checked `sqlx` queries with checked-in offline query metadata so CI builds and tests without a live database.
- The connect-token private key is handled securely; tokens are issued only after authentication and delivered over TLS.
- Solo-operable: single-host PostgreSQL + Redis for the modest-scale phase (SAD/PRD).

## Integration Points *(mandatory for technical and operational specs)*

- **IP-001**: `crates/persistence` depends on `crates/sim` domain types (the `serde`-derived entities from E001/E006/E007) for serialize→store→load; it MUST NOT depend on Bevy, rendering, or the netcode transport.
- **IP-002**: The account-backed `TokenIssuer` plugs into E003's existing `TokenIssuer` trait and `secure_server`/`secure_client` path (`renet_adapter`, `udp` feature), drop-in replacing `StubTokenIssuer`.
- **IP-003**: The headless `server` consumes `persistence` through the off-thread async bridge (command/result channels) — load/save at session boundaries, write-behind mid-session, never on the tick thread.
- **IP-004**: E005 (Shared persistent world) depends on this layer's accounts, repositories, and migrations for login, cross-session persistence, and spawn/respawn; E013/E016/E017 depend on it for durable economy/research/reputation state.
- **IP-005**: PostgreSQL and Redis are external services the server connects to via configuration; operational provisioning and backup are owned by deployment/ops.

## Requirements *(mandatory)*

### Technical Requirements *(technical specs only)*

- **TR-001**: System MUST provide a `crates/persistence` crate exposing repository abstractions over PostgreSQL (durable) and Redis (hot), with managed connection pooling.
- **TR-002**: System MUST manage schema via versioned `sqlx` migrations that apply to a fresh database and support rollback.
- **TR-003**: System MUST round-trip `serde`-derived domain entities (serialize → store in PostgreSQL → load → deserialize) to an equal value under a versioned persisted format.
- **TR-004**: System MUST persist account credentials as salted Argon2id PHC hashes and MUST NOT store, log, or return plaintext or reversibly-encoded credentials.
- **TR-005**: System MUST support account creation that rejects duplicates and authentication that verifies a password against the stored hash.
- **TR-006**: System MUST issue a `renet_netcode` connect token bound to an authenticated account behind E003's `TokenIssuer` trait, and MUST NOT issue a token without successful authentication.
- **TR-007**: System MUST leave the E003 secure-connection handshake unchanged so an account-backed token connects a client identically to the stub issuer.
- **TR-008**: System MUST perform all persistence IO off the authoritative tick thread (an async runtime on a separate thread reached via non-blocking channels) so a persistence stall never blocks or delays the fixed-step tick.
- **TR-009**: System MUST keep the `sim` crate free of any database or async-runtime dependency, and MUST NOT run persistence IO inside `sim` fixed-step systems.
- **TR-010**: System MUST preserve server determinism — the determinism and bot-harness/botkit equivalence tests MUST stay bit-identical with the persistence layer present.
- **TR-011**: System MUST provide Redis hot-path read/write for ephemeral state (e.g., presence/cache) via a cache-aside pattern with TTLs, where a Redis miss falls back to PostgreSQL.
- **TR-012**: System MUST wrap multi-step durable mutations in transactions so a mid-operation failure rolls back atomically.
- **TR-013**: When PostgreSQL is unavailable, the system MUST serve reads from the Redis hot layer where possible, queue durable writes for bounded later replay, and refuse risky state-mutating actions until storage recovers.
- **TR-014**: System MUST build and test in CI without a live database (checked-in `sqlx` offline query metadata) and MUST provide an ephemeral-database integration-test path (disposable PostgreSQL + Redis) covering repository, migration round-trip, and entity round-trip tests.
- **TR-015**: System MUST expose PostgreSQL and Redis connection configuration via configuration/environment rather than hardcoded values.

### Key Entities *(include for product or technical specs if feature involves data)*

- **Account**: a player's durable identity — a unique handle, a salted Argon2id credential hash, and a stable account id used as the connect-token client id; stores no plaintext credential and minimal personal data.
- **Repository**: an abstraction over durable (PostgreSQL) and hot (Redis) storage for a domain entity type, offering create/read/update/delete and the serialize→store→load round-trip.
- **Migration set**: the ordered, versioned schema-change scripts applied (and rolled back) via `sqlx`.
- **Persisted entity format**: the versioned serialized representation of a `serde`-derived `sim`/domain entity stored durably; version-checked on load.
- **Account-backed connect token**: a `renet_netcode` token minted for an authenticated account, keyed to its account id, issued behind E003's `TokenIssuer` seam.

## Assumptions & Risks *(mandatory)*

### Assumptions

- PostgreSQL and Redis are available as external services the server can reach (single-host for the modest-scale phase).
- The `serde`-derived `sim`/fitting/damage domain types are the entities to persist; their serialized form is the basis of the versioned persisted format.
- E003's `TokenIssuer` trait + secure-connect path are the integration seam, with the stub swappable behind it.
- The server may run async IO on additional OS threads without violating the deterministic single-threaded fixed step (the tick stays on its own thread; IO runs elsewhere).
- The 13+ / data-minimization posture limits stored personal data to a handle + credential hash, keeping account-data compliance light.

### Risks

- **Async IO coupling to the deterministic tick** *(likelihood: medium, impact: high)*: the server's first async runtime could accidentally block or perturb the fixed-step tick — mitigate via the off-thread actor + non-blocking channels and the determinism regression tests (TR-008/TR-010).
- **Silent data corruption on format/schema evolution** *(likelihood: medium, impact: high)*: a version mismatch could corrupt loaded state — mitigate via versioned formats, migration apply/rollback round-trip tests, and backup-before-migrate.
- **Credential-handling mistake** *(likelihood: low, impact: high)*: plaintext leak or weak hashing would be a security failure — mitigate via Argon2id, no-plaintext/no-log tests, and review.

## Implementation Signals *(mandatory)*

- **NEW-ENTITY** — `Account`, plus the repository and persisted-entity abstractions.
- **MIGRATION** — the initial `sqlx` migration set and the versioned-format apply/rollback workflow.
- **EXTERNAL-SERVICE** — PostgreSQL and Redis as external services the server connects to.
- **NEW-WORKER** — the off-thread async persistence runtime/actor (the server's first async/IO path).
- **NEW-CONFIG** — PostgreSQL + Redis connection configuration and connect-token private-key handling.
- **BREAKING-CHANGE** — replacing the E003 `StubTokenIssuer` with an account-backed issuer on the secure-connect path (connecting now requires a real account).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** [OBJ1]: Migrations apply to a fresh database and roll back cleanly, and a serde-derived domain entity stored to PostgreSQL and reloaded equals the original under the versioned format.
- **SC-002** [OBJ1]: A Redis hot-path read/write succeeds and a cache miss transparently falls back to PostgreSQL.
- **SC-003** [OBJ2]: A created account authenticates with the correct password and is rejected with the wrong one, and no plaintext or reversible credential exists in the store or logs.
- **SC-004** [OBJ3]: A connect token is issued only after successful authentication and is bound to the account id; an unauthenticated request yields no token; an account-backed token completes the E003 secure handshake identically to the stub.
- **SC-005** [OBJ4]: With the persistence layer active and an induced IO stall, the determinism + bot-harness/botkit equivalence tests stay bit-identical and the fixed-step tick is not delayed.
- **SC-006** [OBJ5]: With PostgreSQL induced-unavailable, reads are served from Redis where possible, durable writes are queued for replay, and risky state-mutating actions are refused until recovery, with no lost or duplicated writes on drain.

## Glossary *(include when spec introduces 2+ domain-specific terms)*

| Term | Definition |
|------|------------|
| Durable store | PostgreSQL — the authoritative, persistent source of truth for accounts and durable game state. |
| Hot layer | Redis — ephemeral, fast-access cache for presence and transient state; never the sole source of truth. |
| Repository | A storage abstraction for a domain entity over the durable + hot stores, hiding SQL/cache detail. |
| Migration | A versioned, ordered schema-change script applied (and rollback-able) via `sqlx`. |
| Persisted entity format | The versioned serialized representation of a domain entity stored durably; version-checked on load. |
| Connect token | A `renet_netcode` credential a client presents to establish a secure session; here bound to an authenticated account. |
| Cache-aside | A caching pattern: read the cache, on miss load durable storage and backfill; on write, update durable storage then invalidate the cache. |
| Async IO bridge | An off-thread async runtime reached by non-blocking channels so database IO never blocks the deterministic tick. |

## Compliance Check

**Result: PASS** — no blocking violations against `project-instructions.md`, `AGENTS.md`, `specs/prd.md`, `specs/sad.md`, ADR-0007, or ADR-0003.

Validated against the deterministic-`sim` invariant (ADR-0003 / PI II), `sim` IO-freedom (ADR-0007 crate boundaries), credential security + Argon2id (SAD Security), data-minimization / 13+ (PRD Constraints), never-pay-to-win (PRD), anti-cheat / anti-RMT (SAD Security), versioned-format + backup-before-migrate + PG-down degradation (ADR-0007 / SAD), and solo-sustainable single-host footprint (PRD / SAD).

### Evidence Summary

| Governance rule | Source | Verdict | Evidence in spec |
|-----------------|--------|---------|------------------|
| Determinism preserved — no IO in `sim` fixed-step; bit-identical tick | PI II, ADR-0003, SAD | PASS | OBJ4, TR-008/TR-009/TR-010, Constraint "Determinism", Edge "persistence stall", SC-005 |
| `sim` stays IO-free (no DB / async-runtime dep) | ADR-0007, PI Source Layout | PASS | TR-009, Constraint "`sim` crate stays IO-free", OBJ4 deliverable, IP-001 |
| Credential security — Argon2id PHC, never plaintext / never logged | SAD Security, PRD | PASS | OBJ2, TR-004, Constraint "Credentials", Key Entity "Account", Risk "Credential-handling" |
| Data-minimization — handle + hash only (13+ posture) | PRD Constraints, SAD Security | PASS | TR-004, Constraint "Credentials", Assumption "13+/data-minimization", Key Entity "Account" |
| Crate-boundary cleanliness (`persistence` ≠ Bevy/render/transport) | ADR-0007 Option H, PI Source Layout | PASS | IP-001, IP-002, Scope (new `crates/persistence`) |
| PostgreSQL durable truth; Redis cache only | ADR-0007 Option F, SAD Data Mgmt | PASS | TR-011, Constraint "PostgreSQL is the durable source of truth", Glossary |
| Versioned formats + `sqlx` migrations + backup-before-migrate | ADR-0007, SAD Data Mgmt | PASS | TR-002/TR-003, OBJ1, Constraint, Risk "Silent data corruption" |
| PG-down graceful degradation | SAD Failure Paths / Reliability | PASS | OBJ5, TR-013, SC-006 |
| Drop-in E003 `TokenIssuer` seam unchanged (verified in code) | ADR-0007/ADR-0014, E003 | PASS | OBJ3, TR-006/TR-007, IP-002 — matches `crates/protocol/src/renet_adapter.rs` (`TokenIssuer::issue(client_id, server_addr) -> Result<ConnectToken, NetcodeError>`, `StubTokenIssuer`, unchanged `secure_client`/`secure_server`) |
| Connect-token private key handled securely; TLS delivery; auth-gated | SAD Security | PASS | TR-006, Constraint "connect-token private key", BREAKING-CHANGE signal |
| Anti-cheat / anti-RMT — auth required to connect | PRD, SAD Security | PASS | OBJ3 (no token without auth), TR-006, BREAKING-CHANGE |
| Never pay-to-win | PRD Constraints | N/A | Technical persistence substrate; introduces no monetization / power path |
| Solo-sustainable single-host footprint | PRD Constraints, SAD | PASS | Constraint "Solo-operable", Assumptions, Excluded (sharding/replicas deferred) |
| CI builds/tests without live DB (offline `sqlx` metadata) | SAD Observability/Ops | PASS | TR-014, Constraint "Compile-time-checked `sqlx` queries" |

### Notes (non-blocking)

- N1 (advisory): The deterministic invariant in ADR-0003 is server↔server / bot-harness bit-identical (ADR-0003 explicitly does NOT require cross-machine bit-determinism). The spec's "bit-identical tick" wording (OBJ4 / TR-010 / SC-005) is scoped to the determinism + bot-harness/botkit equivalence suites, which is consistent with ADR-0003. No change required.
- N2 (advisory): ADR-0007 lists `sqlx`/`redis`/`tokio`/`tracing` as the chosen server stack but does not itself dictate Argon2id; the Argon2id requirement is sourced from the SAD Security section ("proper credential hashing") and OWASP — correctly reflected in TR-004. No conflict.
