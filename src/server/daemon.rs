use std::env;
use std::process::Child;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wctl::Endpoint as WctlEndpoint;
use crate::protocols::wprs::Endpoint as WprsEndpoint;
#[cfg(feature = "rdp")]
use crate::protocols::wprs::Endpoint;
use crate::protocols::wprs::Event as ProtoEvent;
use crate::protocols::wprs::Request as ProtoRequest;
use crate::protocols::wprs::Serializer;
use crate::server::backends;
use crate::server::config::IntegrationMode;
use crate::server::config::WprsdBackend;
use crate::server::config::WprsdConfig;
use crate::server::runtime::backend::ServerBackend;
use crate::server::runtime::backend::TickMode;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn infer_backend(config: &WprsdConfig) -> Result<WprsdBackend> {
    if let Some(backend) = config.backend {
        return Ok(backend);
    }

    if cfg!(feature = "wayland") {
        return Ok(WprsdBackend::Wayland);
    }

    if cfg!(target_os = "macos") {
        return Ok(WprsdBackend::MacosFullscreen);
    }
    if cfg!(windows) {
        return Ok(WprsdBackend::WindowsFullscreen);
    }
    if cfg!(unix) {
        if env::var_os("DISPLAY").is_some() {
            return Ok(WprsdBackend::X11Fullscreen);
        }
        bail!(
            "no backend selected and $DISPLAY is not set; set `backend = \"wayland\"` and rebuild with `--features wayland`, or set $DISPLAY / choose an X11 backend explicitly"
        )
    }

    Ok(WprsdBackend::WindowsFullscreen)
}

pub fn make_server_serializer(
    config: &WprsdConfig,
) -> Result<Serializer<ProtoRequest, ProtoEvent>> {
    match &config.endpoint {
        Some(endpoint) => Serializer::new_server_endpoint(endpoint.clone()).location(loc!()),
        None => Serializer::new_server(&config.socket).location(loc!()),
    }
}

pub fn resolve_control_endpoint(config: &WprsdConfig) -> WctlEndpoint {
    if let Some(endpoint) = &config.control_endpoint {
        return endpoint.clone();
    }
    #[cfg(unix)]
    {
        return WctlEndpoint::Unix {
            path: config.control_socket.clone(),
        };
    }
    #[cfg(not(unix))]
    {
        WctlEndpoint::Tcp {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 48200)),
        }
    }
}

pub fn run(config: &WprsdConfig) -> Result<()> {
    if config.endpoint.is_none() {
        std::fs::create_dir_all(config.socket.parent().location(loc!())?).location(loc!())?;
    } else if let Some(WprsEndpoint::Unix { path }) = &config.endpoint {
        std::fs::create_dir_all(path.parent().location(loc!())?).location(loc!())?;
    }

    let control_endpoint = resolve_control_endpoint(config);
    #[cfg(unix)]
    if let WctlEndpoint::Unix { path } = &control_endpoint {
        std::fs::create_dir_all(path.parent().location(loc!())?).location(loc!())?;
    }

    let serializer = make_server_serializer(config).location(loc!())?;
    let _rdp_bridge = maybe_start_rdp_bridge(config).location(loc!())?;

    let backend_kind = infer_backend(config).location(loc!())?;
    let (backend, macos_target_pid) = build_backend(&backend_kind, config).location(loc!())?;

    let wprs_endpoint = match &config.endpoint {
        Some(endpoint) => endpoint.to_string(),
        None => format!("unix://{}", config.socket.display()),
    };

    let server_info = wctl::ServerInfo {
        wprs_endpoint,
        wayland_display: if backend_kind == WprsdBackend::Wayland {
            Some(config.wayland.display.clone())
        } else {
            None
        },
        xwayland_display: if backend_kind == WprsdBackend::Wayland {
            config.wayland.xwayland.as_ref().and_then(|x| x.display)
        } else {
            None
        },
    };

    {
        std::thread::spawn(move || {
            let handler = Arc::new(ControlHandler::new(server_info, macos_target_pid));
            wctl::server::serve(&control_endpoint, handler).log_and_ignore(loc!());
        });
    }

    let tick_interval = match backend.tick_mode() {
        TickMode::Polling => Some(Duration::from_secs_f64(
            1.0 / (config.framerate.max(1) as f64),
        )),
        TickMode::EventDriven => None,
    };

    backend.run(serializer, tick_interval).location(loc!())
}

pub fn start_in_thread(config: WprsdConfig) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        run(&config).log_and_ignore(loc!());
    })
}

