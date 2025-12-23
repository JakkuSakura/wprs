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
    let profile = preference_profile(&hello.preferences);
    let mut codec = select_base_codec(hello);

    // NOTE: H.264 support may be compiled in by default, but it is intentionally
    // not selected as the default negotiated codec unless the client indicates
    // bandwidth pressure.
    #[cfg(feature = "video-h264")]
    {
        let supports_h264 = hello
            .supported_codecs
            .contains(&transport::TransportCodec::H264);
        let prefers_bandwidth = profile
            .bandwidth_weight
            .saturating_sub(profile.clarity_weight)
            >= 30;
        let observed_bandwidth_pressure = observed_tx_kbps
            .map(|tx| tx > hello.preferences.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
            .unwrap_or(false);
        let low_target_bitrate = hello
            .preferences
            .target_bitrate_kbps
            .map_or(false, |kbps| kbps < 8_000);

        if profile.allow_lossy
            && supports_h264
            && (prefers_bandwidth || observed_bandwidth_pressure || low_target_bitrate)
        {
            codec = transport::TransportCodec::H264;
        }
    }

    // Best-effort "clarity-first" policy for image payloads.
    let prefers_clarity = profile
        .clarity_weight
        .saturating_sub(profile.bandwidth_weight)
        >= 30;
    if prefers_clarity && codec != transport::TransportCodec::H264 {
        if hello.supported_codecs.contains(&transport::TransportCodec::Png)
            && profile.bandwidth_weight < 50
        {
            codec = transport::TransportCodec::Png;
        } else if profile.allow_lossy
            && hello.supported_codecs.contains(&transport::TransportCodec::Jpeg)
        {
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
    let profile = preference_profile(&hello.preferences);

    let surface_px = (metadata.width.max(1) as u64) * (metadata.height.max(1) as u64);
    let large_surface = surface_px >= 1280 * 720;

    #[cfg(feature = "video-h264")]
    {
        let supports_h264 = hello
            .supported_codecs
            .contains(&transport::TransportCodec::H264);
        let prefers_bandwidth = profile
            .bandwidth_weight
            .saturating_sub(profile.clarity_weight)
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

        if profile.allow_lossy
            && supports_h264
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
    let prefers_clarity = profile
        .clarity_weight
        .saturating_sub(profile.bandwidth_weight)
        >= 30;
    if prefers_clarity && cfg.codec != transport::TransportCodec::H264 {
        if hello.supported_codecs.contains(&transport::TransportCodec::Png)
            && profile.bandwidth_weight < 50
        {
            cfg.codec = transport::TransportCodec::Png;
        } else if profile.allow_lossy && hello.supported_codecs.contains(&transport::TransportCodec::Jpeg)
        {
            cfg.codec = transport::TransportCodec::Jpeg;
        }
    }

    let _ = surface;
    cfg
}

struct PreferenceProfile {
    latency_weight: u8,
    bandwidth_weight: u8,
    cpu_weight: u8,
    clarity_weight: u8,
    allow_lossy: bool,
    allow_droppy: bool,
    prefer_compression: bool,
}

fn preference_profile(prefs: &transport::TransportPreferences) -> PreferenceProfile {
    match prefs.usage_goal {
        Some(transport::UsageGoal::Gaming) => PreferenceProfile {
            latency_weight: 35,
            bandwidth_weight: 50,
            cpu_weight: 5,
            clarity_weight: 10,
            allow_lossy: true,
            allow_droppy: true,
            prefer_compression: true,
        },
        Some(transport::UsageGoal::Office) => PreferenceProfile {
            latency_weight: 10,
            bandwidth_weight: 10,
            cpu_weight: 10,
            clarity_weight: 70,
            allow_lossy: false,
            allow_droppy: true,
            prefer_compression: true,
        },
        Some(transport::UsageGoal::Media) => PreferenceProfile {
            latency_weight: 5,
            bandwidth_weight: 20,
            cpu_weight: 5,
            clarity_weight: 70,
            allow_lossy: false,
            allow_droppy: false,
            prefer_compression: true,
        },
        None => PreferenceProfile {
            latency_weight: prefs.latency_weight,
            bandwidth_weight: prefs.bandwidth_weight,
            cpu_weight: prefs.cpu_weight,
            clarity_weight: prefs.clarity_weight,
            allow_lossy: true,
            allow_droppy: true,
            prefer_compression: true,
        },
    }
}
