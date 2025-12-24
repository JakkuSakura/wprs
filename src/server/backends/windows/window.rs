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
use crate::protocols::wprs::xdg_shell;
use crate::server::backend::BackendObservation;
use crate::server::backend::BackendBgraFrame;
use crate::server::backend::BackendSurfaceDescriptor;
use crate::server::backend::BackendSurfaceRole;
use crate::server::backend::PollingBackend;

#[derive(Debug, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }
}

#[derive(Debug, Clone)]
struct TrackedWindow {
    rect: Rect,
}

#[derive(Clone, Debug)]
pub struct WindowsTargetPid {
    pid: Arc<AtomicU32>,
}

impl WindowsTargetPid {
    pub fn new() -> Self {
        Self {
            pid: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn get(&self) -> Option<u32> {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid == 0 { None } else { Some(pid) }
    }

    pub fn set(&self, pid: Option<u32>) {
        self.pid.store(pid.unwrap_or(0), Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct WindowsWindowBackend {
    display_config: DisplayConfig,
    windows: HashMap<u64, TrackedWindow>,
    pressed_buttons: u32,
    target_pid: WindowsTargetPid,
}

impl WindowsWindowBackend {
    pub fn new() -> Self {
        Self {
            display_config: DisplayConfig::default(),
            windows: HashMap::new(),
            pressed_buttons: 0,
            target_pid: WindowsTargetPid::new(),
        }
    }

    pub fn target_pid_handle(&self) -> WindowsTargetPid {
        self.target_pid.clone()
    }

    fn surface_descriptor_for_window(
        &self,
        hwnd_key: u64,
        title: &str,
    ) -> BackendSurfaceDescriptor {
        BackendSurfaceDescriptor {
            client: ClientId(1),
            id: WlSurfaceId(hwnd_key),
            role: BackendSurfaceRole::XdgToplevel {
                id: xdg_shell::XdgToplevelId(hwnd_key),
                title: Some(title.to_string()),
                app_id: Some("windows".to_string()),
            },
            buffer_scale: 1,
        }
    }

    fn handle_pointer_event(&mut self, e: wayland::PointerEvent) -> Result<()> {
        let hwnd_key = e.surface_id.0;
        let Some(tracked) = self.windows.get(&hwnd_key) else {
            return Ok(());
        };

        let local_x = e
            .position
            .x
            .clamp(0.0, (tracked.rect.width().max(1) - 1) as f64);
        let local_y = e
            .position
            .y
            .clamp(0.0, (tracked.rect.height().max(1) - 1) as f64);

        let x = tracked.rect.left + local_x.round() as i32;
        let y = tracked.rect.top + local_y.round() as i32;

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
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => post_scroll(
                horizontal.absolute.round() as i32,
                vertical.absolute.round() as i32,
            )
            .location(loc!()),
        }
    }
}

impl PollingBackend for WindowsWindowBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities { xwayland: false }
    }

    fn display_config(&self) -> DisplayConfig {
        self.display_config.clone()
    }

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        let windows = list_windows().location(loc!())?;
        let mut out = Vec::new();
        for w in windows {
            let (metadata, bgra) = capture_window_bgra(w.hwnd_key).location(loc!())?;
            self.windows
                .insert(w.hwnd_key, TrackedWindow { rect: w.rect });
            out.push(BackendObservation::SurfaceCommit {
                surface: self.surface_descriptor_for_window(w.hwnd_key, &w.title),
                frame: Some(BackendBgraFrame { metadata, bgra }),
            });
        }
        Ok(out)
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        // Skeleton for per-session filtering.
        //
        // For now, we expose the target pid via wctl StartSession/StopSession,
        // but the Windows capture implementation does not yet retrieve per-
        // window owner pid, so we cannot filter the enumeration.
        let _ = self.target_pid.get();

        let mut out = Vec::new();
        let windows = list_windows().location(loc!())?;
        let mut seen = std::collections::HashSet::new();
        for w in &windows {
            seen.insert(w.hwnd_key);
        }

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
                surface: WlSurfaceId(id),
            });
        }

        for w in windows {
            let (metadata, bgra) = capture_window_bgra(w.hwnd_key).location(loc!())?;
            self.windows
                .insert(w.hwnd_key, TrackedWindow { rect: w.rect });
            let surface = self.surface_descriptor_for_window(w.hwnd_key, &w.title);
            out.push(BackendObservation::SurfaceCommit {
                surface,
                frame: Some(BackendBgraFrame { metadata, bgra }),
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

impl Default for WindowsWindowBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn button_mask(button: u32) -> u32 {
    match button {
        272 => 1 << 0,
        273 => 1 << 1,
        274 => 1 << 2,
        _ => 0,
    }
}

#[derive(Debug, Clone)]
struct WindowInfo {
    hwnd_key: u64,
    title: String,
    rect: Rect,
}

fn list_windows() -> Result<Vec<WindowInfo>> {
    #[cfg(target_os = "windows")]
    {
        win::list_windows().location(loc!())
    }

    #[cfg(not(target_os = "windows"))]
    {
        bail!(Error::Unsupported(
            "Windows window capture backend is only supported on Windows".to_string(),
        ))
    }
}

fn capture_window_bgra(hwnd_key: u64) -> Result<(BufferMetadata, Vec<u8>)> {
    #[cfg(target_os = "windows")]
    {
        win::capture_window_bgra(hwnd_key).location(loc!())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = hwnd_key;
        bail!(Error::Unsupported(
            "Windows window capture backend is only supported on Windows".to_string(),
        ))
    }
}

fn post_mouse_motion(button_mask: u32, x: i32, y: i32) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win::post_mouse_motion(button_mask, x, y).location(loc!())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (button_mask, x, y);
        Ok(())
    }
}

fn post_mouse_button(down: bool, button: u32, x: i32, y: i32) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win::post_mouse_button(down, button, x, y).location(loc!())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (down, button, x, y);
        Ok(())
    }
}

