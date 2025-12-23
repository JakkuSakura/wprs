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
    client_max_fps: Option<u32>,
) -> transport::TransportConfig {
    let profile = preference_profile(&hello.preferences);
    let network = network_hints(&hello.preferences, observed_tx_kbps, profile.dynamic_selection);
    let mut codec = select_base_codec(hello);

    if profile.selection_mode == transport::SelectionMode::Manual {
        if let Some(manual) = hello.preferences.manual_codec {
            if hello.supported_codecs.contains(&manual)
                && codec_allowed_by_network(&network, manual)
            {
                codec = manual;
            }
        }
        return transport::TransportConfig {
            codec,
            buffer_patches: transport::BufferPatchConfig {
                enabled: hello.supports_buffer_patches,
                ..Default::default()
            },
            max_fps: client_max_fps,
        };
    }

    // NOTE: H.264 support may be compiled in by default, but it is intentionally
    // not selected as the default negotiated codec unless the client indicates
    // bandwidth pressure.
    #[cfg(feature = "video-h264")]
    {
        let supports_h264 = hello
            .supported_codecs
            .contains(&transport::TransportCodec::H264);
        let decode_is_plausible = hello.gpu.has_hw_video_decode
            || hello
                .gpu
                .hw_decode_codecs
                .iter()
                .any(|c| c.eq_ignore_ascii_case("h264"))
            || hello.cpu.avx2
            || hello.cpu.neon;
        let prefers_bandwidth = profile
            .bandwidth_weight
            .saturating_sub(profile.clarity_weight)
            >= 30;
        let max_bitrate_kbps = hello.preferences.max_bitrate_kbps;
        let observed_bandwidth_pressure = observed_tx_kbps
            .zip(max_bitrate_kbps)
            .map(|(tx, cap)| tx > cap)
            .unwrap_or(false);
        let low_target_bitrate = max_bitrate_kbps.map_or(false, |kbps| kbps < 8_000);
        let allow_without_pressure = prefers_bandwidth && profile.latency_weight < 20;

        if profile.allow_lossy
            && supports_h264
            && decode_is_plausible
            && (observed_bandwidth_pressure || low_target_bitrate || allow_without_pressure)
            && codec_allowed_by_network(&network, transport::TransportCodec::H264)
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
            && codec_allowed_by_network(&network, transport::TransportCodec::Png)
        {
            codec = transport::TransportCodec::Png;
        } else if profile.allow_lossy
            && hello.supported_codecs.contains(&transport::TransportCodec::Jpeg)
            && codec_allowed_by_network(&network, transport::TransportCodec::Jpeg)
        {
            codec = transport::TransportCodec::Jpeg;
        }
    }

    let max_bitrate_kbps = hello.preferences.max_bitrate_kbps;
    let bandwidth_pressure = observed_tx_kbps
        .zip(max_bitrate_kbps)
        .map(|(tx, cap)| tx > cap)
        .unwrap_or(false);
    let low_target_bitrate = max_bitrate_kbps.map_or(false, |kbps| kbps < 8_000);
    let avoid_raw = profile.drop_tolerance == transport::DropTolerance::Avoid
        || profile.retransmit_policy == transport::RetransmitPolicy::Avoid;
    if !avoid_raw
        && profile.latency_weight >= 30
        && !bandwidth_pressure
        && !low_target_bitrate
        && hello.supported_codecs.contains(&transport::TransportCodec::ShardedRaw)
        && codec_allowed_by_network(&network, transport::TransportCodec::ShardedRaw)
    {
        codec = transport::TransportCodec::ShardedRaw;
    }

    transport::TransportConfig {
        codec,
        buffer_patches: transport::BufferPatchConfig {
            enabled: hello.supports_buffer_patches,
            ..Default::default()
        },
        max_fps: client_max_fps,
    }
}

pub struct SurfaceDecisionInput {
    pub surface_tx_kbps: Option<u32>,
    pub total_tx_kbps: Option<u32>,
    pub estimated_fps: Option<f32>,
    pub client_max_fps: Option<u32>,
}

