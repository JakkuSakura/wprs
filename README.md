# wprsx

wprsx (wprs eXtended) is a fork of
[wprs](https://github.com/wayland-transpositor/wprs) with additional fixes and
improvements. It keeps the original architecture and workflow while extending
it for better stability, usability, and long-term maintenance.

Like [xpra](https://en.wikipedia.org/wiki/Xpra), but for Wayland, and written in
Rust.

wprsx implements rootless remote desktop access for remote Wayland (and X11, via
XWayland) applications.

Fork notes:

- This repository is named `wprsx`, but the binaries and package names may
  still be `wprs`, `wprsc`, and `wprsd` for compatibility with existing setups.
- This fork carries additional fixes and incremental improvements; review the
  commit history for a detailed change list.
- If you are looking for upstream behavior or documentation, refer to the
  original wprs repository linked above.

## Building

wprsx is primarily developed and tested on x86-64. The compression path has an
AVX2-optimized implementation that is compiled when the target enables AVX2
(for example via `-C target-feature=+avx2` or `-C target-cpu=native`). Without
AVX2, a scalar fallback is used and will be slower. Other architectures (for
example ARM) may build but are less exercised today.

### Platform Support

- `wprsc` (client) is intended to be cross-platform and should build on Linux/macOS/Windows.
  - Wayland client backend (Linux/Wayland): requires the `wayland-client` feature and Wayland system libraries.
  - Cross-platform `winit` + `wgpu` backend (default).
- `wprsd` has multiple backends:
  - Wayland compositor backend (Linux/Wayland): requires the `wayland` feature (Smithay).
  - X11 fullscreen backend (Linux/X11).
  - macOS fullscreen and window/seamless backends (requires Screen Recording + Accessibility permissions).
  - Windows fullscreen and window/seamless backends.
  - Mock backend (used by the `wprsd_demo` example).

In practice:

- For macOS/Windows development, build `wprsc` (default `winit` + `wgpu`) and the
  native `wprsd` backends if needed.
- For Linux deployment, build both `wprsc` and `wprsd`. Enable `wayland` for the
  compositor backend and `wayland-client` for the Wayland client backend.

### Source

```bash
cargo build --profile=release-lto  # or release, but debug is unusably slow
```

### Cross Compilation (cross)

This repo includes a `Cross.toml` with common Linux targets and the system
dependencies needed to build the Wayland components.

Examples:

```bash
# Linux x86_64
cross build --target x86_64-unknown-linux-gnu --profile=release-lto --bin wprsc

# Linux aarch64
cross build --target aarch64-unknown-linux-gnu --profile=release-lto --bin wprsc
```

On non-Linux hosts (for example macOS), the Wayland compositor backend is not
available; `wprsc` uses the cross-platform client backend by default. Build the
platform-native `wprsd` backends if needed:

```bash
cargo build --bin wprsc
cargo build --bin wprsd
```

You can also run a self-contained server demo that speaks the protocol and streams a
synthetic surface (no Wayland compositor / Smithay required):

```bash
cargo run --example wprsd_demo
```

Then connect to it with the cross-platform client backend:

```bash
cargo run --bin wprsc -- --socket /path/printed/by/demo.sock
```

Wayland-related components (`wprsc` Wayland backend and `wprsd` Wayland backend) require:

* libxkbcommon (-dev on debian)
* libwayland (-dev on debian)

The launcher (`wprs`) requires:

* python3
* psutil (python3-psutil on debian)
* ssh client

## Packaging

This repo includes packaging templates (Arch/Nix/Homebrew) and a local packaging script.

### Local Packaging Script

For local, repeatable packaging into per-target artifacts (archives + optional distro-native packages), use:

```bash
./scripts/package.sh
```

For `deb`/`rpm` outputs, the script uses a small Docker/Podman container with packaging tools installed from the distro repos.

Outputs are written under `dist/`.

### Arch-Linux (AUR)

Upstream wprs is available from the
[Arch User Repository](https://aur.archlinux.org/packages/wprs-git) as
`wprs-git`. wprsx includes packaging templates you can adapt if you need a
separate forked package.

## Usage

On the remote host, put the `wprsd.service` file into place:

```bash
mkdir -p ~/.config/systemd/user
cp package/wprsd.service ~/.config/systemd/user
```

and enable wprsd:

```bash
loginctl enable-linger
systemctl --user enable --now wprsd.service
```

On the local host:
```bash
# starts application on the remote host (starts ssh connection, forwards sockets, starts wprsc, runs application)
wprs <remote_host> run <application>

# stops local wprs connections, leaving remote session running (tear down ssh connection and forwarded sockets, stops wprsc)
wprs <remote_host> detach

# attaches to remote wprs session (starts ssh connection, forwards sockets, starts wprsc)
wprs <remote_host> attach
```

## System Tuning

Increasing Linux's socket buffer limits as described in
<https://wiki.archlinux.org/title/sysctl#Increase_the_memory_dedicated_to_the_network_interfaces>
can result in improved performance.

TODO: test SSH socket forwarding performance with different values of
wmem_default. wprsx uses setsockopt to increase its buffer size; verify whether
SSH forwarding applies similar buffering.

## Configuration Files

You can create configuration files for `wprsc` and `wprsd` instead of passing additional
arguments to `wprs`. To see what options are available, run `wprsc --help` and
`wprsd --help`.

To generate the default configs, run:
```bash
# on your local machine
wprsc --print-default-config-and-exit=true > ~/.config/wprs/wprsc.ron
```
and
```bash
# on your remote machine
wprsd --print-default-config-and-exit=true > ~/.config/wprs/wprsd.ron
```

Then update the `wprsc.ron` and `wprsd.ron` files with your desired settings.

### Running `wprsc` Without Wayland (Experimental)

`wprsc` can use a Wayland client backend or a cross-platform backend based on
`winit` + `wgpu`.

When `--backend auto` is selected (the default), `wprsc` prefers the Wayland
backend if a compositor is available; otherwise it falls back to the
`winit-wgpu` backend (when compiled with `winit-wgpu-client`). You can override
the selection with `--backend auto|wayland|winit-wgpu`.

If you compile `wprsc` with one of the `smithay_*` feature bundles, the same
bundle names are also accepted as `--backend` values (they currently alias to
`winit-wgpu`). See `docs/SmithayFeatures.md`.

```bash
cargo run --profile dev --bin wprsc
```

Keyboard behavior is configurable:

* `--keyboard-mode=keymap` (default): try to send an explicit XKB keymap to the
  server. By default `wprsc` will try to generate one at runtime using external
  tools (`setxkbmap` + `xkbcomp`). You can override by providing an explicit
  file via `--xkb-keymap-file=/path/to/keymap`.
* `--keyboard-mode=evdev`: send Linux evdev keycodes without sending a keymap.

Current limitations of the `winit` + `wgpu` backend:

* Subsurfaces are not fully supported.
* Input forwarding is best-effort (pointer/keyboard/gestures).
  Keyboard events are translated to Linux evdev keycodes and may be incomplete
  on non-Linux hosts.
* Touch input is not forwarded end-to-end (trackpad gestures are supported).

## Current Limitations

Protocol coverage is evolving. The WPRS protocol currently includes core
surface/buffer state, `xdg-shell`, decoration metadata, viewporter state, and
data-device/primary-selection events in `src/protocols/wprs`, but not every
Wayland protocol is implemented.

* Drag-and-drop support is best-effort and may be unreliable.
* XWayland selection/drag-and-drop bridging is still TODO.

Generally, wprsx will aim to support as many protocols as feasible; it is a
question of time and prioritization.

## Architecture

On the remote (server) side, `wprsd` can implement a Wayland compositor using
[Smithay](https://github.com/Smithay/smithay) when built with the `wayland`
feature. Instead of compositing and rendering, wprsd serializes the state of the
Wayland session and sends it to the connected wprsc client using a custom
protocol.

On the local (client) side, `wprsc` implements a Wayland client (using the
[Smithay Client Toolkit](https://github.com/Smithay/client-toolkit)) when built
with the `wayland-client` feature. It creates local Wayland objects that
correspond to remote Wayland objects. For example, if a remote application
running against wprsd creates a surface and an xdg-toplevel, wprsc will create a
surface with the same contents, an xdg-toplevel with the same metadata, etc.
From the local compositor's point of view, wprsc is just a normal application
with a bunch of windows. Input and other events from the local compositor that
wprsc receives are serialized and sent to wprsd, which forwards them to the
appropriate application (the owner of the surface which the wprsc surface which
received the events corresponds to).

wprsx supports session resumption across temporary disconnects. By default,
`wprsc` will automatically reconnect to `wprsd` (disable with
`wprsc --no-auto-reconnect`). The wayland protocol is not natively resumable in this way
because it relies on shared state between the compositor and client
applications. By implementing a wayland compositor locally relative to the
application, wprsd stores all state necessary for wayland applications and is
also able to store sufficient state (e.g., the buffer contents for each surface
as of the last commit) for a newly-connected wprsc to correctly set up all
necessary wayland objects. wprsc is stateless, but wprsd is not, so a wprsd
restart will still terminate all wayland applications running against it, like
with any other wayland compositor.

Communication between wprsd and wprsc happens over Unix domain sockets by
default; wprsd creates a socket and wprsc connects to it. The protocol also
supports TCP endpoints (and SSH tunnels via the `Endpoint` URI syntax). The
default mode of operation is to use SSH to forward a local socket to the remote
wprsd socket, but a different transport could be used with, for example, socat
or a custom proxy application. A launcher script (`wprs`) is provided which sets
up the SSH socket forwarding.

### Protocol

The custom protocol used to serialize and transmit wayland state between wprsc
and wprsd is a simplified version of the wayland protocol. Wayland objects are
represented as rust types and serialized using
[rkyv](https://github.com/rkyv/rkyv). Unlike the wayland protocol, the wprs
protocol tries to be idempotent when possible. For example, instead of the
repeated back-and-forth involved in creating a surface, creating an xdg-surface,
creating an xdg-toplevel, waiting for it to be configured, creating a buffer,
attaching the buffer, and committing it, wprsd will send a single commit message
to wprsc with the complete state of the surface (surface's attached buffer
contents (if any), its role (if any) and any associated metadata, etc.) and
wprsc will execute the appropriate dance with the local compositor.

Frame callbacks are scheduled locally by wprsd at the configured framerate, they
are not forwarded from wprsc as that would introduce an unacceptable amount of
frame latency due to network round-trips. When no wprsc is connected, wprsd
pauses sending frame callbacks to wayland applications.

Buffer compression is handled using a custom multithreaded and SIMD-accelerated
lossless image compression algorithm:

1. Transpose the image from an [array of structures to a struct of
   arrays](https://en.wikipedia.org/wiki/AoS_and_SoA). This makes the subsequent
   steps significantly faster by letting them be implemented with SIMD
   instructions and additionally improves the compression ratio because each
   color channel is more closely spatially correlated with itself than with the
   other
   channels.
2. Apply an adjacent (wrapping) difference to each color channel (differential
   pulse-code modulation). This improves the compression ratio by taking
   advantage of spatial correlation and transforms (for example) a solid-colored
   line into a single color byte and then a sequence of 0-bytes, or a gradient
   into a sequence of 1-bytes, etc.
3. Transform each color channel into a
   [YUV](https://en.wikipedia.org/wiki/Y%E2%80%B2UV)-like color space: `y := g,
   u := b - g, v := r - g, a := a`. This improves the compression ratio in a
   similar way as the previous step but by taking advantage of cross-color
   correlation.
4. Compress the data with zstd.

This algorithm was designed for good compression ratios while remaining fast;
performance depends on resolution and CPU. Decompression is done by inverting
those steps.

Protocol compatibility is not guaranteed across builds. A version handshake
warns when versions differ, but mixing binaries from different revisions (or
different dependency/rustc versions) may still break.

### Comparison to Waypipe

[Waypipe](https://gitlab.freedesktop.org/mstoeckl/waypipe)'s model is analogous
to X forwarding, while wprsx's model is analogous to Xpra. Waypipe forwards
Wayland protocol messages between the local compositor and the remote
application, so the client ends up being stateful and sessions are resumed via
network reconnections rather than client restarts. There are tradeoffs to the
two approaches. Waypipe's approach can be forward-compatible with newer Wayland
protocols, but those protocols may be unreliable if they use shared resources in
ways Waypipe does not handle. wprsx, on the other hand, requires explicit
implementation for each Wayland protocol.

### XWayland

XWayland support is provided only as a built-in integration in `wprsd`.

- Build with `--features xwayland`.
- Configure it under the Wayland config:
  - `wayland = { xwayland = {} }`

### HiDPI

wprsx tracks scale using Wayland-style semantics:

- `SurfaceState.buffer_scale` is the number of buffer pixels per logical point.
  For example, a Retina capture typically uses `buffer_scale = 2`.
- `wprsd` sends a `DisplayConfig` message on connect (currently used by capture
  backends to advertise a best-effort DPI/scale).

The `winit-wgpu` viewer backend periodically refreshes and re-sends local output
information to the server (so scale changes from moving between monitors can be
reflected without reconnecting).

For the macOS fullscreen capture backend, wprsd detects the main display scale
factor and reports it via both `DisplayConfig.scale_factor` and
`SurfaceState.buffer_scale`.

Override knobs:

- Server DPI (generic): set `display_dpi = Some(110)` in the `wprsd` config (or
  pass `--display-dpi 110`). This only affects backends that use it.
- Client-side scaling (generic): set `ui_scale_factor = 1.25` in the `wprsc`
  config (or pass `--ui-scale-factor 1.25`) to scale window sizes for
  cross-platform clients.
- Client output scale floor (winit-wgpu): set `min_output_scale_factor = Some(2)`
  in the `wprsc` config (or pass `--min-output-scale-factor 2`) to clamp the
  output scale reported to the server. On macOS, the default behavior is
  equivalent to `2` to avoid blurry rendering on Retina displays.

### Security

When using the Wayland backend, wprsd is a Wayland compositor, so it has access
to all surfaces displayed by applications running against it and it can inject
input into them. Any process which implements the wprs protocol and connects to
the wprsd socket will have
the same access. By default, the socket is created in `$XDG_RUNTIME_DIR` (or a
per-user directory under `/tmp` if `XDG_RUNTIME_DIR` is unset). Ensure that
directory permissions are restricted to the current user. Malicious applications
running as the same user as wprsd can still access this socket, but at that
point you have bigger problems.

wprsx does not do any auth of its own, it relies entirely on whatever transport
is being used (ssh, in the default case).

## Thanks

Huge thanks to the following excellent projects for making this project
significantly easier than it otherwise would have been:

* [Smithay](https://github.com/Smithay)
* [rkyv](https://github.com/rkyv/rkyv)
* [tracing](https://github.com/tokio-rs/tracing)
* [Tracy](https://github.com/wolfpld/tracy)

Thanks to [Waypipe](https://gitlab.freedesktop.org/mstoeckl/waypipe) and
[xwayland-proxy-virtwl](https://github.com/talex5/wayland-proxy-virtwl#xwayland-support)
for paving the way in this problem space.
