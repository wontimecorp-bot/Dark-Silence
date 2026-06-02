# Tasks: Authoritative Networking (E003)

**Input**: Design documents from `specs/00003-authoritative-networking/`
**Prerequisites**: `plan.md`, `spec.md`, `research.md`, `contracts/protocol.md`, `checklists/{security,testing,performance}.md` (all CHK items complete)

**Tests**: Included — the spec is heavily test-specified (TR-032..047, SC-001..013) and the project Testing Policy mandates a headless-bot integration harness. Test tasks for a requirement precede its implementation tasks within a phase.

## Project Mode

`Brownfield`

- **ADDS**: `crates/protocol` (`+`), `crates/server` (`+`).
- **MODIFIES**: `crates/client` (`~`) — networkized (BREAKING: local-only sim → networked client).
- **REUSES (unchanged)**: `crates/sim` (E001) and the bulk of `crates/client` (E002). No E001/E002 keystone is re-bootstrapped or rewritten; their existing tests are untouched.
- `+` = new file/crate · `~` = modified existing file/crate.

## Epic / Capability Map

- `[OBJ1]` → Authoritative server & session (P1) — headless `bevy_ecs` tick loop, session manager, loopback wiring, rate-default invariants
- `[OBJ2]` → Protocol crate & transport isolation (P1) — wire messages, `NetTransport` seam, renet UDP adapter, secure mode
- `[OBJ3]` → Prediction & reconciliation (P1) — numbered input, local predict, re-seed + replay, smoothed correction
- `[OBJ4]` → Remote-entity interpolation (P1) — snapshot buffer, ~100 ms interpolation, stale/dup/out-of-order snapshot discard
- `[OBJ5]` → Server-side input validation (P1) — per-field clamp/reject, fire-rate gate, hit authority + lag-comp rewind, DoS/buffer/malformed guards
- `[OBJ6]` → Bandwidth baseline & bot harness (P2) — delta+quantize properties, bytes/client/sec, encode-cost, headless bot scenario set

## Brownfield Notes

- **Existing flows touched**: `crates/client/src/main.rs`, `crates/client/src/input.rs`, `crates/client/src/render_sync.rs` (networkized). Keyboard → `sim::ShipIntent` becomes keyboard → numbered `protocol::ClientInput` → server; render reads predicted local state + interpolated remote snapshots, reusing the E002 render-sync/interpolation seam (ADR-0013).
- **Reuse, not fork**: server and client both depend on `crates/sim` verbatim; reconciliation replays through the same deterministic `sim` systems with the E002 `sim::FixedDt`. `sim::ShipIntent`/components/`weapon` cooldown are reused unchanged (IP-001, HINT-003).
- **Seam discipline**: `protocol`'s public surface (`NetTransport`, messages) names no renet type; renet is confined to `renet_adapter` — mirrors E001's `sim::Physics`/`RapierPhysics` confinement (HINT-002, TR-005, SC-006).
- **Ordering keystone (HINT-001)**: prove prediction + reconciliation over the in-memory loopback (deterministic, no UDP) BEFORE wiring the renet UDP adapter and secure mode.
- **Compatibility / migration**: loopback mode preserves runnable solo play (Principle VII); E001/E002 tests must keep passing (regression focus).
- **OD-001 feel constants** (`RECON_EPS`, `MAX_SNAP`, `MAX_INTERP_DELTA`) are **accepted-provisional defaults** (2026-06-02) — recorded in `sim::tuning`/protocol constants, not a blocking decision. See T046.

## Phase 1: Setup (Repository / Workspace Delta)