pub fn select_surface_transport_config(
    global: &transport::TransportConfig,
    hello: Option<&transport::ClientHello>,
    stats: SurfaceDecisionInput,
    surface: WlSurfaceId,
    metadata: &BufferMetadata,
) -> transport::TransportConfig {
    let mut cfg = global.clone();
    let Some(hello) = hello else {
        return cfg;
    };
    let profile = preference_profile(&hello.preferences);
    let network = network_hints(
        &hello.preferences,
        stats.total_tx_kbps,
        profile.dynamic_selection,
    );

    if profile.selection_mode == transport::SelectionMode::Manual {
        return cfg;
    }

    let surface_px = (metadata.width.max(1) as u64) * (metadata.height.max(1) as u64);
    let large_surface = surface_px >= 1280 * 720;

    let surface_hints = SurfaceHints {
        surface_px,
        large_surface,
        estimated_fps: stats.estimated_fps,
        surface_tx_kbps: stats.surface_tx_kbps,
        total_tx_kbps: stats.total_tx_kbps,
        client_max_fps: stats.client_max_fps,
    };

    cfg.codec = select_codec_for_surface(hello, &profile, &network, &surface_hints);
    cfg.max_fps = stats.client_max_fps.or(cfg.max_fps);

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
    selection_mode: transport::SelectionMode,
    dynamic_selection: bool,
    drop_tolerance: transport::DropTolerance,
    retransmit_policy: transport::RetransmitPolicy,
}

fn preference_profile(prefs: &transport::TransportPreferences) -> PreferenceProfile {
    let default_weights = transport::TransportPreferences::default();
    let weights_are_default = prefs.latency_weight == default_weights.latency_weight
        && prefs.bandwidth_weight == default_weights.bandwidth_weight
        && prefs.cpu_weight == default_weights.cpu_weight
        && prefs.clarity_weight == default_weights.clarity_weight;

    let (preset_weights, allow_lossy, allow_droppy) = match prefs.usage_goal {
        Some(transport::UsageGoal::Gaming) => ((35, 50, 5, 10), true, true),
        Some(transport::UsageGoal::Office) => ((10, 10, 10, 70), false, true),
        Some(transport::UsageGoal::Media) => ((5, 20, 5, 70), true, false),
        None => ((
            prefs.latency_weight,
            prefs.bandwidth_weight,
            prefs.cpu_weight,
            prefs.clarity_weight,
        ), true, true),
    };

    let (latency_weight, bandwidth_weight, cpu_weight, clarity_weight) = if weights_are_default {
        preset_weights
    } else {
        (
            prefs.latency_weight,
            prefs.bandwidth_weight,
            prefs.cpu_weight,
            prefs.clarity_weight,
        )
    };

    let drop_tolerance = prefs
        .drop_tolerance
        .unwrap_or(if allow_droppy {
            transport::DropTolerance::Allow
        } else {
            transport::DropTolerance::Avoid
        });
    let retransmit_policy = prefs
        .retransmit_policy
        .unwrap_or(transport::RetransmitPolicy::Allow);

    PreferenceProfile {
        latency_weight,
        bandwidth_weight,
        cpu_weight,
        clarity_weight,
        allow_lossy,
        allow_droppy,
        prefer_compression: true,
        selection_mode: prefs.selection_mode,
        dynamic_selection: prefs.selection_mode == transport::SelectionMode::Dynamic,
        drop_tolerance,
        retransmit_policy,
    }
}

#[derive(Debug, Clone, Copy)]
struct NetworkHints {
    max_bitrate_kbps: Option<u32>,
    max_rtt_ms: Option<u32>,
    observed_tx_kbps: Option<u32>,
}

fn network_hints(
    prefs: &transport::TransportPreferences,
    observed_tx_kbps: Option<u32>,
    allow_dynamic: bool,
) -> NetworkHints {
    NetworkHints {
        max_bitrate_kbps: prefs.max_bitrate_kbps,
        max_rtt_ms: prefs.max_rtt_ms,
        observed_tx_kbps: if allow_dynamic { observed_tx_kbps } else { None },
    }
}

