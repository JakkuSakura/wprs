use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Mutex;

use crate::prelude::*;

use crate::protocols::wprs::transport;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::DisplayConfig;
use crate::protocols::wprs::types::Request;
use crate::protocols::wprs::wayland::ClientSurface;
use crate::protocols::wprs::wayland::CursorImage;
use crate::protocols::wprs::wayland::DataRequest;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::SurfaceState;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::protocols::wprs::xdg_shell::PopupRequest;
use crate::protocols::wprs::xdg_shell::ToplevelRequest;
use crate::protocols::wprs::types::ClientId;

#[derive(Clone, Debug)]
pub enum ClientEvent {
    Capabilities(Capabilities),
    DisplayConfig(DisplayConfig),
    TransportConfig(transport::TransportConfig),
    TransportConfigScoped {
        surface: WlSurfaceId,
        config: transport::TransportConfig,
    },
    CursorImage(CursorImage),
    Toplevel(ToplevelRequest),
    Popup(PopupRequest),
    Data(DataRequest),
    ClientDisconnected(ClientId),
}

#[derive(Debug, Default)]
pub struct ClientState {
    surfaces: Mutex<HashMap<ClientSurface, SurfaceState>>,
    surface_updates: Mutex<HashMap<ClientSurface, SurfaceState>>,
    surface_removals: Mutex<HashSet<ClientSurface>>,
    events: Mutex<Vec<ClientEvent>>,
    display_config: Mutex<Option<DisplayConfig>>,
    capabilities: Mutex<Option<Capabilities>>,
    transport_config: Mutex<transport::TransportConfig>,
    transport_config_by_surface: Mutex<HashMap<WlSurfaceId, transport::TransportConfig>>,
}

impl ClientState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_request(&self, request: Request) -> bool {
        match request {
            Request::Surface(surface) => {
                crate::client::surface_registry::record_surface(&surface);
                match surface.payload {
                    SurfaceRequestPayload::Commit(state) => {
                        let key = ClientSurface {
                            client: surface.client,
                            surface: surface.surface,
                        };
                        let mut surfaces = self.surfaces.lock().unwrap();
                        let changed = surfaces
                            .get(&key)
                            .map(|prev| prev != &state)
                            .unwrap_or(true);
                        let update = if changed { Some(state.clone()) } else { None };
                        surfaces.insert(key, state);
                        drop(surfaces);
                        if let Some(update) = update {
                            self.surface_updates.lock().unwrap().insert(key, update);
                            self.surface_removals.lock().unwrap().remove(&key);
                        }
                    }
                    SurfaceRequestPayload::Destroyed => {
                        let key = ClientSurface {
                            client: surface.client,
                            surface: surface.surface,
                        };
                        let mut surfaces = self.surfaces.lock().unwrap();
                        surfaces.remove(&key);
                        drop(surfaces);
                        self.surface_updates.lock().unwrap().remove(&key);
                        self.surface_removals.lock().unwrap().insert(key);
                    }
                }
                true
            }
            Request::Capabilities(caps) => {
                *self.capabilities.lock().unwrap() = Some(caps.clone());
                self.events.lock().unwrap().push(ClientEvent::Capabilities(caps));
                true
            }
            Request::DisplayConfig(cfg) => {
                *self.display_config.lock().unwrap() = Some(cfg.clone());
                self.events.lock().unwrap().push(ClientEvent::DisplayConfig(cfg));
                true
            }
            Request::Transport(req) => {
                match req {
                    transport::TransportRequest::Config(cfg) => {
                        info!(
                            "transport config updated: codec={:?} max_fps={:?} buffer_patches={} jpeg_quality={:?} h264_bitrate_kbps={:?}",
                            cfg.codec,
                            cfg.max_fps,
                            cfg.buffer_patches.enabled,
                            cfg.jpeg_quality,
                            cfg.h264_bitrate_kbps,
                        );
                        *self.transport_config.lock().unwrap() = cfg.clone();
                        self.events
                            .lock()
                            .unwrap()
                            .push(ClientEvent::TransportConfig(cfg));
                    }
                    transport::TransportRequest::ConfigScoped { scope, config } => match scope {
                        transport::TransportScope::Global => {
                            info!(
                                "transport config updated: codec={:?} max_fps={:?} buffer_patches={} jpeg_quality={:?} h264_bitrate_kbps={:?}",
                                config.codec,
                                config.max_fps,
                                config.buffer_patches.enabled,
                                config.jpeg_quality,
                                config.h264_bitrate_kbps,
                            );
                            *self.transport_config.lock().unwrap() = config.clone();
                            self.events
                                .lock()
                                .unwrap()
                                .push(ClientEvent::TransportConfig(config));
                        }
                        transport::TransportScope::Surface(surface) => {
                            debug!(
                                "surface transport config updated: surface={surface:?} codec={:?} max_fps={:?} buffer_patches={} jpeg_quality={:?} h264_bitrate_kbps={:?}",
                                config.codec,
                                config.max_fps,
                                config.buffer_patches.enabled,
                                config.jpeg_quality,
                                config.h264_bitrate_kbps,
                            );
                            self.transport_config_by_surface
                                .lock()
                                .unwrap()
                                .insert(surface, config.clone());
                            self.events
                                .lock()
                                .unwrap()
                                .push(ClientEvent::TransportConfigScoped {
                                    surface,
                                    config,
                                });
                        }
                    },
                    transport::TransportRequest::Pong(_) => {}
                }
                true
            }
            Request::CursorImage(cursor) => {
                self.events.lock().unwrap().push(ClientEvent::CursorImage(cursor));
                true
            }
            Request::Toplevel(req) => {
                self.events.lock().unwrap().push(ClientEvent::Toplevel(req));
                true
            }
            Request::Popup(req) => {
                self.events.lock().unwrap().push(ClientEvent::Popup(req));
                true
            }
            Request::Data(req) => {
                self.events.lock().unwrap().push(ClientEvent::Data(req));
                true
            }
            Request::ClientDisconnected(client) => {
                self.events
                    .lock()
                    .unwrap()
                    .push(ClientEvent::ClientDisconnected(client));
                true
            }
        }
    }

    pub fn snapshot_surfaces(&self) -> Vec<SurfaceState> {
        let surfaces = self.surfaces.lock().unwrap();
        let mut out: Vec<_> = surfaces.values().cloned().collect();
        out.sort_by_key(|state| (state.client.0, state.id.0));
        out
    }

    pub fn drain_surface_updates(&self) -> SurfaceDelta {
        let mut updates = self.surface_updates.lock().unwrap();
        let mut removals = self.surface_removals.lock().unwrap();

        let mut updated: Vec<_> = updates.drain().map(|(_, state)| state).collect();
        updated.sort_by_key(|state| (state.client.0, state.id.0));

        let mut removed: Vec<_> = removals.drain().collect();
        removed.sort_by_key(|key| (key.client.0, key.surface.0));

        SurfaceDelta { updated, removed }
    }

    pub fn drain_events(&self) -> Vec<ClientEvent> {
        let mut events = self.events.lock().unwrap();
        let drained = events.drain(..).collect::<Vec<_>>();
        drained
    }

    pub fn transport_config(&self) -> transport::TransportConfig {
        self.transport_config.lock().unwrap().clone()
    }

    pub fn transport_config_for_surface(
        &self,
        surface: WlSurfaceId,
    ) -> Option<transport::TransportConfig> {
        self.transport_config_by_surface
            .lock()
            .unwrap()
            .get(&surface)
            .cloned()
    }
}