- [X] T001 Add `renet` 2.0, `renet_netcode`, `bevy_renet`, `bitcode` to `[workspace.dependencies]` in Cargo.toml, pinned to the Bevy 0.18-compatible versions (HINT-005); register `crates/protocol` + `crates/server` as workspace members
- [X] T002 [P] Create crate manifest crates/protocol/Cargo.toml (deps: renet, renet_netcode, bevy_renet, bitcode, glam, serde, sim) after:T001
- [X] T003 [P] Create crate manifest crates/server/Cargo.toml (deps: bevy_ecs, sim, protocol, glam) after:T001
- [X] T004 [P] Add `.cargo/audit.toml` config for `cargo audit` (renet + transitive vuln scan) per Testing Strategy Security tier
- [X] T005 Document MSVC + build-env workarounds (CARGO_HTTP_CHECK_REVOKE=false, sandbox off, AV exclusion for target/) in crates/protocol/README.md for the grown build tree (HINT-005)

---

## Phase 2: Foundational (Cross-Work-Item Blockers — the loopback-first keystone)

**Builds the `protocol` crate: wire messages + library-agnostic `NetTransport` trait + in-memory loopback transport + `quantize`, plus the reused-`sim` wiring. This is the keystone every OBJ depends on; loopback is proven here before any renet UDP (HINT-001/002).**

- [X] T006 [P] {TR-004} Create crates/protocol/src/lib.rs declaring modules (messages, transport, quantize, loopback) — no renet types in public surface (HINT-002) after:T002
- [X] T007 [P] {TR-005} Define ConnectionId, NetStats, EntityId, EntityKind newtypes in crates/protocol/src/messages.rs → exports: ConnectionId, NetStats{bytes_out,bytes_in}, EntityKind{Ship,Projectile,Target} after:T002
- [X] T008 {TR-004,TR-024,TR-025,TR-026,TR-044} Define handshake messages (Connect, ConnectAccepted{tick/snapshot/interp params}, ConnectRejected{version|full|banned}, Disconnect) per contracts/protocol.md in crates/protocol/src/messages.rs ← T007:ConnectionId → exports: Connect, ConnectAccepted, ConnectRejected
- [X] T009 {TR-004,TR-007,TR-027} Define ClientInput{seq,tick,redundant tail bound 8 of intents} + SnapshotAck per contracts/protocol.md in crates/protocol/src/messages.rs ← T008:Connect → exports: ClientInput(seq,tick,intents), SnapshotAck
- [X] T010 {TR-004,TR-013} Define Snapshot{server_tick,acked_input_seq,baseline_id,entities,removed} + EntityRecord{id,kind,pos,vel,heading,flags} per contracts/protocol.md in crates/protocol/src/messages.rs ← T009:ClientInput → exports: Snapshot, EntityRecord
- [X] T011 [P] {TR-004,TR-013,TR-045} Implement bitcode bit-packed encode/decode for the Message union in crates/protocol/src/messages.rs ← T010:Snapshot → exports: Message::encode/decode
- [X] T012 [P] {TR-013,TR-045} Implement QVec2 (sector-relative bounds) + QAngle fixed-bit quantize↔dequantize with build-pinned widths/ranges in crates/protocol/src/quantize.rs → exports: QVec2, QAngle, quantize/dequantize after:T002
- [X] T013 {TR-005} Define the NetTransport trait (connect/accept/send_reliable/send_unreliable/recv/disconnect/stats) using only protocol/glam/sim types in crates/protocol/src/transport.rs ← T011:Message,T007:NetStats → exports: NetTransport, NetTransport::stats()->NetStats
- [X] T014 {TR-003,TR-005} Implement the in-memory loopback NetTransport (deterministic, zero-latency, loss-free) in crates/protocol/src/loopback.rs ← T013:NetTransport → exports: LoopbackTransport: NetTransport
- [X] T015 {TR-005,TR-014} Implement NetStats bookkeeping (bytes out/in per connection) on LoopbackTransport in crates/protocol/src/loopback.rs ← T014:LoopbackTransport
- [X] T016 {TR-004} [COMPLETES TR-004] Unit test: every protocol message round-trips encode→decode to an equal value (OBJ2 VC-1; independent of the bot harness, TR-043) in crates/protocol/tests/roundtrip.rs ← T011:Message
- [X] T017 {TR-013,TR-045} Unit test: QVec2/QAngle quantize round-trip within field tolerance; encoded size deterministic per build in crates/protocol/tests/quantize.rs ← T012:QVec2

