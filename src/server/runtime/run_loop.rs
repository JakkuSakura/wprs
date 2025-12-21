use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::ensure;
use calloop::EventLoop as CalloopEventLoop;
use calloop::channel::Event as CalloopChannelEvent;
use calloop::timer::TimeoutAction;
use calloop::timer::Timer;

use crate::buffer_pointer::BufferPointer;
use crate::filtering;
use crate::prelude::*;
use crate::protocols::wprs::Event;
use crate::protocols::wprs::RecvType;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::SendType;
use crate::protocols::wprs::Serializer;
use crate::protocols::wprs::core::handshake;
use crate::protocols::wprs::core::surface_request_from_state;
use crate::protocols::wprs::transport;
use crate::protocols::wprs::transport::TransportCodec;
use crate::protocols::wprs::wayland::BufferAssignment;
use crate::protocols::wprs::wayland::BufferData;
use crate::protocols::wprs::wayland::BufferUpdate;
use crate::protocols::wprs::wayland::CompressedBufferData;
use crate::protocols::wprs::wayland::SurfaceRequestPayload;
use crate::server::runtime::backend::BackendObservation;
use crate::server::runtime::backend::PollingBackend;
use crate::sharding_compression::ShardingCompressor;

struct State<B> {
    backend: B,
    serializer: Serializer<Request, Event>,
    compressor: ShardingCompressor,
    transport: TransportState,
    frames: std::collections::HashMap<crate::protocols::wprs::wayland::WlSurfaceId, FrameTracker>,
}

#[derive(Debug, Clone)]
struct TransportState {
    hello: Option<transport::ClientHello>,
    config: transport::TransportConfig,
    stats: Option<transport::TransportStats>,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            hello: None,
            config: transport::TransportConfig::default(),
            stats: None,
        }
    }
}

#[derive(Debug, Clone)]
struct FrameTracker {
    metadata: crate::protocols::wprs::wayland::BufferMetadata,
    last_bgra: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy)]
struct DirtyScan {
    bbox_x: i32,
    bbox_y: i32,
    bbox_w: i32,
    bbox_h: i32,
    changed_tiles: u32,
    total_tiles: u32,
}

fn scan_dirty_tiles(
    prev: &[u8],
    cur: &[u8],
    metadata: &crate::protocols::wprs::wayland::BufferMetadata,
    tile_px: u32,
) -> Option<DirtyScan> {
    let width_px: u32 = metadata.width.try_into().ok()?;
    let height_px: u32 = metadata.height.try_into().ok()?;
    let stride: usize = metadata.stride.try_into().ok()?;
    if width_px == 0 || height_px == 0 || tile_px == 0 {
        return None;
    }

    let tile_px = tile_px.max(1);
    let tiles_x = (width_px + tile_px - 1) / tile_px;
    let tiles_y = (height_px + tile_px - 1) / tile_px;
    let total_tiles = tiles_x.saturating_mul(tiles_y);

    let mut changed_tiles = 0u32;
    let mut min_tx = tiles_x;
    let mut min_ty = tiles_y;
    let mut max_tx = 0u32;
    let mut max_ty = 0u32;

    for ty in 0..tiles_y {
        let y0 = ty * tile_px;
        let y1 = (y0 + tile_px).min(height_px);
        for tx in 0..tiles_x {
            let x0 = tx * tile_px;
            let x1 = (x0 + tile_px).min(width_px);

            let tile_w_bytes = (x1 - x0) as usize * 4;
            let mut different = false;
            for y in y0..y1 {
                let row_off = y as usize * stride + x0 as usize * 4;
                let a = &prev[row_off..row_off + tile_w_bytes];
                let b = &cur[row_off..row_off + tile_w_bytes];
                if a != b {
                    different = true;
                    break;
                }
            }

            if different {
                changed_tiles += 1;
                min_tx = min_tx.min(tx);
                min_ty = min_ty.min(ty);
                max_tx = max_tx.max(tx);
                max_ty = max_ty.max(ty);
            }
        }
    }

    if changed_tiles == 0 {
        return None;
    }

    let bbox_x = (min_tx * tile_px) as i32;
    let bbox_y = (min_ty * tile_px) as i32;
    let bbox_w = ((max_tx - min_tx + 1) * tile_px).min(width_px) as i32 - bbox_x;
    let bbox_h = ((max_ty - min_ty + 1) * tile_px).min(height_px) as i32 - bbox_y;

    Some(DirtyScan {
        bbox_x,
        bbox_y,
        bbox_w,
        bbox_h,
        changed_tiles,
        total_tiles,
    })
}

