use crate::prelude::*;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::ClientId;
use crate::protocols::wprs::types::DisplayConfig;
use crate::protocols::wprs::types::Event;
use crate::protocols::wprs::wayland;
use crate::protocols::wprs::wayland::AxisSource;
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::PointerEventKind;
use crate::protocols::wprs::wayland::PointerGestureEvent;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::protocols::wprs::xdg_shell;
use crate::server::backend::BackendObservation;
use crate::server::backend::BackendBgraFrame;
use crate::server::backend::BackendSurfaceDescriptor;
use crate::server::backend::BackendSurfaceRole;
use crate::server::backend::PollingBackend;
use macos::capture_main_display_bgra;
use macos::post_mouse_button;
use macos::post_mouse_motion;
use macos::post_scroll;

#[derive(Debug)]
pub struct MacosFullscreenBackend {
    surface: BackendSurfaceDescriptor,
    pressed_buttons: u32,
    last_pos: (f64, f64),
    display_config: DisplayConfig,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ScrollUnit {
    Line,
    Pixel,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosFullscreenBackendConfig {
    pub dpi: Option<u32>,
}

impl MacosFullscreenBackend {
    pub fn new(config: MacosFullscreenBackendConfig) -> Self {
        let (detected_scale_factor, detected_dpi) =
            display_scale_factor_and_dpi().unwrap_or((1, None));
        let scale_factor = detected_scale_factor.max(1);
        let dpi = config.dpi.or(detected_dpi);
        let display_config = DisplayConfig { scale_factor, dpi };

        let surface = BackendSurfaceDescriptor {
            client: ClientId(1),
            id: WlSurfaceId(1),
            role: BackendSurfaceRole::XdgToplevel {
                id: xdg_shell::XdgToplevelId(1),
                title: Some("wprs (macOS)".to_string()),
                app_id: Some("wprs".to_string()),
            },
            buffer_scale: display_config.scale_factor,
        };

        Self {
            surface,
            pressed_buttons: 0,
            last_pos: (0.0, 0.0),
            display_config,
        }
    }
}

impl PollingBackend for MacosFullscreenBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities { xwayland: false }
    }

    fn supports_hidpi(&self) -> bool {
        false
    }

    fn display_config(&self) -> DisplayConfig {
        self.display_config.clone()
    }

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        let (metadata, bgra) = capture_main_display_bgra().location(loc!())?;
        Ok(vec![BackendObservation::SurfaceCommit {
            surface: self.surface.clone(),
            frame: Some(BackendBgraFrame { metadata, bgra }),
        }])
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        let (metadata, bgra) = capture_main_display_bgra().location(loc!())?;
        Ok(vec![BackendObservation::SurfaceCommit {
            surface: self.surface.clone(),
            frame: Some(BackendBgraFrame { metadata, bgra }),
        }])
    }

    fn handle_client_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::PointerFrame(events) => {
                for e in events {
                    self.handle_pointer_event(e).log_and_ignore(loc!());
                }
            },
            Event::PointerGesture(event) => {
                self.handle_pointer_gesture(event).log_and_ignore(loc!());
            },
            // Keyboard input injection is currently best-effort.
            // wprsc primarily emits Linux evdev raw codes, which don't map 1:1
            // to macOS CGKeyCode.
            Event::KeyboardEvent(_) => {},
            _ => {},
        }
        Ok(())
    }
}

impl MacosFullscreenBackend {
    fn handle_pointer_event(&mut self, e: wayland::PointerEvent) -> Result<()> {
        let x = e.position.x;
        let y = e.position.y;
        self.last_pos = (x, y);

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
                source,
            } => {
                let unit = match source {
                    Some(AxisSource::Finger | AxisSource::Continuous) => ScrollUnit::Pixel,
                    _ => ScrollUnit::Line,
                };

                match unit {
                    ScrollUnit::Line => {
                        post_scroll(unit, horizontal.discrete, vertical.discrete, false)
                            .location(loc!())
                    },
                    ScrollUnit::Pixel => post_scroll(
                        unit,
                        horizontal.absolute.round() as i32,
                        vertical.absolute.round() as i32,
                        false,
                    )
                    .location(loc!()),
                }
            },
        }
    }

    fn handle_pointer_gesture(&mut self, e: PointerGestureEvent) -> Result<()> {
        match e {
            PointerGestureEvent::SwipeBegin { .. }
            | PointerGestureEvent::SwipeUpdate { .. }
            | PointerGestureEvent::SwipeEnd { .. }
            | PointerGestureEvent::HoldBegin { .. }
            | PointerGestureEvent::HoldEnd { .. }
            | PointerGestureEvent::PinchBegin { .. } => Ok(()),
            PointerGestureEvent::PinchUpdate { scale, .. } => {
                // Best-effort mapping: translate pinch-to-zoom into Ctrl+scroll.
                // `scale` is absolute (1.0 at begin).
                let delta = ((scale - 1.0) * 120.0).round() as i32;
                if delta != 0 {
                    post_scroll(ScrollUnit::Pixel, 0, delta, true).location(loc!())?;
                }
                Ok(())
            },
            PointerGestureEvent::PinchEnd { .. } => Ok(()),
        }
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

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::error::ensure;
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEvent,
        CGEventFlags,
        CGEventTapLocation,
        CGEventType,
        CGMouseButton,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    pub(super) fn capture_main_display_bgra() -> Result<(BufferMetadata, Vec<u8>)> {
        let display = CGDisplay::main();
        let image = display.image().ok_or_else(|| {
            Error::Internal("CGDisplayCreateImage returned null (Screen Recording permission?)".to_string())
        })?;

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
        ensure!(
            bytes.len() >= (stride as usize) * (height as usize),
            Error::Internal("CGImage data smaller than expected".to_string()),
        );

        let metadata = BufferMetadata {
            width,
            height,
            stride,
            format: crate::protocols::wprs::wayland::BufferFormat::Argb8888,
        };

        Ok((metadata, bytes.to_vec()))
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

    pub(super) fn post_scroll(unit: ScrollUnit, dx: i32, dy: i32, with_ctrl: bool) -> Result<()> {
        let source = event_source()?;
        let units = match unit {
            ScrollUnit::Line => ScrollEventUnit::LINE,
            ScrollUnit::Pixel => ScrollEventUnit::PIXEL,
        };

        let ev = CGEvent::new_scroll_event(source, units, 2, dy, dx, 0)
            .map_err(|_| Error::Internal("CGEventCreateScrollWheelEvent failed".to_string()))?;
        if with_ctrl {
            ev.set_flags(CGEventFlags::CGEventFlagControl);
        }
        ev.post(CGEventTapLocation::HID);
        Ok(())
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
}