---

## Phase 3: Authoritative server & session (Priority: P1) 🎯 MVP

**[OBJ1]** Headless authoritative server running shared `sim` at the fixed tick; session manager; loopback embedding; server-announced rate defaults + snapshot<tick invariant. Reuses `crates/sim` verbatim (IP-001).

- [X] T018 [OBJ1] {TR-001,TR-044} Create headless bevy_ecs app, fixed 30 Hz tick loop (recv→validate→sim→encode→send) stepping reused sim at sim::FixedDt in crates/server/src/main.rs ← T013:NetTransport,T009:ClientInput → exports: ServerApp.run()
- [X] T019 [OBJ1] {TR-044} Assert snapshot-rate < tick-rate at start; emit defaults 30/20/100 as ConnectAccepted params (no negotiation) in crates/server/src/main.rs ← T008:ConnectAccepted after:T018
- [X] T020 [OBJ1] {TR-002,TR-008} Implement Session: connection table, per-client last-processed seq + last-acked bookkeeping, ack emission into snapshots in crates/server/src/session.rs ← T018:ServerApp,T010:Snapshot → exports: Session
- [X] T021 [OBJ1] {TR-024,TR-025,TR-026} Implement handshake: exact-match version → Rejected{version}; capacity ceiling 8 → Rejected{full} no slot alloc; reserved Rejected{banned} reject-and-close in crates/server/src/session.rs ← T020:Session,T008:ConnectRejected → exports: Session::handshake()
- [X] T022 [OBJ1] {TR-003,TR-018} Wire loopback mode (embedded server + client, one process) through the identical session/validation path — not a bypass in crates/server/src/main.rs ← T014:LoopbackTransport,T020:Session after:T021 → exports: ServerApp::loopback()
- [X] T023 [P] [OBJ1] {TR-001,TR-002} Integration test: two clients connect over loopback, share one authoritative world, see each other's entities (OBJ1 VC-1, SC-001 share path) in crates/server/tests/session.rs ← T022:ServerApp::loopback,T020:Session
- [X] T024 [P] [OBJ1] {TR-044} [COMPLETES TR-044] Test: server rejects snapshot rate ≥ tick rate; ConnectAccepted announces 30/20/100 defaults the client adopts in crates/server/tests/rates.rs after:T019 ← T018:ServerApp

---

## Phase 4: Protocol transport — renet UDP adapter & secure mode (Priority: P1)

**[OBJ2]** With loopback proven (Phase 2/3), wire the renet UDP adapter behind `NetTransport` (renet confined to `renet_adapter`), then renet_netcode **secure mode** with a stub local token-issuer. Channel mapping per HINT-004/AD-006.

