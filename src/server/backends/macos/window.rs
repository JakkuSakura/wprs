use std::collections::HashMap;
use std::sync::Arc;

use crate::prelude::*;
use crate::protocols::wprs::Capabilities;
use crate::protocols::wprs::ClientId;
use crate::protocols::wprs::DisplayConfig;
use crate::protocols::wprs::Event;
use crate::protocols::wprs::wayland;
use crate::protocols::wprs::wayland::Buffer;
use crate::protocols::wprs::wayland::BufferAssignment;
use crate::protocols::wprs::wayland::BufferData;
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::PointerEventKind;
use crate::protocols::wprs::wayland::SurfaceState;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::protocols::wprs::xdg_shell;
use crate::server::runtime::backend::BackendObservation;
use crate::server::runtime::backend::PollingBackend;
use crate::server::runtime::backend::SurfaceSnapshot;

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosWindowBackendConfig {
    pub dpi: Option<u32>,
    pub target_pid: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct WindowBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone)]
struct TrackedWindow {
    bounds: WindowBounds,
}

#[derive(Debug)]
pub struct MacosWindowBackend {
    display_config: DisplayConfig,
    windows: HashMap<u32, TrackedWindow>,
    pressed_buttons: u32,
    target_pid: Option<u32>,
}

impl MacosWindowBackend {
    pub fn new(config: MacosWindowBackendConfig) -> Self {
        let (detected_scale_factor, detected_dpi) =
            display_scale_factor_and_dpi().unwrap_or((1, None));
        let scale_factor = detected_scale_factor.max(1);
        let dpi = config.dpi.or(detected_dpi);
        let display_config = DisplayConfig { scale_factor, dpi };

        Self {
            display_config,
            windows: HashMap::new(),
            pressed_buttons: 0,
            target_pid: config.target_pid,
        }
    }

    fn surface_state_for_window(
        &self,
        window_id: u32,
        title: &str,
        app_id: &str,
        metadata: BufferMetadata,
    ) -> SurfaceState {
        let toplevel = xdg_shell::XdgToplevelState {
            id: xdg_shell::XdgToplevelId(window_id as u64),
            parent: None,
            title: Some(title.to_string()),
            app_id: Some(app_id.to_string()),
            decoration_mode: None,
            maximized: None,
            fullscreen: None,
        };

        SurfaceState {
            client: ClientId(1),
            id: WlSurfaceId(window_id as u64),
            buffer: Some(BufferAssignment::New(Buffer {
                metadata,
                data: BufferData::External,
            })),
            buffer_update: None,
            role: Some(wayland::Role::XdgToplevel(toplevel)),
            buffer_scale: self.display_config.scale_factor,
            buffer_transform: None,
            opaque_region: None,
            input_region: None,
            z_ordered_children: Vec::new(),
            damage: None,
            output_ids: Vec::new(),
            viewport_state: None,
            xdg_surface_state: Some(xdg_shell::XdgSurfaceState::default()),
        }
    }

    fn handle_pointer_event(&mut self, e: wayland::PointerEvent) -> Result<()> {
        let window_id = e.surface_id.0 as u32;
        let Some(tracked) = self.windows.get(&window_id) else {
            return Ok(());
        };

        let local_x = e.position.x.clamp(0.0, tracked.bounds.width.max(1.0) - 1.0);
        let local_y = e
            .position
            .y
            .clamp(0.0, tracked.bounds.height.max(1.0) - 1.0);

        let x = (tracked.bounds.x + local_x).round();
        // Convert from (0,0)=top-left surface-local to Quartz global coords (0,0)=bottom-left.
        let y = (tracked.bounds.y + tracked.bounds.height - local_y).round();

        match e.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Leave { .. } => Ok(()),
            PointerEventKind::Motion => {
                post_mouse_motion(self.pressed_buttons, x, y).location(loc!())
            },
            PointerEventKind::Press { button, .. } => {
                let mask = button_mask(button);
                self.pressed_buttons |= mask;
                post_mouse_button(true, button, x, y).location(loc!())
            },
            PointerEventKind::Release { button, .. } => {
                let mask = button_mask(button);
                self.pressed_buttons &= !mask;
                post_mouse_button(false, button, x, y).location(loc!())
            },
            PointerEventKind::Axis { .. } => Ok(()),
        }
    }
}

