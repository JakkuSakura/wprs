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

#[cfg(feature = "winit-wgpu")]
pub mod winit {
    use winit::dpi::PhysicalPosition;
    use winit::window::Window;

    use super::Point;
    use super::ServerBufferScale;
    use super::UiScaleFactor;

    pub fn physical_to_window_logical(window: &dyn Window, pos: PhysicalPosition<f64>) -> Point<f64> {
        let logical = pos.to_logical::<f64>(window.scale_factor());
        Point {
            x: logical.x,
            y: logical.y,
        }
    }

    pub fn window_logical_to_physical(window: &dyn Window, pos: Point<f64>) -> PhysicalPosition<f64> {
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

    /// Convert a popup offset expressed in the parent surface coordinate space into a host-window
    /// pixel delta.
    pub fn popup_offset_to_host_px(
        window: &dyn Window,
        ui_scale: UiScaleFactor,
        server_scale: ServerBufferScale,
        dx_server: i32,
        dy_server: i32,
    ) -> (i32, i32) {
        popup_offset_to_host_px_scaled(
            window.scale_factor(),
            ui_scale,
            server_scale,
            dx_server,
            dy_server,
        )
    }

    pub fn popup_offset_to_host_px_scaled(
        client_scale: f64,
        ui_scale: UiScaleFactor,
        server_scale: ServerBufferScale,
        dx_server: i32,
        dy_server: i32,
    ) -> (i32, i32) {
        let client_scale = client_scale.max(0.1);
        let server_scale = server_scale.normalized();
        let total_scale = (client_scale / server_scale) * ui_scale.normalized();

        let dx = (dx_server as f64 * total_scale).round() as i32;
        let dy = (dy_server as f64 * total_scale).round() as i32;
        (dx, dy)
    }
}

#[cfg(all(test, feature = "winit-wgpu"))]
mod tests {
    use super::winit::popup_offset_to_host_px_scaled;
    use super::{ServerBufferScale, UiScaleFactor};

    #[test]
    fn popup_offset_scaled_identity() {
        let (dx, dy) =
            popup_offset_to_host_px_scaled(1.0, UiScaleFactor(1.0), ServerBufferScale(1), 10, -3);
        assert_eq!((dx, dy), (10, -3));
    }

    #[test]
    fn popup_offset_scaled_accounts_for_server_scale() {
        let (dx, dy) =
            popup_offset_to_host_px_scaled(2.0, UiScaleFactor(1.0), ServerBufferScale(2), 10, 10);
        assert_eq!((dx, dy), (10, 10));
    }

    #[test]
    fn popup_offset_scaled_accounts_for_ui_scale() {
        let (dx, dy) =
            popup_offset_to_host_px_scaled(2.0, UiScaleFactor(1.5), ServerBufferScale(1), 10, 10);
        assert_eq!((dx, dy), (30, 30));
    }
}
