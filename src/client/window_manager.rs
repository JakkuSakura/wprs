use std::collections::HashMap;

use crate::protocols::wprs::wayland::ClientSurface;
use crate::protocols::wprs::wayland::Role;
use crate::protocols::wprs::wayland::SurfaceState;
use crate::protocols::wprs::wayland::WlSurfaceId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WlSurfaceId,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub size: Option<(u32, u32)>,
}

impl WindowInfo {
    pub fn display_title(&self) -> Option<&str> {
        self.title.as_deref().or(self.app_id.as_deref())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct WindowDelta {
    pub upserts: Vec<WindowInfo>,
    pub removed: Vec<WlSurfaceId>,
}

#[derive(Debug, Default)]
pub struct WindowManager {
    windows: HashMap<WlSurfaceId, WindowInfo>,
}

impl WindowManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_surface_updates(
        &mut self,
        updated: &[SurfaceState],
        removed: &[ClientSurface],
    ) -> WindowDelta {
        let mut delta = WindowDelta::default();

        for state in updated {
            let Some(info) = Self::window_info_from_state(state) else {
                if self.windows.remove(&state.id).is_some() {
                    delta.removed.push(state.id);
                }
                continue;
            };

            let is_changed = self
                .windows
                .get(&state.id)
                .map(|existing| existing != &info)
                .unwrap_or(true);
            if is_changed {
                self.windows.insert(state.id, info.clone());
                delta.upserts.push(info);
            }
        }

        for removed_surface in removed {
            if self.windows.remove(&removed_surface.surface).is_some() {
                delta.removed.push(removed_surface.surface);
            }
        }

        delta
    }

    fn window_info_from_state(state: &SurfaceState) -> Option<WindowInfo> {
        let Role::XdgToplevel(toplevel) = state.role.as_ref()? else {
            return None;
        };
        let size = state
            .bitmap
            .as_ref()
            .and_then(|assignment| assignment.as_new())
            .and_then(|bitmap| {
                let width = u32::try_from(bitmap.metadata.width).ok()?;
                let height = u32::try_from(bitmap.metadata.height).ok()?;
                Some((width, height))
            });

        Some(WindowInfo {
            id: state.id,
            title: toplevel.title.clone(),
            app_id: toplevel.app_id.clone(),
            size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::wprs::wayland::SurfaceState;

    fn make_state(id: u64, title: Option<&str>) -> SurfaceState {
        SurfaceState {
            client: crate::protocols::wprs::types::ClientId(1),
            id: WlSurfaceId(id),
            bitmap: None,
            bitmap_update: None,
            role: Some(Role::XdgToplevel(crate::protocols::wprs::xdg_shell::XdgToplevelState {
                id: crate::protocols::wprs::xdg_shell::XdgToplevelId(id),
                parent: None,
                title: title.map(|t| t.to_string()),
                app_id: None,
                decoration_mode: None,
                maximized: None,
                fullscreen: None,
            })),
            buffer_scale: 1,
            buffer_transform: None,
            opaque_region: None,
            input_region: None,
            z_ordered_children: Vec::new(),
            damage: None,
            output_ids: Vec::new(),
            viewport_state: None,
            xdg_surface_state: None,
        }
    }

    #[test]
    fn upserts_window_without_title() {
        let mut manager = WindowManager::new();
        let state = make_state(42, None);
        let delta = manager.apply_surface_updates(&[state], &[]);
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].id, WlSurfaceId(42));
    }
}