impl PollingBackend for MacosWindowBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities { xwayland: false }
    }

    fn display_config(&self) -> DisplayConfig {
        self.display_config.clone()
    }

    fn initial_snapshot(&mut self) -> Result<Vec<SurfaceSnapshot>> {
        let mut out = Vec::new();

        let windows = list_windows().location(loc!())?;
        for w in windows {
            if let Some(pid) = self.target_pid {
                if w.owner_pid != pid {
                    continue;
                }
            }
            // Capture once to get the initial window size.
            let (metadata, _bgra) = capture_window_bgra(w.window_id).location(loc!())?;
            self.windows
                .insert(w.window_id, TrackedWindow { bounds: w.bounds });

            out.push(SurfaceSnapshot {
                state: self.surface_state_for_window(w.window_id, &w.title, &w.app_id, metadata),
            });
        }

        Ok(out)
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        let mut out = Vec::new();

        let windows = list_windows().location(loc!())?;
        let windows: Vec<WindowInfo> = if let Some(pid) = self.target_pid {
            windows.into_iter().filter(|w| w.owner_pid == pid).collect()
        } else {
            windows
        };
        let mut seen = std::collections::HashSet::new();
        for w in &windows {
            seen.insert(w.window_id);
        }

        // Emit destroys for windows that disappeared.
        let mut removed = Vec::new();
        for id in self.windows.keys().copied() {
            if !seen.contains(&id) {
                removed.push(id);
            }
        }
        for id in removed {
            self.windows.remove(&id);
            out.push(BackendObservation::SurfaceDestroyed {
                client: ClientId(1),
                surface: WlSurfaceId(id as u64),
            });
        }

        // Emit commits for currently-visible windows.
        for w in windows {
            let (metadata, bgra) = capture_window_bgra(w.window_id).location(loc!())?;
            self.windows
                .insert(w.window_id, TrackedWindow { bounds: w.bounds });
            let state = self.surface_state_for_window(w.window_id, &w.title, &w.app_id, metadata);

            out.push(BackendObservation::SurfaceCommit {
                state,
                bgra: Some(Arc::from(bgra.into_boxed_slice())),
            });
        }

        Ok(out)
    }

    fn handle_client_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::PointerFrame(events) => {
                for e in events {
                    self.handle_pointer_event(e).log_and_ignore(loc!());
                }
            },
            _ => {},
        }
        Ok(())
    }
}

fn button_mask(button: u32) -> u32 {
    // Linux input-event-codes: BTN_LEFT=272, BTN_RIGHT=273, BTN_MIDDLE=274
    match button {
        272 => 1 << 0,
        273 => 1 << 1,
        274 => 1 << 2,
        _ => 0,
    }
}

fn display_scale_factor_and_dpi() -> Result<(i32, Option<u32>)> {
    #[cfg(target_os = "macos")]
    {
        macos::main_display_scale_factor_and_dpi().location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok((1, None))
    }
}

#[derive(Debug, Clone)]
struct WindowInfo {
    window_id: u32,
    title: String,
    app_id: String,
    owner_pid: u32,
    bounds: WindowBounds,
}

fn list_windows() -> Result<Vec<WindowInfo>> {
    #[cfg(target_os = "macos")]
    {
        macos::list_windows().location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        bail!("macOS window capture backend is only supported on macOS")
    }
}

fn capture_window_bgra(window_id: u32) -> Result<(BufferMetadata, Vec<u8>)> {
    #[cfg(target_os = "macos")]
    {
        macos::capture_window_bgra(window_id).location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window_id;
        bail!("macOS window capture backend is only supported on macOS")
    }
}

fn post_mouse_motion(button_mask: u32, x: f64, y: f64) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::post_mouse_motion(button_mask, x, y).location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (button_mask, x, y);
        Ok(())
    }
}

