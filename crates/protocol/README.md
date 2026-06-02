# `protocol` — netcode wire protocol & transport seam (E003)

Library-agnostic wire messages, the `NetTransport` adapter trait, bit-packed
quantization, and an in-memory loopback transport for Dark Silence's
authoritative netcode. The netcode library (renet) is confined behind the
`udp` feature so the deterministic logic builds and is provable without it.

## `udp` feature gate (loopback-first)

```toml
# default build — no renet, loopback only
protocol = { path = "../protocol" }
# real-UDP build — pulls in renet + renet_netcode
protocol = { path = "../protocol", features = ["udp"] }
```

- `default = []` — `messages`, `transport`, `quantize`, and `loopback` build
  with **no renet present**. This realizes HINT-001 (prove
  prediction/reconciliation over the in-memory loopback before wiring UDP) at
  the compile level.
- `udp = ["dep:renet", "dep:renet_netcode"]` — adds the `renet_adapter` module
  (Phase 4). renet types stay confined to that module body; the public surface
  (`NetTransport`, messages) names no renet type (HINT-002, SC-006).
- `bevy_renet` is intentionally **not** a dependency: per AD-002 the headless
  server polls renet directly each fixed tick (no Bevy plugin).

## Build environment (Windows dev host)

The growing dependency tree (renet, `renet_netcode`, and transitively `blake3`)
needs these host-specific workarounds. They are not optional on this machine.

### 1. MSVC toolchain (required)

`blake3` (pulled in transitively via the netcode stack) **fails to build on the
GNU/MinGW toolchain**. The repo carries a directory override to MSVC; confirm it
is active:

```sh
rustup show            # expect: stable-x86_64-pc-windows-msvc (active)
# if not active, from the repo root:
rustup override set stable-x86_64-pc-windows-msvc
```

### 2. Disable cert-revocation check

This host cannot reach the certificate revocation server, so cargo's crate
downloads stall/fail. Set:

```sh
CARGO_HTTP_CHECK_REVOKE=false cargo build
```

### 3. Sandbox-disabled cargo

cargo needs real network access (crate downloads) and full filesystem access to
the target directory; run it outside the restricted sandbox.

### 4. Antivirus exclusion for `target/`

Add `target/` to your AV scanner's exclusion list. Real-time scanning of the
many small artifacts written during a build slows the (already large) dep-tree
compile substantially and can cause spurious file-lock errors.

## Module map (filled across later phases)

| Module          | Feature | Purpose |
|-----------------|---------|---------|
| `messages`      | default | `Connect`/`ClientInput`/`Snapshot`/`SnapshotAck`/`Disconnect` |
| `transport`     | default | `NetTransport` trait (glam/sim/protocol types only) |
| `quantize`      | default | `QVec2` / `QAngle` encode↔decode (bitcode) |
| `loopback`      | default | in-memory `NetTransport` (deterministic tests / solo) |
| `renet_adapter` | `udp`   | `RenetTransport: NetTransport` (renet confined here) |
