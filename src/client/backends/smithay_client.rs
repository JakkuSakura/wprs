// Smithay compositor-style client backend.
//
// Current scope: winit GL/glow backends only. DRM backends require session/udev/libinput wiring
// and output scanning similar to Smithay's anvil udev backend, which is not yet integrated.
// X11 is intentionally not implemented.
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use std::collections::HashMap;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use std::time::Duration;

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::allocator::Fourcc;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource as SmithayAxisSource, InputEvent, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent,
};
#[cfg(feature = "smithay_winit_glow_wayland")]
use smithay::backend::renderer::glow::GlowRenderer;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::renderer::gles::GlesRenderer;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportMem, Renderer, Texture};
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::SwapBuffersError;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::backend::egl::EGLSurface;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::client::backend::ClientContext;
use crate::client::config::ClientBackend as ClientBackendKind;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use crate::client::config::KeyboardMode;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use crate::client::state::{drain_client_updates, ClientState};
use crate::prelude::*;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use crate::protocols::wprs as proto;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use crate::protocols::wprs::serializer::SendType;
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
use crate::protocols::wprs::wayland::{
    AxisScroll, AxisSource, KeyInner, KeyState, KeyboardEvent, ModifierState, PointerEvent,
    PointerEventKind, SurfaceState, WlSurfaceId,
};

#[derive(Debug, Clone, Copy)]
enum SmithayBackendKind {
    #[cfg(feature = "smithay_winit_gl_wayland")]
    WinitGl,
    #[cfg(feature = "smithay_winit_glow_wayland")]
    WinitGlow,
    #[cfg(feature = "smithay_x11_gl_wayland")]
    X11Gl,
    #[cfg(feature = "smithay_x11_glow_wayland")]
    X11Glow,
    #[cfg(feature = "smithay_drm_gbm_egl_gl_wayland")]
    DrmGbmEglGl,
    #[cfg(feature = "smithay_drm_gbm_egl_glow_wayland")]
    DrmGbmEglGlow,
    #[cfg(feature = "smithay_drm_pixman_wayland")]
    DrmPixman,
    #[cfg(feature = "smithay_drm_multi_gpu_wayland")]
    DrmMultiGpu,
}

#[derive(Debug)]
pub struct SmithayClientBackend {
    config: ClientBackendConfig,
    backend: SmithayBackendKind,
    name: &'static str,
}

impl SmithayClientBackend {
    pub fn new(
        config: ClientBackendConfig,
        backend: ClientBackendKind,
        name: &'static str,
    ) -> Result<Self> {
        let backend = match backend {
            #[cfg(feature = "smithay_winit_gl_wayland")]
            ClientBackendKind::SmithayWinitGlWayland => SmithayBackendKind::WinitGl,
            #[cfg(feature = "smithay_winit_glow_wayland")]
            ClientBackendKind::SmithayWinitGlowWayland => SmithayBackendKind::WinitGlow,
            #[cfg(feature = "smithay_x11_gl_wayland")]
            ClientBackendKind::SmithayX11GlWayland => SmithayBackendKind::X11Gl,
            #[cfg(feature = "smithay_x11_glow_wayland")]
            ClientBackendKind::SmithayX11GlowWayland => SmithayBackendKind::X11Glow,
            #[cfg(feature = "smithay_drm_gbm_egl_gl_wayland")]
            ClientBackendKind::SmithayDrmGbmEglGlWayland => SmithayBackendKind::DrmGbmEglGl,
            #[cfg(feature = "smithay_drm_gbm_egl_glow_wayland")]
            ClientBackendKind::SmithayDrmGbmEglGlowWayland => SmithayBackendKind::DrmGbmEglGlow,
            #[cfg(feature = "smithay_drm_pixman_wayland")]
            ClientBackendKind::SmithayDrmPixmanWayland => SmithayBackendKind::DrmPixman,
            #[cfg(feature = "smithay_drm_multi_gpu_wayland")]
            ClientBackendKind::SmithayDrmMultiGpuWayland => SmithayBackendKind::DrmMultiGpu,
            _ => {
                return Err(Error::InvalidArgument(
                    "smithay backend requested without smithay variant".to_string(),
                ))
            }
        };
        Ok(Self {
            config,
            backend,
            name,
        })
    }
}