fn pick_transport_config(
    hello: &transport::ClientHello,
    stats: Option<&transport::TransportStats>,
) -> transport::TransportConfig {
    let mut want_raw = hello.preferences.prefer_low_cpu || hello.preferences.prefer_low_latency;
    if let Some(rtt) = stats.map(|s| s.rtt_ms) {
        if let Some(max_rtt) = hello.preferences.max_rtt_ms {
            if rtt > max_rtt {
                want_raw = true;
            }
        }
    }
    if let Some(kbps) = hello.preferences.target_bitrate_kbps {
        if kbps < 20_000 {
            want_raw = false;
        }
    }

    let mut codec = TransportCodec::ShardedZstd { level: 1 };
    if want_raw {
        codec = TransportCodec::ShardedRaw;
    } else if !hello
        .supported_codecs
        .iter()
        .any(|c| matches!(c, TransportCodec::ShardedZstd { .. }))
    {
        codec = TransportCodec::ShardedRaw;
    }

    transport::TransportConfig {
        codec,
        buffer_patches: transport::BufferPatchConfig {
            enabled: hello.supports_buffer_patches,
            tile_px: 64,
            full_frame_threshold: 0.6,
        },
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

                s.buffer_update = None;

                // Optional dirty detection and patching.
                if state.transport.config.buffer_patches.enabled {
                    if let Some(prev) = state.frames.get(&s.id) {
                        if prev.metadata == buf.metadata {
                            if let Some(scan) = scan_dirty_tiles(
                                &prev.last_bgra,
                                &bgra,
                                &buf.metadata,
                                state.transport.config.buffer_patches.tile_px,
                            ) {
                                let frac =
                                    (scan.changed_tiles as f32) / (scan.total_tiles.max(1) as f32);
                                if frac < state.transport.config.buffer_patches.full_frame_threshold
                                    && scan.bbox_w > 0
                                    && scan.bbox_h > 0
                                    && (scan.bbox_w < buf.metadata.width
                                        || scan.bbox_h < buf.metadata.height)
                                {
                                    let stride_full: usize =
                                        buf.metadata.stride.try_into().unwrap();
                                    let patch_w = scan.bbox_w as usize;
                                    let patch_h = scan.bbox_h as usize;
                                    let patch_stride = patch_w * 4;
                                    let mut patch_aos = vec![0u8; patch_stride * patch_h];
                                    for row in 0..patch_h {
                                        let src_off = (scan.bbox_y as usize + row) * stride_full
                                            + scan.bbox_x as usize * 4;
                                        let dst_off = row * patch_stride;
                                        patch_aos[dst_off..dst_off + patch_stride].copy_from_slice(
                                            &bgra[src_off..src_off + patch_stride],
                                        );
                                    }

                                    let patch_ptr = patch_aos.as_ptr();
                                    // SAFETY: patch_ptr points to patch_aos.len() bytes for this call.
                                    let patch_buf =
                                        unsafe { BufferPointer::new(&patch_ptr, patch_aos.len()) };
                                    let patch_shards = filtering::filter_and_compress(
                                        patch_buf,
                                        &mut state.compressor,
                                    );

                                    // Update tracker before sending.
                                    state.frames.insert(
                                        s.id,
                                        FrameTracker {
                                            metadata: buf.metadata.clone(),
                                            last_bgra: Arc::clone(&bgra),
                                        },
                                    );

                                    // Send patch rawbuffer + commit.
                                    state
                                        .serializer
                                        .writer()
                                        .send(SendType::RawBuffer(Arc::new(patch_shards)));
                                    s.buffer_update = Some(BufferUpdate::Patch {
                                        x: scan.bbox_x,
                                        y: scan.bbox_y,
                                        width: scan.bbox_w,
                                        height: scan.bbox_h,
                                        stride: (patch_stride as i32),
                                    });
                                    state.serializer.writer().send(SendType::Object(
                                        Request::Surface(surface_request_from_state(s)),
                                    ));
                                    return Ok(());
                                }
                            } else {
                                // No changes; keep the tracker but skip sending.
                                state.frames.insert(
                                    s.id,
                                    FrameTracker {
                                        metadata: buf.metadata.clone(),
                                        last_bgra: Arc::clone(&bgra),
                                    },
                                );
                                return Ok(());
                            }
                        }
                    }
                }

                // Full-frame path.
                let bgra_ptr = bgra.as_ptr();
                // SAFETY: `bgra_ptr` points to `bgra.len()` bytes for the duration of this call.
                let data = unsafe { BufferPointer::new(&bgra_ptr, bgra.len()) };
                let shards = filtering::filter_and_compress(data, &mut state.compressor);
                buf.data = BufferData::Compressed(CompressedBufferData(Arc::new(shards)));

                state.frames.insert(
                    s.id,
                    FrameTracker {
                        metadata: buf.metadata.clone(),
                        last_bgra: Arc::clone(&bgra),
                    },
                );
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

fn apply_transport_event<B: PollingBackend>(
    state: &mut State<B>,
    event: transport::TransportEvent,
) -> Result<()> {
    match event {
        transport::TransportEvent::ClientHello(hello) => {
            state.transport.hello = Some(hello);
            let hello = state.transport.hello.as_ref().unwrap();
            let desired = pick_transport_config(hello, state.transport.stats.as_ref());
            if desired != state.transport.config {
                state.transport.config = desired.clone();
                match desired.codec {
                    TransportCodec::ShardedRaw => state.compressor.set_compression_enabled(false),
                    TransportCodec::ShardedZstd { level } => {
                        state.compressor.set_compression_enabled(true);
                        state.compressor.set_compression_level(level);
                    },
                }
                state
                    .serializer
                    .writer()
                    .send(SendType::Object(Request::Transport(
                        transport::TransportRequest::Config(desired),
                    )));
            }
        },
        transport::TransportEvent::Ping(ping) => {
            state
                .serializer
                .writer()
                .send(SendType::Object(Request::Transport(
                    transport::TransportRequest::Pong(transport::Pong {
                        seq: ping.seq,
                        sent_at_ms: ping.sent_at_ms,
                    }),
                )));
        },
        transport::TransportEvent::Stats(stats) => {
            state.transport.stats = Some(stats);
            if let Some(hello) = &state.transport.hello {
                let desired = pick_transport_config(hello, state.transport.stats.as_ref());
                if desired != state.transport.config {
                    state.transport.config = desired.clone();
                    match desired.codec {
                        TransportCodec::ShardedRaw => {
                            state.compressor.set_compression_enabled(false)
                        },
                        TransportCodec::ShardedZstd { level } => {
                            state.compressor.set_compression_enabled(true);
                            state.compressor.set_compression_level(level);
                        },
                    }
                    state
                        .serializer
                        .writer()
                        .send(SendType::Object(Request::Transport(
                            transport::TransportRequest::Config(desired),
                        )));
                }
            }
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

    let mut state = State {
        backend,
        serializer,
        compressor: ShardingCompressor::new(NonZeroUsize::new(16).unwrap(), 1).location(loc!())?,
        transport: TransportState::default(),
        frames: std::collections::HashMap::new(),
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
                    RecvType::Object(Event::Transport(event)) => {
                        apply_transport_event(state, event).log_and_ignore(loc!());
                    },
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
