use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use crate::prelude::*;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::ClientId;
use crate::protocols::wprs::types::DisplayConfig;
use crate::protocols::wprs::types::Event;
use crate::protocols::wprs::wayland;
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::PointerEventKind;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::server::backend::BackendObservation;
use crate::server::backend::BackendBgraFrame;
use crate::server::backend::BackendSurfaceDescriptor;
use crate::server::backend::BackendSurfaceRole;
use crate::server::backend::PollingBackend;
use crate::protocols::wprs::xdg_shell;
use sysinfo::Pid;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosWindowBackendConfig {
    pub dpi: Option<u32>,
    pub target_pid: Option<u32>,
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    struct ChildGuard(Option<std::process::Child>);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    #[ignore]
    fn open_finder_and_list_windows() {
        let child = Command::new("/usr/bin/open")
            .arg("-W")
            .arg("-a")
            .arg("Finder")
            .spawn()
            .expect("failed to launch Finder via open");
        let _guard = ChildGuard(Some(child));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let windows = list_windows().expect("failed to list windows");
            let has_finder = windows.iter().any(|w| w.app_id == "Finder");
            if has_finder {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "Finder windows not found after wait; windows={:?}",
                    windows
                        .iter()
                        .take(5)
                        .map(|w| format!("{}:{}", w.app_id, w.title))
                        .collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[test]
    #[ignore]
    fn open_finder_and_capture_window() {
        let child = Command::new("/usr/bin/open")
            .arg("-W")
            .arg("-a")
            .arg("Finder")
            .spawn()
            .expect("failed to launch Finder via open");
        let _guard = ChildGuard(Some(child));

        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let windows = list_windows().expect("failed to list windows");
            if let Some(window) = windows.iter().find(|w| w.app_id == "Finder") {
                let capture = capture_window_bgra(window.window_id)
                    .expect("failed to capture window")
                    .expect("window capture returned none");
                let (metadata, bytes) = capture;
                assert!(metadata.width > 0);
                assert!(metadata.height > 0);
                assert!(!bytes.is_empty());
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "Finder window not found for capture; windows={:?}",
                    windows
                        .iter()
                        .take(5)
                        .map(|w| format!("{}:{}", w.app_id, w.title))
                        .collect::<Vec<_>>()
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

#[derive(Clone, Debug)]
pub struct MacosTargetPid(Arc<AtomicU32>);

impl MacosTargetPid {
    pub fn new(initial: Option<u32>) -> Self {
        Self(Arc::new(AtomicU32::new(initial.unwrap_or(0))))
    }

    pub fn get(&self) -> Option<u32> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            pid => Some(pid),
        }
    }

    pub fn set(&self, pid: Option<u32>) {
        let prev = self.get();
        if prev != pid {
            match pid {
                Some(pid) => info!("macos backend: target pid set to {pid}"),
                None => info!("macos backend: target pid cleared"),
            }
        }
        self.0.store(pid.unwrap_or(0), Ordering::Relaxed);
    }
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
    target_pid: MacosTargetPid,
    last_target_pid: Option<u32>,
    last_window_count: Option<usize>,
    process_tree: System,
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
            target_pid: MacosTargetPid::new(config.target_pid),
            last_target_pid: config.target_pid,
            last_window_count: None,
            process_tree: System::new(),
        }
    }

    pub fn target_pid_handle(&self) -> MacosTargetPid {
        self.target_pid.clone()
    }

    fn surface_descriptor_for_window(
        &self,
        window_id: u32,
        title: &str,
        app_id: &str,
    ) -> BackendSurfaceDescriptor {
        BackendSurfaceDescriptor {
            client: ClientId(1),
            id: WlSurfaceId(window_id as u64),
            role: BackendSurfaceRole::XdgToplevel {
                id: xdg_shell::XdgToplevelId(window_id as u64),
                title: Some(title.to_string()),
                app_id: Some(app_id.to_string()),
            },
            buffer_scale: self.display_config.scale_factor,
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

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        let mut out = Vec::new();

        let windows = list_windows().location(loc!())?;
        let windows = self.filter_windows_by_target_pid(windows);
        for w in windows {
            // Capture once to get the initial window size.
            let Some((metadata, bgra)) = capture_window_bgra(w.window_id).location(loc!())? else {
                continue;
            };
            self.windows
                .insert(w.window_id, TrackedWindow { bounds: w.bounds });
            let surface = self.surface_descriptor_for_window(w.window_id, &w.title, &w.app_id);

            out.push(BackendObservation::SurfaceCommit {
                surface,
                frame: Some(BackendBgraFrame {
                    metadata,
                    bgra,
                }),
            });
        }

        self.log_capture_status();

        Ok(out)
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        let mut out = Vec::new();

        let windows = list_windows().location(loc!())?;
        let windows = self.filter_windows_by_target_pid(windows);
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
            let Some((metadata, bgra)) = capture_window_bgra(w.window_id).location(loc!())? else {
                continue;
            };
            self.windows
                .insert(w.window_id, TrackedWindow { bounds: w.bounds });
            let surface = self.surface_descriptor_for_window(w.window_id, &w.title, &w.app_id);
            out.push(BackendObservation::SurfaceCommit {
                surface,
                frame: Some(BackendBgraFrame { metadata, bgra }),
            });
        }

        self.log_capture_status();

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

impl MacosWindowBackend {
    fn log_capture_status(&mut self) {
        let target_pid = self.target_pid.get();
        let window_count = self.windows.len();

        if self.last_target_pid != target_pid || self.last_window_count != Some(window_count) {
            match target_pid {
                Some(pid) => info!(
                    "macos backend: capturing pid={pid} windows={window_count}"
                ),
                None => info!("macos backend: capturing all windows={window_count}"),
            }
            self.last_target_pid = target_pid;
            self.last_window_count = Some(window_count);
        }
    }

    fn filter_windows_by_target_pid(&mut self, windows: Vec<WindowInfo>) -> Vec<WindowInfo> {
        let Some(root_pid) = self.target_pid.get() else {
            return windows;
        };

        let allowed = self.collect_descendant_pids(root_pid);
        let allowed_len = allowed.len();
        let filtered: Vec<WindowInfo> = windows
            .into_iter()
            .filter(|w| allowed.contains(&w.owner_pid))
            .collect();
        if filtered.is_empty() {
            debug!(
                "macos backend: target pid={root_pid} has no windows in subtree (tracked_pids={allowed_len})"
            );
        }
        filtered
    }

    fn collect_descendant_pids(&mut self, root_pid: u32) -> std::collections::HashSet<u32> {
        self.process_tree
            .refresh_processes(ProcessesToUpdate::All, true);

        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in self.process_tree.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }

        let root = Pid::from_u32(root_pid);
        let mut out = std::collections::HashSet::new();
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            if out.insert(pid.as_u32()) {
                if let Some(kids) = children.get(&pid) {
                    stack.extend(kids.iter().copied());
                }
            }
        }
        out
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
    macos::main_display_scale_factor_and_dpi().location(loc!())
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
    macos::list_windows().location(loc!())
}

fn capture_window_bgra(window_id: u32) -> Result<Option<(BufferMetadata, Vec<u8>)>> {
    macos::capture_window_bgra(window_id).location(loc!())
}

fn post_mouse_motion(button_mask: u32, x: f64, y: f64) -> Result<()> {
    macos::post_mouse_motion(button_mask, x, y).location(loc!())
}

fn post_mouse_button(down: bool, button: u32, x: f64, y: f64) -> Result<()> {
    macos::post_mouse_button(down, button, x, y).location(loc!())
}

#[cfg(target_os = "macos")]
mod macos {
    #![allow(unexpected_cfgs)]
    use super::*;
    use crate::error::ensure;
    use block::ConcreteBlock;
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEvent,
        CGEventTapLocation,
        CGEventType,
        CGMouseButton,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use core_graphics::image::CGImage;
    use core_graphics::sys::CGImage as CGImageSys;
    use foreign_types::ForeignType;
    use core_graphics::window;
    use objc::rc::autoreleasepool;
    use objc::runtime::{Class, Object, BOOL, NO, YES};
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CStr;
    use std::os::raw::c_char;
    use std::sync::mpsc;
    use std::time::Duration;

    type CGSConnectionID = u32;
    type CGWindowID = u32;

    const CGS_COPY_WINDOWS_OPTION_INVISIBLE1: i32 = 1 << 0;
    const CGS_COPY_WINDOWS_OPTION_SCREENSAVER_LEVEL_1000: i32 = 1 << 1;
    const CGS_COPY_WINDOWS_OPTION_INVISIBLE2: i32 = 1 << 2;
    const CGS_COPY_WINDOWS_OPTION_UNKNOWN1: i32 = 1 << 3;
    const CGS_COPY_WINDOWS_OPTION_UNKNOWN2: i32 = 1 << 4;
    const CGS_COPY_WINDOWS_OPTION_DESKTOP_ICON_LEVEL: i32 = 1 << 5;

    const CGS_CAPTURE_OPTION_IGNORE_GLOBAL_CLIP_SHAPE: u32 = 1 << 11;
    const CGS_CAPTURE_OPTION_BEST_RESOLUTION: u32 = 1 << 8;
    const CGS_CAPTURE_OPTION_FULL_SIZE: u32 = 1 << 19;

    #[link(name = "SkyLight", kind = "framework")]
    unsafe extern "C" {
        fn CGSMainConnectionID() -> CGSConnectionID;
        fn CGSCopyManagedDisplaySpaces(cid: CGSConnectionID) -> *const std::ffi::c_void;
        fn CGSCopyWindowsWithOptionsAndTags(
            cid: CGSConnectionID,
            owner: i32,
            spaces: *const std::ffi::c_void,
            options: i32,
            set_tags: *mut i32,
            clear_tags: *mut i32,
        ) -> *const std::ffi::c_void;
        fn CGSHWCaptureWindowList(
            cid: CGSConnectionID,
            window_list: *mut CGWindowID,
            window_count: u32,
            options: u32,
        ) -> *const std::ffi::c_void;
    }


    pub(super) fn list_windows() -> Result<Vec<WindowInfo>> {
        if let Some(windows) = list_windows_screencapturekit().location(loc!())? {
            if !windows.is_empty() {
                return Ok(windows);
            }
        }
        if let Some(windows) = list_windows_skylight().location(loc!())? {
            if !windows.is_empty() {
                return Ok(windows);
            }
        }
        list_windows_public().location(loc!())
    }

    fn list_windows_public() -> Result<Vec<WindowInfo>> {
        let options = window::kCGWindowListOptionOnScreenOnly
            | window::kCGWindowListExcludeDesktopElements;
        let array = window::copy_window_info(options, window::kCGNullWindowID)
            .ok_or_else(|| Error::Internal("CGWindowListCopyWindowInfo returned null".to_string()))?;

        let mut out = Vec::new();
        for idx in 0..array.len() {
            let dict_ref = *unsafe { array.get_unchecked(idx) };
            let dict: CFDictionary<CFString, CFType> = unsafe {
                CFDictionary::wrap_under_get_rule(dict_ref as _)
            };
            if let Some(info) = window_info_from_dict(&dict, false) {
                out.push(info);
            }
        }

        Ok(out)
    }

    fn list_windows_skylight() -> Result<Option<Vec<WindowInfo>>> {
        let window_ids = skylight_window_ids().location(loc!())?;
        if window_ids.is_empty() {
            return Ok(None);
        }

        let mut out = Vec::new();
        for id in window_ids {
            if let Some(info) = window_info_for_id(id, true).location(loc!())? {
                out.push(info);
            }
        }

        Ok(Some(out))
    }

    fn list_windows_screencapturekit() -> Result<Option<Vec<WindowInfo>>> {
        if !ensure_screencapturekit_loaded() {
            return Ok(None);
        }
        let class = match Class::get("SCShareableContent") {
            Some(class) => class,
            None => return Ok(None),
        };

        let (tx, rx) = mpsc::channel();
        let block = ConcreteBlock::new(move |content: *mut Object, error: *mut Object| {
            let (content, error) = autoreleasepool(|| {
                unsafe {
                    if !content.is_null() {
                        let _: *mut Object = msg_send![content, retain];
                    }
                    if !error.is_null() {
                        let _: *mut Object = msg_send![error, retain];
                    }
                }
                (content, error)
            });
            let _ = tx.send((content, error));
        })
        .copy();

        unsafe {
            let _: () = msg_send![
                class,
                getShareableContentExcludingDesktopWindows: YES
                onScreenWindowsOnly: NO
                completionHandler: &*block
            ];
        }

        let Ok((content, error)) = rx.recv_timeout(Duration::from_secs(2)) else {
            return Ok(None);
        };
        if !error.is_null() || content.is_null() {
            if !error.is_null() {
                unsafe {
                    let _: () = msg_send![error, release];
                }
            }
            if !content.is_null() {
                unsafe {
                    let _: () = msg_send![content, release];
                }
            }
            return Ok(None);
        }

        let windows_obj: *mut Object = unsafe { msg_send![content, windows] };
        if windows_obj.is_null() {
            unsafe {
                let _: () = msg_send![content, release];
            }
            return Ok(None);
        }

        let count: usize = unsafe { msg_send![windows_obj, count] };
        let mut out = Vec::new();
        for idx in 0..count {
            let window_obj: *mut Object = unsafe { msg_send![windows_obj, objectAtIndex: idx] };
            if window_obj.is_null() {
                continue;
            }
            let window_id: u32 = unsafe { msg_send![window_obj, windowID] };
            let title = nsstring_to_string(unsafe { msg_send![window_obj, title] })
                .unwrap_or_default();
            let frame: CGRect = unsafe { msg_send![window_obj, frame] };
            let bounds = WindowBounds {
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
            };

            if bounds.width < 2.0 || bounds.height < 2.0 {
                continue;
            }

            let app_obj: *mut Object = unsafe { msg_send![window_obj, owningApplication] };
            let (app_id, owner_pid) = if app_obj.is_null() {
                ("macos".to_string(), 0)
            } else {
                let name = nsstring_to_string(unsafe { msg_send![app_obj, applicationName] })
                    .or_else(|| nsstring_to_string(unsafe { msg_send![app_obj, bundleIdentifier] }))
                    .unwrap_or_else(|| "macos".to_string());
                let pid: i32 = unsafe { msg_send![app_obj, processID] };
                (name, pid.max(0) as u32)
            };

            let title = if title.is_empty() { app_id.clone() } else { title };
            out.push(WindowInfo {
                window_id,
                title,
                app_id,
                owner_pid,
                bounds,
            });
        }

        unsafe {
            let _: () = msg_send![content, release];
        }
        Ok(Some(out))
    }

    fn window_info_for_id(window_id: u32, allow_transparent: bool) -> Result<Option<WindowInfo>> {
        let array = window::copy_window_info(
            window::kCGWindowListOptionIncludingWindow,
            window_id,
        )
        .ok_or_else(|| Error::Internal("CGWindowListCopyWindowInfo returned null".to_string()))?;
        if array.len() == 0 {
            return Ok(None);
        }
        let dict_ref = *unsafe { array.get_unchecked(0) };
        let dict: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(dict_ref as _) };
        Ok(window_info_from_dict(&dict, allow_transparent))
    }

    fn window_info_from_dict(
        dict: &CFDictionary<CFString, CFType>,
        allow_transparent: bool,
    ) -> Option<WindowInfo> {
        let window_id = cf_dict_u32(dict, unsafe { window::kCGWindowNumber })?;
        let layer = cf_dict_i64(dict, unsafe { window::kCGWindowLayer }).unwrap_or(0);
        if layer != 0 {
            return None;
        }

        let alpha = cf_dict_f64(dict, unsafe { window::kCGWindowAlpha }).unwrap_or(1.0);
        if !allow_transparent && alpha <= 0.0 {
            return None;
        }

        let owner = cf_dict_string(dict, unsafe { window::kCGWindowOwnerName })
            .unwrap_or_else(|| "macos".to_string());
        let owner_pid = cf_dict_i64(dict, unsafe { window::kCGWindowOwnerPID })
            .unwrap_or(0)
            .max(0) as u32;
        let name = cf_dict_string(dict, unsafe { window::kCGWindowName }).unwrap_or_default();
        let title = if name.is_empty() { owner.clone() } else { name };

        let bounds = bounds_from_dict(dict)?;
        if bounds.width < 2.0 || bounds.height < 2.0 {
            return None;
        }

        Some(WindowInfo {
            window_id,
            title,
            app_id: owner,
            owner_pid,
            bounds,
        })
    }

    pub(super) fn capture_window_bgra(
        window_id: u32,
    ) -> Result<Option<(BufferMetadata, Vec<u8>)>> {
        if let Ok((metadata, bgra)) = capture_window_bgra_skylight(window_id) {
            return Ok(Some((metadata, bgra)));
        }

        capture_window_bgra_public(window_id).location(loc!())
    }

    fn capture_window_bgra_public(
        window_id: u32,
    ) -> Result<Option<(BufferMetadata, Vec<u8>)>> {
        let bounds = list_windows_public()
            .location(loc!())?
            .into_iter()
            .find(|w| w.window_id == window_id)
            .map(|w| w.bounds);
        let Some(bounds) = bounds else {
            return Ok(None);
        };

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

        let image = window::create_image(
            rect,
            window::kCGWindowListOptionIncludingWindow,
            window_id,
            window::kCGWindowImageBoundsIgnoreFraming | window::kCGWindowImageBestResolution,
        )
        .ok_or_else(|| {
            Error::Internal(
                "CGWindowListCreateImage returned null (Screen Recording permission?)"
                    .to_string(),
            )
        })?;

        image_to_bgra(image).location(loc!()).map(Some)
    }

    fn capture_window_bgra_skylight(window_id: u32) -> Result<(BufferMetadata, Vec<u8>)> {
        let mut id = window_id;
        let options = CGS_CAPTURE_OPTION_IGNORE_GLOBAL_CLIP_SHAPE
            | CGS_CAPTURE_OPTION_BEST_RESOLUTION
            | CGS_CAPTURE_OPTION_FULL_SIZE;
        let images_ref = unsafe { CGSHWCaptureWindowList(cgs_connection_id(), &mut id, 1, options) };
        if images_ref.is_null() {
            bail!(Error::Internal("CGSHWCaptureWindowList returned null".to_string()));
        }
        let images: CFArray<*const std::ffi::c_void> = unsafe {
            CFArray::wrap_under_create_rule(images_ref as _)
        };
        if images.len() == 0 {
            bail!(Error::Missing(format!("CGSHWCaptureWindowList empty for {window_id}")));
        }
        let image_ptr = *unsafe { images.get_unchecked(0) } as *mut CGImageSys;
        if image_ptr.is_null() {
            bail!(Error::Missing(format!("CGSHWCaptureWindowList null image for {window_id}")));
        }
        unsafe {
            core_foundation::base::CFRetain(image_ptr as *const _);
        }
        let image = unsafe { CGImage::from_ptr(image_ptr) };
        image_to_bgra(image).location(loc!())
    }

    pub(super) fn post_mouse_motion(button_mask: u32, x: f64, y: f64) -> Result<()> {
        let source = event_source()?;
        let (event_type, button) = match button_mask {
            mask if (mask & (1 << 0)) != 0 => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
            mask if (mask & (1 << 1)) != 0 => (CGEventType::RightMouseDragged, CGMouseButton::Right),
            mask if (mask & (1 << 2)) != 0 => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
            _ => (CGEventType::MouseMoved, CGMouseButton::Left),
        };

        let ev = CGEvent::new_mouse_event(source, event_type, CGPoint { x, y }, button)
            .map_err(|_| Error::Internal("CGEventCreateMouseEvent failed".to_string()))?;
        ev.post(CGEventTapLocation::HID);
        Ok(())
    }

    pub(super) fn post_mouse_button(down: bool, button: u32, x: f64, y: f64) -> Result<()> {
        let source = event_source()?;
        let (event_type, mouse_button) = match button {
            272 => (
                if down { CGEventType::LeftMouseDown } else { CGEventType::LeftMouseUp },
                CGMouseButton::Left,
            ),
            273 => (
                if down { CGEventType::RightMouseDown } else { CGEventType::RightMouseUp },
                CGMouseButton::Right,
            ),
            _ => (
                if down { CGEventType::OtherMouseDown } else { CGEventType::OtherMouseUp },
                CGMouseButton::Center,
            ),
        };

        let ev = CGEvent::new_mouse_event(source, event_type, CGPoint { x, y }, mouse_button)
            .map_err(|_| Error::Internal("CGEventCreateMouseEvent failed".to_string()))?;
        ev.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn cgs_connection_id() -> CGSConnectionID {
        unsafe { CGSMainConnectionID() }
    }

    fn skylight_window_ids() -> Result<Vec<u32>> {
        let space_ids = skylight_space_ids().location(loc!())?;
        if space_ids.is_empty() {
            return Ok(Vec::new());
        }

        let cf_numbers: Vec<CFNumber> = space_ids
            .into_iter()
            .map(|id| CFNumber::from(id as i64))
            .collect();
        let spaces = CFArray::from_CFTypes(&cf_numbers);
        let mut set_tags = 0i32;
        let mut clear_tags = 0i32;
        let options = CGS_COPY_WINDOWS_OPTION_SCREENSAVER_LEVEL_1000
            | CGS_COPY_WINDOWS_OPTION_INVISIBLE1
            | CGS_COPY_WINDOWS_OPTION_INVISIBLE2
            | CGS_COPY_WINDOWS_OPTION_UNKNOWN1
            | CGS_COPY_WINDOWS_OPTION_UNKNOWN2
            | CGS_COPY_WINDOWS_OPTION_DESKTOP_ICON_LEVEL;

        let windows_ref = unsafe {
            CGSCopyWindowsWithOptionsAndTags(
                cgs_connection_id(),
                0,
                spaces.as_concrete_TypeRef() as _,
                options,
                &mut set_tags,
                &mut clear_tags,
            )
        };
        if windows_ref.is_null() {
            return Ok(Vec::new());
        }
        let windows: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(windows_ref as _) };
        let mut out = Vec::with_capacity(windows.len() as usize);
        for idx in 0..windows.len() {
            let value = unsafe { windows.get_unchecked(idx) };
            let number = unsafe { CFNumber::wrap_under_get_rule(value.as_CFTypeRef() as _) };
            if let Some(id) = number.to_i64().and_then(|id| u32::try_from(id).ok()) {
                out.push(id);
            }
        }
        Ok(out)
    }

    fn skylight_space_ids() -> Result<Vec<u64>> {
        let spaces_ref = unsafe { CGSCopyManagedDisplaySpaces(cgs_connection_id()) };
        if spaces_ref.is_null() {
            return Ok(Vec::new());
        }

        let displays: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(spaces_ref as _) };
        let mut out = Vec::new();
        for idx in 0..displays.len() {
            let dict_ref = unsafe { displays.get_unchecked(idx) };
            let display_dict: CFDictionary<CFString, CFType> = unsafe {
                CFDictionary::wrap_under_get_rule(dict_ref.as_CFTypeRef() as _)
            };
            let spaces_array = match cf_dict_array(&display_dict, "Spaces") {
                Some(spaces_array) => spaces_array,
                None => continue,
            };
            for space_idx in 0..spaces_array.len() {
                let space_ref = unsafe { spaces_array.get_unchecked(space_idx) };
                let space_dict: CFDictionary<CFString, CFType> = unsafe {
                    CFDictionary::wrap_under_get_rule(space_ref.as_CFTypeRef() as _)
                };
                if let Some(id) = cf_dict_u64(&space_dict, "id64") {
                    out.push(id);
                }
            }
        }

        Ok(out)
    }

    fn image_to_bgra(image: CGImage) -> Result<(BufferMetadata, Vec<u8>)> {
        let width = image.width() as i32;
        let height = image.height() as i32;
        let stride = image.bytes_per_row() as i32;
        let bpp = image.bits_per_pixel();
        let bpc = image.bits_per_component();

        ensure!(
            bpp == 32 && bpc == 8,
            Error::Unsupported(format!(
                "unsupported capture format: bpp={bpp}, bpc={bpc}"
            )),
        );
        ensure!(
            stride > 0 && width > 0 && height > 0,
            Error::Internal("invalid captured dimensions".to_string()),
        );

        let data = image.data();
        let bytes = data.bytes();
        let expected_len = (stride as usize) * (height as usize);
        ensure!(
            bytes.len() >= expected_len,
            Error::Internal("CGImage data smaller than expected".to_string()),
        );

        let metadata = BufferMetadata {
            width,
            height,
            stride,
            format: crate::protocols::wprs::wayland::BufferFormat::Argb8888,
        };

        Ok((metadata, bytes[..expected_len].to_vec()))
    }


    pub(super) fn main_display_scale_factor_and_dpi() -> Result<(i32, Option<u32>)> {
        let display = CGDisplay::main();
        let mode = display
            .display_mode()
            .ok_or_else(|| Error::Internal("CGDisplayCopyDisplayMode returned null".to_string()))?;
        let width_points = mode.width() as f64;
        let height_points = mode.height() as f64;
        let width_pixels = mode.pixel_width() as f64;
        let _height_pixels = mode.pixel_height() as f64;
        let scale_factor = if width_points > 0.0 && height_points > 0.0 {
            let s = (width_pixels / width_points).round();
            s.max(1.0) as i32
        } else {
            1
        };

        let screen_mm = display.screen_size();
        let dpi = if screen_mm.width > 0.0 {
            let inches = screen_mm.width / 25.4;
            Some((width_pixels / inches).round() as u32)
        } else {
            None
        };

        Ok((scale_factor, dpi))
    }

    fn event_source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| Error::Internal("CGEventSourceCreate failed".to_string()))
    }

    fn cf_dict_string(dict: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<String> {
        let key = unsafe { CFString::wrap_under_get_rule(key) };
        dict.find(key)
            .and_then(|value| value.downcast::<CFString>())
            .map(|value| value.to_string())
    }

    fn cf_dict_i64(dict: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<i64> {
        let key = unsafe { CFString::wrap_under_get_rule(key) };
        dict.find(key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i64())
    }

    fn cf_dict_u32(dict: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<u32> {
        cf_dict_i64(dict, key).and_then(|val| u32::try_from(val).ok())
    }

    fn cf_dict_f64(dict: &CFDictionary<CFString, CFType>, key: CFStringRef) -> Option<f64> {
        let key = unsafe { CFString::wrap_under_get_rule(key) };
        dict.find(key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_f64())
    }

    fn cf_dict_u64(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<u64> {
        let key = CFString::new(key);
        dict.find(&key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_i64())
            .and_then(|value| u64::try_from(value).ok())
    }

    fn cf_dict_array(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<CFArray<CFType>> {
        let key = CFString::new(key);
        let value = dict.find(&key)?;
        let array_untyped = value.downcast::<CFArray>()?;
        let array: CFArray<CFType> = unsafe {
            CFArray::wrap_under_get_rule(array_untyped.as_concrete_TypeRef())
        };
        Some(array)
    }

    fn bounds_from_dict(dict: &CFDictionary<CFString, CFType>) -> Option<WindowBounds> {
        let key = unsafe { CFString::wrap_under_get_rule(window::kCGWindowBounds) };
        let bounds_value = dict.find(key)?;
        let bounds_dict_untyped = bounds_value.downcast::<CFDictionary>()?;
        let bounds_dict: CFDictionary<CFString, CFType> = unsafe {
            CFDictionary::wrap_under_get_rule(bounds_dict_untyped.as_concrete_TypeRef())
        };

        let x = dict_value_f64(&bounds_dict, "X")?;
        let y = dict_value_f64(&bounds_dict, "Y")?;
        let width = dict_value_f64(&bounds_dict, "Width")?;
        let height = dict_value_f64(&bounds_dict, "Height")?;

        Some(WindowBounds {
            x,
            y,
            width,
            height,
        })
    }

    fn dict_value_f64(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<f64> {
        let key = CFString::new(key);
        dict.find(&key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|value| value.to_f64())
    }

    fn nsstring_to_string(obj: *mut Object) -> Option<String> {
        if obj.is_null() {
            return None;
        }
        unsafe {
            let cstr: *const c_char = msg_send![obj, UTF8String];
            if cstr.is_null() {
                return None;
            }
            Some(CStr::from_ptr(cstr).to_string_lossy().into_owned())
        }
    }

    fn ensure_screencapturekit_loaded() -> bool {
        let nsbundle = match Class::get("NSBundle") {
            Some(class) => class,
            None => return false,
        };
        let path = match nsstring_from_str(
            "/System/Library/Frameworks/ScreenCaptureKit.framework",
        ) {
            Some(path) => path,
            None => return false,
        };
        let bundle: *mut Object = unsafe { msg_send![nsbundle, bundleWithPath: path] };
        if bundle.is_null() {
            return false;
        }
        let loaded: BOOL = unsafe { msg_send![bundle, load] };
        loaded == YES
    }

    fn nsstring_from_str(value: &str) -> Option<*mut Object> {
        let class = Class::get("NSString")?;
        let cstr = std::ffi::CString::new(value).ok()?;
        let obj: *mut Object = unsafe { msg_send![class, stringWithUTF8String: cstr.as_ptr()] };
        if obj.is_null() {
            None
        } else {
            Some(obj)
        }
    }
}
