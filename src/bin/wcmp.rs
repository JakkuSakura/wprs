use std::path::PathBuf;

use clap::Parser;

use wprs::client::config::KeyboardMode;
#[cfg(feature = "wayland")]
use wprs::config;
#[cfg(feature = "wayland")]
use wprs::config::SerializableLevel;
use wprs::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "wcmp")]
struct Args {
    /// Optional path to a `wprsd.ron` config file.
    #[arg(long, value_name = "PATH")]
    server_config: Option<PathBuf>,

    /// Optional path to a `wprsc.ron` config file for client tuning.
    #[arg(long, value_name = "PATH")]
    client_config: Option<PathBuf>,

    /// Override the Wayland display name.
    #[arg(long, value_name = "NAME")]
    wayland_display: Option<String>,

    /// Override the server framerate.
    #[arg(long, value_name = "FPS")]
    framerate: Option<u32>,

    /// Enable KDE server-side decorations.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    kde_server_side_decorations: bool,

    /// Disable XWayland integration.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    no_xwayland: bool,

    /// Preferred XWayland display number.
    #[arg(long, value_name = "NUM")]
    xwayland_display: Option<u32>,

    /// Enable Wayland protocol debug for XWayland.
    #[arg(long, value_name = "BOOL")]
    xwayland_wayland_debug: Option<bool>,

    /// Client UI scale factor.
    #[arg(long, value_name = "SCALE")]
    ui_scale_factor: Option<f64>,

    /// Minimum output scale factor for the client.
    #[arg(long, value_name = "SCALE")]
    min_output_scale_factor: Option<i32>,

    /// Keyboard mode for the client.
    #[arg(long, value_name = "MODE")]
    keyboard_mode: Option<KeyboardMode>,

    /// Optional XKB keymap file for the client.
    #[arg(long, value_name = "PATH")]
    xkb_keymap_file: Option<PathBuf>,
}

#[cfg(feature = "wayland")]
fn load_server_config(path: Option<PathBuf>) -> Result<(wprs::server::config::WprsdConfig, bool)> {
    let config_file = path.unwrap_or_else(|| config::default_config_file("wprsd"));
    let from_file =
        config::maybe_read_ron_file::<wprs::server::config::WprsdConfig>(&config_file)
            .location(loc!())?;
    let from_file_missing = from_file.is_none();
    let mut cfg = from_file.unwrap_or_default();
    if from_file_missing {
        cfg.stderr_log_level = SerializableLevel(tracing::Level::INFO);
    }
    if from_file_missing {
        error!("config file does not exist at {config_file:?}");
    }
    Ok((cfg, from_file_missing))
}

#[cfg(feature = "wayland")]
fn load_client_config(path: Option<PathBuf>) -> Result<wprs::client::config::WprscConfig> {
    let config_file = path.unwrap_or_else(|| config::default_config_file("wprsc"));
    let from_file =
        config::maybe_read_ron_file::<wprs::client::config::WprscConfig>(&config_file)
            .location(loc!())?;
    let missing = from_file.is_none();
    let cfg = from_file.unwrap_or_default();
    if missing {
        error!("config file does not exist at {config_file:?}");
    }
    Ok(cfg)
}

#[cfg(not(feature = "wayland"))]
fn main() -> Result<()> {
    bail!(Error::Unsupported(
        "wcmp requires the `wayland` feature".to_string(),
    ))
}

#[cfg(feature = "wayland")]
fn main() -> Result<()> {
    use wprs::client::backend::ClientBackendConfig;
    use wprs::client::config::ClientBackend;
    use wprs::client::config::WprscConfig;
    use wprs::client::runner::run_client_for_serializer;
    use wprs::protocols::wprs::serializer::new_inproc_serializer_pair;
    use wprs::protocols::wprs::types::Event;
    use wprs::protocols::wprs::types::Request;
    use wprs::server::backend::ServerBackend;
    use wprs::server::backends::wayland::WaylandSmithayBackend;
    use wprs::server::backends::wayland::WaylandSmithayBackendConfig;
    use wprs::server::config::WprsdConfig;
    use wprs::utils;

    let args = Args::parse();

    let (mut server_cfg, _server_missing) =
        load_server_config(args.server_config.clone()).location(loc!())?;
    let mut client_cfg = load_client_config(args.client_config.clone()).location(loc!())?;

    if let Some(display) = args.wayland_display {
        server_cfg.wayland.display = display;
    }
    if let Some(fps) = args.framerate {
        server_cfg.framerate = fps;
    }
    if args.kde_server_side_decorations {
        server_cfg.wayland.kde_server_side_decorations = true;
    }
    if args.no_xwayland {
        server_cfg.wayland.xwayland = None;
    }
    if let Some(display) = args.xwayland_display {
        server_cfg
            .wayland
            .xwayland
            .get_or_insert_with(Default::default)
            .display = Some(display);
    }
    if let Some(debug) = args.xwayland_wayland_debug {
        server_cfg
            .wayland
            .xwayland
            .get_or_insert_with(Default::default)
            .wayland_debug = debug;
    }

    if let Some(scale) = args.ui_scale_factor {
        client_cfg.ui_scale_factor = scale;
    }
    if let Some(scale) = args.min_output_scale_factor {
        client_cfg.min_output_scale_factor = Some(scale);
    }
    if let Some(mode) = args.keyboard_mode {
        client_cfg.keyboard_mode = mode;
    }
    if let Some(path) = args.xkb_keymap_file {
        client_cfg.xkb_keymap_file = Some(path);
    }

    config::set_log_priv_data(server_cfg.log_priv_data);
    utils::configure_tracing(
        server_cfg.stderr_log_level.0,
        server_cfg.log_file.clone(),
        server_cfg.file_log_level.0,
    )
    .location(loc!())?;
    utils::exit_on_thread_panic();

    client_cfg.present_backend = ClientBackend::WinitWgpu;
    client_cfg.endpoint = None;
    client_cfg.auto_reconnect = false;

    let client_backend_config = ClientBackendConfig {
        title_prefix: client_cfg.title_prefix,
        control_socket: client_cfg.control_socket,
        keyboard_mode: client_cfg.keyboard_mode,
        xkb_keymap_file: client_cfg.xkb_keymap_file,
        ui_scale_factor: client_cfg.ui_scale_factor,
        min_output_scale_factor: client_cfg.min_output_scale_factor,
        html_bind_addr: client_cfg.html_bind_addr,
    };

    #[cfg(not(feature = "winit-wgpu"))]
    {
        let _ = (server_cfg, client_cfg, client_backend_config);
        bail!(Error::Unsupported(
            "wcmp requires the `winit-wgpu` feature".to_string(),
        ))
    }

    #[cfg(feature = "winit-wgpu")]
    {
        let (server_serializer, client_serializer) =
            new_inproc_serializer_pair::<Request, Event>().location(loc!())?;
        let config = WaylandSmithayBackendConfig {
            wayland_display: server_cfg.wayland.display.clone(),
            framerate: server_cfg.framerate,
            xwayland: server_cfg.wayland.xwayland.clone(),
            kde_server_side_decorations: server_cfg.wayland.kde_server_side_decorations,
        };
        let server_thread = std::thread::spawn(move || {
            let backend = WaylandSmithayBackend::new(config);
            if let Err(err) = Box::new(backend).run(server_serializer, None) {
                error!("wcmp server failed: {err:?}");
            }
        });

        run_client_for_serializer(
            client_serializer,
            ClientBackend::WinitWgpu,
            client_backend_config,
        )
        .location(loc!())?;

        let _ = server_thread.join();
        Ok(())
    }
}
