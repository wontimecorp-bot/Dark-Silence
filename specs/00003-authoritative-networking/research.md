# Research: Authoritative Networking (E003)

Technical research informing objectives, validation criteria, constraints, and edge cases for the server-authoritative netcode that networkizes the E002 single-player slice. Patterns only — the netcode library is a later plan/ADR concern (isolated behind a `protocol` crate + adapter).

## Client-side prediction & server reconciliation

Recommendation: number every client input; apply it locally on send (prediction) and buffer unacknowledged inputs. On each authoritative snapshot, snap the local ship to the server state, then **deterministically replay** all inputs after the server's last-acked sequence through the shared `sim`. Validation signals: predicted vs authoritative state converges within an epsilon, and corrections are smoothed/blended (imperceptible) rather than teleporting. Avoid: divergent client/server sim paths, trying to predict *other* players' collisions, hard-snapping every packet, unbounded input buffers. Source: Gambetta — client-side prediction & server reconciliation.

## Remote-entity interpolation

Recommendation: render remote entities ~100 ms in the past from a snapshot buffer, interpolating between the two bracketing snapshots so jitter and a single lost packet are hidden by the delay window; size the buffer to the 15–20 Hz send interval. Validation: remote motion stays smooth under simulated loss/jitter with no teleporting. Avoid: extrapolation/dead-reckoning for agile ships (only safe for constrained motion), and buffers too shallow (force extrapolation when packets arrive late). Source: Gambetta — entity interpolation.

## Server-authoritative input validation

Recommendation: the server is the sole source of truth — accept only **inputs** (sequence + control intents), never client-reported positions or hit claims. Clamp thrust/turn to physical bounds, rate-limit fire; the shared `sim` produces all motion and collisions. Resolve hits server-side, rewinding targets to the firer's viewed time (interpolation delay + latency). Baseline: bounds + rate limits + server-resolved hits; defer full lag-comp tuning and anti-cheat heuristics. Avoid: trusting any client-reported world state. Sources: Gambetta — client/server architecture; SnapNet — snapshot interpolation.

## Bandwidth baseline & snapshot model

Recommendation: send **delta** snapshots relative to each client's last-acknowledged snapshot (unchanged entities cost ~1 bit), quantize position/velocity/orientation into bounded fixed-bit fields, and send at 15–20 Hz (below the 30 Hz tick). Send recent inputs redundantly over UDP so one lost packet self-heals. Measure and record baseline bytes (and kbps) per client per second, snapshot size vs MTU, and per-client encode cost. Avoid: full-state sends, float-precision fields, snapshots fragmented past MTU. Interest-management/AOI is a later epic (E009). Sources: Gaffer — snapshot compression; Gaffer — state synchronization.

## Netcode library choice (plan decision → ADR-0014)

Recommendation (chosen): a **transport-level** library — **renet 2.0** (`renet_netcode` UDP + `bevy_renet`) — behind the `protocol` adapter, with the game owning prediction, reconciliation, interpolation, and delta snapshots. renet supplies exactly the missing layer (reliable + unreliable UDP channels, connection management, fragmentation/auth) and nothing that competes with our own machinery; it is more mature (v2.0, 2026) than lightyear (0.26), shrinking the maturity risk ADR-0007 flagged. Channel mapping: handshake = reliable-ordered; `ClientInput` = unreliable + redundant recent inputs; `Snapshot` = unreliable + delta + ack; loopback via renet's in-memory channel. Avoid: a full framework (lightyear / bevy_replicon) whose bundled prediction/replication/interpolation overlaps or sits inert next to OBJ3/OBJ4; and raw `UdpSocket` (rebuilds connection/fragmentation/ack for no gameplay gain — rejected in ADR-0007). Documented fallbacks behind the same adapter: bevy_replicon, aeronet. Sources: renet (lib.rs / github), lightyear & bevy_replicon repos.
