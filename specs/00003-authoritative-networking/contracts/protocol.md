# Wire Protocol Contract — `protocol` crate (E003)

The `protocol` crate defines the **library-agnostic** wire messages and the transport-adapter trait. This is a **binary UDP protocol** (not HTTP/REST), so the contract is a message catalog rather than OpenAPI. No netcode-library (renet) type appears in these definitions or in any consumer (TR-005, SC-006). Encoding is `bitcode` bit-packed with quantized fields (ADR-0007, TR-013).

## Transport adapter trait (the swap seam)

`NetTransport` — implemented by the renet-backed adapter (and any fallback). Signatures use only `protocol`/`glam`/`sim` types:

| Operation | Direction | Notes |
|-----------|-----------|-------|
| `connect(endpoint) -> ConnectionId` | client | open a session to a server endpoint (or in-process loopback) |
| `accept() -> [ConnectionId]` | server | accept newly-connected clients |
| `send_reliable(conn, Message)` | both | reliable-ordered channel (handshake, disconnect) |
| `send_unreliable(conn, Message)` | both | unreliable channel (`ClientInput`, `Snapshot`) |
| `recv(conn) -> [Message]` | both | drain received messages |
| `disconnect(conn, reason)` | both | close a connection |
| `stats(conn) -> NetStats` | both | bytes in/out for the bandwidth baseline (TR-014) |

Loopback uses an in-memory transport implementing the same trait.

## Message catalog

| Message | Direction | Channel | Payload (fields) | Notes |
|---------|-----------|---------|------------------|-------|
| `Connect` | client→server | reliable-ordered | `protocol_version: u16`, `client_token` | version check; rejects mismatched clients |
| `ConnectAccepted` | server→client | reliable-ordered | `client_id`, `tick_rate_hz`, `snapshot_rate_hz`, `interp_delay_ms`, `server_tick` | session params; client aligns its fixed step |
| `ConnectRejected` | server→client | reliable-ordered | `reason` (version / full / banned) | |
| `ClientInput` | client→server | unreliable | `seq: u32`, `tick: u32`, redundant tail of recent inputs each: `{ forward, strafe, turn (quantized −1..=1), fire: bool, toggle_assist: bool }` | redundant recent inputs so one lost packet self-heals (TR-006/007) |
| `Snapshot` | server→client | unreliable | `server_tick: u32`, `acked_input_seq: u32`, `baseline_id: u16` (delta-from), `entities: [EntityRecord]`, `removed: [EntityId]` | delta vs the client's last-acked snapshot; carries the per-client input ack (TR-008/013) |
| `SnapshotAck` | client→server | unreliable | `last_snapshot_id: u16` | lets the server delta against a known baseline (may piggyback on `ClientInput`) |
| `Disconnect` | both | reliable-ordered | `reason` | clean session teardown |

### `EntityRecord` (quantized)

`{ id: EntityId, kind: EntityKind {Ship|Projectile|Target}, pos: QVec2, vel: QVec2, heading: QAngle, flags }` — quantized to bounded fixed-bit ranges (position/velocity to sector-relative bounds; heading to a fixed-bit angle). Only changed fields/entities are sent (delta); unchanged entities cost ~1 bit.

## Reconciliation anchor

Each `Snapshot` carries `acked_input_seq`: the client snaps its own ship to the authoritative state in that snapshot, then replays all buffered inputs with `seq > acked_input_seq` through the shared `sim` (TR-009). Remote entities from the snapshot feed the interpolation buffer (TR-010).

## Out of scope (later epics)

Interest-management / AOI filtering of `entities`, the per-client bandwidth budget, and snapshot prioritization are **E009** (this contract sends the whole small session). Persistence/account identity behind `client_token` is **E004**.
