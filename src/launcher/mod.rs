use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::process::Command;
use std::thread::JoinHandle;
use std::time;
use std::time::Duration;

use anyhow::ensure;

use crate::client::ClientBackendConfig;
use crate::client::config::ClientBackend;
use crate::config;
use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wprs;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::types::Event as WprsEvent;
use crate::protocols::wprs::types::Request as WprsRequest;
use crate::server::config::WprsdBackend;
use crate::server::config::WprsdConfig;
use crate::server::daemon;

const ENV_WCTL_SOCKET: &str = "WCTL_SOCKET";

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub wprsd_config_file: Option<PathBuf>,
    pub client_backend: Option<ClientBackend>,
    pub no_wayland: bool,
    pub no_x11: bool,
    pub cmd: Vec<OsString>,
}

struct DaemonInstance {
    control_endpoint: wctl::Endpoint,
    client: wctl::client::Client,
    inproc_client_serializer: Option<Serializer<WprsEvent, WprsRequest>>,
    embedded_server_thread: Option<JoinHandle<()>>,
}

struct CaptureTargetPidLease {
    client: wctl::client::Client,
}

impl Drop for CaptureTargetPidLease {
    fn drop(&mut self) {
        if cfg!(target_os = "macos") {
            self.client.stop_session().log_and_ignore(loc!());
        }
    }
}

fn start_capture_target_pid_lease(
    client: wctl::client::Client,
    pid: u32,
) -> Option<CaptureTargetPidLease> {
    if !cfg!(target_os = "macos") {
        return None;
    }

    client.start_session(pid).log_and_ignore(loc!());
    Some(CaptureTargetPidLease { client })
}

