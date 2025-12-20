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
- **Wayland** (as a protocol adapter)
  - In some deployments, the Wayland protocol itself can be treated as the “wire protocol”.
  - This is useful when a Wayland-capable client wants to interact with a server-side Wayland compositor/session through a proxy or remoting layer.
  - The same architectural rules apply: translate between the shared model and Wayland protocol objects/events without duplicating session logic.
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

In practice, the same protocol may appear in several roles at once:

- It can be **published** (server-side listener / server-side offering).
- It can be **consumed** (client-side connector / client-side usage).
- It can be hosted **inside** `wprsd`, by a helper process **supervised** by `wprsd`, by an external **proxy** (forward/reverse), or by an OS-native **external client**.

The goal is to keep these deployment choices *orthogonal* to the shared model and backend logic.

### Embedded / Spawned / External

For integrations that require helper processes or alternate protocol listeners, `wprs` uses a common pattern:

- **Embedded**: run the integration in-process.
- **Spawned**: `wprsd` (or a launcher) spawns and manages a helper process.
- **External**: the integration is managed outside of `wprs` (systemd, user scripts, another supervisor).

This is used today for Xwayland proxying and RDP bridging.

## Protocol hosting/consumption patterns (explained)

This section enumerates the common ways a protocol adapter may be used.

### 1) Embedded service in the server (publish)

**What it means**

- The protocol adapter runs *in-process* inside `wprsd`.
- `wprsd` opens the protocol listener and serves clients directly.

**When to use**

- Lowest operational complexity (one daemon to deploy).
- Tight integration needs (low-latency control, shared memory, direct access to compositor state).

**Trade-offs**

- Increases the blast radius: protocol bugs can crash the compositor.
- More dependencies in the server process.

### 2) Embedded service in the server (consume)

**What it means**

- The server process itself acts as a client of some protocol to reach another service.
- Example pattern: `wprsd` consumes a “capture” or “session” protocol to obtain surfaces, then republishes via a different protocol.

**When to use**

- Server-side aggregation or gateway use-cases.
- Bridging across environments where the compositor is not local.

**Trade-offs**

- More moving parts inside the server; debugging requires separating “upstream protocol” vs “downstream protocol”.

### 3) Supervised by the server (publish)

**What it means**

- `wprsd` spawns a helper process that *publishes* a protocol (listens for incoming connections).
- `wprsd` manages lifecycle (start/stop, pass configuration), but the protocol code is isolated.

**When to use**

- Protocol stack is large or experimental.
- You want crash isolation but still want a single “system unit” (wprsd) to manage it.

**Trade-offs**

- Requires IPC between `wprsd` and the helper.
- Requires clean configuration/health management.

### 4) Supervised by the server (consume)

**What it means**

- `wprsd` spawns a helper that *consumes* an upstream protocol and feeds the shared model.
- Example pattern: a helper connects to an upstream RDP/VNC session and presents it as local surfaces.

**When to use**

- You want `wprsd` to act as a gateway/relay.

**Trade-offs**

- More complex dataflow; be explicit about ownership of credentials and encryption termination.

### 5) External forward proxy/tunnel (publish or consume)

**What it means**

- A separate process provides connectivity by forwarding traffic.
- It can be used for either direction:
  - **Publish**: expose a local-only listener to a remote client.
  - **Consume**: let a local client reach a remote-only listener via a local endpoint.

**Examples**

- **Publish**: `ssh -R` (reverse tunnel) to expose a server-side local-only listener to a client machine.
- **Consume**: `ssh -L` (local forward) so a client can connect to `127.0.0.1:<port>` while traffic is carried to the remote server.

**wprs-specific examples**

- When running `wprs-rdp-bridge` on the server bound to `127.0.0.1:3389`, a local-forward tunnel (`ssh -L`) lets the client consume RDP as `127.0.0.1:3389`.
- For Xwayland integration, `xwayland-xdg-shell` is *not* a network tunnel; it is a protocol bridge/helper (see “Supervised by the server”).

**When to use**

- You want to bind listeners to localhost and rely on SSH/VPN for security.
- You want to avoid TLS in the protocol itself because the tunnel already provides encryption.

**Trade-offs**

- Operational dependency on the tunnel/proxy.
- Requires clear documentation of the trust boundary (where encryption/auth terminates).

### 6) External reverse proxy/gateway (publish)

**What it means**

- Clients connect to a gateway process.
- The gateway forwards or terminates connections and routes them to internal services.

**Examples**

- An RDP gateway product that terminates TLS/NLA and routes sessions to internal RDP servers.
- A TCP reverse proxy/load balancer routing `tcp://` endpoints.

**wprs-specific examples**

- `wprs-rdp-bridge` is *not* a reverse proxy. It is a protocol bridge that both **consumes** WPRS and **publishes** RDP.
- A true reverse proxy in front of `wprsd` would be something like a generic TCP reverse proxy for `Endpoint::Tcp`, or an RDP gateway in front of `wprs-rdp-bridge`.

**When to use**

- Multi-user deployments with centralized policy, auditing, or access control.
- Internet exposure where you want a single hardened ingress.

**Trade-offs**

- May require protocol-aware proxying for full features; TCP-level proxying may be sufficient for basic transport.

### 7) Implemented as a client adapter (consume)

**What it means**

- The client side (`wprsc` or another client backend) implements a protocol client stack directly.
- It consumes the protocol and renders via a local OS backend.

**When to use**

- You want a single integrated client UX (windowing, clipboard, audio) without external dependencies.

**Trade-offs**

- More code and dependencies in the client.
- Requires careful cross-platform support.

### 8) Consumed by an external client (consume)

**What it means**

- `wprs` does not implement the protocol client; instead it orchestrates an existing external client.
- Example: start a local bridge and launch `xfreerdp`/`mstsc`/Microsoft Remote Desktop.

**When to use**

- Fastest path to broad compatibility.
- Leverage mature clients and their device redirection features.

**Trade-offs**

- UX and feature set depend on the external client.
- Harder to tightly integrate with wprs-native features.

### 9) Multiple ways at once (publish + consume)

**What it means**

- A deployment can combine the above patterns.
- Example: `wprsd` publishes WPRS and RDP, while also consuming Wayland or another upstream protocol for a subset of surfaces.

**Guideline**

- Keep the shared model as the central hub.
- Make each protocol adapter a focused translation layer (minimize cross-adapter coupling).

### Client can connect “through” another protocol

The client side is intentionally flexible:

- A client backend may speak WPRS natively.
- A client backend may connect via another protocol (RDP/VNC) if the server exposes that protocol.
- A client backend may connect via Wayland protocol proxying/remoting if the server exposes Wayland as an adapter.
- A launcher may start an external client (e.g. `xfreerdp`, `mstsc`) pointed at a local forwarded port.

The same concept applies to “Wayland”: depending on deployment, Wayland protocol proxying/remoting may be embedded, delegated to helper processes, or externally orchestrated.

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
