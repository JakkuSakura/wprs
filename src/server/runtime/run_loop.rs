use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::Duration;
use std::time::Instant;

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
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::Role;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::protocols::wprs::wayland::SurfaceState;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::server::runtime::backend::BackendObservation;
use crate::server::runtime::backend::BackendSurfaceRole;
use crate::server::runtime::backend::PollingBackend;
use crate::utils::sharding_compression::ShardingCompressor;
use crate::utils::sharding_compression::CompressedShards;
use crate::protocols::wprs::xdg_shell;

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
    surface_transport_config: HashMap<WlSurfaceId, transport::TransportConfig>,
    client_hello: Option<transport::ClientHello>,
    client_stats: Option<transport::TransportStats>,
    observed_tx_kbps: u32,
    observed_tx_kbps_by_surface: HashMap<WlSurfaceId, u32>,
    sent_bytes_since_update: u64,
    sent_bytes_by_surface_since_update: HashMap<WlSurfaceId, u64>,
    last_stats_update: Instant,
    tick_fps: u32,
    #[cfg(feature = "video-h264")]
    h264: HashMap<WlSurfaceId, H264EncodeState>,
}

fn select_base_codec(hello: &transport::ClientHello) -> transport::TransportCodec {
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

    codec
}

fn select_global_transport_config(
    hello: &transport::ClientHello,
    observed_tx_kbps: Option<u32>,
) -> transport::TransportConfig {
    let mut codec = select_base_codec(hello);

    // NOTE: H.264 support is compiled in by default, but it is intentionally
    // not selected as the default negotiated codec unless the client indicates
    // bandwidth pressure.
    #[cfg(feature = "video-h264")]
    {
        let supports_h264 = hello
            .supported_codecs
            .contains(&transport::TransportCodec::H264);
        let prefers_bandwidth = hello
            .preferences
            .bandwidth_weight
            .saturating_sub(hello.preferences.clarity_weight)
            >= 30;
        let observed_bandwidth_pressure = observed_tx_kbps
            .map(|tx| tx > hello.preferences.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
            .unwrap_or(false);
        let low_target_bitrate = hello
            .preferences
            .target_bitrate_kbps
            .map_or(false, |kbps| kbps < 8_000);

        if supports_h264 && (prefers_bandwidth || observed_bandwidth_pressure || low_target_bitrate) {
            codec = transport::TransportCodec::H264;
        }
    }

    // Best-effort "clarity-first" policy for image payloads.
    //
    // This is intentionally conservative: we only switch away from the sharded
    // filtered BGRA path when the client strongly prefers clarity.
    let prefers_clarity = hello
        .preferences
        .clarity_weight
        .saturating_sub(hello.preferences.bandwidth_weight)
        >= 30;
    if prefers_clarity && codec != transport::TransportCodec::H264 {
        if hello.supported_codecs.contains(&transport::TransportCodec::Png)
            && hello.preferences.bandwidth_weight < 50
        {
            codec = transport::TransportCodec::Png;
        } else if hello.supported_codecs.contains(&transport::TransportCodec::Jpeg) {
            codec = transport::TransportCodec::Jpeg;
        }
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

fn select_surface_transport_config(
    global: &transport::TransportConfig,
    hello: Option<&transport::ClientHello>,
    observed_tx_kbps: Option<u32>,
    surface: WlSurfaceId,
    metadata: &BufferMetadata,
) -> transport::TransportConfig {
    let mut cfg = global.clone();
    let Some(hello) = hello else {
        return cfg;
    };

    let surface_px = (metadata.width.max(1) as u64) * (metadata.height.max(1) as u64);
    let large_surface = surface_px >= 1280 * 720;

    #[cfg(feature = "video-h264")]
    {
        let supports_h264 = hello
            .supported_codecs
            .contains(&transport::TransportCodec::H264);
        let prefers_bandwidth = hello
            .preferences
            .bandwidth_weight
            .saturating_sub(hello.preferences.clarity_weight)
            >= 20;
        let low_target_bitrate = hello
            .preferences
            .target_bitrate_kbps
            .map_or(false, |kbps| kbps < 10_000);
        let observed_bandwidth_pressure = observed_tx_kbps
            .map(|tx| tx > hello.preferences.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
            .unwrap_or(false);
        let decode_is_plausible = hello.gpu.has_hw_video_decode
            || hello
                .gpu
                .hw_decode_codecs
                .iter()
                .any(|c| c.eq_ignore_ascii_case("h264"))
            || hello.cpu.avx2
            || hello.cpu.neon;

        if supports_h264
            && decode_is_plausible
            && large_surface
            && (prefers_bandwidth || low_target_bitrate || observed_bandwidth_pressure)
        {
            cfg.codec = transport::TransportCodec::H264;
            return cfg;
        }

        // If we previously selected H.264 globally but this surface is small, prefer sharded.
        if cfg.codec == transport::TransportCodec::H264 && !large_surface {
            cfg.codec = select_base_codec(hello);
        }
    }

    // For small surfaces, allow clarity-first image codecs.
    let prefers_clarity = hello
        .preferences
        .clarity_weight
        .saturating_sub(hello.preferences.bandwidth_weight)
        >= 30;
    if prefers_clarity && cfg.codec != transport::TransportCodec::H264 {
        if hello.supported_codecs.contains(&transport::TransportCodec::Png)
            && hello.preferences.bandwidth_weight < 50
        {
            cfg.codec = transport::TransportCodec::Png;
        } else if hello.supported_codecs.contains(&transport::TransportCodec::Jpeg) {
            cfg.codec = transport::TransportCodec::Jpeg;
        }
    }

    let _ = surface;
    cfg
}

fn record_sent_bytes<B: PollingBackend>(state: &mut State<B>, surface: WlSurfaceId, bytes: usize) {
    state.sent_bytes_since_update = state.sent_bytes_since_update.saturating_add(bytes as u64);
    let entry = state
        .sent_bytes_by_surface_since_update
        .entry(surface)
        .or_insert(0);
    *entry = entry.saturating_add(bytes as u64);
}

fn maybe_update_observed_bandwidth<B: PollingBackend>(state: &mut State<B>) {
    let elapsed = state.last_stats_update.elapsed();
    if elapsed < Duration::from_secs(1) {
        return;
    }

    let secs = elapsed.as_secs_f64().max(0.001);
    state.observed_tx_kbps = ((state.sent_bytes_since_update as f64) * 8.0 / 1000.0 / secs) as u32;
    state.sent_bytes_since_update = 0;
    state.last_stats_update = Instant::now();

    let mut new_by_surface = HashMap::new();
    for (surface, bytes) in std::mem::take(&mut state.sent_bytes_by_surface_since_update) {
        let kbps = ((bytes as f64) * 8.0 / 1000.0 / secs) as u32;
        new_by_surface.insert(surface, kbps);
    }
    state.observed_tx_kbps_by_surface = new_by_surface;

    if let Some(hello) = state.client_hello.as_ref() {
        let config = select_global_transport_config(hello, Some(state.observed_tx_kbps));
        if state.transport_config != config {
            state.transport_config = config.clone();
            state.surface_transport_config.clear();
            state
                .serializer
                .writer()
                .send(SendType::Object(Request::Transport(
                    transport::TransportRequest::Config(config),
                )));
        }
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
    surface: &crate::server::runtime::backend::BackendSurfaceDescriptor,
    buffer: Option<BufferAssignment>,
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
        buffer,
        buffer_update: None,
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

fn encode_bgra_frame(
    surface_id: WlSurfaceId,
    transport_config: &transport::TransportConfig,
    compressor: &mut ShardingCompressor,
    #[cfg(feature = "video-h264")] h264: &mut HashMap<WlSurfaceId, H264EncodeState>,
    #[allow(unused_variables)]
    tick_fps: u32,
    metadata: BufferMetadata,
    bgra: &[u8],
) -> Result<(crate::protocols::wprs::RawBufferKind, CompressedShards)> {
    let expected_len = metadata.len();
    ensure!(
        bgra.len() == expected_len,
        "bgra size mismatch: expected {expected_len} bytes, got {}",
        bgra.len()
    );

    let bgra_ptr = bgra.as_ptr();
    // SAFETY: `bgra_ptr` points to `bgra.len()` bytes for the duration of this call.
    let data = unsafe { BufferPointer::new(&bgra_ptr, bgra.len()) };

    match transport_config.codec {
        #[cfg(feature = "video-h264")]
        transport::TransportCodec::H264 => {
            let width = u32::try_from(metadata.width)
                .ok()
                .filter(|w| *w > 0)
                .ok_or_else(|| anyhow!("invalid h264 width: {}", metadata.width))
                .location(loc!())?;
            let height = u32::try_from(metadata.height)
                .ok()
                .filter(|h| *h > 0)
                .ok_or_else(|| anyhow!("invalid h264 height: {}", metadata.height))
                .location(loc!())?;

            let should_reinit = match h264.get(&surface_id) {
                Some(existing) => existing.width != width || existing.height != height,
                None => true,
            };
            if should_reinit {
                let encoder = H264Encoder::new(width, height, tick_fps).location(loc!())?;
                h264.insert(
                    surface_id,
                    H264EncodeState {
                        width,
                        height,
                        encoder,
                    },
                );
            }

            let encoder_state = h264.get_mut(&surface_id).unwrap();
            let encoded = encoder_state
                .encoder
                .encode(bgra, metadata.stride as usize)
                .location(loc!())?;
            Ok((
                crate::protocols::wprs::RawBufferKind::H264,
                CompressedShards::single_uncompressed(encoded),
            ))
        }
        transport::TransportCodec::Png => {
            let png_bytes = crate::protocols::wprs::transport::encode_png_from_bgra(
                metadata.width as u32,
                metadata.height as u32,
                metadata.stride as usize,
                bgra,
            )
            .location(loc!())?;
            Ok((
                crate::protocols::wprs::RawBufferKind::Png,
                CompressedShards::single_uncompressed(png_bytes),
            ))
        }
        transport::TransportCodec::Jpeg => {
            let jpeg_bytes = crate::protocols::wprs::transport::encode_jpeg_from_bgra(
                metadata.width as u32,
                metadata.height as u32,
                metadata.stride as usize,
                bgra,
            )
            .location(loc!())?;
            Ok((
                crate::protocols::wprs::RawBufferKind::Jpeg,
                CompressedShards::single_uncompressed(jpeg_bytes),
            ))
        }
        transport::TransportCodec::ShardedRaw => {
            let filtered = filtering::filter_to_vec4u8s(data);
            Ok((
                crate::protocols::wprs::RawBufferKind::FilteredBgra,
                CompressedShards::single_uncompressed(filtered.into()),
            ))
        }
        _ => Ok((
            crate::protocols::wprs::RawBufferKind::FilteredBgra,
            filtering::filter_and_compress(data, compressor),
        )),
    }
}

fn apply_observation<B: PollingBackend>(
    state: &mut State<B>,
    obs: BackendObservation,
) -> Result<()> {
    match obs {
        BackendObservation::SurfaceCommit { surface, frame } => {
            let buffer = frame.as_ref().map(|frame| {
                BufferAssignment::New(crate::protocols::wprs::wayland::Buffer {
                    metadata: frame.metadata,
                    data: BufferData::External,
                })
            });
            let state_to_send = surface_state_for_descriptor(&surface, buffer);

            if let Some(frame) = frame {
                let observed_surface_tx = state.observed_tx_kbps_by_surface.get(&surface.id).copied();
                let desired = select_surface_transport_config(
                    &state.transport_config,
                    state.client_hello.as_ref(),
                    observed_surface_tx,
                    surface.id,
                    &frame.metadata,
                );
                let prior = state.surface_transport_config.get(&surface.id);
                if prior != Some(&desired) {
                    state.surface_transport_config.insert(surface.id, desired.clone());

                    // Best-effort: inform the client of the per-surface policy.
                    state
                        .serializer
                        .writer()
                        .send(SendType::Object(Request::Transport(
                            transport::TransportRequest::ConfigScoped {
                                scope: transport::TransportScope::Surface(surface.id),
                                config: desired.clone(),
                            },
                        )));

                    #[cfg(feature = "video-h264")]
                    if desired.codec != transport::TransportCodec::H264 {
                        state.h264.remove(&surface.id);
                    }
                }

                let (kind, shards) = encode_bgra_frame(
                    surface.id,
                    &desired,
                    &mut state.compressor,
                    #[cfg(feature = "video-h264")]
                    &mut state.h264,
                    state.tick_fps,
                    frame.metadata,
                    &frame.bgra,
                )
                .location(loc!())?;
                record_sent_bytes(state, surface.id, shards.size());
                maybe_update_observed_bandwidth(state);
                state
                    .serializer
                    .writer()
                    .send(SendType::RawBuffer(crate::protocols::wprs::RawBufferPayload {
                        surface: surface.id,
                        kind,
                        shards,
                    }));
            }

            for msg in handshake::surface_messages(state_to_send).location(loc!())? {
                state.serializer.writer().send(msg);
            }
        }

        BackendObservation::SurfaceDestroyed { client, surface } => {
            #[cfg(feature = "video-h264")]
            {
                state.h264.remove(&surface);
            }
            state.surface_transport_config.remove(&surface);
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
        surface_transport_config: HashMap::new(),
        client_hello: None,
        client_stats: None,
        observed_tx_kbps: 0,
        observed_tx_kbps_by_surface: HashMap::new(),
        sent_bytes_since_update: 0,
        sent_bytes_by_surface_since_update: HashMap::new(),
        last_stats_update: Instant::now(),
        tick_fps,
        #[cfg(feature = "video-h264")]
        h264: HashMap::new(),
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
                        state.client_hello = Some(hello.clone());
                        let config = select_global_transport_config(&hello, Some(state.observed_tx_kbps));
                        if state.transport_config != config {
                            state.transport_config = config.clone();
                            state.surface_transport_config.clear();
                        }
                        state
                            .serializer
                            .writer()
                            .send(SendType::Object(Request::Transport(
                                transport::TransportRequest::Config(config),
                            )));
                    },
                    RecvType::Object(Event::Transport(transport::TransportEvent::Stats(stats))) => {
                        // Client-reported stats are best-effort. The server still uses its own
                        // observed tx-kbps for transport policy.
                        state.client_stats = Some(stats);
                        if let Some(hello) = state.client_hello.as_ref() {
                            let config = select_global_transport_config(hello, Some(state.observed_tx_kbps));
                            if state.transport_config != config {
                                state.transport_config = config.clone();
                                state.surface_transport_config.clear();
                                state
                                    .serializer
                                    .writer()
                                    .send(SendType::Object(Request::Transport(
                                        transport::TransportRequest::Config(config),
                                    )));
                            }
                        }
                    }
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
