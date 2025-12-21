use crate::client::config::WprscConfig;
use crate::prelude::*;
use anyhow::ensure;

pub fn run(config: WprscConfig) -> Result<()> {
    ensure!(
        !config.forward_only,
        "--forward-only is only meaningful for role=viewer"
    );

    #[cfg(feature = "wayland")]
    return wayland_server_impl::run(config).location(loc!());

    #[cfg(not(feature = "wayland"))]
    {
        let _ = config;
        bail!("role=wayland-server requires building wprsc with `--features wayland`")
    }
}

#[cfg(feature = "wayland")]
mod wayland_server_impl {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;

    use crate::client::ClientBackendConfig;
    use crate::client::build_client_backend;
    use crate::client::config::WprscConfig;
    use crate::prelude::*;
    use crate::protocols::wprs as proto;
    use crate::protocols::wprs::Serializer;
    use crate::protocols::wprs::transport;
    use crate::server::backends::wayland::backend::WaylandSmithayBackend;
    use crate::server::backends::wayland::backend::WaylandSmithayBackendConfig;
    use crate::server::config::XwaylandMode;
    use crate::server::runtime::backend::ServerBackend as _;

    pub fn run(config: WprscConfig) -> Result<()> {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir());

        let wayland_display = format!("wprsc-{}", std::process::id());
        let wayland_socket_path = runtime_dir.join(&wayland_display);
        info!(
            "wprsc wayland-server listening: XDG_RUNTIME_DIR={:?} WAYLAND_DISPLAY={:?} socket={:?}",
            runtime_dir, wayland_display, wayland_socket_path
        );

        // Create an internal wprs transport between the embedded server backend
        // and the presentation backend.
        let internal_dir = env::temp_dir().join(format!(
            "wprsc-internal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&internal_dir).location(loc!())?;
        let internal_socket = internal_dir.join("wprs.sock");

        let server_socket = internal_socket.clone();
        let server_wayland_display = wayland_display.clone();
        thread::spawn(move || {
            let serializer: Serializer<proto::Request, proto::Event> =
                warn_and_return!(Serializer::new_server(&server_socket));

            let backend = WaylandSmithayBackend::new(WaylandSmithayBackendConfig {
                wayland_display: server_wayland_display,
                framerate: 60,
                enable_xwayland: false,
                xwayland_mode: XwaylandMode::External,
                xwayland_display: None,
                xwayland_xdg_shell_path: "xwayland-xdg-shell".to_string(),
                xwayland_xdg_shell_wayland_debug: false,
                xwayland_xdg_shell_args: Vec::new(),
                kde_server_side_decorations: true,
            });

            if let Err(err) = Box::new(backend).run(serializer, None) {
                warn!("wprsc wayland-server backend terminated: {err:?}");
            }
        });

        // Connect the local presentation backend as a wprs client.
        let serializer_options = proto::SerializerClientOptions {
            auto_reconnect: true,
            on_connect: vec![proto::SendType::Object(proto::Event::WprsClientConnect)],
        };
        let serializer: Serializer<proto::Event, proto::Request> =
            Serializer::new_client_with_options(&internal_socket, serializer_options)
                .with_context(loc!(), || {
                    format!("failed to connect to internal wprs socket {internal_socket:?}")
                })?;

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

        info!(
            "wprsc presenting wayland-server via backend: {}",
            backend.name()
        );

        // Allow the presentation backend to tell the embedded server how to tune compression.
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
            let hello = transport::ClientHello {
                supported_codecs: vec![
                    transport::TransportCodec::ShardedZstd { level: 1 },
                    transport::TransportCodec::ShardedRaw,
                ],
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
}
