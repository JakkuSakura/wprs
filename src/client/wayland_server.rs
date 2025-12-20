use crate::client::config::WprscConfig;
use crate::prelude::*;
use anyhow::ensure;

pub fn run(config: WprscConfig) -> Result<()> {
    ensure!(
        !config.forward_only,
        "--forward-only is only meaningful for role=viewer"
    );

    #[cfg(all(target_os = "linux", feature = "wayland"))]
    {
        let _ = config;
        bail!("role=wayland-server is not implemented yet (planned: Smithay nested compositor)");
    }

    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    {
        let _ = config;
        bail!("role=wayland-server is only supported on Linux builds with the `wayland` feature enabled")
    }
}