impl ClientBackend for SmithayClientBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(self: Box<Self>, ctx: ClientContext) -> Result<()> {
        match self.backend {
            #[cfg(feature = "smithay_winit_gl_wayland")]
            SmithayBackendKind::WinitGl => {
                run_winit::<GlesRenderer>(ctx, self.config).location(loc!())
            }
            #[cfg(feature = "smithay_winit_glow_wayland")]
            SmithayBackendKind::WinitGlow => {
                run_winit::<GlowRenderer>(ctx, self.config).location(loc!())
            }
            #[cfg(feature = "smithay_x11_gl_wayland")]
            SmithayBackendKind::X11Gl => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (X11 backend intentionally unsupported)",
                self.name
            ))),
            #[cfg(feature = "smithay_x11_glow_wayland")]
            SmithayBackendKind::X11Glow => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (X11 backend intentionally unsupported)",
                self.name
            ))),
            #[cfg(feature = "smithay_drm_gbm_egl_gl_wayland")]
            SmithayBackendKind::DrmGbmEglGl => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (DRM backends need session/udev/libinput + output scanning)",
                self.name
            ))),
            #[cfg(feature = "smithay_drm_gbm_egl_glow_wayland")]
            SmithayBackendKind::DrmGbmEglGlow => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (DRM backends need session/udev/libinput + output scanning)",
                self.name
            ))),
            #[cfg(feature = "smithay_drm_pixman_wayland")]
            SmithayBackendKind::DrmPixman => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (DRM backends need session/udev/libinput + output scanning)",
                self.name
            ))),
            #[cfg(feature = "smithay_drm_multi_gpu_wayland")]
            SmithayBackendKind::DrmMultiGpu => Err(Error::Unsupported(format!(
                "{} backend is not implemented in the Smithay client compositor (DRM backends need session/udev/libinput + output scanning)",
                self.name
            ))),
        }
    }
}