#[derive(Debug, Clone, Copy)]
struct SurfaceHints {
    surface_px: u64,
    large_surface: bool,
    estimated_fps: Option<f32>,
    surface_tx_kbps: Option<u32>,
    total_tx_kbps: Option<u32>,
    client_max_fps: Option<u32>,
}

fn codec_allowed_by_network(hints: &NetworkHints, codec: transport::TransportCodec) -> bool {
    let Some(max_kbps) = hints.max_bitrate_kbps else {
        return true;
    };

    match codec {
        transport::TransportCodec::ShardedRaw => max_kbps >= 20_000,
        transport::TransportCodec::Png => max_kbps >= 8_000,
        transport::TransportCodec::Jpeg => max_kbps >= 5_000,
        transport::TransportCodec::H264 => max_kbps >= 3_000,
        _ => true,
    }
}

fn select_codec_for_surface(
    hello: &transport::ClientHello,
    profile: &PreferenceProfile,
    network: &NetworkHints,
    surface: &SurfaceHints,
) -> transport::TransportCodec {
    let mut candidates = hello.supported_codecs.clone();
    candidates.sort_by_key(|c| match c {
        transport::TransportCodec::H264 => 5,
        transport::TransportCodec::Png => 4,
        transport::TransportCodec::Jpeg => 3,
        transport::TransportCodec::ShardedRaw => 2,
        transport::TransportCodec::ShardedLz4 => 1,
        transport::TransportCodec::ShardedZstd { .. } => 0,
    });

    let decode_is_plausible = hello.gpu.has_hw_video_decode
        || hello
            .gpu
            .hw_decode_codecs
            .iter()
            .any(|c| c.eq_ignore_ascii_case("h264"))
        || hello.cpu.avx2
        || hello.cpu.neon;
    let max_bitrate_kbps = network.max_bitrate_kbps;
    let bandwidth_pressure = network
        .observed_tx_kbps
        .zip(max_bitrate_kbps)
        .map(|(tx, cap)| tx > cap)
        .unwrap_or(false);
    let low_target_bitrate = max_bitrate_kbps.map_or(false, |kbps| kbps < 8_000);
    let tight_rtt = network.max_rtt_ms.map_or(false, |rtt| rtt < 20);
    let high_fps = match surface.client_max_fps {
        Some(max_fps) if max_fps > 0 => surface
            .estimated_fps
            .unwrap_or(0.0)
            >= (max_fps as f32 * 0.8),
        _ => surface.estimated_fps.unwrap_or(0.0) >= 30.0,
    };

    let mut best = select_base_codec(hello);
    let mut best_score = i32::MIN;

    let has_non_h264 = candidates
        .iter()
        .any(|codec| *codec != transport::TransportCodec::H264);

    for codec in candidates {
        if codec == transport::TransportCodec::H264
            && !surface.large_surface
            && !high_fps
            && has_non_h264
        {
            continue;
        }
        if !codec_allowed_by_network(network, codec) {
            continue;
        }

        let score = score_codec(
            codec,
            profile,
            surface,
            decode_is_plausible,
            bandwidth_pressure,
            low_target_bitrate,
            tight_rtt,
        );
        if score > best_score {
            best_score = score;
            best = codec;
        }
    }

    best
}

