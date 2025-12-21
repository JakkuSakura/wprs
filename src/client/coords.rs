use crate::protocols::wprs::geometry::Point;

#[derive(Debug, Clone, Copy)]
pub struct UiScaleFactor(pub f64);

impl UiScaleFactor {
    pub fn normalized(self) -> f64 {
        self.0.max(0.1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ServerBufferScale(pub i32);

impl ServerBufferScale {
    pub fn normalized(self) -> f64 {
        (self.0.max(1)) as f64
    }
}

#[cfg(feature = "winit-wgpu-client")]
pub mod winit {
    use winit::dpi::LogicalPosition;
    use winit::dpi::PhysicalPosition;
    use winit::window::Window;

    use super::Point;
    use super::ServerBufferScale;
    use super::UiScaleFactor;

    pub fn physical_to_window_logical(window: &Window, pos: PhysicalPosition<f64>) -> Point<f64> {
        let logical = pos.to_logical::<f64>(window.scale_factor());
        Point {
            x: logical.x,
            y: logical.y,
        }
    }

    pub fn window_logical_to_physical(window: &Window, pos: Point<f64>) -> PhysicalPosition<f64> {
        let scale = window.scale_factor();
        PhysicalPosition::new(pos.x * scale, pos.y * scale)
    }

    pub fn window_logical_to_remote_logical(ui_scale: UiScaleFactor, pos: Point<f64>) -> Point<f64> {
        let scale = ui_scale.normalized();
        Point {
            x: pos.x / scale,
            y: pos.y / scale,
        }
    }

    pub fn remote_logical_to_window_logical(ui_scale: UiScaleFactor, pos: Point<f64>) -> Point<f64> {
        let scale = ui_scale.normalized();
        Point {
            x: pos.x * scale,
            y: pos.y * scale,
        }
    }

    /// Position to pass to `Window::show_window_menu` on Wayland.
    ///
    /// Internally winit will convert from `Position` to logical coordinates using the window
    /// scale factor; feeding it a logical `u32` avoids HiDPI ambiguity.
    pub fn window_menu_position(window: &Window, cursor_physical: PhysicalPosition<f64>) -> LogicalPosition<u32> {
        let scale = window.scale_factor().max(0.1);
        let x = (cursor_physical.x / scale).round().max(0.0) as u32;
        let y = (cursor_physical.y / scale).round().max(0.0) as u32;
        LogicalPosition::new(x, y)
    }

    /// Convert a popup offset expressed in the parent surface coordinate space into a host-window
    /// pixel delta.
    pub fn popup_offset_to_host_px(
        window: &Window,
        ui_scale: UiScaleFactor,
        server_scale: ServerBufferScale,
        dx_server: i32,
        dy_server: i32,
    ) -> (i32, i32) {
        let client_scale = window.scale_factor();
        let server_scale = server_scale.normalized();
        let total_scale = (client_scale / server_scale) * ui_scale.normalized();

        let dx = (dx_server as f64 * total_scale).round() as i32;
        let dy = (dy_server as f64 * total_scale).round() as i32;
        (dx, dy)
    }
}

