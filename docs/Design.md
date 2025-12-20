# Design

This document describes the intended architecture of `wprs` as a *protocol-centric* remote desktop system.

The goal is to support multiple wire protocols (WPRS, RDP, VNC, …) without duplicating core logic, while keeping OS-specific code isolated.

## Overview

At a high level, the system is a pipeline:

```
OS backend (server)
  -> Surface/Buffer model
  -> Protocol adapter (WPRS / RDP / VNC / ...)
  -> Surface/Buffer model
  -> OS backend (client)
```

Key properties:

- **OS backends are thin**: they capture or display, and inject input.
- **Protocols are central**: they translate between the OS backends and a shared, protocol-neutral model.
- **The protocol-neutral model is shared**: new protocol implementations should reuse the same model and not re-implement “remote desktop semantics”.

## Layering

### 1) Server backend (OS-specific)

Responsibilities:

- Capture surface updates (buffers + metadata) from the local OS compositor/window system.
- Provide stable surface identity, ordering, and role information where possible.
- Collect input focus / cursor state if available.
- Emit updates to the protocol-neutral model.

Examples:

- Wayland/Smithay backend (Linux)
- X11 fullscreen backend
- Windows fullscreen backend
- macOS fullscreen backend

### 2) Protocol-neutral model (“Buffer model”)

This is the shared core abstraction.

Conceptually it is a state machine over:

- **Surfaces**: identity, hierarchy, role (toplevel/popup/subsurface), damage, output association.
- **Buffers**: pixel data + metadata (size/stride/format), plus optional compression.
- **Cursor**: hidden/named/surface-based cursor images.
- **Input**: keyboard, pointer, gestures.
- **Clipboard / data transfer**: selection, drag-and-drop, and content transfer.
- **Display configuration**: outputs/scale.

This model is what *all* protocols map to and from.

Implementation note:

- The current WPRS protocol already contains many of these types (e.g. `SurfaceState`, `BufferAssignment`, input events).
- The intent is to treat those types as “the model” and have each wire protocol provide a loss-minimized mapping.

### 3) Protocol adapter (wire protocol)

Responsibilities:

- Serialize/transport updates and input across the network or IPC.
- Encode/decode framebuffer updates (raw or compressed) in the protocol’s format.
- Map protocol-native concepts into the shared model.

Protocols:

- **WPRS** (native)
  - Own transport framing + serialization.
  - Can be tunneled (e.g. SSH).
- **RDP**
  - Supported via a bridge implementation using `ironrdp-server`.
  - The RDP side sees a “desktop framebuffer”; the bridge translates that to/from the shared model.
- **VNC** (future)
  - Similar role as RDP: transport + framebuffer update formats + input mapping.

### 4) Client backend (OS-specific)

Responsibilities:

- Render the received buffers to the local display.
- Implement a windowing strategy (single window, multi-window, “seamless” windows, etc.).
- Send input events back through the protocol adapter.
- Integrate clipboard/audio where supported.

## Deployment modes

Protocol adapters can be hosted in multiple ways.

### Embedded / Spawned / External

For integrations that require helper processes or alternate protocol listeners, `wprs` uses a common pattern:

- **Embedded**: run the integration in-process.
- **Spawned**: `wprsd` (or a launcher) spawns and manages a helper process.
- **External**: the integration is managed outside of `wprs` (systemd, user scripts, another supervisor).

This is used today for Xwayland proxying and RDP bridging.

### Client can connect “through” another protocol

The client side is intentionally flexible:

- A client backend may speak WPRS natively.
- A client backend may connect via another protocol (RDP/VNC) if the server exposes that protocol.
- A launcher may start an external client (e.g. `xfreerdp`, `mstsc`) pointed at a local forwarded port.

The same concept applies to “Wayland”: depending on deployment, Wayland support may be embedded, delegated to helper processes, or externally orchestrated.

## “Protocol-centric” means no duplicated session logic

To avoid duplicating code when adding protocols, keep these rules:

1) **Do not re-implement surface tracking in each protocol.**
   - Implement it once in the shared model.
   - Protocol adapters translate to/from it.

2) **Do not re-implement input semantics per protocol.**
   - Normalize to the shared input event types.
   - Provide conversion shims at protocol boundaries.

3) **Keep encoding/decoding local to protocol adapters.**
   - RDP codecs belong in the RDP adapter.
   - VNC encodings belong in the VNC adapter.
   - WPRS sharding/compression belongs in the WPRS adapter.

4) **OS backends should be replaceable.**
   - The server backend should not know whether the client is WPRS, RDP, or VNC.
   - It only knows how to feed the shared model.

## Current state (what exists today)

- Native WPRS protocol and transport (`src/protocols/wprs/…`).
- Multiple OS backends.
- RDP bridge binary (`wprs-rdp-bridge`) that acts as:
  - WPRS client (connects to `wprsd`)
  - RDP server (accepts RDP clients)
  - Translator between the two.
- `wprsd` can spawn or embed the RDP bridge (feature gated).
- The `wprs` Python script can orchestrate:
  - where the bridge runs (client vs server)
  - external RDP clients on each platform
  - SSH port forwarding.

## Adding a new protocol

When adding a protocol adapter (e.g. VNC):

1) Define the minimal mapping to/from the shared model:
   - Framebuffer updates (full frame vs damage)
   - Cursor
   - Input
   - Clipboard

2) Implement the transport and codec logic in the protocol module.

3) Provide one or both of:
   - An embedded mode for `wprsd` (optional)
   - A standalone bridge executable (recommended for iteration)

4) Extend orchestration tooling (`wprs` launcher) as needed to support:
   - client placement (local bridge + local UI)
   - server placement (remote bridge + forwarded port)
   - external clients.