- [X] T025 [OBJ2] {TR-005,TR-006} Implement RenetTransport: NetTransport over renet/renet_netcode UDP — renet types confined to this module body (HINT-002) in crates/protocol/src/renet_adapter.rs ← T013:NetTransport,T011:Message → exports: RenetTransport
- [X] T026 [OBJ2] {TR-006} Map channels: handshake reliable-ordered; ClientInput unreliable + redundant recent tail; Snapshot unreliable + delta + ack (HINT-004/AD-006) in crates/protocol/src/renet_adapter.rs ← T025:RenetTransport,T009:ClientInput after:T025
- [X] T027 [OBJ2] {TR-048} Implement stub local connect-token issuer (local signing key), isolated so E004 swaps it without touching the secure-connection path in crates/protocol/src/renet_adapter.rs ← T025:RenetTransport → exports: StubTokenIssuer
- [X] T028 [OBJ2] {TR-048} Configure renet_netcode secure mode (authenticated + encrypted connect-token sessions); reject any unauthenticated/unsecure connect in crates/protocol/src/renet_adapter.rs ← T027:StubTokenIssuer after:T027 → exports: RenetTransport::secure()
- [X] T029 [OBJ2] {TR-005,TR-006} [COMPLETES TR-006] Implement NetStats bytes out/in bookkeeping (application-payload bytes, transport headers excluded) on RenetTransport in crates/protocol/src/renet_adapter.rs ← T025:RenetTransport after:T026
- [X] T030 [P] [OBJ2] {TR-005} [COMPLETES TR-005] Test: no renet type appears in protocol/sim/consumer public surfaces; swapping Loopback↔Renet needs no sim/gameplay change (SC-006) in crates/protocol/tests/isolation.rs ← T025:RenetTransport,T014:LoopbackTransport
- [X] T031 [P] [OBJ2] {TR-041} Integration test: RenetTransport exercised bound to 127.0.0.1, distinct from the in-memory transport (AD-004) in crates/protocol/tests/renet_udp.rs ← T028:RenetTransport::secure
- [X] T032 [P] [OBJ2] {TR-048} [COMPLETES TR-048] Test: an unauthenticated/unsecure connect is rejected; an established session's channel is secure (SC-013) in crates/protocol/tests/secure_connect.rs ← T028:RenetTransport::secure

---

## Phase 5: Prediction & reconciliation (Priority: P1)

**[OBJ3]** Networkize the client (BREAKING): numbered `ClientInput`, local prediction via shared `sim`, reconciliation by re-seed + deterministic replay with smoothed correction. Proven over loopback first (HINT-001/003). Reuses `sim::FixedDt`/`ShipIntent` unchanged.

- [X] T033 [OBJ3] {TR-007} Map keyboard → numbered protocol::ClientInput (monotonic seq, sim::ShipIntent payload + redundant tail) in crates/client/src/input.rs ← T009:ClientInput,sim::ShipIntent → exports: build_client_input()
- [X] T034 [OBJ3] {TR-007,TR-027} Implement local prediction: apply each numbered input to the local ship via shared sim; buffer unacked inputs (cap 64) in crates/client/src/prediction.rs ← T033:build_client_input,sim::FixedDt → exports: InputBuffer, predict_local()
- [X] T035 [OBJ3] {TR-009,TR-016} Implement reconciliation: on each Snapshot re-seed local ship to acked state, deterministically replay inputs seq>acked_input_seq via shared sim in crates/client/src/prediction.rs ← T034:InputBuffer,T010:Snapshot after:T034 → exports: reconcile()
- [X] T036 [OBJ3] {TR-033} [COMPLETES TR-009] Implement smoothed correction: blend the residual over ≤5 ticks, no single tick exceeding MAX_SNAP (OD-001), non-increasing residual (no teleport) in crates/client/src/prediction.rs ← T035:reconcile after:T035 → exports: smooth_correction()
- [X] T037 [OBJ3] {TR-034,TR-016,TR-032} Determinism test: fixed seed + identical input stream → server sim and client predicted sim bit-identical (epsilon=0) after N ticks over loopback (SC-007) in crates/server/tests/determinism.rs ← T035:reconcile,T014:LoopbackTransport
- [X] T038 [OBJ3] {TR-035} Implement reproducible forced-mismatch injection (scripted deterministic divergence: one-tick authoritative override / server-resolved ram) in the loopback harness in crates/server/tests/harness.rs ← T022:ServerApp::loopback → exports: inject_mismatch(seed)
- [X] T039 [OBJ3] {TR-033} [COMPLETES TR-033] Convergence test: after forced mismatch, predicted reaches authoritative within RECON_EPS in ≤5 snapshots, non-increasing residual, no correction > MAX_SNAP (SC-002) in crates/client/tests/reconciliation.rs ← T036:smooth_correction,T038:inject_mismatch
- [X] T040 [P] [OBJ3] {TR-007} Test: own ship responds immediately to predicted input (no perceptible delay) over loopback (SC-001 prediction path) in crates/client/tests/prediction.rs ← T034:predict_local