fn maybe_start_rdp_bridge(config: &WprsdConfig) -> Result<Option<ChildGuard>> {
    if !config.enable_rdp {
        return Ok(None);
    }

    match config.rdp_mode {
        IntegrationMode::External => Ok(None),

        IntegrationMode::Embedded => {
            #[cfg(feature = "rdp")]
            {
                let wprs_endpoint: Endpoint = match &config.endpoint {
                    Some(endpoint) => endpoint.clone(),
                    None => format!("unix://{}", config.socket.display())
                        .parse()
                        .location(loc!())?,
                };
                let listen = config.rdp_listen;
                info!(
                    "starting embedded RDP bridge: wprs_endpoint={wprs_endpoint} rdp_listen={listen}"
                );

                std::thread::spawn(move || {
                    crate::rdp::run_bridge(wprs_endpoint, listen, crate::rdp::Security::None)
                        .log_and_ignore(loc!());
                });

                Ok(None)
            }

            #[cfg(not(feature = "rdp"))]
            {
                bail!("rdp_mode=embedded requires building wprsd with `--features rdp`")
            }
        },

        IntegrationMode::Spawned => {
            let wprs_endpoint = match &config.endpoint {
                Some(endpoint) => endpoint.to_string(),
                None => format!("unix://{}", config.socket.display()),
            };

            let mut cmd = Command::new(&config.rdp_bridge_path);
            cmd.arg("--wprs-endpoint")
                .arg(wprs_endpoint)
                .arg("--rdp-listen")
                .arg(config.rdp_listen.to_string())
                .arg("--security")
                .arg("none")
                .args(&config.rdp_bridge_args);

            info!("starting RDP bridge: {cmd:?}");
            let child = cmd.spawn().location(loc!())?;
            info!("RDP bridge spawned pid={pid}", pid = child.id());
            Ok(Some(ChildGuard(child)))
        },
    }
}

fn build_backend(
    backend: &WprsdBackend,
    config: &WprsdConfig,
) -> Result<(
    Box<dyn ServerBackend>,
    Option<backends::macos::MacosTargetPid>,
)> {
    match backend {
        WprsdBackend::X11Fullscreen => Ok((
            Box::new(
                backends::x11::X11FullscreenBackend::connect(config.x11_title.clone())
                    .location(loc!())?,
            ),
            None,
        )),
        WprsdBackend::WindowsFullscreen => Ok((
            Box::new(backends::windows::WindowsFullscreenBackend::new()),
            None,
        )),
        WprsdBackend::MacosFullscreen => Ok((
            Box::new(backends::macos::MacosFullscreenBackend::new(
                backends::macos::MacosFullscreenBackendConfig {
                    dpi: config.display_dpi,
                },
            )),
            None,
        )),
        WprsdBackend::WindowsSeamless => Ok((
            Box::new(backends::windows::WindowsWindowBackend::new()),
            None,
        )),
        WprsdBackend::MacosSeamless => {
            let backend = backends::macos::MacosWindowBackend::new(
                backends::macos::MacosWindowBackendConfig {
                    dpi: config.display_dpi,
                    target_pid: None,
                },
            );
            let pid = backend.target_pid_handle();
            Ok((Box::new(backend), Some(pid)))
        },
        WprsdBackend::Wayland => {
            #[cfg(feature = "wayland")]
            {
                Ok((
                    Box::new(backends::wayland::backend::WaylandSmithayBackend::new(
                        backends::wayland::backend::WaylandSmithayBackendConfig {
                            wayland_display: config.wayland.display.clone(),
                            framerate: config.framerate,
                            xwayland: config.wayland.xwayland.clone(),
                            kde_server_side_decorations: config.wayland.kde_server_side_decorations,
                        },
                    )),
                    None,
                ))
            }
            #[cfg(not(feature = "wayland"))]
            {
                let _ = config;
                bail!("wayland backend requires building wprsd with `--features wayland`")
            }
        },
    }
}

struct ControlHandler {
    server_info: wctl::ServerInfo,
    macos_target_pid: Option<backends::macos::MacosTargetPid>,
}

impl ControlHandler {
    fn new(
        server_info: wctl::ServerInfo,
        macos_target_pid: Option<backends::macos::MacosTargetPid>,
    ) -> Self {
        Self {
            server_info,
            macos_target_pid,
        }
    }
}

impl wctl::server::Handler for ControlHandler {
    fn handle(&self, req: wctl::Request) -> wctl::Response {
        match req {
            wctl::Request::Ping => wctl::Response::Pong,
            wctl::Request::ServerInfo => wctl::Response::ServerInfo(self.server_info.clone()),
            wctl::Request::SetCaptureTargetPid { pid } => {
                let Some(handle) = &self.macos_target_pid else {
                    return wctl::Response::Error {
                        message: "capture target pid is not supported by the current backend"
                            .to_string(),
                    };
                };
                handle.set(pid);
                wctl::Response::Ok
            },
        }
    }
}
