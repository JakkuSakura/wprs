use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::ensure;
use clap::Parser;
use clap::ValueEnum;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use tracing::Level;

use crate::config;
use crate::config::SerializableLevel;
use crate::prelude::*;
use crate::protocols::wprs::endpoint::Endpoint;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum ClientBackend {
    Auto,
    Wayland,
    WinitWgpu,
    SgrPixels,
    Html,

    // --- Smithay feature bundle aliases ---
    //
    // These variants exist so users can reference the same `smithay_*` feature
    // bundle names from config/CLI. At the moment, wprsc does not have a
    // Smithay-based presentation backend; these variants are treated as aliases
    // to the existing `winit-wgpu` backend.
    //
    // Keeping them feature-gated ensures `--help` only shows values that were
    // actually compiled into the binary.
    #[cfg(feature = "smithay_winit_gl_wayland")]
    SmithayWinitGlWayland,
    #[cfg(feature = "smithay_winit_glow_wayland")]
    SmithayWinitGlowWayland,
    #[cfg(feature = "smithay_x11_gl_wayland")]
    SmithayX11GlWayland,
    #[cfg(feature = "smithay_x11_glow_wayland")]
    SmithayX11GlowWayland,
    #[cfg(feature = "smithay_drm_gbm_egl_gl_wayland")]
    SmithayDrmGbmEglGlWayland,
    #[cfg(feature = "smithay_drm_gbm_egl_glow_wayland")]
    SmithayDrmGbmEglGlowWayland,
    #[cfg(feature = "smithay_drm_pixman_wayland")]
    SmithayDrmPixmanWayland,
    #[cfg(feature = "smithay_drm_multi_gpu_wayland")]
    SmithayDrmMultiGpuWayland,
    #[cfg(feature = "smithay_xwayland")]
    SmithayXwayland,
    #[cfg(feature = "smithay_vulkan_support")]
    SmithayVulkanSupport,
    #[cfg(feature = "smithay_default_all")]
    SmithayDefaultAll,
    #[cfg(feature = "smithay_all_linux")]
    SmithayAllLinux,
}

