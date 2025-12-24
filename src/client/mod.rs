pub mod backend;
pub mod backends;
pub mod config;
pub mod coords;
pub mod error;
pub mod runner;
pub mod state;
pub mod wayland_server;
pub mod surface_registry;
pub mod window_manager;

#[cfg(feature = "wayland-client")]
pub use backends::wayland::*;

#[cfg(feature = "winit-wgpu")]
pub use backends::winit_wgpu;

pub use backend::ClientBackend;
pub use backend::ClientBackendConfig;
pub use backend::build_client_backend;
pub use backend::resolve_client_backend;
pub use runner::run_wprsc;
