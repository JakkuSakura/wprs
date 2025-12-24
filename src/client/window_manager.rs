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
            let already_known = self.windows.contains_key(&state.id);
            let info = match state.role.as_ref() {
                Some(Role::XdgToplevel(toplevel)) => Some(WindowInfo {
                    id: state.id,
                    title: toplevel.title.clone(),
                    app_id: toplevel.app_id.clone(),
                    size: bitmap_size(state),
                }),
                Some(_) => {
                    if self.windows.remove(&state.id).is_some() {
                        delta.removed.push(state.id);
                    }
                    None
                }
                None => {
                    if let Some(size) = bitmap_size(state) {
                        Some(WindowInfo {
                            id: state.id,
                            title: None,
                            app_id: None,
                            size: Some(size),
                        })
                    } else if already_known {
                        None
                    } else {
                        None
                    }
                }
            };

            let Some(info) = info else {
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

}

fn bitmap_size(state: &SurfaceState) -> Option<(u32, u32)> {
    state
        .bitmap
        .as_ref()
        .and_then(|assignment| assignment.as_new())
        .and_then(|bitmap| {
            let width = u32::try_from(bitmap.metadata.width).ok()?;
            let height = u32::try_from(bitmap.metadata.height).ok()?;
            Some((width, height))
        })
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

    fn make_state_with_bitmap(id: u64) -> SurfaceState {
        let mut state = make_state(id, None);
        state.role = None;
        state.bitmap = Some(crate::protocols::wprs::wayland::BitmapAssignment::New(
            crate::protocols::wprs::wayland::Bitmap {
                metadata: crate::protocols::wprs::wayland::BufferMetadata {
                    width: 1,
                    height: 1,
                    stride: 4,
                    format: crate::protocols::wprs::wayland::BufferFormat::Argb8888,
                },
                data: crate::protocols::wprs::wayland::BufferPoolHandle::from(vec![0, 0, 0, 0]),
            },
        ));
        state
    }

    #[test]
    fn upserts_window_without_title() {
        let mut manager = WindowManager::new();
        let state = make_state(42, None);
        let delta = manager.apply_surface_updates(&[state], &[]);
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].id, WlSurfaceId(42));
    }

    #[test]
    fn upserts_window_without_role_when_bitmap_present() {
        let mut manager = WindowManager::new();
        let state = make_state_with_bitmap(7);
        let delta = manager.apply_surface_updates(&[state], &[]);
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].id, WlSurfaceId(7));
    }

    #[test]
    fn removes_window_when_role_is_not_toplevel() {
        use crate::protocols::wprs::geometry::Point;

        let mut manager = WindowManager::new();
        let state = make_state(1, Some("demo"));
        let _ = manager.apply_surface_updates(&[state], &[]);

        let mut cursor_state = make_state(1, Some("demo"));
        cursor_state.role = Some(Role::Cursor(Point { x: 0, y: 0 }));
        let delta = manager.apply_surface_updates(&[cursor_state], &[]);
        assert_eq!(delta.removed, vec![WlSurfaceId(1)]);
    }
}