---

## Phase 6: Remote-entity interpolation (Priority: P1)

**[OBJ4]** Render remote entities at a fixed ~100 ms delay from a capped snapshot buffer; ride out loss/jitter; discard stale/duplicate/out-of-order snapshots by `server_tick`. Reuses E002 render-sync/interpolation seam (ADR-0013).

- [X] T041 [OBJ4] {TR-010,TR-027} Implement per-client snapshot/interpolation buffer (cap 32, oldest-dropped) feeding remote transforms in crates/client/src/interpolation.rs ← T010:Snapshot → exports: SnapshotBuffer(cap 32)
- [X] T042 [OBJ4] {TR-010} Interpolate remote entity transforms at the fixed ~100 ms delay between the two bracketing buffered snapshots in crates/client/src/interpolation.rs ← T041:SnapshotBuffer after:T041 → exports: interpolate_remotes(now)
- [X] T043 [OBJ4] {TR-037} [COMPLETES TR-037] Discard snapshots whose server_tick is older than the newest applied (stale) and duplicates; buffer advances monotonically in crates/client/src/interpolation.rs ← T041:SnapshotBuffer after:T041 → exports: SnapshotBuffer::push()
- [X] T044 [OBJ4] {TR-010,TR-016} [COMPLETES TR-010] Networkize render_sync: local render from predicted state, remotes from interpolated snapshots (reuse ADR-0013 seam) in crates/client/src/render_sync.rs ← T036:smooth_correction,T042:interpolate_remotes after:T042
- [X] T045 [OBJ4] {TR-002,TR-003,TR-007} [COMPLETES TR-002] [COMPLETES TR-003] [COMPLETES TR-007] Add the client net plugin to FixedUpdate (send/recv/reconcile/interpolate) wiring transport in crates/client/src/main.rs after:T044 → exports: NetClientPlugin
- [X] T046 [P] [OBJ4] Record OD-001 provisional constants (RECON_EPS, MAX_SNAP, MAX_INTERP_DELTA) in crates/client/src/prediction.rs; note tune-in-playtest (non-blocking)
- [X] T047 [P] [OBJ4] {TR-036} Add fixed loss/jitter params (5% uniform single-packet loss, ±50 ms jitter, scripted consecutive-drop) to the loopback transport for tests in crates/protocol/src/loopback.rs ← T014:LoopbackTransport → exports: LoopbackTransport::with_loss_jitter()
- [X] T048 [OBJ4] {TR-036} [COMPLETES TR-036] Loss/jitter test: no remote jumps > MAX_INTERP_DELTA for single-drop + 5%/±50 ms; ~100 ms buffer rides out one drop; consecutive-drop stall bound (SC-004) in crates/client/tests/interpolation.rs ← T042:interpolate_remotes,T047:with_loss_jitter
- [X] T049 [P] [OBJ4] {TR-037} Test: deliberately reordered/duplicated snapshot delivery — interpolation buffer advances monotonically, no backward jump (SC-012) in crates/client/tests/snapshot_order.rs ← T043:SnapshotBuffer::push

---

## Phase 7: Server-side input validation (Priority: P1)

**[OBJ5]** Server trusts only inputs: per-field clamp/reject, fire-rate gate at the `sim` cooldown, replay/stale/out-of-order discard, malformed/oversize drop, per-client buffer/rate caps, idle timeout, anti-cheat logging, and server-authoritative hit resolution with lag-compensated rewind.