fn post_scroll(horizontal: i32, vertical: i32) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        win::post_scroll(horizontal, vertical).location(loc!())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (horizontal, vertical);
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod win {
    use super::*;
    use crate::error::ensure;
    use std::ffi::c_void;
    use std::ptr;

    type BOOL = i32;
    type UINT = u32;
    type DWORD = u32;
    type LONG = i32;
    type WORD = u16;
    type WPARAM = usize;
    type LPARAM = isize;
    type HANDLE = *mut c_void;
    type HDC = HANDLE;
    type HBITMAP = HANDLE;
    type HWND = HANDLE;
    type HGDIOBJ = HANDLE;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct RECT {
        left: LONG,
        top: LONG,
        right: LONG,
        bottom: LONG,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct BITMAPINFOHEADER {
        biSize: DWORD,
        biWidth: LONG,
        biHeight: LONG,
        biPlanes: WORD,
        biBitCount: WORD,
        biCompression: DWORD,
        biSizeImage: DWORD,
        biXPelsPerMeter: LONG,
        biYPelsPerMeter: LONG,
        biClrUsed: DWORD,
        biClrImportant: DWORD,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct RGBQUAD {
        rgbBlue: u8,
        rgbGreen: u8,
        rgbRed: u8,
        rgbReserved: u8,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER,
        bmiColors: [RGBQUAD; 1],
    }

    #[repr(C)]
    struct INPUT {
        r#type: DWORD,
        union_: INPUTUNION,
    }

    #[repr(C)]
    union INPUTUNION {
        mi: MOUSEINPUT,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct MOUSEINPUT {
        dx: LONG,
        dy: LONG,
        mouseData: DWORD,
        dwFlags: DWORD,
        time: DWORD,
        dwExtraInfo: usize,
    }

    const BI_RGB: DWORD = 0;

    const INPUT_MOUSE: DWORD = 0;
    const MOUSEEVENTF_MOVE: DWORD = 0x0001;
    const MOUSEEVENTF_LEFTDOWN: DWORD = 0x0002;
    const MOUSEEVENTF_LEFTUP: DWORD = 0x0004;
    const MOUSEEVENTF_RIGHTDOWN: DWORD = 0x0008;
    const MOUSEEVENTF_RIGHTUP: DWORD = 0x0010;
    const MOUSEEVENTF_MIDDLEDOWN: DWORD = 0x0020;
    const MOUSEEVENTF_MIDDLEUP: DWORD = 0x0040;
    const MOUSEEVENTF_WHEEL: DWORD = 0x0800;
    const MOUSEEVENTF_HWHEEL: DWORD = 0x01000;
    const MOUSEEVENTF_ABSOLUTE: DWORD = 0x8000;

    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;

    const PW_RENDERFULLCONTENT: UINT = 0x00000002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(cb: extern "system" fn(HWND, LPARAM) -> BOOL, lparam: LPARAM) -> BOOL;
        fn IsWindowVisible(hwnd: HWND) -> BOOL;
        fn IsIconic(hwnd: HWND) -> BOOL;
        fn GetWindowTextLengthW(hwnd: HWND) -> i32;
        fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: i32) -> i32;
        fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
        fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
        fn ClientToScreen(hwnd: HWND, point: *mut POINT) -> BOOL;
        fn GetForegroundWindow() -> HWND;
        fn SetForegroundWindow(hwnd: HWND) -> BOOL;
        fn GetSystemMetrics(index: i32) -> i32;
        fn SendInput(n: UINT, inputs: *const INPUT, cb_size: i32) -> UINT;
        fn PrintWindow(hwnd: HWND, hdc: HDC, flags: UINT) -> BOOL;
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct POINT {
        x: LONG,
        y: LONG,
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetDC(hwnd: HWND) -> HDC;
        fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
        fn CreateCompatibleDC(hdc: HDC) -> HDC;
        fn DeleteDC(hdc: HDC) -> BOOL;
        fn CreateCompatibleBitmap(hdc: HDC, w: i32, h: i32) -> HBITMAP;
        fn SelectObject(hdc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
        fn DeleteObject(obj: HGDIOBJ) -> BOOL;
        fn BitBlt(
            hdc: HDC,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            src: HDC,
            x1: i32,
            y1: i32,
            rop: DWORD,
        ) -> BOOL;
        fn GetDIBits(
            hdc: HDC,
            hbmp: HBITMAP,
            start: UINT,
            lines: UINT,
            bits: *mut c_void,
            info: *mut BITMAPINFO,
            usage: UINT,
        ) -> i32;
    }

    const SRCCOPY: DWORD = 0x00CC0020;
    const DIB_RGB_COLORS: UINT = 0;

    pub(super) fn list_windows() -> Result<Vec<WindowInfo>> {
        unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let vec_ptr = lparam as *mut Vec<WindowInfo>;
            if vec_ptr.is_null() {
                return 1;
            }

            unsafe {
                if IsWindowVisible(hwnd) == 0 {
                    return 1;
                }
                if IsIconic(hwnd) != 0 {
                    return 1;
                }

                let len = GetWindowTextLengthW(hwnd);
                if len <= 0 {
                    return 1;
                }

                let mut buf = vec![0u16; (len as usize) + 1];
                let got = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                if got <= 0 {
                    return 1;
                }
                let title = String::from_utf16_lossy(&buf[..(got as usize)]);
                if title.trim().is_empty() {
                    return 1;
                }

                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if GetWindowRect(hwnd, &mut rect as *mut RECT) == 0 {
                    return 1;
                }
                if rect.right <= rect.left || rect.bottom <= rect.top {
                    return 1;
                }

                let hwnd_key = hwnd as usize as u64;
                (*vec_ptr).push(WindowInfo {
                    hwnd_key,
                    title,
                    rect: Rect {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                    },
                });
                1
            }
        }

        unsafe {
            let mut out: Vec<WindowInfo> = Vec::new();
            let ok = EnumWindows(enum_cb, (&mut out as *mut Vec<WindowInfo>) as isize);
            ensure!(
                ok != 0,
                Error::Internal("EnumWindows failed".to_string()),
            );
            Ok(out)
        }
    }

    pub(super) fn capture_window_bgra(hwnd_key: u64) -> Result<(BufferMetadata, Vec<u8>)> {
        unsafe {
            let hwnd = hwnd_key as usize as HWND;

            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            ensure!(
                GetWindowRect(hwnd, &mut rect as *mut RECT) != 0,
                Error::Internal("GetWindowRect failed".to_string()),
            );

            let width = (rect.right - rect.left).max(1);
            let height = (rect.bottom - rect.top).max(1);

            let window_dc = GetDC(hwnd);
            ensure!(
                !window_dc.is_null(),
                Error::Internal("GetDC returned null".to_string()),
            );
            let mem_dc = CreateCompatibleDC(window_dc);
            ensure!(
                !mem_dc.is_null(),
                Error::Internal("CreateCompatibleDC returned null".to_string()),
            );

            let bmp = CreateCompatibleBitmap(window_dc, width, height);
            ensure!(
                !bmp.is_null(),
                Error::Internal("CreateCompatibleBitmap returned null".to_string()),
            );
            let old = SelectObject(mem_dc, bmp as HGDIOBJ);
            ensure!(
                !old.is_null(),
                Error::Internal("SelectObject failed".to_string()),
            );

            // Prefer PrintWindow for occluded windows; fall back to BitBlt.
            let printed = PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT);
            if printed == 0 {
                let _ = BitBlt(mem_dc, 0, 0, width, height, window_dc, 0, 0, SRCCOPY);
            }

            let stride = width * 4;
            let mut out = vec![0u8; (stride as usize) * (height as usize)];

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
                    biWidth: width,
                    // negative = top-down
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };

            let got = GetDIBits(
                mem_dc,
                bmp,
                0,
                height as UINT,
                out.as_mut_ptr() as *mut c_void,
                &mut bmi as *mut BITMAPINFO,
                DIB_RGB_COLORS,
            );
            ensure!(
                got != 0,
                Error::Internal("GetDIBits failed".to_string()),
            );

            // Many sources produce an undefined alpha channel; force it opaque.
            for px in out.chunks_exact_mut(4) {
                px[3] = 0xFF;
            }

            SelectObject(mem_dc, old);
            DeleteObject(bmp as HGDIOBJ);
            DeleteDC(mem_dc);
            ReleaseDC(hwnd, window_dc);

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

    fn mouse_absolute(x: i32, y: i32) -> (i32, i32) {
        unsafe {
            let screen_w = GetSystemMetrics(SM_CXSCREEN).max(1);
            let screen_h = GetSystemMetrics(SM_CYSCREEN).max(1);
            let denom_w = (screen_w as i64 - 1).max(1);
            let denom_h = (screen_h as i64 - 1).max(1);
            let ax = ((x.clamp(0, screen_w - 1) as i64) * 65535 / denom_w) as i32;
            let ay = ((y.clamp(0, screen_h - 1) as i64) * 65535 / denom_h) as i32;
            (ax, ay)
        }
    }

    pub(super) fn post_mouse_motion(button_mask: u32, x: i32, y: i32) -> Result<()> {
        unsafe {
            let (ax, ay) = mouse_absolute(x, y);
            let flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE;
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                union_: INPUTUNION {
                    mi: MOUSEINPUT {
                        dx: ax,
                        dy: ay,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let sent = SendInput(
                1,
                &input as *const INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
            ensure!(
                sent == 1,
                Error::Internal("SendInput failed".to_string()),
            );
            let _ = button_mask;
            Ok(())
        }
    }

    pub(super) fn post_mouse_button(down: bool, button: u32, x: i32, y: i32) -> Result<()> {
        unsafe {
            // Ensure cursor is positioned.
            post_mouse_motion(0, x, y).location(loc!())?;

            let flag = match (button, down) {
                (272, true) => MOUSEEVENTF_LEFTDOWN,
                (272, false) => MOUSEEVENTF_LEFTUP,
                (273, true) => MOUSEEVENTF_RIGHTDOWN,
                (273, false) => MOUSEEVENTF_RIGHTUP,
                (274, true) => MOUSEEVENTF_MIDDLEDOWN,
                (274, false) => MOUSEEVENTF_MIDDLEUP,
                _ => return Ok(()),
            };

            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                union_: INPUTUNION {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let sent = SendInput(
                1,
                &input as *const INPUT,
                std::mem::size_of::<INPUT>() as i32,
            );
            ensure!(
                sent == 1,
                Error::Internal("SendInput failed".to_string()),
            );
            Ok(())
        }
    }

    pub(super) fn post_scroll(horizontal: i32, vertical: i32) -> Result<()> {
        unsafe {
            if vertical != 0 {
                let mut input = INPUT {
                    r#type: INPUT_MOUSE,
                    union_: INPUTUNION {
                        mi: MOUSEINPUT {
                            dx: 0,
                            dy: 0,
                            mouseData: (vertical * 120) as DWORD,
                            dwFlags: MOUSEEVENTF_WHEEL,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let sent = SendInput(
                    1,
                    &input as *const INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
                ensure!(
                    sent == 1,
                    Error::Internal("SendInput failed".to_string()),
                );
            }
            if horizontal != 0 {
                let mut input = INPUT {
                    r#type: INPUT_MOUSE,
                    union_: INPUTUNION {
                        mi: MOUSEINPUT {
                            dx: 0,
                            dy: 0,
                            mouseData: (horizontal * 120) as DWORD,
                            dwFlags: MOUSEEVENTF_HWHEEL,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let sent = SendInput(
                    1,
                    &input as *const INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
                ensure!(
                    sent == 1,
                    Error::Internal("SendInput failed".to_string()),
                );
            }
            Ok(())
        }
    }
}
