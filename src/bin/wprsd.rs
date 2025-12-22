// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::env;
use std::fs;
use std::process::Child;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use wprs::config;
use wprs::prelude::*;
use wprs::protocols::wctl;
#[cfg(feature = "rdp")]
use wprs::protocols::wprs::Endpoint;
use wprs::protocols::wprs::Event as ProtoEvent;
use wprs::protocols::wprs::Request as ProtoRequest;
use wprs::protocols::wprs::Serializer;
use wprs::server::backends;
use wprs::server::config::IntegrationMode;
use wprs::server::config::WprsdArgs;
use wprs::server::config::WprsdBackend;
use wprs::server::config::WprsdConfig;
use wprs::server::runtime::backend::ServerBackend;
use wprs::server::runtime::backend::TickMode;
use wprs::utils;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn infer_backend(config: &WprsdConfig) -> Result<WprsdBackend> {
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

fn main() -> Result<()> {
    let args = WprsdArgs::parse();
    let config = args.load_config().location(loc!())?;

    config::set_log_priv_data(config.log_priv_data);
    utils::configure_tracing(
        config.stderr_log_level.0,
        config.log_file.clone(),
        config.file_log_level.0,
    )
    .location(loc!())?;
    utils::exit_on_thread_panic();

    if config.endpoint.is_none() {
        fs::create_dir_all(config.socket.parent().location(loc!())?).location(loc!())?;
    }

    fs::create_dir_all(config.control_socket.parent().location(loc!())?).location(loc!())?;

    run_selected_backend(&config).location(loc!())
}

fn make_server_serializer(config: &WprsdConfig) -> Result<Serializer<ProtoRequest, ProtoEvent>> {
    match &config.endpoint {
        Some(endpoint) => Serializer::new_server_endpoint(endpoint.clone()).location(loc!()),
        None => Serializer::new_server(&config.socket).location(loc!()),
    }
}

fn run_selected_backend(config: &WprsdConfig) -> Result<()> {
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

    #[cfg(unix)]
    {
        let control_socket = config.control_socket.clone();
        std::thread::spawn(move || {
            let handler = Arc::new(ControlHandler::new(server_info, macos_target_pid));
            wctl::unix::serve(&control_socket, handler).log_and_ignore(loc!());
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

fn maybe_start_rdp_bridge(config: &WprsdConfig) -> Result<Option<ChildGuard>> {
    if !config.enable_rdp {
        return Ok(None);
    }

    match config.rdp_mode {
        IntegrationMode::External => {
            info!(
                "enable_rdp=true but rdp_mode=external; expecting external RDP bridge management"
            );
            Ok(None)
        },
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
                    wprs::rdp::run_bridge(wprs_endpoint, listen, wprs::rdp::Security::None)
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

#[cfg(unix)]
struct ControlHandler {
    server_info: wctl::ServerInfo,
    macos_target_pid: Option<backends::macos::MacosTargetPid>,
}

#[cfg(unix)]
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

#[cfg(unix)]
impl wctl::unix::Handler for ControlHandler {
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