- [X] T050 [OBJ5] {TR-011,TR-020} Per-field validation: clamp analog forward/strafe/turn to −1..=1, accept toggle_assist, reject unknown EntityKind/decode-fail in crates/server/src/validation.rs ← T009:ClientInput → exports: validate_input()
- [X] T051 [OBJ5] {TR-021} Implement fire-rate gate: reject a fire intent arriving before the firing entity's authoritative sim weapon cooldown has elapsed (bound = sim::Weapon cooldown) in crates/server/src/validation.rs ← T050:validate_input,sim::Weapon after:T050
- [X] T052 [OBJ5] {TR-022,TR-023} Implement seq/tick discard: replay (seq ≤ last-processed) & stale (tick < window) discarded; out-of-order applied at most once per seq in crates/server/src/session.rs ← T020:Session,T009:ClientInput after:T020
- [X] T053 [OBJ5] {TR-012,TR-019} Treat client positions/hits as non-authoritative: motion from server sim; validated-but-impossible input yields sim-constraint-resolved outcome in crates/server/src/validation.rs ← T050:validate_input,sim::simulate → exports: apply_authoritative()
- [X] T054 [OBJ5] {TR-012,TR-017} [COMPLETES TR-017] Server-authoritative hit resolution with lag-comp target rewind = interp delay + RTT, capped 500 ms, oldest-retained fallback in crates/server/src/validation.rs ← T053:apply_authoritative,sim::collision after:T053
- [X] T055 [OBJ5] {TR-027,TR-028} Enforce per-client inbound rate limit (≤120 msg/s = 4× send rate, distinct from fire gate; drop excess + flag) + bounded buffers in crates/server/src/session.rs ← T020:Session after:T052
- [X] T056 [OBJ5] {TR-029,TR-030} Malformed/oversize guard: decode-fail / unknown type / truncated / payload > MTU (≤1200 B) → drop, no state mutation in crates/server/src/session.rs ← T020:Session,T011:Message after:T052
- [X] T057 [OBJ5] {TR-031} Anti-cheat logging (offending id + reason + tick; no raw payloads/thresholds) + 10 s idle timeout dropping only that session, no slot leak in crates/server/src/session.rs ← T021:Session::handshake after:T055
- [X] T058 [OBJ5] {TR-018} [COMPLETES TR-018] Test: an out-of-bounds / excessive-rate input over loopback is clamped/rejected exactly as over UDP — loopback is not an authority bypass (SC-009) in crates/server/tests/validation_parity.rs ← T022:ServerApp::loopback,T050:validate_input after:T051
- [X] T059 [OBJ5] {TR-038,TR-011,TR-020,TR-021,TR-022,TR-023} [COMPLETES TR-038] Per-class rejection tests (clamp / fire-gate / replay-stale discard / asserted position-hit ignored), TR-039 signal each in crates/server/tests/validation.rs ← T050:validate_input after:T051 after:T052 after:T053
- [X] T060 [OBJ5] {TR-039} [COMPLETES TR-039] State-equality assertion: server sim state byte-for-byte identical pre/post a rejected/ignored input except input-ack bookkeeping; clamp case asserts the clamped bound in crates/server/tests/validation.rs after:T059
- [X] T061 [P] [OBJ5] {TR-027,TR-028,TR-030,TR-031} [COMPLETES TR-027] [COMPLETES TR-028] [COMPLETES TR-030] Test: malformed/replay/stale input + buffer/rate overflow each discarded, state unchanged, logged (SC-010) in crates/server/tests/dos_guard.rs after:T055 after:T056 after:T057
- [X] T062 [P] [OBJ5] {TR-024,TR-025,TR-029} Test: capacity-ceiling Connect → ConnectRejected{full} no leaked slot; each emitted Snapshot fits the MTU payload bound, no fragmentation (SC-011) in crates/server/tests/capacity_mtu.rs ← T021:Session::handshake,T056

---

## Phase 8: Bandwidth baseline & bot harness (Priority: P2)

**[OBJ6]** Delta+quantize snapshot encoder with assertable properties, bytes/client/sec + encode-cost recorded over the renet UDP path (recorded-only, no gate), and the headless bot harness driving the enumerated scenario set with ≥2 clients, no rendering.

