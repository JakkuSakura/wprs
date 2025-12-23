use std::path::PathBuf;

use crate::client::config;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::Serializer;

#[derive(Debug, Clone)]
pub struct ClientBackendConfig {
    pub title_prefix: String,
    pub control_socket: PathBuf,
    pub keyboard_mode: config::KeyboardMode,
    pub xkb_keymap_file: Option<PathBuf>,
    pub ui_scale_factor: f64,
    pub min_output_scale_factor: Option<i32>,
}

pub trait ClientBackend {
    fn name(&self) -> &'static str;

    fn run(self: Box<Self>, serializer: Serializer<proto::Event, proto::Request>) -> Result<()>;
}

fn build_winit_wgpu_backend(config: ClientBackendConfig) -> Result<Box<dyn ClientBackend>> {
    #[cfg(feature = "winit-wgpu-client")]
    {
        Ok(Box::new(
            crate::client::backends::winit_wgpu::WinitWgpuClientBackend::new(config),
        ))
    }

    #[cfg(not(feature = "winit-wgpu-client"))]
    {
        let _ = config;
        bail!(
            "winit-wgpu backend requested but not compiled in. Rebuild with `--features winit-wgpu-client`."
        )
    }
}

#[cfg(any(
    feature = "smithay_winit_gl_wayland",
    feature = "smithay_winit_glow_wayland",
    feature = "smithay_x11_gl_wayland",
    feature = "smithay_x11_glow_wayland",
    feature = "smithay_drm_gbm_egl_gl_wayland",
    feature = "smithay_drm_gbm_egl_glow_wayland",
    feature = "smithay_drm_pixman_wayland",
    feature = "smithay_drm_multi_gpu_wayland",
    feature = "smithay_xwayland",
    feature = "smithay_vulkan_support",
    feature = "smithay_default_all",
    feature = "smithay_all_linux",
))]
fn build_winit_wgpu_backend_aliased(
    alias: config::ClientBackend,
    config: ClientBackendConfig,
) -> Result<Box<dyn ClientBackend>> {
    info!("wprsc backend={alias:?} is currently an alias for backend=winit-wgpu");
    build_winit_wgpu_backend(config)
}

pub fn build_client_backend(
    requested: config::ClientBackend,
    config: ClientBackendConfig,
) -> Result<Box<dyn ClientBackend>> {
    match requested {
        config::ClientBackend::Auto => {
            bail!("ClientBackend::Auto must be resolved before calling build_client_backend")
        },
        #[cfg(feature = "smithay_winit_gl_wayland")]
        config::ClientBackend::SmithayWinitGlWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_winit_glow_wayland")]
        config::ClientBackend::SmithayWinitGlowWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_x11_gl_wayland")]
        config::ClientBackend::SmithayX11GlWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_x11_glow_wayland")]
        config::ClientBackend::SmithayX11GlowWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_drm_gbm_egl_gl_wayland")]
        config::ClientBackend::SmithayDrmGbmEglGlWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_drm_gbm_egl_glow_wayland")]
        config::ClientBackend::SmithayDrmGbmEglGlowWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_drm_pixman_wayland")]
        config::ClientBackend::SmithayDrmPixmanWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_drm_multi_gpu_wayland")]
        config::ClientBackend::SmithayDrmMultiGpuWayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_xwayland")]
        config::ClientBackend::SmithayXwayland => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_vulkan_support")]
        config::ClientBackend::SmithayVulkanSupport => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_default_all")]
        config::ClientBackend::SmithayDefaultAll => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        #[cfg(feature = "smithay_all_linux")]
        config::ClientBackend::SmithayAllLinux => {
            build_winit_wgpu_backend_aliased(requested, config)
        },
        config::ClientBackend::Wayland => {
            #[cfg(feature = "wayland-client")]
            {
                Ok(Box::new(
                    crate::client::backends::wayland::WaylandClientBackend::connect_to_env(config)
                        .location(loc!())?,
                ))
            }

            #[cfg(not(feature = "wayland-client"))]
            {
                let _ = config;
                bail!(
                    "Wayland backend requested but not compiled in. Rebuild with `--features wayland-client`."
                )
            }
        },
        config::ClientBackend::WinitWgpu => build_winit_wgpu_backend(config),
        config::ClientBackend::SgrPixels => Ok(Box::new(
            crate::client::backends::sgr_pixels::SgrPixelsClientBackend::new(config),
        )),
    }
}

pub fn resolve_client_backend(requested: config::ClientBackend) -> Result<config::ClientBackend> {
    match requested {
        config::ClientBackend::Auto => resolve_auto_backend().location(loc!()),
        other => Ok(other),
    }
}

fn resolve_auto_backend() -> Result<config::ClientBackend> {
    #[cfg(feature = "wayland-client")]
    {
        use smithay_client_toolkit::reexports::client::ConnectError;
        use smithay_client_toolkit::reexports::client::Connection;

        match Connection::connect_to_env() {
            Ok(_) => return Ok(config::ClientBackend::Wayland),
            Err(ConnectError::NoCompositor) => {
                // No compositor; fall through.
            },
            Err(e) => return Err(anyhow!(e)),
        }
    }

    #[cfg(feature = "winit-wgpu-client")]
    return Ok(config::ClientBackend::WinitWgpu);

    #[cfg(not(feature = "winit-wgpu-client"))]
    Ok(config::ClientBackend::SgrPixels)
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn explicit_backend_is_not_modified() {
        assert_eq!(
            resolve_client_backend(config::ClientBackend::SgrPixels).unwrap(),
            config::ClientBackend::SgrPixels
        );
    }

    #[test]
    #[cfg(all(feature = "winit-wgpu-client", not(feature = "wayland-client")))]
    fn auto_prefers_winit_when_wayland_client_missing() {
        assert_eq!(
            resolve_client_backend(config::ClientBackend::Auto).unwrap(),
            config::ClientBackend::WinitWgpu
        );
    }
}