fn post_mouse_button(down: bool, button: u32, x: f64, y: f64) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::post_mouse_button(down, button, x, y).location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (down, button, x, y);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use anyhow::ensure;
    use std::ffi::c_void;
    use std::ptr;

    type Boolean = u8;
    type CFIndex = isize;
    type CFTypeRef = *const c_void;
    type CFArrayRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFNumberRef = *const c_void;
    type CFDataRef = *const c_void;
    type CGImageRef = *const c_void;
    type CGDataProviderRef = *const c_void;
    type CGWindowID = u32;
    type CGEventRef = *const c_void;
    type CGEventType = u32;
    type CGMouseButton = u32;
    type CGEventTapLocation = u32;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyDisplayMode(display_id: u32) -> *const c_void;
        fn CGDisplayModeGetWidth(mode: *const c_void) -> usize;
        fn CGDisplayModeGetHeight(mode: *const c_void) -> usize;
        fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
        fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize;
        fn CGDisplayScreenSize(display_id: u32) -> CGSize;

        fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: CGWindowID) -> CFArrayRef;
        fn CGWindowListCreateImage(
            rect: CGRect,
            option: u32,
            window_id: CGWindowID,
            image_option: u32,
        ) -> CGImageRef;
        fn CGRectMakeWithDictionaryRepresentation(
            dict: CFDictionaryRef,
            rect: *mut CGRect,
        ) -> Boolean;

        fn CGImageGetWidth(image: CGImageRef) -> usize;
        fn CGImageGetHeight(image: CGImageRef) -> usize;
        fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
        fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
        fn CGImageGetBitsPerComponent(image: CGImageRef) -> usize;
        fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
        fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;

        fn CGEventCreateMouseEvent(
            source: *const c_void,
            event_type: CGEventType,
            mouse_cursor_position: CGPoint,
            mouse_button: CGMouseButton,
        ) -> CGEventRef;

        fn CGEventPost(tap: CGEventTapLocation, event: CGEventRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(the_array: CFArrayRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: CFIndex) -> *const c_void;
        fn CFDictionaryGetValue(the_dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
        fn CFStringGetCStringPtr(the_string: CFStringRef, encoding: u32) -> *const u8;
        fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut u8,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> Boolean;
        fn CFNumberGetValue(number: CFNumberRef, the_type: i32, value_ptr: *mut c_void) -> Boolean;
        fn CFDataGetLength(the_data: CFDataRef) -> CFIndex;
        fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
        fn CFRelease(cf: CFTypeRef);
    }

    // CFStringRef keys exported by CoreGraphics.
    unsafe extern "C" {
        static kCGWindowNumber: CFStringRef;
        static kCGWindowOwnerPID: CFStringRef;
        static kCGWindowOwnerName: CFStringRef;
        static kCGWindowName: CFStringRef;
        static kCGWindowBounds: CFStringRef;
        static kCGWindowLayer: CFStringRef;
        static kCGWindowAlpha: CFStringRef;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
    const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 16;
    const K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 8;
    const K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING: u32 = 1;
    const K_CG_WINDOW_IMAGE_BEST_RESOLUTION: u32 = 2;

    const K_CG_EVENT_TAP_HID: CGEventTapLocation = 0;

    const K_CG_EVENT_MOUSE_MOVED: CGEventType = 5;
    const K_CG_EVENT_LEFT_MOUSE_DOWN: CGEventType = 1;
    const K_CG_EVENT_LEFT_MOUSE_UP: CGEventType = 2;
    const K_CG_EVENT_RIGHT_MOUSE_DOWN: CGEventType = 3;
    const K_CG_EVENT_RIGHT_MOUSE_UP: CGEventType = 4;
    const K_CG_EVENT_OTHER_MOUSE_DOWN: CGEventType = 25;
    const K_CG_EVENT_OTHER_MOUSE_UP: CGEventType = 26;
    const K_CG_EVENT_LEFT_MOUSE_DRAGGED: CGEventType = 6;
    const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: CGEventType = 7;
    const K_CG_EVENT_OTHER_MOUSE_DRAGGED: CGEventType = 27;

    const K_CG_MOUSE_BUTTON_LEFT: CGMouseButton = 0;
    const K_CG_MOUSE_BUTTON_RIGHT: CGMouseButton = 1;
    const K_CG_MOUSE_BUTTON_CENTER: CGMouseButton = 2;

    pub(super) fn list_windows() -> Result<Vec<WindowInfo>> {
        unsafe {
            let options =
                K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS;
            let array = CGWindowListCopyWindowInfo(options, 0);
            ensure!(!array.is_null(), "CGWindowListCopyWindowInfo returned null");

            let count = CFArrayGetCount(array);
            let mut out = Vec::new();
            for idx in 0..count {
                let dict = CFArrayGetValueAtIndex(array, idx) as CFDictionaryRef;
                if dict.is_null() {
                    continue;
                }

                let window_id = cf_dict_u32(dict, kCGWindowNumber)?;
                let layer = cf_dict_i64(dict, kCGWindowLayer).unwrap_or(0);
                if layer != 0 {
                    continue;
                }

                let alpha = cf_dict_f64(dict, kCGWindowAlpha).unwrap_or(1.0);
                if alpha <= 0.0 {
                    continue;
                }

                let owner =
                    cf_dict_string(dict, kCGWindowOwnerName).unwrap_or_else(|| "macos".to_string());
                let owner_pid = cf_dict_i64(dict, kCGWindowOwnerPID).unwrap_or(0).max(0) as u32;
                let name = cf_dict_string(dict, kCGWindowName).unwrap_or_else(|| "".to_string());
                let title = if name.is_empty() { owner.clone() } else { name };

                let bounds_dict = CFDictionaryGetValue(dict, kCGWindowBounds);
                if bounds_dict.is_null() {
                    continue;
                }

                let mut rect = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 0.0,
                        height: 0.0,
                    },
                };
                let ok = CGRectMakeWithDictionaryRepresentation(
                    bounds_dict as CFDictionaryRef,
                    &mut rect as *mut CGRect,
                );
                if ok == 0 {
                    continue;
                }

                if rect.size.width < 2.0 || rect.size.height < 2.0 {
                    continue;
                }

                out.push(WindowInfo {
                    window_id,
                    title,
                    app_id: owner,
                    owner_pid,
                    bounds: WindowBounds {
                        x: rect.origin.x,
                        y: rect.origin.y,
                        width: rect.size.width,
                        height: rect.size.height,
                    },
                });
            }

            CFRelease(array as CFTypeRef);
            Ok(out)
        }
    }

    pub(super) fn capture_window_bgra(window_id: u32) -> Result<(BufferMetadata, Vec<u8>)> {
        unsafe {
            // Best-effort: capture at the bounds reported by CGWindowListCopyWindowInfo.
            let bounds = list_windows()
                .location(loc!())?
                .into_iter()
                .find(|w| w.window_id == window_id)
                .ok_or_else(|| anyhow!("unknown window id {window_id}"))?
                .bounds;

            let rect = CGRect {
                origin: CGPoint {
                    x: bounds.x,
                    y: bounds.y,
                },
                size: CGSize {
                    width: bounds.width,
                    height: bounds.height,
                },
            };

            let img = CGWindowListCreateImage(
                rect,
                K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
                window_id,
                K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | K_CG_WINDOW_IMAGE_BEST_RESOLUTION,
            );
            ensure!(
                !img.is_null(),
                "CGWindowListCreateImage returned null (Screen Recording permission?)"
            );

            let width = CGImageGetWidth(img) as i32;
            let height = CGImageGetHeight(img) as i32;
            let stride = CGImageGetBytesPerRow(img) as i32;
            let bpp = CGImageGetBitsPerPixel(img);
            let bpc = CGImageGetBitsPerComponent(img);

            ensure!(
                bpp == 32 && bpc == 8,
                "unsupported capture format: bpp={bpp}, bpc={bpc}"
            );
            ensure!(
                stride > 0 && width > 0 && height > 0,
                "invalid captured dimensions"
            );

            let provider = CGImageGetDataProvider(img);
            ensure!(!provider.is_null(), "CGImageGetDataProvider returned null");
            let cf_data = CGDataProviderCopyData(provider);
            ensure!(!cf_data.is_null(), "CGDataProviderCopyData returned null");
            let len = CFDataGetLength(cf_data) as usize;
            let ptr = CFDataGetBytePtr(cf_data);
            ensure!(!ptr.is_null(), "CFDataGetBytePtr returned null");
            ensure!(
                len >= (height as usize) * (stride as usize),
                "captured buffer is smaller than expected"
            );

            let bytes = std::slice::from_raw_parts(ptr, (height as usize) * (stride as usize));
            let out = bytes.to_vec();

            CFRelease(cf_data as CFTypeRef);
            CFRelease(img as CFTypeRef);

            Ok((
                BufferMetadata {
                    width,
                    height,
                    stride,
                    format: wayland::BufferFormat::Argb8888,
                },
                out,
            ))
        }
    }

    pub(super) fn post_mouse_motion(button_mask: u32, x: f64, y: f64) -> Result<()> {
        unsafe {
            let p = CGPoint { x, y };

            let (event_type, mouse_button) = if button_mask & (1 << 0) != 0 {
                (K_CG_EVENT_LEFT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_LEFT)
            } else if button_mask & (1 << 1) != 0 {
                (K_CG_EVENT_RIGHT_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_RIGHT)
            } else if button_mask & (1 << 2) != 0 {
                (K_CG_EVENT_OTHER_MOUSE_DRAGGED, K_CG_MOUSE_BUTTON_CENTER)
            } else {
                (K_CG_EVENT_MOUSE_MOVED, K_CG_MOUSE_BUTTON_LEFT)
            };

            let ev = CGEventCreateMouseEvent(ptr::null(), event_type, p, mouse_button);
            ensure!(!ev.is_null(), "CGEventCreateMouseEvent returned null");
            CGEventPost(K_CG_EVENT_TAP_HID, ev);
            CFRelease(ev as CFTypeRef);
            Ok(())
        }
    }

    pub(super) fn post_mouse_button(down: bool, button: u32, x: f64, y: f64) -> Result<()> {
        unsafe {
            let p = CGPoint { x, y };

            let (event_type, mouse_button) = match button {
                272 => (
                    if down {
                        K_CG_EVENT_LEFT_MOUSE_DOWN
                    } else {
                        K_CG_EVENT_LEFT_MOUSE_UP
                    },
                    K_CG_MOUSE_BUTTON_LEFT,
                ),
                273 => (
                    if down {
                        K_CG_EVENT_RIGHT_MOUSE_DOWN
                    } else {
                        K_CG_EVENT_RIGHT_MOUSE_UP
                    },
                    K_CG_MOUSE_BUTTON_RIGHT,
                ),
                274 => (
                    if down {
                        K_CG_EVENT_OTHER_MOUSE_DOWN
                    } else {
                        K_CG_EVENT_OTHER_MOUSE_UP
                    },
                    K_CG_MOUSE_BUTTON_CENTER,
                ),
                _ => return Ok(()),
            };

            let ev = CGEventCreateMouseEvent(ptr::null(), event_type, p, mouse_button);
            ensure!(!ev.is_null(), "CGEventCreateMouseEvent returned null");
            CGEventPost(K_CG_EVENT_TAP_HID, ev);
            CFRelease(ev as CFTypeRef);
            Ok(())
        }
    }

    pub(super) fn main_display_scale_factor_and_dpi() -> Result<(i32, Option<u32>)> {
        unsafe {
            let display = CGMainDisplayID();
            let mode = CGDisplayCopyDisplayMode(display);
            ensure!(!mode.is_null(), "CGDisplayCopyDisplayMode returned null");

            let width_points = CGDisplayModeGetWidth(mode) as f64;
            let height_points = CGDisplayModeGetHeight(mode) as f64;
            let width_pixels = CGDisplayModeGetPixelWidth(mode) as f64;
            let height_pixels = CGDisplayModeGetPixelHeight(mode) as f64;

            CFRelease(mode as CFTypeRef);

            ensure!(
                width_points > 0.0 && height_points > 0.0,
                "invalid display mode size"
            );
            ensure!(
                width_pixels > 0.0 && height_pixels > 0.0,
                "invalid display mode pixel size"
            );

            let scale_w = width_pixels / width_points;
            let scale_h = height_pixels / height_points;
            let mut scale = scale_w;
            if (scale_w - scale_h).abs() > 0.1 {
                scale = (scale_w + scale_h) / 2.0;
            }
            let scale_factor = (scale.round() as i32).max(1);

            let screen_mm = CGDisplayScreenSize(display);
            let dpi = if screen_mm.width > 0.0 {
                let inches = screen_mm.width / 25.4;
                if inches > 0.0 {
                    Some((width_pixels / inches).round() as u32)
                } else {
                    None
                }
            } else {
                None
            };

            Ok((scale_factor, dpi))
        }
    }

    fn cf_dict_u32(dict: CFDictionaryRef, key: CFStringRef) -> Result<u32> {
        let v = unsafe { CFDictionaryGetValue(dict, key) };
        ensure!(!v.is_null(), "missing required key");
        let mut out: i64 = 0;
        let ok =
            unsafe { CFNumberGetValue(v as CFNumberRef, 4, &mut out as *mut i64 as *mut c_void) };
        ensure!(ok != 0, "CFNumberGetValue failed");
        Ok(out as u32)
    }

    fn cf_dict_i64(dict: CFDictionaryRef, key: CFStringRef) -> Option<i64> {
        unsafe {
            let v = CFDictionaryGetValue(dict, key);
            if v.is_null() {
                return None;
            }
            let mut out: i64 = 0;
            let ok = CFNumberGetValue(v as CFNumberRef, 4, &mut out as *mut i64 as *mut c_void);
            if ok == 0 {
                return None;
            }
            Some(out)
        }
    }

    fn cf_dict_f64(dict: CFDictionaryRef, key: CFStringRef) -> Option<f64> {
        unsafe {
            let v = CFDictionaryGetValue(dict, key);
            if v.is_null() {
                return None;
            }
            let mut out: f64 = 0.0;
            let ok = CFNumberGetValue(v as CFNumberRef, 13, &mut out as *mut f64 as *mut c_void);
            if ok == 0 {
                return None;
            }
            Some(out)
        }
    }

    fn cf_dict_string(dict: CFDictionaryRef, key: CFStringRef) -> Option<String> {
        unsafe {
            let v = CFDictionaryGetValue(dict, key);
            if v.is_null() {
                return None;
            }
            cf_string_to_string(v as CFStringRef)
        }
    }

    fn cf_string_to_string(s: CFStringRef) -> Option<String> {
        unsafe {
            if s.is_null() {
                return None;
            }
            let cptr = CFStringGetCStringPtr(s, K_CF_STRING_ENCODING_UTF8);
            if !cptr.is_null() {
                let cstr = std::ffi::CStr::from_ptr(cptr as *const i8);
                return Some(cstr.to_string_lossy().into_owned());
            }

            let length = CFStringGetLength(s);
            if length <= 0 {
                return Some(String::new());
            }
            let max = CFStringGetMaximumSizeForEncoding(length, K_CF_STRING_ENCODING_UTF8);
            if max <= 0 {
                return None;
            }
            let mut buf = vec![0u8; (max as usize) + 1];
            let ok = CFStringGetCString(
                s,
                buf.as_mut_ptr(),
                buf.len() as CFIndex,
                K_CF_STRING_ENCODING_UTF8,
            );
            if ok == 0 {
                return None;
            }
            let cstr = std::ffi::CStr::from_ptr(buf.as_ptr() as *const i8);
            Some(cstr.to_string_lossy().into_owned())
        }
    }
}
