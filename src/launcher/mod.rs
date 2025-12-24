use std::ffi::OsString;
use std::process;
use std::process::Command;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::client::ClientBackendConfig;
use crate::client::config::ClientBackend;
use crate::config;
use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wprs;
use crate::protocols::wprs::types::Event as WprsEvent;
use crate::protocols::wprs::types::Request as WprsRequest;
use crate::server::config::WprsdBackend;
use crate::server::config::WprsdConfig;
use crate::server::daemon;

const ENV_WCTL_SOCKET: &str = "WCTL_SOCKET";

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub wprsd_config_from_file: Option<WprsdConfig>,
    pub backend: Option<ClientBackend>,
    pub no_wayland: bool,
    pub no_x11: bool,
    pub cmd: Vec<OsString>,
}

struct DaemonInstance {
    control_endpoint: Option<wctl::Endpoint>,
    client: ControlClient,
    inproc_client_serializer: Option<wprs::serializer::Serializer<WprsEvent, WprsRequest>>,
    embedded_server_thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum ControlClient {
    Socket(wctl::client::Client),
    Inproc(wctl::inproc::Client),
}

impl ControlClient {
    fn ping(&self) -> Result<()> {
        match self {
            Self::Socket(client) => client.ping(),
            Self::Inproc(client) => client.ping(),
        }
    }

    fn server_info(&self) -> Result<wctl::ServerInfo> {
        match self {
            Self::Socket(client) => client.server_info(),
            Self::Inproc(client) => client.server_info(),
        }
    }

    fn start_session(&self, child_pid: u32) -> Result<()> {
        match self {
            Self::Socket(client) => client.start_session(child_pid),
            Self::Inproc(client) => client.start_session(child_pid),
        }
    }

    fn stop_session(&self) -> Result<()> {
        match self {
            Self::Socket(client) => client.stop_session(),
            Self::Inproc(client) => client.stop_session(),
        }
    }

    fn wait_ready(&self, timeout: Duration) -> Result<()> {
        match self {
            Self::Socket(client) => client.wait_ready(timeout),
            Self::Inproc(client) => client.wait_ready(timeout),
        }
    }
}

struct CaptureTargetPidLease {
    client: ControlClient,
}

impl Drop for CaptureTargetPidLease {
    fn drop(&mut self) {
        if cfg!(target_os = "macos") {
            self.client.stop_session().log_and_ignore(loc!());
        }
    }
}

fn start_capture_target_pid_lease(client: ControlClient, pid: u32) -> Option<CaptureTargetPidLease> {
    if !cfg!(target_os = "macos") {
        return None;
    }

    client.start_session(pid).log_and_ignore(loc!());
    Some(CaptureTargetPidLease { client })
}

pub fn run(cfg: RunConfig) -> Result<i32> {
    ensure!(
        !cfg.cmd.is_empty(),
        Error::Missing("command; try: wrun -- <cmd> [args...]".to_string()),
    );

    let wprsd_config_from_file = cfg.wprsd_config_from_file.clone();
    let mut daemon =
        connect_or_start_daemon(&cfg, wprsd_config_from_file.clone()).location(loc!())?;
    let server_info = daemon.client.server_info().location(loc!())?;
    if cfg.backend.is_none() {
        println!("{}", server_info.wprs_endpoint);
    } else {
        info!("wrun: wprs_endpoint={}", server_info.wprs_endpoint);
    }

    let (program, args) = cfg
        .cmd
        .split_first()
        .ok_or_else(|| Error::Missing("command".to_string()))
        .location(loc!())?;

    let mut child = Command::new(program);
    child.args(args);

    if let Some(endpoint) = &daemon.control_endpoint {
        child.env(ENV_WCTL_SOCKET, endpoint.to_string());
    }

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

    if let Some(present_backend) = cfg.backend {
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
            html_bind_addr: crate::client::config::default_html_bind_addr(),
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
    _cfg: &RunConfig,
    wprsd_config_from_file: Option<WprsdConfig>,
) -> Result<DaemonInstance> {
    let probe_endpoints = resolve_wctl_probe_endpoints(&wprsd_config_from_file).location(loc!())?;
    for candidate in &probe_endpoints {
        let client = ControlClient::Socket(wctl::client::Client::new(candidate.clone()));
        if client.ping().is_ok() {
            return Ok(DaemonInstance {
                control_endpoint: Some(candidate.clone()),
                client,
                inproc_client_serializer: None,
                embedded_server_thread: None,
            });
        }
    }

    let (control_client_raw, control_server) = wctl::inproc::channel_pair();
    let control_client = ControlClient::Inproc(control_client_raw);
    info!(
        "wrun: external wprsd not detected; starting embedded wprsd (inproc control)"
    );

    let wprsd_config =
        derive_wprsd_config_for_wrun(wprsd_config_from_file, process::id()).location(loc!())?;

    let embedded_server_thread =
        Some(daemon::start_in_thread_with_control(wprsd_config, control_server));
    let inproc_client_serializer = None;
    control_client.wait_ready(Duration::from_secs(5)).location(loc!())?;

    Ok(DaemonInstance {
        control_endpoint: None,
        client: control_client,
        inproc_client_serializer,
        embedded_server_thread,
    })
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

fn derive_wprsd_config_for_wrun(from_file: Option<WprsdConfig>, pid: u32) -> Result<WprsdConfig> {
    let from_file_is_none = from_file.is_none();
    let mut cfg = from_file.unwrap_or_default();

    if from_file_is_none {
        #[cfg(unix)]
        {
            let path = std::env::temp_dir().join(format!("wrun-{}-wprs.sock", pid));
            let dir = path.parent().unwrap_or_else(|| std::path::Path::new("/")).to_path_buf();
            cfg.socket = path;
            cfg.endpoint = None;
            cfg.control_endpoint = None;
            cfg.control_socket = dir.join(format!("wrun-{}-ctrl.sock", pid));
        }
        #[cfg(not(unix))]
        {
            let addr = pick_free_loopback_port().location(loc!())?;
            cfg.endpoint = Some(crate::protocols::wprs::endpoint::Endpoint::Tcp { addr });
            cfg.control_endpoint = None;
        }
        if cfg!(target_os = "macos") && cfg.backend.is_none() {
            cfg.backend = Some(WprsdBackend::MacosSeamless);
        }
    }

    Ok(cfg)
}

#[cfg(not(unix))]
fn pick_free_loopback_port() -> Result<std::net::SocketAddr> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").location(loc!())?;
    let addr = listener.local_addr().location(loc!())?;
    Ok(addr)
}

// Intentionally omitted: wrun no longer mutates the daemon config in-place.