#[derive(Debug, Default)]
pub struct SurfaceDelta {
    pub updated: Vec<SurfaceState>,
    pub removed: Vec<ClientSurface>,
}

#[derive(Debug, Default)]
pub struct ClientUpdateBatch {
    pub events: Vec<ClientEvent>,
    pub surfaces: SurfaceDelta,
}

pub fn drain_client_updates(
    notify_rx: &std::sync::mpsc::Receiver<()>,
    state: &ClientState,
) -> Result<Option<ClientUpdateBatch>> {
    let mut notified = false;
    while notify_rx.try_recv().is_ok() {
        notified = true;
    }
    if !notified {
        return Ok(None);
    }
    let events = state.drain_events();
    let surfaces = state.drain_surface_updates();
    Ok(Some(ClientUpdateBatch { events, surfaces }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::wprs::serializer::new_inproc_serializer_pair;
    use crate::protocols::wprs::serializer::SendType;
    use crate::protocols::wprs::serializer::RecvType;
    use crate::protocols::wprs::wayland::Bitmap;
    use crate::protocols::wprs::wayland::BitmapAssignment;
    use crate::protocols::wprs::wayland::BufferMetadata;
    use crate::protocols::wprs::wayland::BufferFormat;
    use crate::protocols::wprs::wayland::SurfaceRequest;
    use crate::protocols::wprs::wayland::SurfaceRequestPayload;
    use crate::protocols::wprs::wayland::WlSurfaceId;

    #[test]
    fn server_to_client_surface_commit_updates_state() {
        let (mut server, mut client) =
            new_inproc_serializer_pair::<Request, Event>().expect("serializer pair");
        let mut reader = client.reader().expect("reader");

        let surface_id = WlSurfaceId(99);
        let bitmap = Bitmap {
            metadata: BufferMetadata {
                width: 2,
                height: 1,
                stride: 8,
                format: BufferFormat::Argb8888,
            },
            data: crate::protocols::wprs::wayland::BufferPoolHandle::from(vec![
                1, 2, 3, 4, 0, 0, 0, 0,
            ]),
        };
        let state = SurfaceState {
            client: ClientId(1),
            id: surface_id,
            bitmap: Some(BitmapAssignment::New(bitmap)),
            bitmap_update: None,
            role: None,
            buffer_scale: 1,
            buffer_transform: None,
            opaque_region: None,
            input_region: None,
            z_ordered_children: Vec::new(),
            damage: None,
            output_ids: Vec::new(),
            viewport_state: None,
            xdg_surface_state: None,
        };
        let request = Request::Surface(SurfaceRequest {
            client: ClientId(1),
            surface: surface_id,
            payload: SurfaceRequestPayload::Commit(state),
        });

        server.writer().send(SendType::Object(request));

        let msg = reader.recv().expect("recv message");
        let RecvType::Object(request) = msg else {
            panic!("unexpected recv type");
        };

        let client_state = ClientState::new();
        assert!(client_state.apply_request(request));
        let delta = client_state.drain_surface_updates();
        assert_eq!(delta.updated.len(), 1);
        assert_eq!(delta.updated[0].id, surface_id);
    }
}