#[derive(Debug)]
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
struct SurfaceEntry<T> {
    id: WlSurfaceId,
    metadata: crate::protocols::wprs::wayland::BufferMetadata,
    data: Vec<u8>,
    texture: Option<T>,
    dirty: bool,
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
impl<T> SurfaceEntry<T> {
    fn size(&self) -> Size<i32, Buffer> {
        Size::from((self.metadata.width, self.metadata.height))
    }
}

#[derive(Debug, Clone, Copy)]
#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
struct SurfaceLayout {
    id: WlSurfaceId,
    rect: Rectangle<i32, Physical>,
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
struct CompositorState<R>
where
    R: Renderer + ImportMem,
{
    serializer: proto::serializer::Serializer<proto::types::Event, proto::types::Request>,
    state: std::sync::Arc<ClientState>,
    notify_rx: std::sync::mpsc::Receiver<()>,
    surfaces: HashMap<WlSurfaceId, SurfaceEntry<R::TextureId>>,
    layout: Vec<SurfaceLayout>,
    hovered_surface: Option<WlSurfaceId>,
    focused_surface: Option<WlSurfaceId>,
    serial_counter: u32,
    pressed_keycodes: std::collections::HashSet<u32>,
    last_cursor_pos: (f64, f64),
    window_size: Size<i32, Physical>,
    keyboard_mode: KeyboardMode,
    xkb_keymap_file: Option<std::path::PathBuf>,
    keymap_sent: bool,
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
impl<R> CompositorState<R>
where
    R: Renderer + ImportMem,
{
    fn new(
        ctx: ClientContext,
        keyboard_mode: KeyboardMode,
        xkb_keymap_file: Option<std::path::PathBuf>,
        window_size: Size<i32, Physical>,
    ) -> Self {
        Self {
            serializer: ctx.serializer,
            state: ctx.state,
            notify_rx: ctx.notify_rx,
            surfaces: HashMap::new(),
            layout: Vec::new(),
            hovered_surface: None,
            focused_surface: None,
            serial_counter: 1,
            pressed_keycodes: std::collections::HashSet::new(),
            last_cursor_pos: (0.0, 0.0),
            window_size,
            keyboard_mode,
            xkb_keymap_file,
            keymap_sent: false,
        }
    }

    fn next_serial(&mut self) -> u32 {
        self.serial_counter = self.serial_counter.wrapping_add(1);
        if self.serial_counter == 0 {
            self.serial_counter = 1;
        }
        self.serial_counter
    }

    fn maybe_send_keymap(&mut self) {
        if self.keymap_sent {
            return;
        }
        self.keymap_sent = true;

        if self.keyboard_mode != KeyboardMode::Keymap {
            return;
        }

        let keymap = if let Some(path) = self.xkb_keymap_file.as_ref() {
            match std::fs::read_to_string(path).with_context(loc!(), || {
                format!("failed to read xkb keymap file {path:?}")
            }) {
                Ok(keymap) => keymap,
                Err(err) => {
                    warn!("{err:?}; continuing with evdev mapping");
                    return;
                }
            }
        } else {
            match generate_keymap_from_tools() {
                Ok(keymap) => keymap,
                Err(err) => {
                    warn!(
                        "failed to generate xkb keymap via tools: {err:?}; continuing with evdev mapping"
                    );
                    return;
                }
            }
        };

        self.serializer
            .writer()
            .send(SendType::Object(proto::types::Event::KeyboardEvent(
                KeyboardEvent::Keymap(keymap),
            )));
    }

    fn set_keyboard_focus(&mut self, surface_id: Option<WlSurfaceId>) {
        self.maybe_send_keymap();
        if self.focused_surface == surface_id {
            return;
        }

        if self.focused_surface.is_some() {
            let serial = self.next_serial();
            self.serializer
                .writer()
                .send(SendType::Object(proto::types::Event::KeyboardEvent(
                    KeyboardEvent::Leave { serial },
                )));
        }

        self.focused_surface = surface_id;

        if let Some(surface_id) = surface_id {
            let mut keycodes: Vec<u32> = self.pressed_keycodes.iter().copied().collect();
            keycodes.sort_unstable();
            let serial = self.next_serial();
            self.serializer
                .writer()
                .send(SendType::Object(proto::types::Event::KeyboardEvent(
                    KeyboardEvent::Enter {
                        serial,
                        surface_id,
                        keycodes,
                        keysyms: Vec::new(),
                    },
                )));
        }
    }

    fn send_key(&mut self, keycode: u32, state: KeyState) {
        if self.focused_surface.is_none() {
            return;
        }
        let serial = self.next_serial();
        self.serializer
            .writer()
            .send(SendType::Object(proto::types::Event::KeyboardEvent(
                KeyboardEvent::Key(KeyInner {
                    serial,
                    raw_code: keycode,
                    state,
                }),
            )));
    }

    fn send_modifiers(&mut self, modifiers: smithay::backend::input::ModifiersState) {
        self.serializer
            .writer()
            .send(SendType::Object(proto::types::Event::KeyboardEvent(
                KeyboardEvent::Modifiers {
                    modifier_state: ModifierState {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        caps_lock: modifiers.caps_lock,
                        logo: modifiers.logo,
                        num_lock: modifiers.num_lock,
                    },
                    layout_index: 0,
                },
            )));
    }

    fn send_pointer_event(
        &mut self,
        surface_id: WlSurfaceId,
        position: crate::protocols::wprs::geometry::Point<f64>,
        kind: PointerEventKind,
    ) {
        self.serializer
            .writer()
            .send(SendType::Object(proto::types::Event::PointerFrame(vec![
                PointerEvent {
                    surface_id,
                    position,
                    kind,
                },
            ])));
    }

    fn update_hover(&mut self, surface_id: Option<WlSurfaceId>, global: (f64, f64)) {
        if self.hovered_surface == surface_id {
            return;
        }

        if let Some(prev) = self.hovered_surface {
            let (local_x, local_y) = self.local_pos_for(prev, global.0, global.1);
            let serial = self.next_serial();
            let pos = crate::protocols::wprs::geometry::Point {
                x: local_x,
                y: local_y,
            };
            self.send_pointer_event(prev, pos, PointerEventKind::Leave { serial });
        }

        self.hovered_surface = surface_id;

        if let Some(surface_id) = surface_id {
            let (local_x, local_y) = self.local_pos_for(surface_id, global.0, global.1);
            let serial = self.next_serial();
            let pos = crate::protocols::wprs::geometry::Point {
                x: local_x,
                y: local_y,
            };
            self.send_pointer_event(surface_id, pos, PointerEventKind::Enter { serial });
        }
    }

    fn apply_surface_state(&mut self, surface: &SurfaceState) {
        if let Some(bitmap) = surface.bitmap.as_ref() {
            match bitmap {
                crate::models::surface::BitmapAssignment::New(bitmap) => {
                    let entry = SurfaceEntry {
                        id: surface.id,
                        metadata: bitmap.metadata,
                        data: bitmap.bytes().to_vec(),
                        texture: None,
                        dirty: true,
                    };
                    self.surfaces.insert(surface.id, entry);
                }
                crate::models::surface::BitmapAssignment::Removed => {
                    self.surfaces.remove(&surface.id);
                }
            }
        }

        if let Some(update) = surface.bitmap_update.as_ref() {
            if let Some(entry) = self.surfaces.get_mut(&surface.id) {
                apply_patch(entry, update);
                entry.dirty = true;
            } else {
                warn!("bitmap patch without base bitmap for surface {:?}", surface.id);
            }
        }
    }

    fn recompute_layout(&mut self) {
        let mut layouts = Vec::new();
        let padding = 8;
        let mut x = 0;
        let mut y = 0;
        let mut row_height = 0;
        let max_width = self.window_size.w.max(1);

        let mut surfaces: Vec<_> = self
            .surfaces
            .values()
            .map(|entry| (entry.id, entry.metadata.width, entry.metadata.height))
            .collect();
        surfaces.sort_by_key(|(id, _, _)| id.0);

        for (id, width, height) in surfaces {
            let width = width.max(1);
            let height = height.max(1);
            if x + width > max_width && x > 0 {
                x = 0;
                y += row_height + padding;
                row_height = 0;
            }
            layouts.push(SurfaceLayout {
                id,
                rect: Rectangle::new((x, y).into(), (width, height).into()),
            });
            x += width + padding;
            row_height = row_height.max(height);
        }

        self.layout = layouts;
    }

    fn surface_at(&self, x: f64, y: f64) -> Option<(WlSurfaceId, f64, f64)> {
        for layout in self.layout.iter().rev() {
            let rect = layout.rect;
            let rx = rect.loc.x as f64;
            let ry = rect.loc.y as f64;
            let rw = rect.size.w as f64;
            let rh = rect.size.h as f64;
            if x >= rx && x <= rx + rw && y >= ry && y <= ry + rh {
                return Some((layout.id, x - rx, y - ry));
            }
        }
        None
    }

    fn local_pos_for(&self, surface_id: WlSurfaceId, x: f64, y: f64) -> (f64, f64) {
        for layout in &self.layout {
            if layout.id == surface_id {
                let rect = layout.rect;
                return (x - rect.loc.x as f64, y - rect.loc.y as f64);
            }
        }
        (x, y)
    }

    fn update_from_client(&mut self) -> Result<bool> {
        let Some(batch) = drain_client_updates(&self.notify_rx, &self.state).location(loc!())? else {
            return Ok(false);
        };
        let mut changed = false;

        for surface in batch.surfaces.updated {
            self.apply_surface_state(&surface);
            changed = true;
        }
        for removed in batch.surfaces.removed {
            self.surfaces.remove(&removed.surface);
            changed = true;
        }

        if changed {
            self.recompute_layout();
        }

        Ok(changed)
    }

    fn handle_winit_event(&mut self, event: WinitEvent) -> bool {
        match event {
            WinitEvent::Resized { size, .. } => {
                self.window_size = size;
                self.recompute_layout();
                true
            }
            WinitEvent::Redraw => true,
            WinitEvent::CloseRequested => {
                info!("Smithay winit backend: close requested");
                std::process::exit(0);
            }
            WinitEvent::Focus(focused) => {
                if !focused {
                    self.set_keyboard_focus(None);
                }
                true
            }
            WinitEvent::Input(input) => self.handle_input(input),
        }
    }

    fn handle_input(&mut self, input: InputEvent<smithay::backend::winit::WinitInput>) -> bool {
        match input {
            InputEvent::PointerMotionAbsolute { event } => {
                let x = event.x();
                let y = event.y();
                self.last_cursor_pos = (x, y);
                if let Some((surface_id, local_x, local_y)) = self.surface_at(x, y) {
                    let pos = crate::protocols::wprs::geometry::Point {
                        x: local_x,
                        y: local_y,
                    };
                    self.update_hover(Some(surface_id), (x, y));
                    self.send_pointer_event(surface_id, pos, PointerEventKind::Motion);
                } else {
                    self.update_hover(None, (x, y));
                }
                true
            }
            InputEvent::PointerButton { event } => {
                let button = event.button_code();
                let state = event.state();
                let (x, y) = self.last_cursor_pos;
                if let Some((surface_id, local_x, local_y)) = self.surface_at(x, y) {
                    let pos = crate::protocols::wprs::geometry::Point {
                        x: local_x,
                        y: local_y,
                    };
                    if state == smithay::backend::input::ButtonState::Pressed {
                        self.set_keyboard_focus(Some(surface_id));
                        let serial = self.next_serial();
                        self.send_pointer_event(
                            surface_id,
                            pos,
                            PointerEventKind::Press { button, serial },
                        );
                    } else {
                        let serial = self.next_serial();
                        self.send_pointer_event(
                            surface_id,
                            pos,
                            PointerEventKind::Release { button, serial },
                        );
                    }
                }
                true
            }
            InputEvent::PointerAxis { event } => {
                let (x, y) = self.last_cursor_pos;
                if let Some((surface_id, local_x, local_y)) = self.surface_at(x, y) {
                    let pos = crate::protocols::wprs::geometry::Point {
                        x: local_x,
                        y: local_y,
                    };
                    let horizontal = axis_scroll_for(&event, Axis::Horizontal);
                    let vertical = axis_scroll_for(&event, Axis::Vertical);
                    let source = match event.source() {
                        SmithayAxisSource::Wheel => Some(AxisSource::Wheel),
                        SmithayAxisSource::Finger => Some(AxisSource::Finger),
                        SmithayAxisSource::Continuous => Some(AxisSource::Continuous),
                        SmithayAxisSource::WheelTilt => Some(AxisSource::WheelTilt),
                        _ => None,
                    };
                    self.send_pointer_event(
                        surface_id,
                        pos,
                        PointerEventKind::Axis {
                            horizontal,
                            vertical,
                            source,
                        },
                    );
                }
                true
            }
            InputEvent::Keyboard { event } => {
                let raw = event.key_code().raw();
                let pressed = event.state() == smithay::backend::input::KeyState::Pressed;
                if pressed {
                    self.pressed_keycodes.insert(raw);
                    self.send_key(raw, KeyState::Pressed);
                } else {
                    self.pressed_keycodes.remove(&raw);
                    self.send_key(raw, KeyState::Released);
                }
                true
            }
            InputEvent::Modifiers { event } => {
                self.send_modifiers(event.state());
                true
            }
            _ => false,
        }
    }
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn axis_scroll_for(
    event: &impl PointerAxisEvent<smithay::backend::winit::WinitInput>,
    axis: Axis,
) -> AxisScroll {
    let absolute = event.amount(axis).unwrap_or(0.0);
    let discrete = event
        .amount_v120(axis)
        .map(|v| (v / 120.0).round() as i32)
        .unwrap_or(0);
    AxisScroll {
        absolute,
        discrete,
        stop: false,
    }
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn apply_patch<T>(entry: &mut SurfaceEntry<T>, update: &crate::models::surface::BitmapUpdate) {
    let crate::models::surface::BitmapUpdate::Patch {
        x,
        y,
        width,
        height,
        stride,
        data,
    } = update;
    if entry.metadata.stride != *stride {
        warn!("bitmap patch stride mismatch for surface {:?}", entry.id);
    }
    let bytes_per_row = entry.metadata.stride as usize;
    let patch_bytes_per_row = *stride as usize;
    let width_bytes = (*width as usize).saturating_mul(4);
    for row in 0..(*height as usize) {
        let dst_y = *y as usize + row;
        let dst_x = *x as usize * 4;
        let dst_offset = dst_y * bytes_per_row + dst_x;
        let src_offset = row * patch_bytes_per_row;
        if dst_offset + width_bytes > entry.data.len() {
            break;
        }
        if src_offset + width_bytes > data.len() {
            break;
        }
        entry.data[dst_offset..dst_offset + width_bytes]
            .copy_from_slice(&data.as_slice()[src_offset..src_offset + width_bytes]);
    }
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn pack_buffer(metadata: &crate::protocols::wprs::wayland::BufferMetadata, data: &[u8]) -> Vec<u8> {
    let row_bytes = (metadata.width as usize).saturating_mul(4);
    let stride = metadata.stride as usize;
    if stride == row_bytes {
        return data.to_vec();
    }
    let mut packed = vec![0u8; row_bytes * metadata.height as usize];
    for row in 0..metadata.height as usize {
        let src_offset = row * stride;
        let dst_offset = row * row_bytes;
        if src_offset + row_bytes > data.len() {
            break;
        }
        packed[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&data[src_offset..src_offset + row_bytes]);
    }
    packed
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn fourcc_for(format: crate::models::surface::BufferFormat) -> Fourcc {
    match format {
        crate::models::surface::BufferFormat::Argb8888 => Fourcc::Argb8888,
        crate::models::surface::BufferFormat::Xrgb8888 => Fourcc::Xrgb8888,
    }
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn render_scene<R>(
    backend: &mut WinitGraphicsBackend<R>,
    state: &mut CompositorState<R>,
) -> Result<()>
where
    R: Renderer + ImportMem + Bind<EGLSurface>,
    SwapBuffersError: From<R::Error>,
{
    let (renderer, mut framebuffer) = backend.bind().location(loc!())?;
    let output_size = backend.window_size();
    let mut frame = renderer
        .render(&mut framebuffer, output_size, Transform::Normal)
        .location(loc!())?;

    frame
        .clear(
            Color32F::new(0.08, 0.08, 0.08, 1.0),
            &[Rectangle::from_size(output_size)],
        )
        .location(loc!())?;

    for layout in state.layout.iter() {
        let Some(entry) = state.surfaces.get_mut(&layout.id) else {
            continue;
        };

        if entry.dirty {
            let packed = pack_buffer(&entry.metadata, &entry.data);
            let size = entry.size();
            let format = fourcc_for(entry.metadata.format);
            if let Some(texture) = entry.texture.as_ref() {
                if texture.size() == size {
                    renderer
                        .update_memory(texture, &packed, Rectangle::from_size(size))
                        .location(loc!())?;
                } else {
                    entry.texture = None;
                }
            }
            if entry.texture.is_none() {
                let texture = renderer
                    .import_memory(&packed, format, size, false)
                    .location(loc!())?;
                entry.texture = Some(texture);
            }
            entry.dirty = false;
        }

        let Some(texture) = entry.texture.as_ref() else {
            continue;
        };

        let src = Rectangle::from_size(texture.size()).to_f64();
        let dst = layout.rect;
        frame
            .render_texture_from_to(
                texture,
                src,
                dst,
                &[dst],
                &[],
                Transform::Normal,
                1.0,
            )
            .location(loc!())?;
    }

    frame.finish().location(loc!())?;
    backend.submit(None).location(loc!())?;
    Ok(())
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn run_winit<R>(ctx: ClientContext, config: ClientBackendConfig) -> Result<()>
where
    R: Renderer + ImportMem + Bind<EGLSurface> + From<GlesRenderer>,
    SwapBuffersError: From<R::Error>,
{
    let (mut backend, mut event_loop) = winit::init::<R>().location(loc!())?;
    let window_size = backend.window_size();

    let mut state = CompositorState::<R>::new(
        ctx,
        config.keyboard_mode,
        config.xkb_keymap_file.clone(),
        window_size,
    );

    let mut needs_redraw = true;
    loop {
        event_loop.dispatch_new_events(|event| {
            if state.handle_winit_event(event) {
                needs_redraw = true;
            }
        });

        if state.update_from_client().location(loc!())? {
            needs_redraw = true;
        }

        if needs_redraw {
            render_scene(&mut backend, &mut state).location(loc!())?;
            needs_redraw = false;
        }

        std::thread::sleep(Duration::from_millis(4));
    }
}

#[cfg(any(feature = "smithay_winit_gl_wayland", feature = "smithay_winit_glow_wayland"))]
fn generate_keymap_from_tools() -> Result<String> {
    let xkbcomp = std::process::Command::new("xkbcomp")
        .arg("-xkb")
        .arg("-")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| Error::Internal(format!("xkbcomp spawn failed: {err:?}")))?;
    let output = xkbcomp
        .wait_with_output()
        .map_err(|err| Error::Internal(format!("xkbcomp wait failed: {err:?}")))?;
    if !output.status.success() {
        bail!(Error::Internal(format!(
            "xkbcomp -xkb - - failed: {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8(output.stdout).location(loc!())?)
}
