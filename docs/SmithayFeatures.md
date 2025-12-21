# Smithay Features in wprs

This repo uses [Smithay](https://crates.io/crates/smithay) as the Wayland compositor toolkit.
Smithay’s feature flags are fairly granular. In `wprs`, we provide a small set of **`smithay_*` convenience bundles** to make it easier to pick a backend/renderer combination.

## Terminology (Smithay)

- **Backend**: the platform integration used to host the compositor (DRM/KMS, X11, winit, …)
- **Renderer**: how pixels are rendered (OpenGL, glow, pixman software, multi-GPU)

In `Cargo.toml`, the `smithay_*` features are **just aliases** to upstream `smithay/<feature>` flags.

In `wprsc`, the same names are also accepted in the `backend` option (CLI/config). For now, these
values **alias to `winit-wgpu`** until a Smithay-based presentation backend exists.

## Upstream Smithay features (0.7.x)

These are the upstream building blocks (from `smithay 0.7.0`):

- Backends:
  - `smithay/backend_drm`
  - `smithay/backend_gbm`
  - `smithay/backend_egl`
  - `smithay/backend_libinput`
  - `smithay/backend_udev`
  - `smithay/backend_session_libseat` (Linux; requires system `libseat`)
  - `smithay/backend_x11`
  - `smithay/backend_winit`
  - `smithay/backend_vulkan` (plumbing; not a full “wgpu renderer”)
- Renderers:
  - `smithay/renderer_gl`
  - `smithay/renderer_glow`
  - `smithay/renderer_pixman`
  - `smithay/renderer_multi`
- Protocol/frontend:
  - `smithay/wayland_frontend`
  - `smithay/desktop`
  - `smithay/xwayland`

## wprs feature bundles

The following `wprs` crate features are intended to be “reasonable combos”:

- `smithay_wayland_frontend`
  - Minimal Wayland compositor protocol plumbing: `smithay/wayland_frontend` + `smithay/desktop`.

- `smithay_winit_gl_wayland`
  - Nested compositor (winit) + OpenGL renderer.

- `smithay_winit_glow_wayland`
  - Nested compositor (winit) + glow renderer.

- `smithay_x11_gl_wayland`
  - Nested compositor (X11) + OpenGL renderer.

- `smithay_x11_glow_wayland`
  - Nested compositor (X11) + glow renderer.

- `smithay_drm_gbm_egl_gl_wayland`
  - DRM/KMS backend with GBM/EGL + OpenGL renderer + libinput/udev/libseat session.

- `smithay_drm_gbm_egl_glow_wayland`
  - DRM/KMS backend with GBM/EGL + glow renderer + libinput/udev/libseat session.

- `smithay_drm_pixman_wayland`
  - DRM/KMS backend + pixman software renderer + libinput/udev/libseat session.

- `smithay_drm_multi_gpu_wayland`
  - DRM/KMS backend + multi-GPU renderer (builds on `smithay_drm_gbm_egl_gl_wayland`).

- `smithay_xwayland`
  - XWayland integration (requires `smithay_wayland_frontend`).

- `smithay_vulkan_support`
  - Enables Smithay Vulkan backend plumbing.

- `smithay_default_all`
  - Turns on upstream `smithay/default` (heavy; mostly Linux).

- `smithay_all_linux`
  - Curated “enable most Linux backends/renderers” bundle without using upstream `default`.
  - Requires system libraries when you include libseat/libinput/udev/DRM/GBM/EGL.

## System dependencies (Linux)

Some Smithay backends are gated by system libraries discoverable via `pkg-config`. If you enable DRM/GBM/libseat/libinput, you typically need:

- `libseat` (dev headers / `libseat.pc`)
- `libinput` (dev headers)
- `libudev`
- `libdrm`, `libgbm`, `EGL`/Mesa

Exact package names depend on your distro.

If you see build errors like `libseat-sys` failing with `libseat.pc not found`, it means the development package is missing (or `PKG_CONFIG_PATH` is not configured).

## Transport adaptation (WIP)

This repo has an experimental transport negotiation path for polling/capture-style backends:

- Client sends `Event::Transport(TransportEvent::ClientHello(...))`.
- Server may respond with `Request::Transport(TransportRequest::Config(...))` to adjust compression.
- `winit-wgpu` can also receive dirty-region patches (sub-rect updates) when enabled.

This is currently used to toggle between:

- `ShardedZstd { level }` (current default)
- `ShardedRaw` (no compression)

The server side uses best-effort RTT/bitrate hints and client CPU/GPU capability hints.
