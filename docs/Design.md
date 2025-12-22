# Design

This document describes the process topology and protocol boundaries of `wprs`.

## Invariants

- **WPRS is the canonical model and data plane** (surfaces/buffers/cursor/input).
- **`wprsd` publishes only WPRS**.
- **Protocol adaptation is implemented by clients** (including bridges).
  - A bridge consumes WPRS from `wprsd` and publishes another protocol endpoint (RDP/VNC/...).

## Binaries

- `wprsd`: host daemon/server; runs host backends and serves WPRS.
- `wrun`: launcher/session orchestrator; talks to `wprsd` via `wctl`.
- `wprsc`: viewer; WPRS client; renders via native UI (winit+wgpu) and may provide a web gateway.
- `wprs-rdp-bridge`: example bridge; WPRS client upstream + RDP server downstream.

## Runtime modes

### Mode A: daemon mode (default)

```
wrun  --wctl-->  wprsd  --wprs-->  wprsc  --> display
```

- `wrun` manages sessions/apps via `wctl`.
- `wprsd` runs host backends and multiplexes WPRS clients.
- `wprsc` connects over WPRS and renders.

Linux specifics (when `wprsd` is a Wayland compositor, optionally with XWayland):

- Wayland apps connect via `WAYLAND_DISPLAY`.
- X11 apps connect via `DISPLAY` to XWayland hosted by `wprsd`.
- `wrun` requests session env values from `wprsd` and spawns the target app with those env vars.

macOS specifics (capture backends):

- Apps do not connect to `wprsd`; `wprsd` captures existing windows or the main display.
- Capture requires Screen Recording permission; input injection requires Accessibility permission.
- `wrun` may spawn a short-lived `wprsd` to capture a single app in a daemon-mode-like topology.
  - On macOS window capture, `wrun` spawns the app and passes the PID to `wprsd` over `wctl` (never via CLI flags).

### Mode B: daemon-less mode (fallback)

```
wrun  --wprs-->  wprsc  --> display
```

- `wrun` embeds the host engine in-process.
- Constraints: single app, single client, short-lived.

### Mode C: agent/launcher mode (optional)

```
wrun  --wctl-->  wprsd  --wprs-->  wprsc
```

Use case: remote launch (e.g. over SSH) while keeping capture/streaming in `wprsd`.

Note: `wrun` is not part of the WPRS data plane.

## Protocol planes

### `wprs` (data plane)

WPRS carries the canonical model and high-rate traffic:

- surface/window model updates
- buffer/frame delivery
- cursor state
- input events
- optional clipboard/data transfer

In daemon mode, `wprsd` is the WPRS server and `wprsc` is the WPRS client.

### `wctl` (control plane)

`wctl` is a local control protocol between `wrun` and `wprsd`.

- session lifecycle (create/list/stop)
- app/window target management
- environment allocation (`WAYLAND_DISPLAY`, `DISPLAY`, ...)
- introspection (session/window/surface IDs)

Transport: local Unix socket / named pipe.

## Bridges (RDP/VNC/...)

Bridges terminate non-WPRS protocols; `wprsd` does not.

Example:

```
wprsd --wprs--> wprs-rdp-bridge --RDP--> external RDP client
```

## Source map

- WPRS protocol + model: `src/protocols/wprs/`
- Host daemon: `src/bin/wprsd.rs`
- Launcher: `src/bin/wrun.rs`
- Viewer: `src/bin/wprsc.rs`
- RDP bridge: `src/bin/wprs-rdp-bridge.rs` and `src/protocols/rdp/`

## Adding a new bridge

- Implement a WPRS client that consumes the WPRS model from `wprsd`.
- Translate to/from the target protocol.
- Publish the target protocol endpoint for external clients.