pub fn run(cfg: RunConfig) -> Result<i32> {
    ensure!(
        !cfg.cmd.is_empty(),
        "missing command; try: wrun -- <cmd> [args...]"
    );

    let wprsd_config_from_file =
        maybe_load_wprsd_config(cfg.wprsd_config_file.clone()).location(loc!())?;
    let mut daemon =
        connect_or_start_daemon(&cfg, wprsd_config_from_file.clone()).location(loc!())?;
    let server_info = daemon.client.server_info().location(loc!())?;
    if cfg.client_backend.is_none() {
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

    child.env(ENV_WCTL_SOCKET, daemon.control_endpoint.to_string());

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

    let capture_lease = start_capture_target_pid_lease(daemon.client.clone(), pid);

    if let Some(present_backend) = cfg.client_backend {
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let wait_endpoint = daemon.control_endpoint.clone();
        let mut capture_lease = capture_lease;

        let wait_thread = std::thread::spawn(move || {
            let _ = wait_endpoint;
            loop {
                if cancel_rx.try_recv().is_ok() {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(capture_lease.take());
                    return 1;
                }

                match child.try_wait() {
                    Ok(Some(status)) => {
                        let exit_code = status.code().unwrap_or(1);
                        drop(capture_lease.take());
                        std::process::exit(exit_code);
                    },
                    Ok(None) => {},
                    Err(_) => std::process::exit(1),
                }

                std::thread::sleep(Duration::from_millis(25));
            }
        });

        let backend_config = ClientBackendConfig {
            title_prefix: "wrun".to_string(),
            control_socket: config::default_control_socket_path("wprsc"),
            keyboard_mode: crate::client::config::KeyboardMode::default(),
            xkb_keymap_file: None,
            ui_scale_factor: 1.0,
            min_output_scale_factor: None,
        };

        if let Some(serializer) = daemon.inproc_client_serializer.take() {
            crate::client::runner::run_client_for_serializer(serializer, present_backend, backend_config)
                .log_and_ignore(loc!());
        } else {
            let wprs_endpoint = server_info.wprs_endpoint.parse().with_context(loc!(), || {
                format!("invalid wprs endpoint: {}", server_info.wprs_endpoint)
            })?;
            crate::client::runner::run_client_for_endpoint(wprs_endpoint, present_backend, backend_config)
                .log_and_ignore(loc!());
        }

        let _ = cancel_tx.send(());
        let exit_code = wait_thread.join().unwrap_or(1);
        drop(daemon.embedded_server_thread);
        return Ok(exit_code);
    }

    let status = child.wait().location(loc!())?;
    let exit_code = status.code().unwrap_or(1);

    drop(capture_lease);

    drop(daemon.embedded_server_thread);
    Ok(exit_code)
}

fn connect_or_start_daemon(
    cfg: &RunConfig,
    wprsd_config_from_file: Option<WprsdConfig>,
) -> Result<DaemonInstance> {
    let probe_endpoints = resolve_wctl_probe_endpoints(&wprsd_config_from_file).location(loc!())?;
    for candidate in &probe_endpoints {
        let client = wctl::client::Client::new(candidate.clone());
        if client.ping().is_ok() {
            return Ok(DaemonInstance {
                control_endpoint: candidate.clone(),
                client,
                inproc_client_serializer: None,
                embedded_server_thread: None,
            });
        }
    }

    let embedded_control_endpoint = resolve_embedded_control_endpoint().location(loc!())?;
    let client = wctl::client::Client::new(embedded_control_endpoint.clone());
    info!(
        "wrun: external wprsd not detected; starting embedded wprsd ({embedded_control_endpoint})"
    );

    let wprsd_config =
        derive_wprsd_config_for_wrun(wprsd_config_from_file, &embedded_control_endpoint)
            .location(loc!())?;

    let (embedded_server_thread, inproc_client_serializer) = if cfg.client_backend.is_some() {
        let (server_serializer, client_serializer) = wprs::serializer::new_inproc_serializer_pair::<
            wprs::types::Request,
            wprs::types::Event,
        >()
        .location(loc!())?;
        let wprs_endpoint = format!("inproc://wrun/{}", process::id());
        (
            Some(daemon::start_in_thread_with_serializer(
                wprsd_config,
                server_serializer,
                wprs_endpoint,
            )),
            Some(client_serializer),
        )
    } else {
        (Some(daemon::start_in_thread(wprsd_config)), None)
    };
    client.wait_ready(Duration::from_secs(5)).location(loc!())?;

    Ok(DaemonInstance {
        control_endpoint: embedded_control_endpoint,
        client,
        inproc_client_serializer,
        embedded_server_thread,
    })
}

fn maybe_load_wprsd_config(config_file: Option<PathBuf>) -> Result<Option<WprsdConfig>> {
    let config_file = config_file.unwrap_or_else(|| config::default_config_file("wprsd"));
    config::maybe_read_ron_file::<WprsdConfig>(&config_file).location(loc!())
}

fn resolve_wctl_probe_endpoints(
    wprsd_config_from_file: &Option<WprsdConfig>,
) -> Result<Vec<wctl::Endpoint>> {
    let mut endpoints = Vec::new();

    if let Some(env) = std::env::var_os(ENV_WCTL_SOCKET) {
        let endpoint: wctl::Endpoint = env
            .to_string_lossy()
            .parse()
            .with_context(loc!(), || format!("invalid {ENV_WCTL_SOCKET} value"))?;
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

fn resolve_embedded_control_endpoint() -> Result<wctl::Endpoint> {
    if let Some(env) = std::env::var_os(ENV_WCTL_SOCKET) {
        let endpoint: wctl::Endpoint = env
            .to_string_lossy()
            .parse()
            .with_context(loc!(), || format!("invalid {ENV_WCTL_SOCKET} value"))?;
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
        match embedded_control_endpoint {
            #[cfg(unix)]
            wctl::Endpoint::Unix { path } => {
                let dir = path.parent().unwrap_or_else(|| Path::new("/"));
                cfg.control_endpoint = Some(embedded_control_endpoint.clone());
                cfg.control_socket = path.clone();
                cfg.socket = dir.join("wprsd.sock");
                cfg.endpoint = None;
            },
            wctl::Endpoint::Tcp { addr } => {
                let port = addr.port();
                let wprs_port = port.saturating_sub(1).max(1025);
                cfg.control_endpoint = Some(embedded_control_endpoint.clone());
                cfg.endpoint = Some(crate::protocols::wprs::endpoint::Endpoint::Tcp {
                    addr: std::net::SocketAddr::from((addr.ip(), wprs_port)),
                });
            },
        }

        if cfg!(target_os = "macos") && cfg.backend.is_none() {
            cfg.backend = Some(WprsdBackend::MacosSeamless);
        }
    }

    Ok(cfg)
}

// Intentionally omitted: wrun no longer mutates the daemon config in-place.