impl Default for ClientBackend {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum KeyboardMode {
    Keymap,
    Evdev,
}

impl Default for KeyboardMode {
    fn default() -> Self {
        Self::Keymap
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum WprscRole {
    /// Connects to an existing wprs server and presents remote surfaces.
    Viewer,
    /// Hosts local Wayland clients (nested compositor) and presents them locally.
    WaylandServer,
}

impl Default for WprscRole {
    fn default() -> Self {
        Self::Viewer
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct WprscConfig {
    #[serde(default)]
    pub role: WprscRole,

    pub socket: PathBuf,
    pub endpoint: Option<Endpoint>,
    pub control_socket: PathBuf,
    pub log_file: Option<PathBuf>,
    pub stderr_log_level: SerializableLevel,
    pub file_log_level: SerializableLevel,
    pub log_priv_data: bool,
    pub title_prefix: String,

    #[serde(default = "default_true")]
    pub auto_reconnect: bool,

    #[serde(alias = "backend")]
    pub present_backend: ClientBackend,

    pub keyboard_mode: KeyboardMode,
    pub xkb_keymap_file: Option<PathBuf>,

    #[serde(default = "default_one")]
    pub ui_scale_factor: f64,

    #[serde(default = "default_html_bind_addr")]
    pub html_bind_addr: SocketAddr,

    /// Minimum output scale factor to report to the server (winit-wgpu backend only).
    ///
    /// This is an integer because the protocol uses Wayland-style integer scaling.
    ///
    /// On macOS, the default behavior is equivalent to `Some(2)` to avoid blurry
    /// rendering on Retina displays if the server uses a low scale.
    #[serde(default)]
    pub min_output_scale_factor: Option<i32>,

    #[serde(skip_serializing, default)]
    pub forward_only: bool,
}

impl Default for WprscConfig {
    fn default() -> Self {
        Self {
            role: WprscRole::default(),
            socket: config::default_socket_path(),
            endpoint: None,
            control_socket: config::default_control_socket_path("wprsc"),
            log_file: None,
            stderr_log_level: SerializableLevel(Level::INFO),
            file_log_level: SerializableLevel(Level::TRACE),
            log_priv_data: false,
            title_prefix: String::new(),

            auto_reconnect: true,

            present_backend: ClientBackend::default(),

            keyboard_mode: KeyboardMode::default(),
            xkb_keymap_file: None,

            ui_scale_factor: default_one(),

            html_bind_addr: default_html_bind_addr(),

            min_output_scale_factor: None,

            forward_only: false,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_one() -> f64 {
    1.0
}

pub(crate) fn default_html_bind_addr() -> SocketAddr {
    "127.0.0.1:7777".parse().expect("valid html bind addr")
}

#[derive(Parser, Debug, Clone)]
#[command(name = "wprsc")]
pub struct WprscArgs {
    #[arg(long, value_name = "BOOL", default_value_t = false, action = clap::ArgAction::Set)]
    pub print_default_config_and_exit: bool,

    #[arg(long, value_name = "PATH")]
    pub config_file: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    #[arg(long, value_name = "ENDPOINT")]
    pub endpoint: Option<Endpoint>,

    #[arg(long, value_name = "PATH")]
    pub control_socket: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    pub log_file: Option<PathBuf>,

    #[arg(long, value_name = "LEVEL")]
    pub stderr_log_level: Option<SerializableLevel>,

    #[arg(long, value_name = "LEVEL")]
    pub file_log_level: Option<SerializableLevel>,

    #[arg(long, value_name = "BOOL")]
    pub log_priv_data: Option<bool>,

    #[arg(long, value_name = "STRING")]
    pub title_prefix: Option<String>,

    #[arg(long, value_name = "ROLE")]
    pub role: Option<WprscRole>,

    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<ClientBackend>,

    #[arg(long, value_name = "MODE")]
    pub keyboard_mode: Option<KeyboardMode>,

    #[arg(long, value_name = "PATH")]
    pub xkb_keymap_file: Option<PathBuf>,

    #[arg(long, value_name = "SCALE")]
    pub ui_scale_factor: Option<f64>,

    #[arg(long, value_name = "SCALE")]
    pub min_output_scale_factor: Option<i32>,

    #[arg(long, value_name = "ADDR")]
    pub html_bind_addr: Option<SocketAddr>,

    #[arg(long, value_name = "BOOL", default_value_t = false, action = clap::ArgAction::Set)]
    pub forward_only: bool,

    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    pub no_auto_reconnect: bool,
}

impl WprscArgs {
    pub fn load_config(self) -> Result<WprscConfig> {
        if self.print_default_config_and_exit {
            config::print_default_config_and_exit::<WprscConfig>();
        }

        let config_file = self
            .config_file
            .clone()
            .unwrap_or_else(|| config::default_config_file("wprsc"));
        let mut cfg = WprscConfig::default();
        if let Some(from_file) =
            config::maybe_read_ron_file::<WprscConfig>(&config_file).location(loc!())?
        {
            cfg = from_file;
        }

        if let Some(socket) = self.socket {
            cfg.socket = socket;
        }
        if let Some(endpoint) = self.endpoint {
            cfg.endpoint = Some(endpoint);
        }
        if let Some(control_socket) = self.control_socket {
            cfg.control_socket = control_socket;
        }
        if let Some(log_file) = self.log_file {
            cfg.log_file = Some(log_file);
        }
        if let Some(level) = self.stderr_log_level {
            cfg.stderr_log_level = level;
        }
        if let Some(level) = self.file_log_level {
            cfg.file_log_level = level;
        }
        if let Some(val) = self.log_priv_data {
            cfg.log_priv_data = val;
        }
        if let Some(prefix) = self.title_prefix {
            cfg.title_prefix = prefix;
        }
        if let Some(role) = self.role {
            cfg.role = role;
        }
        if let Some(backend) = self.backend {
            cfg.present_backend = backend;
        }
        if let Some(mode) = self.keyboard_mode {
            cfg.keyboard_mode = mode;
        }
        if let Some(path) = self.xkb_keymap_file {
            cfg.xkb_keymap_file = Some(path);
        }

        if let Some(scale) = self.ui_scale_factor {
            cfg.ui_scale_factor = scale;
        }

        if let Some(scale) = self.min_output_scale_factor {
            cfg.min_output_scale_factor = Some(scale);
        }

        if let Some(addr) = self.html_bind_addr {
            cfg.html_bind_addr = addr;
        }

        if let Some(scale) = cfg.min_output_scale_factor {
            ensure!(scale >= 1, "min_output_scale_factor must be >= 1");
        }
        if self.forward_only {
            cfg.forward_only = true;
        }

        if self.no_auto_reconnect {
            cfg.auto_reconnect = false;
        }

        Ok(cfg)
    }
}