fn score_codec(
    codec: transport::TransportCodec,
    profile: &PreferenceProfile,
    surface: &SurfaceHints,
    decode_is_plausible: bool,
    bandwidth_pressure: bool,
    low_target_bitrate: bool,
    tight_rtt: bool,
) -> i32 {
    let latency = profile.latency_weight as i32;
    let bandwidth = profile.bandwidth_weight as i32;
    let clarity = profile.clarity_weight as i32;
    let cpu = profile.cpu_weight as i32;
    let large_surface = surface.large_surface;
    let drop_avoid = profile.drop_tolerance == transport::DropTolerance::Avoid;
    let retransmit_avoid = profile.retransmit_policy == transport::RetransmitPolicy::Avoid;
    let estimated_fps = surface.estimated_fps.unwrap_or(0.0);
    let high_fps = match surface.client_max_fps {
        Some(max_fps) if max_fps > 0 => estimated_fps >= (max_fps as f32 * 0.8),
        _ => estimated_fps >= 30.0,
    };
    let surface_share = surface
        .surface_tx_kbps
        .zip(surface.total_tx_kbps)
        .and_then(|(surface, total)| {
            if total > 0 {
                Some(surface as f32 / total as f32)
            } else {
                None
            }
        })
        .unwrap_or(0.0);
    let heavy_surface = surface_share >= 0.5;

    match codec {
        transport::TransportCodec::ShardedRaw => {
            let mut score = latency * 3 - bandwidth * 3 + clarity / 2;
            if bandwidth_pressure || low_target_bitrate {
                score -= 50;
            }
            if large_surface {
                score -= 30;
            }
            if high_fps {
                score -= 20;
            }
            if bandwidth_pressure && heavy_surface {
                score -= 20;
            }
            if drop_avoid {
                score -= 30;
            }
            if retransmit_avoid {
                score -= 25;
            }
            score
        }
        transport::TransportCodec::ShardedLz4 => {
            let mut score = latency * 2 - cpu + clarity / 2 - bandwidth;
            if bandwidth_pressure {
                score += 10;
            }
            if large_surface {
                score += 5;
            }
            if high_fps {
                score += 10;
            }
            if bandwidth_pressure && heavy_surface {
                score += 10;
            }
            if drop_avoid {
                score += 5;
            }
            if retransmit_avoid {
                score += 10;
            }
            score
        }
        transport::TransportCodec::ShardedZstd { .. } => {
            let mut score = latency * 1 - cpu * 2 + clarity / 2 - bandwidth;
            if bandwidth_pressure {
                score += 15;
            }
            if large_surface {
                score += 5;
            }
            if high_fps {
                score += 5;
            }
            if bandwidth_pressure && heavy_surface {
                score += 5;
            }
            if drop_avoid {
                score += 10;
            }
            if retransmit_avoid {
                score += 15;
            }
            score
        }
        transport::TransportCodec::Png => {
            let mut score = clarity * 3 - bandwidth * 2 - cpu;
            if bandwidth_pressure || low_target_bitrate {
                score -= 40;
            }
            if large_surface {
                score -= 20;
            }
            if high_fps {
                score -= 25;
            }
            if tight_rtt {
                score -= 10;
            }
            if bandwidth_pressure && heavy_surface {
                score -= 10;
            }
            if drop_avoid {
                score += 5;
            }
            if retransmit_avoid {
                score -= 10;
            }
            score
        }
        transport::TransportCodec::Jpeg => {
            if !profile.allow_lossy {
                return i32::MIN / 2;
            }
            let mut score = clarity * 2 - bandwidth * 1 - cpu;
            if bandwidth_pressure {
                score += 10;
            }
            if high_fps {
                score += 5;
            }
            if tight_rtt {
                score -= 10;
            }
            if bandwidth_pressure && heavy_surface {
                score += 5;
            }
            if drop_avoid {
                score += 10;
            }
            if retransmit_avoid {
                score += 10;
            }
            score
        }
        transport::TransportCodec::H264 => {
            if !profile.allow_lossy || !decode_is_plausible {
                return i32::MIN / 2;
            }
            let mut score = bandwidth * 3 - clarity + latency * 1 - cpu;
            if bandwidth_pressure || low_target_bitrate {
                score += 30;
            }
            if large_surface {
                score += 15;
            } else {
                score -= 40;
            }
            if high_fps {
                score += 20;
            }
            if tight_rtt {
                score -= 20;
            }
            if bandwidth_pressure && heavy_surface {
                score += 10;
            }
            if drop_avoid {
                score += 10;
            }
            if retransmit_avoid {
                score += 15;
            }
            score
        }
    }
}
