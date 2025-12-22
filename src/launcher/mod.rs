use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::Command;
use std::time;
use std::time::Duration;

use anyhow::ensure;

use crate::client::ClientBackendConfig;
use crate::client::config::ClientBackend;
use crate::config;
use crate::prelude::*;
use crate::protocols::wctl;
use crate::server::config::WprsdBackend;
use crate::server::config::WprsdConfig;
use crate::server::daemon;

const ENV_WCTL_ENDPOINT: &str = "WPRS_WCTL_ENDPOINT";

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub wprsd_config_file: Option<PathBuf>,
    pub wctl_endpoint: Option<String>,
    pub present_backend: Option<ClientBackend>,
    pub no_wayland: bool,
    pub no_x11: bool,
    pub cmd: Vec<OsString>,
}

pub fn run(cfg: RunConfig) -> Result<i32> {
    ensure!(
        !cfg.cmd.is_empty(),
        "missing command; try: wrun -- <cmd> [args...]"
    );

    let wprsd_config_from_file =
        maybe_load_wprsd_config(cfg.wprsd_config_file.clone()).location(loc!())?;
    let probe_endpoints = resolve_wctl_probe_endpoints(&cfg.wctl_endpoint, &wprsd_config_from_file)
        .location(loc!())?;
    let embedded_control_endpoint =
        resolve_embedded_control_endpoint(&cfg.wctl_endpoint).location(loc!())?;

    let mut endpoint = None;
    for candidate in &probe_endpoints {
        let client = wctl::client::Client::new(candidate.clone());
        if client.ping().is_ok() {
            endpoint = Some(candidate.clone());
            break;
        }
    }

    let endpoint = endpoint.unwrap_or_else(|| embedded_control_endpoint.clone());
    let client = wctl::client::Client::new(endpoint.clone());
    let mut embedded_server_thread = None;
    if let Err(err) = client.ping() {
        info!("wrun: external wprsd not detected ({endpoint}): {err:?}; starting embedded wprsd");

        let mut wprsd_config = derive_wprsd_config_for_wrun(
            wprsd_config_from_file.clone(),
            &embedded_control_endpoint,
        )
        .location(loc!())?;

        if cfg!(target_os = "macos") && wprsd_config.backend.is_none() {
            wprsd_config.backend = Some(WprsdBackend::MacosSeamless);
        }

        configure_wprsd_control_endpoint(&mut wprsd_config, embedded_control_endpoint.clone());
        embedded_server_thread = Some(daemon::start_in_thread(wprsd_config));
        client.wait_ready(Duration::from_secs(5)).location(loc!())?;
    }

    let server_info = client.server_info().location(loc!())?;
    if cfg.present_backend.is_none() {
        println!("{}", server_info.wprs_endpoint);
    } else {
        info!("wrun: wprs_endpoint={}", server_info.wprs_endpoint);
    }

    let (program, args) = cfg
        .cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command"))
        .location(loc!())?;

    let mut child = Command::new(program);
    child.args(args);

    if cfg!(target_os = "linux") {
        if !cfg.no_wayland {
            if let Some(display) = &server_info.wayland_display {
                child.env("WAYLAND_DISPLAY", display);
            }
        }

        if !cfg.no_x11 {
            if let Some(display) = server_info.xwayland_display {
                child.env("DISPLAY", format!(":{display}"));
            }
        }
    }

    let mut child = child.spawn().location(loc!())?;
    let pid = child.id();

    if cfg!(target_os = "macos") {
        client
            .set_capture_target_pid(Some(pid))
            .log_and_ignore(loc!());
    }

    if let Some(present_backend) = cfg.present_backend {
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let wait_endpoint = endpoint.clone();

        let wait_thread = std::thread::spawn(move || {
            let client = wctl::client::Client::new(wait_endpoint);
            loop {
                if cancel_rx.try_recv().is_ok() {
                    let _ = child.kill();
                    let _ = child.wait();
                    if cfg!(target_os = "macos") {
                        client.set_capture_target_pid(None).log_and_ignore(loc!());
                    }
                    return 1;
                }

                match child.try_wait() {
                    Ok(Some(status)) => {
                        let exit_code = status.code().unwrap_or(1);
                        if cfg!(target_os = "macos") {
                            client.set_capture_target_pid(None).log_and_ignore(loc!());
                        }
                        std::process::exit(exit_code);
                    },
                    Ok(None) => {},
                    Err(_) => std::process::exit(1),
                }

                std::thread::sleep(Duration::from_millis(25));
            }
        });

        let wprs_endpoint = server_info.wprs_endpoint.parse().with_context(loc!(), || {
            format!("invalid wprs endpoint: {}", server_info.wprs_endpoint)
        })?;

        let backend_config = ClientBackendConfig {
            title_prefix: "wrun".to_string(),
            control_socket: config::default_control_socket_path("wprsc"),
            keyboard_mode: crate::client::config::KeyboardMode::default(),
            xkb_keymap_file: None,
            ui_scale_factor: 1.0,
            min_output_scale_factor: None,
        };

        crate::client::runner::run_viewer_for_endpoint(
            wprs_endpoint,
            present_backend,
            backend_config,
        )
        .log_and_ignore(loc!());

        let _ = cancel_tx.send(());
        let exit_code = wait_thread.join().unwrap_or(1);
        drop(embedded_server_thread);
        return Ok(exit_code);
    }

    let status = child.wait().location(loc!())?;
    let exit_code = status.code().unwrap_or(1);

    if cfg!(target_os = "macos") {
        client.set_capture_target_pid(None).log_and_ignore(loc!());
    }

    drop(embedded_server_thread);
    Ok(exit_code)
}

