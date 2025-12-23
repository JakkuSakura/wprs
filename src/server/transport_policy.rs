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
    let network = network_hints(&hello.preferences, observed_tx_kbps);
    let mut codec = select_base_codec(hello);

    if !profile.dynamic_selection {
        return transport::TransportConfig {
            codec,
            buffer_patches: transport::BufferPatchConfig {
                enabled: hello.supports_buffer_patches,
                ..Default::default()
            },
            max_fps: None,
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
        let observed_bandwidth_pressure = observed_tx_kbps
            .map(|tx| tx > hello.preferences.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
            .unwrap_or(false);
        let low_target_bitrate = hello
            .preferences
            .target_bitrate_kbps
            .map_or(false, |kbps| kbps < 8_000);
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

    let bandwidth_pressure = observed_tx_kbps
        .map(|tx| tx > hello.preferences.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
        .unwrap_or(false);
    let low_target_bitrate = hello
        .preferences
        .target_bitrate_kbps
        .map_or(false, |kbps| kbps < 8_000);
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
    let network = network_hints(&hello.preferences, observed_tx_kbps);

    if !profile.dynamic_selection {
        return cfg;
    }

    let surface_px = (metadata.width.max(1) as u64) * (metadata.height.max(1) as u64);
    let large_surface = surface_px >= 1280 * 720;

    let surface_hints = SurfaceHints {
        surface_px,
        large_surface,
    };

    cfg.codec = select_codec_for_surface(hello, &profile, &network, &surface_hints);

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
        dynamic_selection: prefs.dynamic_selection,
        drop_tolerance,
        retransmit_policy,
    }
}

#[derive(Debug, Clone, Copy)]
struct NetworkHints {
    target_bitrate_kbps: Option<u32>,
    max_rtt_ms: Option<u32>,
    observed_tx_kbps: Option<u32>,
}

fn network_hints(
    prefs: &transport::TransportPreferences,
    observed_tx_kbps: Option<u32>,
) -> NetworkHints {
    NetworkHints {
        target_bitrate_kbps: prefs.target_bitrate_kbps,
        max_rtt_ms: prefs.max_rtt_ms,
        observed_tx_kbps,
    }
}

#[derive(Debug, Clone, Copy)]
struct SurfaceHints {
    surface_px: u64,
    large_surface: bool,
}

fn codec_allowed_by_network(hints: &NetworkHints, codec: transport::TransportCodec) -> bool {
    let Some(target_kbps) = hints.target_bitrate_kbps else {
        return true;
    };

    match codec {
        transport::TransportCodec::ShardedRaw => target_kbps >= 20_000,
        transport::TransportCodec::Png => target_kbps >= 8_000,
        transport::TransportCodec::Jpeg => target_kbps >= 5_000,
        transport::TransportCodec::H264 => target_kbps >= 3_000,
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
    let bandwidth_pressure = network
        .observed_tx_kbps
        .map(|tx| tx > network.target_bitrate_kbps.unwrap_or(12_000) * 12 / 10)
        .unwrap_or(false);
    let low_target_bitrate = network
        .target_bitrate_kbps
        .map_or(false, |kbps| kbps < 8_000);
    let tight_rtt = network.max_rtt_ms.map_or(false, |rtt| rtt < 20);

    let mut best = select_base_codec(hello);
    let mut best_score = i32::MIN;

    let has_non_h264 = candidates
        .iter()
        .any(|codec| *codec != transport::TransportCodec::H264);

    for codec in candidates {
        if codec == transport::TransportCodec::H264 && !surface.large_surface && has_non_h264 {
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

    match codec {
        transport::TransportCodec::ShardedRaw => {
            let mut score = latency * 3 - bandwidth * 3 + clarity / 2;
            if bandwidth_pressure || low_target_bitrate {
                score -= 50;
            }
            if large_surface {
                score -= 30;
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
            if tight_rtt {
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
            if tight_rtt {
                score -= 10;
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
            if tight_rtt {
                score -= 20;
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
