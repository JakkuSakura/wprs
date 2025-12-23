use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::prelude::*;
use crate::protocols::wprs::wayland::BitmapAssignment;
use crate::protocols::wprs::wayland::ClientSurface;
use crate::protocols::wprs::wayland::Role;
use crate::protocols::wprs::wayland::SurfaceRequest;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::SurfaceState;

const LOG_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
struct WindowEntry {
    title: Option<String>,
    app_id: Option<String>,
    size: Option<(i32, i32)>,
    last_updated: Instant,
}

impl WindowEntry {
    fn from_state(state: &SurfaceState) -> Option<Self> {
        let role = state.role.as_ref()?;
        let Role::XdgToplevel(toplevel) = role else {
            return None;
        };
        let size = state
            .bitmap
            .as_ref()
            .and_then(|assignment| match assignment {
                BitmapAssignment::New(bitmap) => {
                    Some((bitmap.metadata.width, bitmap.metadata.height))
                }
                BitmapAssignment::Removed => None,
            });

        Some(Self {
            title: toplevel.title.clone(),
            app_id: toplevel.app_id.clone(),
            size,
            last_updated: Instant::now(),
        })
    }
}

struct SurfaceRegistry {
    entries: Mutex<HashMap<ClientSurface, WindowEntry>>,
}

impl SurfaceRegistry {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn record_surface(&self, surface: &SurfaceRequest) {
        let key = ClientSurface {
            client: surface.client,
            surface: surface.surface,
        };
        match &surface.payload {
            SurfaceRequestPayload::Destroyed => {
                let mut entries = self.entries.lock().unwrap();
                entries.remove(&key);
            }
            SurfaceRequestPayload::Commit(state) => {
                let Some(entry) = WindowEntry::from_state(state) else {
                    return;
                };
                let mut entries = self.entries.lock().unwrap();
                entries.insert(key, entry);
            }
        }
    }

    fn snapshot(&self) -> Vec<(ClientSurface, WindowEntry)> {
        let entries = self.entries.lock().unwrap();
        let mut snapshot: Vec<_> = entries.iter().map(|(k, v)| (*k, v.clone())).collect();
        snapshot.sort_by_key(|(k, _)| (k.client.0, k.surface.0));
        snapshot
    }
}

fn registry() -> &'static SurfaceRegistry {
    static REGISTRY: OnceLock<SurfaceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(SurfaceRegistry::new)
}

fn spawn_logger() {
    thread::spawn(|| loop {
        thread::sleep(LOG_INTERVAL);
        let snapshot = registry().snapshot();
        if snapshot.is_empty() {
            info!("managed windows: (none)");
            continue;
        }
        let mut lines = Vec::new();
        for (surface, entry) in snapshot {
            let size = entry
                .size
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|| "unknown".to_string());
            let age = entry.last_updated.elapsed().as_secs();
            let title = entry.title.unwrap_or_else(|| "<untitled>".to_string());
            let app_id = entry.app_id.unwrap_or_else(|| "<unknown>".to_string());
            lines.push(format!(
                "client={} surface={} title={:?} app_id={:?} size={} age={}s",
                surface.client.0,
                surface.surface.0,
                title,
                app_id,
                size,
                age
            ));
        }
        info!("managed windows ({}): {}", lines.len(), lines.join(" | "));
    });
}

pub fn record_surface(surface: &SurfaceRequest) {
    static LOGGER: OnceLock<()> = OnceLock::new();
    LOGGER.get_or_init(|| {
        spawn_logger();
    });
    registry().record_surface(surface);
}