- [X] T063 [OBJ6] {TR-013,TR-045} [COMPLETES TR-013] Delta snapshot encoder vs each client's last-acked baseline_id (unchanged entity ≤1 bit), quantized, 20 Hz in crates/server/src/snapshot.rs ← T012:QVec2,T020:Session,T010:Snapshot → exports: encode_snapshot()
- [X] T064 [OBJ6] {TR-029,TR-045} [COMPLETES TR-045] Enforce MTU bound (split or drop lowest-priority entities, never oversize); lost-ack degradation (delta vs last acked, else full keyframe) in crates/server/src/snapshot.rs ← T063:encode_snapshot after:T063
- [X] T065 [OBJ6] {TR-012} [COMPLETES TR-012] Encode authoritative state (server-sim transforms/velocities + server-resolved hits only) into snapshots in crates/server/src/snapshot.rs ← T063:encode_snapshot after:T054
- [X] T066 [OBJ6] {TR-014} [COMPLETES TR-014] Wire bytes/client/sec metering off NetTransport::stats into the encode/send path in crates/server/src/snapshot.rs ← T063:encode_snapshot after:T063 after:T015
- [X] T067 [OBJ6] {TR-047} [COMPLETES TR-047] Structure the snapshot encoder as a benchmarkable unit (cargo bench / timed path) so per-client encode cost at baseline scale is observable in crates/server/benches/encode.rs ← T063:encode_snapshot
- [X] T068 [OBJ6] {TR-015,TR-043} Build the headless bot harness: ≥2 networked clients, no rendering, fixed scripted input loops, numeric signals only (state deltas, seq/ack, stats) in crates/server/tests/harness.rs ← T045:NetClientPlugin,T022:ServerApp::loopback → exports: BotHarness, ScriptedBot
- [X] T069 [OBJ6] {TR-040} [COMPLETES TR-040] Loopback-equivalence test across the four named paths under matched loss-free/zero-latency conditions; ignore transport-only diffs (SC-008) in crates/server/tests/equivalence.rs ← T068:BotHarness,T025:RenetTransport,T014:LoopbackTransport
- [X] T070 [OBJ6] {TR-042,TR-046} [COMPLETES TR-042] [COMPLETES TR-046] Bandwidth baseline: fixed 30 s / 2 bots + 4 ships / 20 Hz / seed over renet UDP; emit mean+peak bytes/client/sec (out, payload) + encode cost, recorded-only (SC-005) in crates/server/tests/bandwidth.rs ← T068:BotHarness after:T066 after:T067 after:T031
- [X] T071 [OBJ6] {TR-031,TR-043} [COMPLETES TR-031] Bot scenario — disconnect-mid-session: a client drops, server frees only that slot, others continue (Edge Cases) in crates/server/tests/harness.rs ← T068:BotHarness after:T057
- [X] T072 [OBJ6] {TR-015,TR-032,TR-043} [COMPLETES TR-015] [COMPLETES TR-016] [COMPLETES TR-032] [COMPLETES TR-034] [COMPLETES TR-035] [COMPLETES TR-043] Assemble the scenario set; assert each P1 SC + OBJ2 round-trip traces to a named tier in crates/server/tests/harness.rs after:T068 after:T070 after:T071

---

## Phase 9: Polish & Cross-Cutting Concerns

**Full workspace gate suite + the recorded bandwidth/encode-cost baseline run. Cross-cutting; runs after all delivery phases.**

- [X] T073 Run the full workspace QC gate green: `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo audit` — with the MSVC + build-env workarounds (see T005) after:T072
- [X] T074 [P] Execute the bandwidth + encode-cost baseline run (T070) and record the figure as a structured artifact under the harness output (SC-005 asserted-as-satisfied by presence) after:T070
- [X] T075 [P] Verify E001/E002 regression: existing crates/sim + crates/client tests still pass unchanged (no keystone rewrite) after:T073

---

## Dependencies

