use crate::protocols::wprs::transport;
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::WlSurfaceId;

pub fn select_base_codec(hello: &transport::ClientHello) -> transport::TransportCodec {
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

pub fn select_global_transport_config(
    hello: &transport::ClientHello,
    observed_tx_kbps: Option<u32>,
) -> transport::TransportConfig {
    let mut codec = select_base_codec(hello);

    // NOTE: H.264 support may be compiled in by default, but it is intentionally
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

pub fn select_surface_transport_config(
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
