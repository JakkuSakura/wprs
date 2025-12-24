use std::path::PathBuf;

use clap::Parser;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use tracing::Level;

use crate::config;
use crate::config::SerializableLevel;
use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wprs::endpoint::Endpoint;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationMode {
    /// Run the integration in-process.
    Embedded,
    /// Spawn a helper process and manage its lifetime.
    Spawned,
    /// Do not start or manage any helper; expect external orchestration.
    External,
}

impl Default for IntegrationMode {
    fn default() -> Self {
        Self::External
    }
}

impl std::str::FromStr for IntegrationMode {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "embedded" => Ok(Self::Embedded),
            "spawned" => Ok(Self::Spawned),
            "external" => Ok(Self::External),
            other => bail!(Error::InvalidArgument(format!(
                "invalid integration mode {other:?} (expected: embedded|spawned|external)"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WprsdBackend {
    Wayland,
    WindowsFullscreen,
    MacosFullscreen,
    WindowsSeamless,
    MacosSeamless,
}

// XWayland support is only implemented as a built-in integration (Smithay's XWayland).
// There is no longer an out-of-process helper mode.

impl Default for WprsdBackend {
    fn default() -> Self {
        Self::Wayland
    }
}

impl std::str::FromStr for WprsdBackend {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "wayland" => Ok(Self::Wayland),
            "windows-fullscreen" => Ok(Self::WindowsFullscreen),
            "macos-fullscreen" => Ok(Self::MacosFullscreen),
            "windows-seamless" => Ok(Self::WindowsSeamless),
            "macos-seamless" => Ok(Self::MacosSeamless),
            other => bail!(Error::InvalidArgument(format!(
                "invalid backend {other:?} (expected: wayland|windows-fullscreen|macos-fullscreen|windows-seamless|macos-seamless)"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WprsdConfig {
    pub socket: PathBuf,
    pub control_socket: PathBuf,
    #[serde(default)]
    pub control_endpoint: Option<wctl::Endpoint>,
    pub endpoint: Option<Endpoint>,
    pub backend: Option<WprsdBackend>,
    pub framerate: u32,
    pub log_file: Option<PathBuf>,
    pub stderr_log_level: SerializableLevel,
    pub file_log_level: SerializableLevel,
    pub log_priv_data: bool,
    #[serde(default)]
    pub wayland: WprsdWaylandConfig,

    /// Enable the RDP translation bridge.
    ///
    /// When enabled, `wprsd` can optionally manage an RDP bridge process (see
    /// `rdp_mode`).
    #[serde(default)]
    pub enable_rdp: bool,
    #[serde(default = "default_rdp_mode")]
    pub rdp_mode: IntegrationMode,
    #[serde(default = "default_rdp_listen")]
    pub rdp_listen: std::net::SocketAddr,
    #[serde(default = "default_rdp_bridge_path")]
    pub rdp_bridge_path: String,
    #[serde(default)]
    pub rdp_bridge_args: Vec<String>,

    /// Optional display DPI override (primarily used by capture backends).
    #[serde(default)]
    pub display_dpi: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct WprsdWaylandConfig {
    pub display: String,
    pub kde_server_side_decorations: bool,
    #[serde(default)]
    pub xwayland: Option<XwaylandConfig>,
}

impl Default for WprsdWaylandConfig {
    fn default() -> Self {
        Self {
            display: config::default_wayland_display(),
            kde_server_side_decorations: false,
            // Preserve the historical default of enabling XWayland when present.
            xwayland: Some(XwaylandConfig::default()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct XwaylandConfig {
    /// Preferred XWayland display number.
    ///
    /// When set, `wprsd` will try to start XWayland on this display first (e.g. `:100`).
    /// If the display is already in use, it will fall back to auto-picking a free display.
    pub display: Option<u32>,
    pub wayland_debug: bool,
}

impl Default for XwaylandConfig {
    fn default() -> Self {
        Self {
            display: Some(100),
            wayland_debug: false,
        }
    }
}

fn default_rdp_mode() -> IntegrationMode {
    IntegrationMode::Spawned
}

fn default_rdp_listen() -> std::net::SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], 3389))
}

fn default_rdp_bridge_path() -> String {
    "wprs-rdp-bridge".to_string()
}

impl Default for WprsdConfig {
    fn default() -> Self {
        let mut cfg = Self {
            socket: config::default_socket_path(),
            control_socket: config::default_control_socket_path("wprsd"),
            control_endpoint: None,
            endpoint: None,
            backend: None,
            framerate: 60,
            log_file: None,
            stderr_log_level: SerializableLevel(Level::INFO),
            file_log_level: SerializableLevel(Level::TRACE),
            log_priv_data: false,
            wayland: WprsdWaylandConfig::default(),
            enable_rdp: false,
            rdp_mode: default_rdp_mode(),
            rdp_listen: default_rdp_listen(),
            rdp_bridge_path: default_rdp_bridge_path(),
            rdp_bridge_args: Vec::new(),
            display_dpi: None,
        };

        if !cfg!(unix) {
            cfg.endpoint = Some(Endpoint::Tcp {
                addr: std::net::SocketAddr::from(([127, 0, 0, 1], 48199)),
            });
            cfg.control_endpoint = Some(wctl::Endpoint::Tcp {
                addr: std::net::SocketAddr::from(([127, 0, 0, 1], 48200)),
            });
        }

        cfg
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "wprsd")]
pub struct WprsdArgs {
    #[arg(long, value_name = "BOOL", default_value_t = false, action = clap::ArgAction::Set)]
    pub print_default_config_and_exit: bool,

    #[arg(long, value_name = "PATH")]
    pub config_file: Option<PathBuf>,

    #[arg(long, value_name = "NAME")]
    pub wayland_display: Option<String>,

    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub control_socket: Option<PathBuf>,

    #[arg(long, value_name = "ENDPOINT")]
    pub control_endpoint: Option<wctl::Endpoint>,

    #[arg(long, value_name = "ENDPOINT")]
    pub endpoint: Option<Endpoint>,

    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<WprsdBackend>,

    #[arg(long, value_name = "FPS")]
    pub framerate: Option<u32>,

    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    #[arg(long, value_name = "LEVEL")]
    pub stderr_log_level: Option<SerializableLevel>,

    #[arg(long, value_name = "LEVEL")]
    pub file_log_level: Option<SerializableLevel>,

    #[arg(long, value_name = "BOOL")]
    pub log_priv_data: Option<bool>,

    #[arg(long, value_name = "BOOL")]
    pub kde_server_side_decorations: Option<bool>,

    #[arg(long, value_name = "NUM")]
    pub xwayland_display: Option<u32>,

    #[arg(long, value_name = "BOOL")]
    pub xwayland_wayland_debug: Option<bool>,

    #[arg(long, value_name = "BOOL")]
    pub enable_rdp: Option<bool>,

    #[arg(long, value_name = "MODE")]
    pub rdp_mode: Option<IntegrationMode>,

    #[arg(long, value_name = "ADDR")]
    pub rdp_listen: Option<std::net::SocketAddr>,

    #[arg(long, value_name = "PATH")]
    pub rdp_bridge_path: Option<String>,

    #[arg(long, value_name = "ARG", value_delimiter = ',')]
    pub rdp_bridge_args: Vec<String>,

    #[arg(long, value_name = "DPI")]
    pub display_dpi: Option<u32>,
}

impl WprsdArgs {
    pub fn load_config(self) -> Result<WprsdConfig> {
        if self.print_default_config_and_exit {
            config::print_default_config_and_exit::<WprsdConfig>();
        }

        let config_file = self
            .config_file
            .clone()
            .unwrap_or_else(|| config::default_config_file("wprsd"));
        let mut cfg = WprsdConfig::default();
        if let Some(from_file) =
            config::maybe_read_ron_file::<WprsdConfig>(&config_file).location(loc!())?
        {
            cfg = from_file;
        }

        if let Some(v) = self.wayland_display {
            cfg.wayland.display = v;
        }
        if let Some(v) = self.socket {
            cfg.socket = v;
        }
        if let Some(v) = self.control_socket {
            cfg.control_socket = v;
        }
        if let Some(v) = self.control_endpoint {
            cfg.control_endpoint = Some(v);
        }
        if let Some(v) = self.endpoint {
            cfg.endpoint = Some(v);
        }
        if let Some(v) = self.backend {
            cfg.backend = Some(v);
        }
        if let Some(v) = self.framerate {
            cfg.framerate = v;
        }
        if let Some(v) = self.log_file {
            cfg.log_file = Some(v);
        }
        if let Some(v) = self.stderr_log_level {
            cfg.stderr_log_level = v;
        }
        if let Some(v) = self.file_log_level {
            cfg.file_log_level = v;
        }
        if let Some(v) = self.log_priv_data {
            cfg.log_priv_data = v;
        }
        if let Some(v) = self.kde_server_side_decorations {
            cfg.wayland.kde_server_side_decorations = v;
        }
        if let Some(v) = self.xwayland_display {
            cfg.wayland
                .xwayland
                .get_or_insert_with(XwaylandConfig::default)
                .display = Some(v);
        }
        if let Some(v) = self.xwayland_wayland_debug {
            cfg.wayland
                .xwayland
                .get_or_insert_with(XwaylandConfig::default)
                .wayland_debug = v;
        }

        if let Some(v) = self.enable_rdp {
            cfg.enable_rdp = v;
        }
        if let Some(v) = self.rdp_mode {
            cfg.rdp_mode = v;
        }
        if let Some(v) = self.rdp_listen {
            cfg.rdp_listen = v;
        }
        if let Some(v) = self.rdp_bridge_path {
            cfg.rdp_bridge_path = v;
        }
        if !self.rdp_bridge_args.is_empty() {
            cfg.rdp_bridge_args = self.rdp_bridge_args;
        }

        if let Some(v) = self.display_dpi {
            cfg.display_dpi = Some(v);
        }

        Ok(cfg)
    }
}
