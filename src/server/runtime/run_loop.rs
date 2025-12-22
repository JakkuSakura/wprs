use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::ensure;
use calloop::EventLoop as CalloopEventLoop;
use calloop::channel::Event as CalloopChannelEvent;
use calloop::timer::TimeoutAction;
use calloop::timer::Timer;

use crate::utils::buffer_pointer::BufferPointer;
use crate::utils::filtering;
use crate::prelude::*;
use crate::protocols::wprs::Event;
use crate::protocols::wprs::RecvType;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::SendType;
use crate::protocols::wprs::Serializer;
use crate::protocols::wprs::core::handshake;
use crate::protocols::wprs::transport;
use crate::protocols::wprs::wayland::BufferAssignment;
use crate::protocols::wprs::wayland::BufferData;
use crate::protocols::wprs::wayland::CompressedBufferData;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::server::runtime::backend::BackendObservation;
use crate::server::runtime::backend::PollingBackend;
use crate::utils::sharding_compression::ShardingCompressor;
#[cfg(feature = "video-h264")]
use crate::utils::arc_slice::ArcSlice;

#[cfg(feature = "video-h264")]
use crate::protocols::video::h264::H264Encoder;

#[cfg(feature = "video-h264")]
struct H264EncodeState {
    width: u32,
    height: u32,
    encoder: H264Encoder,
}

struct State<B> {
    backend: B,
    serializer: Serializer<Request, Event>,
    compressor: ShardingCompressor,
    transport_config: transport::TransportConfig,
    tick_fps: u32,
    #[cfg(feature = "video-h264")]
    h264: Option<H264EncodeState>,
}

fn select_transport_config(hello: &transport::ClientHello) -> transport::TransportConfig {
    let mut codec = transport::TransportCodec::ShardedZstd { level: 1 };
    if let Some(found) = hello
        .supported_codecs
        .iter()
        .find(|c| matches!(c, transport::TransportCodec::ShardedZstd { .. }))
    {
        codec = *found;
    } else if hello
        .supported_codecs
        .contains(&transport::TransportCodec::ShardedLz4)
    {
        codec = transport::TransportCodec::ShardedLz4;
    } else if hello
        .supported_codecs
        .contains(&transport::TransportCodec::ShardedRaw)
    {
        codec = transport::TransportCodec::ShardedRaw;
    }

    #[cfg(feature = "video-h264")]
    if hello
        .supported_codecs
        .contains(&transport::TransportCodec::H264)
    {
        codec = transport::TransportCodec::H264;
    }

    transport::TransportConfig {
        codec,
        buffer_patches: transport::BufferPatchConfig {
            enabled: hello.supports_buffer_patches,
            ..Default::default()
        },
        max_fps: None,
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
    for surface in snapshot {
        for msg in handshake::surface_messages(surface.state).location(loc!())? {
            state.serializer.writer().send(msg);
        }
    }
    Ok(())
}

fn apply_observation<B: PollingBackend>(
    state: &mut State<B>,
    obs: BackendObservation,
) -> Result<()> {
    match obs {
        BackendObservation::SurfaceCommit { state: mut s, bgra } => {
            if let Some(bgra) = bgra {
                let Some(BufferAssignment::New(buf)) = s.buffer.as_mut() else {
                    bail!("SurfaceCommit with frame requires BufferAssignment::New")
                };

                let expected_len = buf.metadata.len();
                ensure!(
                    bgra.len() == expected_len,
                    "bgra size mismatch: expected {expected_len} bytes, got {}",
                    bgra.len()
                );

                let bgra_ptr = bgra.as_ptr();
                // SAFETY: `bgra_ptr` points to `bgra.len()` bytes for the duration of this call.
                let data = unsafe { BufferPointer::new(&bgra_ptr, bgra.len()) };
                let shards = match state.transport_config.codec {
                    #[cfg(feature = "video-h264")]
                    transport::TransportCodec::H264 => {
                        let metadata = buf.metadata;
                        let should_reinit = state
                            .h264
                            .as_ref()
                            .map_or(true, |h264| h264.width != metadata.width || h264.height != metadata.height);
                        if should_reinit {
                            state.h264 = Some(H264EncodeState {
                                width: metadata.width,
                                height: metadata.height,
                                encoder: H264Encoder::new(metadata.width, metadata.height, state.tick_fps)
                                    .location(loc!())?,
                            });
                        }
                        let h264 = state.h264.as_mut().unwrap();
                        let encoded = h264
                            .encoder
                            .encode(&bgra, metadata.stride as usize)
                            .location(loc!())?;
                        state
                            .compressor
                            .compress(NonZeroUsize::new(1).unwrap(), ArcSlice::new(encoded))
                    }
                    _ => filtering::filter_and_compress(data, &mut state.compressor),
                };
                buf.data = BufferData::Compressed(CompressedBufferData(Arc::new(shards)));
            }

            for msg in handshake::surface_messages(s).location(loc!())? {
                state.serializer.writer().send(msg);
            }
        },

        BackendObservation::SurfaceDestroyed { client, surface } => {
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
        },
    }
    Ok(())
}

/// Runs a platform-neutral server loop.
///
/// - Waits for `Event::WprsClientConnect`.
/// - Sends capabilities + initial snapshot.
/// - Periodically polls the backend and sends commits.
/// - Forwards client events to the backend.
pub fn run<B: PollingBackend>(
    backend: B,
    serializer: Serializer<Request, Event>,
    tick_interval: Duration,
) -> Result<()> {
    // NOTE: This runner is polling-based and intended for capture-style backends.
    let mut event_loop = CalloopEventLoop::<State<B>>::try_new().location(loc!())?;

    let tick_fps = (1.0 / tick_interval.as_secs_f64()).round().max(1.0) as u32;
    let mut state = State {
        backend,
        serializer,
        compressor: ShardingCompressor::new(NonZeroUsize::new(16).unwrap(), 1).location(loc!())?,
        transport_config: transport::TransportConfig::default(),
        tick_fps,
        #[cfg(feature = "video-h264")]
        h264: None,
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
                    },
                    RecvType::Object(Event::Transport(transport::TransportEvent::ClientHello(
                        hello,
                    ))) => {
                        let config = select_transport_config(&hello);
                        state.transport_config = config.clone();
                        state
                            .serializer
                            .writer()
                            .send(SendType::Object(Request::Transport(
                                transport::TransportRequest::Config(config),
                            )));
                    },
                    RecvType::Object(Event::Transport(transport::TransportEvent::Stats(_))) => {},
                    RecvType::Object(Event::Transport(transport::TransportEvent::Ping(_))) => {},
                    RecvType::Object(other) => {
                        state
                            .backend
                            .handle_client_event(other)
                            .log_and_ignore(loc!());
                    },
                    RecvType::RawBuffer(_) => {
                        warn!("server received RawBuffer from client; ignoring")
                    },
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
                },
                Err(err) => {
                    warn!("backend poll failed: {err:?}");
                },
            }

            TimeoutAction::ToDuration(tick_interval)
        })
        .map_err(|e| anyhow!("insert_source(timer) failed: {e:?}"))?;

    event_loop.run(None, &mut state, |_| {}).location(loc!())?;
    Ok(())
}
