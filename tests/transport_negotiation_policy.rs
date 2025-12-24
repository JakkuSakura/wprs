use wprs::protocols::wprs::transport;
use wprs::protocols::wprs::wayland::BufferFormat;
use wprs::protocols::wprs::wayland::BufferMetadata;
use wprs::protocols::wprs::wayland::WlSurfaceId;
use wprs::protocols::wprs::codecs;

fn hello_with_codecs(codecs: Vec<transport::TransportCodec>) -> transport::ClientHello {
    transport::ClientHello {
        supported_codecs: codecs,
        supports_buffer_patches: false,
        cpu: transport::CpuFeatures::default(),
        gpu: transport::GpuFeatures::default(),
        preferences: transport::TransportPreferences::default(),
    }
}

fn hello_bandwidth_h264() -> transport::ClientHello {
    let mut hello = hello_with_codecs(vec![
        transport::TransportCodec::ShardedZstd { level: 1 },
        transport::TransportCodec::ShardedLz4,
        transport::TransportCodec::ShardedRaw,
        transport::TransportCodec::H264,
    ]);
    hello.preferences.bandwidth_weight = 80;
    hello.preferences.clarity_weight = 10;
    hello.preferences.max_bitrate_kbps = Some(5_000);
    hello.gpu.has_hw_video_decode = true;
    hello
}

fn hello_clarity_png() -> transport::ClientHello {
    let mut hello = hello_with_codecs(vec![
        transport::TransportCodec::ShardedZstd { level: 1 },
        transport::TransportCodec::Png,
    ]);
    hello.preferences.bandwidth_weight = 10;
    hello.preferences.clarity_weight = 80;
    hello
}

fn hello_with_goal(goal: transport::UsageGoal, codecs: Vec<transport::TransportCodec>) -> transport::ClientHello {
    let mut hello = hello_with_codecs(codecs);
    hello.preferences.usage_goal = Some(goal);
    hello
}

fn surface_stats(surface_tx_kbps: Option<u32>) -> codecs::SurfaceDecisionInput {
    codecs::SurfaceDecisionInput {
        surface_tx_kbps,
        total_tx_kbps: None,
        estimated_fps: None,
        client_max_fps: None,
    }
}

#[test]
fn global_default_prefers_sharded_zstd() {
    let hello = hello_with_codecs(vec![
        transport::TransportCodec::ShardedZstd { level: 1 },
        transport::TransportCodec::ShardedLz4,
        transport::TransportCodec::ShardedRaw,
    ]);
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::ShardedZstd { level: 1 });
}

#[test]
fn global_can_pick_h264_under_bandwidth_pressure() {
    let hello = hello_bandwidth_h264();
    let cfg = codecs::select_global_transport_config(&hello, Some(20_000), None);
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn global_avoids_h264_without_decode_support() {
    let mut hello = hello_bandwidth_h264();
    hello.gpu.has_hw_video_decode = false;
    hello.cpu.avx2 = false;
    hello.cpu.neon = false;
    let cfg = codecs::select_global_transport_config(&hello, Some(20_000), None);
    assert_ne!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn max_bitrate_cap_prefers_allowed_codec() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Office,
        vec![
            transport::TransportCodec::Png,
            transport::TransportCodec::H264,
        ],
    );
    hello.preferences.max_bitrate_kbps = Some(3_000);
    hello.gpu.has_hw_video_decode = true;
    let cfg = codecs::select_global_transport_config(&hello, Some(3_500), None);
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn surface_large_prefers_h264_if_supported_and_bandwidth_or_target_low() {
    let hello = hello_bandwidth_h264();
    let global = codecs::select_global_transport_config(&hello, None, None);
    let meta = BufferMetadata {
        width: 1920,
        height: 1080,
        stride: 1920 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        surface_stats(None),
        WlSurfaceId(1),
        &meta,
    );
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn surface_small_avoids_h264_even_if_global_is_h264() {
    let hello = hello_bandwidth_h264();
    let mut global = codecs::select_global_transport_config(&hello, Some(20_000), None);
    global.codec = transport::TransportCodec::H264;

    let meta = BufferMetadata {
        width: 320,
        height: 240,
        stride: 320 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        surface_stats(None),
        WlSurfaceId(1),
        &meta,
    );
    assert_ne!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn surface_small_can_use_png_for_clarity() {
    let hello = hello_clarity_png();
    let global = codecs::select_global_transport_config(&hello, None, None);
    let meta = BufferMetadata {
        width: 640,
        height: 480,
        stride: 640 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        surface_stats(None),
        WlSurfaceId(1),
        &meta,
    );
    assert_eq!(cfg.codec, transport::TransportCodec::Png);
}

#[test]
fn usage_goal_gaming_prefers_raw_globally() {
    let hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedRaw,
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
        ],
    );
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::ShardedRaw);
}

