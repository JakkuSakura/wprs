use crate::client::config::WprscConfig;
use crate::prelude::*;
use anyhow::ensure;

pub fn run(config: WprscConfig) -> Result<()> {
    ensure!(
        !config.forward_only,
        "--forward-only is only meaningful for role=viewer"
    );

    #[cfg(all(target_os = "linux", feature = "wayland"))]
    {
        return linux_wayland_server::run(config).location(loc!());
    }

    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    {
        let _ = config;
        bail!("role=wayland-server is only supported on Linux builds with the `wayland` feature enabled")
    }
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
mod linux_wayland_server {
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
    use crate::server::backends::wayland::backend::WaylandSmithayBackend;
    use crate::server::backends::wayland::backend::WaylandSmithayBackendConfig;
    use crate::server::config::XwaylandMode;
    use crate::server::runtime::backend::ServerBackend as _;

    pub fn run(config: WprscConfig) -> Result<()> {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set; required for role=wayland-server"))
            .location(loc!())?;

        let wayland_display = format!("wprsc-{}", std::process::id());
        let wayland_socket_path = runtime_dir.join(&wayland_display);
        info!(
            "wprsc wayland-server listening: XDG_RUNTIME_DIR={:?} WAYLAND_DISPLAY={:?} socket={:?}",
            runtime_dir,
            wayland_display,
            wayland_socket_path
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
            },
        )
        .location(loc!())?;

        info!("wprsc presenting wayland-server via backend: {}", backend.name());
        backend.run(serializer).location(loc!())
    }
}
