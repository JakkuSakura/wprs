use std::fs;
use std::time::Duration;

use calloop::EventLoop as CalloopEventLoop;
use calloop::channel::Event as CalloopChannelEvent;

use crate::client::ClientBackendConfig;
use crate::client::backend::ClientContext;
use crate::client::build_client_backend;
use crate::client::resolve_client_backend;
use crate::client::state::ClientState;
use crate::client::config::ClientBackend;
use crate::client::config::WprscConfig;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::endpoint::Endpoint;
use crate::protocols::wprs::endpoint::setup_client_transport;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::capabilities;
use crate::protocols::wprs::transport;

pub fn run_wprsc(config: WprscConfig) -> Result<()> {
    if config.forward_only {
        let endpoint = config
            .endpoint
            .clone()
            .ok_or_else(|| {
                Error::InvalidArgument("--forward-only requires --endpoint=ssh://...".to_string())
            })
            .location(loc!())?;

        let (local_endpoint, guard) = setup_client_transport(endpoint).location(loc!())?;
        let _guard = guard
            .ok_or_else(|| {
                Error::InvalidArgument("--forward-only requires an ssh:// endpoint".to_string())
            })
            .location(loc!())?;

        println!("{local_endpoint}");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    let serializer_options = proto::serializer::SerializerClientOptions {
        auto_reconnect: config.auto_reconnect,
        on_connect: vec![proto::serializer::SendType::Object(proto::types::Event::WprsClientConnect)],
    };

    let serializer: Serializer<proto::types::Event, proto::types::Request> =
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

    run_client_for_serializer(
        serializer,
        resolve_client_backend(config.present_backend).location(loc!())?,
        ClientBackendConfig {
            title_prefix: config.title_prefix,
            control_socket: config.control_socket,
            keyboard_mode: config.keyboard_mode,
            xkb_keymap_file: config.xkb_keymap_file,
            ui_scale_factor: config.ui_scale_factor,
            min_output_scale_factor: config.min_output_scale_factor,
            html_bind_addr: config.html_bind_addr,
        },
    )
    .location(loc!())
}

pub fn run_client_for_endpoint(
    endpoint: Endpoint,
    client_backend: ClientBackend,
    backend_config: ClientBackendConfig,
) -> Result<()> {
    let client_backend = resolve_client_backend(client_backend).location(loc!())?;

    let serializer_options = proto::serializer::SerializerClientOptions {
        auto_reconnect: false,
        on_connect: vec![proto::serializer::SendType::Object(proto::types::Event::WprsClientConnect)],
    };

    let serializer: Serializer<proto::types::Event, proto::types::Request> =
        Serializer::new_client_endpoint_with_options(endpoint, serializer_options)
            .location(loc!())?;

    run_client_for_serializer(serializer, client_backend, backend_config).location(loc!())
}

pub fn run_client_for_serializer(
    mut serializer: Serializer<proto::types::Event, proto::types::Request>,
    client_backend: ClientBackend,
    backend_config: ClientBackendConfig,
) -> Result<()> {
    let backend = build_client_backend(client_backend, backend_config).location(loc!())?;

    info!("viewer using backend: {}", backend.name());

    let state = std::sync::Arc::new(ClientState::new());
    let (notify_tx, notify_rx) = std::sync::mpsc::channel::<()>();

    let reader = serializer.reader().location(loc!())?;
    let state_for_reader = std::sync::Arc::clone(&state);
    std::thread::spawn(move || {
        let mut client_sync = crate::protocols::wprs::client_sync::ClientSync::new();
        let mut loop_ = CalloopEventLoop::try_new().expect("calloop init");
        loop_
            .handle()
            .insert_source(reader, move |event, _metadata, _state| {
                if let CalloopChannelEvent::Msg(msg) = event {
                    let msg = match client_sync.handle_message(msg) {
                        Ok(Some(msg)) => msg,
                        Ok(None) => return,
                        Err(err) => {
                            warn!("client sync failed: {err:?}");
                            return;
                        }
                    };
                    let crate::protocols::wprs::serializer::RecvType::Object(req) = msg else {
                        return;
                    };
                    if state_for_reader.apply_request(req) {
                        let _ = notify_tx.send(());
                    }
                }
            })
            .expect("insert serializer reader");
        let _ = loop_.run(None, &mut (), |_| {});
    });

    // Send a best-effort transport hello so the server can tune compression.
    {
        let supports_buffer_patches = backend.name() == "winit-wgpu";
        let cpu = capabilities::CpuFeatures {
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
            codecs.push(transport::TransportCodec::Jpeg);
            #[cfg(feature = "video-h264")]
            if should_advertise_h264() {
                codecs.push(transport::TransportCodec::H264);
            }
            codecs
        };
        let hello = transport::ClientHello {
            supported_codecs,
            supports_buffer_patches,
            cpu,
            gpu: capabilities::GpuFeatures::default(),
            preferences: transport::TransportPreferences::default(),
        };
        serializer
            .writer()
            .send(proto::serializer::SendType::Object(proto::types::Event::Transport(
                transport::TransportEvent::ClientHello(hello),
            )));
    }

    backend
        .run(ClientContext {
            serializer,
            state,
            notify_rx,
        })
        .location(loc!())
}

#[cfg(feature = "video-h264")]
fn should_advertise_h264() -> bool {
    #[cfg(target_os = "macos")]
    let default_allow = false;
    #[cfg(not(target_os = "macos"))]
    let default_allow = true;

    let allow = std::env::var("WPRS_ENABLE_H264")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default_allow);

    if !allow {
        return false;
    }

    match crate::protocols::video::h264::H264Decoder::new() {
        Ok(_) => true,
        Err(err) => {
            warn!("H264 disabled: decoder init failed: {err:?}");
            false
        }
    }
}