#[test]
fn usage_goal_office_prefers_png_over_lossy_codecs() {
    let hello = hello_with_goal(
        transport::UsageGoal::Office,
        vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
            transport::TransportCodec::Png,
        ],
    );
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::Png);
}

#[test]
fn usage_goal_media_prefers_png_over_jpeg() {
    let hello = hello_with_goal(
        transport::UsageGoal::Media,
        vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::Jpeg,
            transport::TransportCodec::Png,
        ],
    );
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::Png);
}

#[test]
fn gaming_with_bandwidth_pressure_prefers_h264() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedRaw,
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
        ],
    );
    hello.gpu.has_hw_video_decode = true;
    let cfg = codecs::select_global_transport_config(&hello, Some(20_000), None);
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn media_large_surface_prefers_h264_if_supported() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Media,
        vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
            transport::TransportCodec::Png,
        ],
    );
    hello.gpu.has_hw_video_decode = true;
    hello.preferences.max_bitrate_kbps = Some(6_000);
    let global = codecs::select_global_transport_config(&hello, None, None);
    let meta = BufferMetadata {
        width: 1920,
        height: 1080,
        stride: 1920 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        codecs::SurfaceDecisionInput {
            surface_tx_kbps: None,
            total_tx_kbps: Some(30_000),
            estimated_fps: None,
            client_max_fps: None,
        },
        WlSurfaceId(1),
        &meta,
    );
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn high_fps_surface_prefers_h264() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
        ],
    );
    hello.gpu.has_hw_video_decode = true;
    hello.preferences.max_bitrate_kbps = Some(8_000);
    let global = codecs::select_global_transport_config(&hello, None, None);
    let meta = BufferMetadata {
        width: 640,
        height: 360,
        stride: 640 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        codecs::SurfaceDecisionInput {
            surface_tx_kbps: None,
            total_tx_kbps: None,
            estimated_fps: Some(120.0),
            client_max_fps: Some(144),
        },
        WlSurfaceId(1),
        &meta,
    );
    assert_eq!(cfg.codec, transport::TransportCodec::H264);
}

#[test]
fn dynamic_selection_can_be_disabled() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::H264,
        ],
    );
    hello.preferences.selection_mode = transport::SelectionMode::Manual;
    let global = codecs::select_global_transport_config(&hello, None, None);
    let meta = BufferMetadata {
        width: 1920,
        height: 1080,
        stride: 1920 * 4,
        format: BufferFormat::Argb8888,
    };
    let cfg = codecs::select_surface_transport_config(
        &global,
        Some(&hello),
        codecs::SurfaceDecisionInput {
            surface_tx_kbps: None,
            total_tx_kbps: Some(30_000),
            estimated_fps: None,
            client_max_fps: None,
        },
        WlSurfaceId(1),
        &meta,
    );
    assert_eq!(cfg.codec, global.codec);
}

#[test]
fn dynamic_selection_disables_global_heuristics() {
    let mut hello = hello_bandwidth_h264();
    hello.preferences.selection_mode = transport::SelectionMode::Manual;
    hello.preferences.manual_codec = Some(transport::TransportCodec::ShardedLz4);
    let cfg = codecs::select_global_transport_config(&hello, Some(20_000), None);
    assert_eq!(cfg.codec, transport::TransportCodec::ShardedLz4);
}

#[test]
fn qos_drop_avoid_prefers_compressed_codecs() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedRaw,
            transport::TransportCodec::ShardedZstd { level: 1 },
        ],
    );
    hello.preferences.drop_tolerance = Some(transport::DropTolerance::Avoid);
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::ShardedZstd { level: 1 });
}

#[test]
fn qos_retransmit_avoid_penalizes_raw() {
    let mut hello = hello_with_goal(
        transport::UsageGoal::Gaming,
        vec![
            transport::TransportCodec::ShardedRaw,
            transport::TransportCodec::ShardedLz4,
        ],
    );
    hello.preferences.retransmit_policy = Some(transport::RetransmitPolicy::Avoid);
    let cfg = codecs::select_global_transport_config(&hello, None, None);
    assert_eq!(cfg.codec, transport::TransportCodec::ShardedLz4);
}
