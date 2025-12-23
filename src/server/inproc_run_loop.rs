use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use calloop::EventLoop as CalloopEventLoop;
use calloop::channel::Event as CalloopChannelEvent;
use calloop::timer::TimeoutAction;
use calloop::timer::Timer;

use crate::prelude::*;
use crate::protocols::wprs::handshake;
use crate::protocols::wprs::serializer::RecvType;
use crate::protocols::wprs::serializer::SendType;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::types::Event;
use crate::protocols::wprs::types::Request;
use crate::protocols::wprs::wayland::Bitmap;
use crate::protocols::wprs::wayland::BitmapAssignment;
use crate::protocols::wprs::wayland::Role;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::SurfaceState;
use crate::protocols::wprs::wayland::BufferPoolHandle;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::protocols::wprs::xdg_shell;
use crate::server::backend::BackendObservation;
use crate::server::backend::BackendSurfaceRole;
use crate::server::backend::PollingBackend;

struct State<B> {
    backend: B,
    serializer: Serializer<Request, Event>,
    buffers: HashMap<WlSurfaceId, SurfaceBufferPool>,
}

struct SurfaceBufferPool {
    slots: [Arc<Vec<u8>>; 3],
    next_slot: usize,
    size_bytes: usize,
}

impl SurfaceBufferPool {
    fn new(size_bytes: usize) -> Self {
        let make = || Arc::new(vec![0; size_bytes]);
        Self {
            slots: [make(), make(), make()],
            next_slot: 0,
            size_bytes,
        }
    }

    fn write_slot<F>(&mut self, size_bytes: usize, fill: F) -> Arc<Vec<u8>>
    where
        F: FnOnce(&mut [u8]),
    {
        if size_bytes != self.size_bytes {
            *self = Self::new(size_bytes);
        }

        let idx = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.slots.len();

        let slot = &mut self.slots[idx];
        if Arc::strong_count(slot) > 1 {
            *slot = Arc::new(vec![0; size_bytes]);
        }

        let slot_mut = Arc::get_mut(slot).expect("buffer slot should be uniquely owned");
        fill(slot_mut.as_mut_slice());

        slot.clone()
    }
}

fn send_initial_snapshot<B: PollingBackend>(state: &mut State<B>) -> Result<()> {
    let caps = state.backend.capabilities();
    state
        .serializer
        .writer()
        .send(SendType::Object(Request::Capabilities(caps)));

    state
        .serializer
        .writer()
        .send(SendType::Object(Request::DisplayConfig(
            state.backend.display_config(),
        )));

    let snapshot = state.backend.initial_snapshot().location(loc!())?;
    for obs in snapshot {
        apply_observation(state, obs).location(loc!())?;
    }
    Ok(())
}

fn surface_state_for_descriptor(
    surface: &crate::server::backend::BackendSurfaceDescriptor,
    bitmap: Option<BitmapAssignment>,
) -> SurfaceState {
    let role = match &surface.role {
        BackendSurfaceRole::XdgToplevel { id, title, app_id } => {
            Role::XdgToplevel(xdg_shell::XdgToplevelState {
                id: *id,
                parent: None,
                title: title.clone(),
                app_id: app_id.clone(),
                decoration_mode: None,
                maximized: None,
                fullscreen: None,
            })
        }
    };

    SurfaceState {
        client: surface.client,
        id: surface.id,
        bitmap,
        bitmap_update: None,
        role: Some(role),
        buffer_scale: surface.buffer_scale,
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

fn apply_observation<B: PollingBackend>(state: &mut State<B>, obs: BackendObservation) -> Result<()> {
    match obs {
        BackendObservation::SurfaceCommit { surface, frame } => {
            let bitmap = frame.as_ref().map(|frame| {
                let size_bytes = frame.metadata.len();
                let pool = state
                    .buffers
                    .entry(surface.id)
                    .or_insert_with(|| SurfaceBufferPool::new(size_bytes));
                let slot = pool.write_slot(size_bytes, |slot_mut| {
                    slot_mut.copy_from_slice(&frame.bgra);
                });

                BitmapAssignment::New(Bitmap {
                    metadata: frame.metadata,
                    data: BufferPoolHandle::from(slot),
                })
            });

            let state_to_send = surface_state_for_descriptor(&surface, bitmap);
            for msg in handshake::surface_messages(state_to_send).location(loc!())? {
                state.serializer.writer().send(msg);
            }
        }

        BackendObservation::SurfaceDestroyed { client, surface } => {
            state.buffers.remove(&surface);
            state
                .serializer
                .writer()
                .send(SendType::Object(Request::Surface(
                    crate::protocols::wprs::wayland::SurfaceRequest {
                        client,
                        surface,
                        payload: SurfaceRequestPayload::Destroyed,
                    },
                )));
        }
    }
    Ok(())
}

/// Runs a platform-neutral in-process server loop.
///
/// - Waits for `Event::WprsClientConnect`.
/// - Sends capabilities + initial snapshot.
/// - Periodically polls the backend and sends commits.
/// - Forwards client events to the backend.
/// - Does not emit RawBuffer messages (panics if any are received).
pub fn run<B: PollingBackend>(
    backend: B,
    serializer: Serializer<Request, Event>,
    tick_interval: Duration,
) -> Result<()> {
    let mut event_loop = CalloopEventLoop::<State<B>>::try_new().location(loc!())?;

    let mut state = State {
        backend,
        serializer,
        buffers: HashMap::new(),
    };

    let reader = state
        .serializer
        .reader()
        .ok_or_else(|| anyhow!("serializer reader already taken"))
        .location(loc!())?;

    event_loop
        .handle()
        .insert_source(reader, |event, _metadata, state| {
            if let CalloopChannelEvent::Msg(msg) = event {
                match msg {
                    RecvType::Object(Event::WprsClientConnect) => {
                        state.serializer.set_other_end_connected(true);
                        send_initial_snapshot(state).log_and_ignore(loc!());
                    }
                    RecvType::Object(other) => {
                        state
                            .backend
                            .handle_client_event(other)
                            .log_and_ignore(loc!());
                    }
                    RecvType::RawBuffer(_) => {
                        panic!("inproc server received RawBuffer from client")
                    }
                }
            }
        })
        .map_err(|e| anyhow!("insert_source(serializer reader) failed: {e:?}"))?;

    event_loop
        .handle()
        .insert_source(Timer::from_duration(tick_interval), move |_, _, state| {
            if !state.serializer.other_end_connected() {
                return TimeoutAction::ToDuration(tick_interval);
            }

            match state.backend.poll() {
                Ok(observations) => {
                    for obs in observations {
                        apply_observation(state, obs).log_and_ignore(loc!());
                    }
                }
                Err(err) => {
                    warn!("backend poll failed: {err:?}");
                }
            }

            TimeoutAction::ToDuration(tick_interval)
        })
        .map_err(|e| anyhow!("insert_source(timer) failed: {e:?}"))?;

    event_loop.run(None, &mut state, |_| {}).location(loc!())?;
    Ok(())
}
