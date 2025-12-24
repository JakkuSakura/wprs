#[cfg(target_os = "windows")]
mod fullscreen;
#[cfg(target_os = "windows")]
mod window;

#[cfg(not(target_os = "windows"))]
mod stub;

#[cfg(target_os = "windows")]
pub use fullscreen::*;
#[cfg(target_os = "windows")]
pub use window::*;

#[cfg(not(target_os = "windows"))]
pub use stub::*;