fn maybe_load_wprsd_config(config_file: Option<PathBuf>) -> Result<Option<WprsdConfig>> {
    let config_file = config_file.unwrap_or_else(|| config::default_config_file("wprsd"));
    config::maybe_read_ron_file::<WprsdConfig>(&config_file).location(loc!())
}

fn resolve_wctl_probe_endpoints(
    cli: &Option<String>,
    wprsd_config_from_file: &Option<WprsdConfig>,
) -> Result<Vec<wctl::Endpoint>> {
    let mut endpoints = Vec::new();

    if let Some(endpoint) = cli {
        let endpoint: wctl::Endpoint = endpoint.parse().location(loc!())?;
        endpoint.ensure_localhost().location(loc!())?;
        endpoints.push(endpoint);
        return Ok(endpoints);
    }

    if let Some(env) = std::env::var_os(ENV_WCTL_ENDPOINT) {
        let endpoint: wctl::Endpoint = env
            .to_string_lossy()
            .parse()
            .with_context(loc!(), || format!("invalid {ENV_WCTL_ENDPOINT} value"))?;
        endpoint.ensure_localhost().location(loc!())?;
        endpoints.push(endpoint);
        return Ok(endpoints);
    }

    if let Some(cfg) = wprsd_config_from_file {
        endpoints.push(daemon::resolve_control_endpoint(cfg));
    }

    endpoints.push(default_wctl_probe_endpoint());

    let mut unique = Vec::new();
    for endpoint in endpoints {
        if !unique.contains(&endpoint) {
            unique.push(endpoint);
        }
    }
    Ok(unique)
}

fn resolve_embedded_control_endpoint(cli: &Option<String>) -> Result<wctl::Endpoint> {
    if let Some(endpoint) = cli {
        let endpoint: wctl::Endpoint = endpoint.parse().location(loc!())?;
        endpoint.ensure_localhost().location(loc!())?;
        return Ok(endpoint);
    }

    if let Some(env) = std::env::var_os(ENV_WCTL_ENDPOINT) {
        let endpoint: wctl::Endpoint = env
            .to_string_lossy()
            .parse()
            .with_context(loc!(), || format!("invalid {ENV_WCTL_ENDPOINT} value"))?;
        endpoint.ensure_localhost().location(loc!())?;
        return Ok(endpoint);
    }

    Ok(default_embedded_wctl_endpoint())
}

fn default_wctl_probe_endpoint() -> wctl::Endpoint {
    #[cfg(unix)]
    {
        return wctl::Endpoint::Unix {
            path: config::default_control_socket_path("wprsd"),
        };
    }
    #[cfg(not(unix))]
    {
        wctl::Endpoint::Tcp {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 48200)),
        }
    }
}

fn default_embedded_wctl_endpoint() -> wctl::Endpoint {
    #[cfg(unix)]
    {
        let dir = unique_runtime_dir();
        return wctl::Endpoint::Unix {
            path: dir.join("wprsd-ctrl.sock"),
        };
    }
    #[cfg(not(unix))]
    {
        wctl::Endpoint::Tcp {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 48200)),
        }
    }
}

fn unique_runtime_dir() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(whoami::username()));
    let nanos = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    runtime
        .join("wprs")
        .join("wrun")
        .join(format!("{}-{nanos}", process::id()))
}

fn derive_wprsd_config_for_wrun(
    from_file: Option<WprsdConfig>,
    embedded_control_endpoint: &wctl::Endpoint,
) -> Result<WprsdConfig> {
    let from_file_is_none = from_file.is_none();
    let mut cfg = from_file.unwrap_or_default();

    if from_file_is_none {
        cfg.control_endpoint = Some(embedded_control_endpoint.clone());

        match embedded_control_endpoint {
            #[cfg(unix)]
            wctl::Endpoint::Unix { path } => {
                let dir = path.parent().unwrap_or_else(|| Path::new("/"));
                cfg.control_socket = path.clone();
                cfg.socket = dir.join("wprsd.sock");
                cfg.endpoint = None;
            },
            wctl::Endpoint::Tcp { addr } => {
                let port = addr.port();
                let wprs_port = port.saturating_sub(1).max(1025);
                cfg.control_endpoint = Some(embedded_control_endpoint.clone());
                cfg.endpoint = Some(crate::protocols::wprs::Endpoint::Tcp {
                    addr: std::net::SocketAddr::from((addr.ip(), wprs_port)),
                });
            },
        }
    }

    Ok(cfg)
}

fn configure_wprsd_control_endpoint(cfg: &mut WprsdConfig, endpoint: wctl::Endpoint) {
    cfg.control_endpoint = Some(endpoint.clone());
    #[cfg(unix)]
    if let wctl::Endpoint::Unix { path } = endpoint {
        cfg.control_socket = path;
    }
}
