pub mod backend;
pub mod backends;
pub mod config;
pub mod coords;
pub mod runner;
pub mod wayland_server;

#[cfg(feature = "wayland-client")]
pub use backends::wayland::*;

#[cfg(feature = "winit-wgpu-client")]
pub use backends::winit_wgpu;

pub use backend::ClientBackend;
pub use backend::ClientBackendConfig;
pub use backend::build_client_backend;
pub use runner::run_wprsc;