Setup (Phase 1) → Foundational (Phase 2) → OBJ1 (Phase 3) → OBJ2 renet/secure (Phase 4) → OBJ3 (Phase 5) → OBJ4 (Phase 6) → OBJ5 (Phase 7) → OBJ6 (Phase 8) → Polish (Phase 9)

- **Phase 1 Setup**: no dependencies; T002/T003/T004 depend on T001 (members/deps); T005 standalone.
- **Phase 2 Foundational** depends on Setup. The `protocol` keystone (messages → transport → loopback → quantize) blocks every OBJ.
- **Loopback-before-renet (HINT-001, critical)**: Phase 3 (server + loopback) and Phase 5 (prediction/reconciliation over loopback, incl. the bit-identical determinism test T037) complete on the **in-memory loopback** before Phase 4's renet UDP adapter + secure mode are required by the cross-transport tests (T069 equivalence, T070 bandwidth-over-UDP). Phase 4 is sequenced after Phase 3 so the renet adapter is wired only once loopback proves the netcode logic; the renet-dependent assertions live in Phases 6/8.
- **Seam preserved (HINT-002)**: renet appears only in T025–T029 (`renet_adapter.rs`); `NetTransport`/messages (T007–T015) name no renet type; T030 asserts this.
- **Reuse, not rewrite (HINT-003)**: T035/T037 replay through the unchanged `crates/sim` with `sim::FixedDt`; no E001/E002 keystone is modified (T075 regression check).
- Tasks marked `[P]` are parallelizable within their phase (distinct files, no intra-batch dependency). No `[P]` batch contains both a task and its `after:`/`←` dependency.
- Tasks with `after:T###` require the referenced task `[X]` before executing.
- **Polish (Phase 9)** depends on all delivery phases (T073 after:T072).

## Requirement Coverage

Every TR-001…TR-048 maps to ≥1 task; `[COMPLETES]` marks each requirement's last task.

| Req | Tasks | Req | Tasks |
|-----|-------|-----|-------|
| TR-001 | T018, T023 | TR-025 | T021, T062 |
| TR-002 | T020, T023, T045 | TR-026 | T021 |
| TR-003 | T014, T022, T045 | TR-027 | T034, T041, T055, T061 |
| TR-004 | T006, T008, T009, T010, T011, T016 | TR-028 | T055, T061 |
| TR-005 | T007, T013, T014, T015, T025, T030 | TR-029 | T056, T062, T064 |
| TR-006 | T025, T026, T029 | TR-030 | T056, T061 |
| TR-007 | T009, T033, T034, T040, T045 | TR-031 | T057, T061, T071 |
| TR-008 | T020 | TR-032 | T037, T072 |
| TR-009 | T035, T036 | TR-033 | T036, T039 |
| TR-010 | T041, T042, T044 | TR-034 | T037, T072 |
| TR-011 | T050, T059 | TR-035 | T038, T072 |
| TR-012 | T053, T054, T065 | TR-036 | T047, T048 |
| TR-013 | T010, T011, T012, T017, T063 | TR-037 | T043, T049 |
| TR-014 | T015, T066 | TR-038 | T059 |
| TR-015 | T068, T072 | TR-039 | T060 |
| TR-016 | T035, T037, T044, T072 | TR-040 | T069 |
| TR-017 | T054 | TR-041 | T031 |
| TR-018 | T022, T058 | TR-042 | T070 |
| TR-019 | T053 | TR-043 | T068, T071, T072 |
| TR-020 | T050, T059 | TR-044 | T008, T018, T019, T024 |
| TR-021 | T051, T059 | TR-045 | T011, T012, T017, T063, T064 |
| TR-022 | T052, T059 | TR-046 | T070 |
| TR-023 | T052, T059 | TR-047 | T067, T070 |
| TR-024 | T021, T062 | TR-048 | T027, T028, T032 |

**No gaps**: TR-001…TR-048 all covered (48/48).
