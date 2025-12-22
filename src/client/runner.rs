use std::fs;
use std::time::Duration;

use crate::client::ClientBackendConfig;
use crate::client::build_client_backend;
use crate::client::config::ClientBackend;
use crate::client::config::WprscConfig;
use crate::client::config::WprscRole;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::Serializer;
use crate::protocols::wprs::transport;

pub fn run_wprsc(config: WprscConfig) -> Result<()> {
    match config.role {
        WprscRole::Viewer => run_viewer(config).location(loc!()),
        WprscRole::WaylandServer => crate::client::wayland_server::run(config).location(loc!()),
    }
}

pub fn run_client_for_endpoint(
    endpoint: proto::Endpoint,
    client_backend: ClientBackend,
    backend_config: ClientBackendConfig,
) -> Result<()> {
    let serializer_options = proto::SerializerClientOptions {
        auto_reconnect: false,
        on_connect: vec![proto::SendType::Object(proto::Event::WprsClientConnect)],
    };

    let serializer: Serializer<proto::Event, proto::Request> =
        Serializer::new_client_endpoint_with_options(endpoint, serializer_options)
            .location(loc!())?;

    let backend = build_client_backend(client_backend, backend_config).location(loc!())?;

    info!("viewer using backend: {}", backend.name());

    // Send a best-effort transport hello so the server can tune compression.
    {
        let supports_buffer_patches = backend.name() == "winit-wgpu";
        let cpu = transport::CpuFeatures {
            #[cfg(all(target_arch = "x86_64"))]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
            #[cfg(not(target_arch = "x86_64"))]
            avx2: false,
            #[cfg(all(target_arch = "aarch64"))]
            neon: std::arch::is_aarch64_feature_detected!("neon"),
            #[cfg(not(target_arch = "aarch64"))]
            neon: false,
        };
        let supported_codecs = {
            let mut codecs = vec![
                transport::TransportCodec::ShardedZstd { level: 1 },
                transport::TransportCodec::ShardedLz4,
                transport::TransportCodec::ShardedRaw,
                transport::TransportCodec::Png,
            ];
            #[cfg(feature = "image-jpeg")]
            codecs.push(transport::TransportCodec::Jpeg);
            #[cfg(feature = "video-h264")]
            codecs.push(transport::TransportCodec::H264);
            codecs
        };
        let hello = transport::ClientHello {
            supported_codecs,
            supports_buffer_patches,
            cpu,
            gpu: transport::GpuFeatures::default(),
            preferences: transport::TransportPreferences::default(),
        };
        serializer
            .writer()
            .send(proto::SendType::Object(proto::Event::Transport(
                transport::TransportEvent::ClientHello(hello),
            )));
    }

    backend.run(serializer).location(loc!())
}

fn run_viewer(config: WprscConfig) -> Result<()> {
    if config.forward_only {
        let endpoint = config
            .endpoint
            .clone()
            .ok_or_else(|| anyhow!("--forward-only requires --endpoint=ssh://..."))
            .location(loc!())?;

        let (local_endpoint, guard) = proto::setup_client_transport(endpoint).location(loc!())?;
        let _guard = guard
            .ok_or_else(|| anyhow!("--forward-only requires an ssh:// endpoint"))
            .location(loc!())?;

        println!("{local_endpoint}");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    let serializer_options = proto::SerializerClientOptions {
        auto_reconnect: config.auto_reconnect,
        on_connect: vec![proto::SendType::Object(proto::Event::WprsClientConnect)],
    };

    let serializer: Serializer<proto::Event, proto::Request> =
        match &config.endpoint {
            Some(endpoint) => {
                Serializer::new_client_endpoint_with_options(endpoint.clone(), serializer_options)
                    .with_context(loc!(), || {
                    format!("Serializer failed to initialize for endpoint {endpoint:?}.")
                })?
            },
            None => {
                fs::create_dir_all(config.socket.parent().location(loc!())?).location(loc!())?;
                Serializer::new_client_with_options(&config.socket, serializer_options)
                    .with_context(loc!(), || {
                        format!(
                            "Serializer failed to initialize for socket {:?}.",
                            &config.socket
                        )
                    })?
            },
        };

    let backend = build_client_backend(
        config.present_backend,
        ClientBackendConfig {
            title_prefix: config.title_prefix,
            control_socket: config.control_socket,
            keyboard_mode: config.keyboard_mode,
            xkb_keymap_file: config.xkb_keymap_file,
            ui_scale_factor: config.ui_scale_factor,
            min_output_scale_factor: config.min_output_scale_factor,
        },
    )
    .location(loc!())?;

    info!("wprsc using backend: {}", backend.name());

    // Send a best-effort transport hello so the server can tune compression.
    {
        let supports_buffer_patches = backend.name() == "winit-wgpu";
        let cpu = transport::CpuFeatures {
            #[cfg(all(target_arch = "x86_64"))]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
            #[cfg(not(target_arch = "x86_64"))]
            avx2: false,
            #[cfg(all(target_arch = "aarch64"))]
            neon: std::arch::is_aarch64_feature_detected!("neon"),
            #[cfg(not(target_arch = "aarch64"))]
            neon: false,
        };
        let mut supported_codecs = vec![
            transport::TransportCodec::ShardedZstd { level: 1 },
            transport::TransportCodec::ShardedLz4,
            transport::TransportCodec::ShardedRaw,
            transport::TransportCodec::Png,
        ];
        #[cfg(feature = "image-jpeg")]
        supported_codecs.push(transport::TransportCodec::Jpeg);
        #[cfg(feature = "video-h264")]
        supported_codecs.push(transport::TransportCodec::H264);
        let hello = transport::ClientHello {
            supported_codecs,
            supports_buffer_patches,
            cpu,
            gpu: transport::GpuFeatures::default(),
            preferences: transport::TransportPreferences::default(),
        };
        serializer
            .writer()
            .send(proto::SendType::Object(proto::Event::Transport(
                transport::TransportEvent::ClientHello(hello),
            )));
    }

    backend.run(serializer).location(loc!())
}
