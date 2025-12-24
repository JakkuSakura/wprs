#[cfg(feature = "wayland")]
pub mod wayland;

pub mod mock;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(target_os = "macos"))]
pub mod macos {
    use crate::prelude::*;
    use crate::protocols::wprs::types::Capabilities;
    use crate::protocols::wprs::types::DisplayConfig;
    use crate::protocols::wprs::types::Event;
    use crate::server::backend::BackendObservation;
    use crate::server::backend::PollingBackend;

    #[derive(Debug, Clone, Copy, Default)]
    pub struct MacosWindowBackendConfig {
        pub dpi: Option<u32>,
        pub target_pid: Option<u32>,
    }

    #[derive(Clone, Debug, Default)]
    pub struct MacosTargetPid;

    impl MacosTargetPid {
        pub fn new(_initial: Option<u32>) -> Self {
            Self
        }

        pub fn get(&self) -> Option<u32> {
            None
        }

        pub fn set(&self, _pid: Option<u32>) {}
    }

    #[derive(Debug, Default)]
    pub struct MacosWindowBackend;

    impl MacosWindowBackend {
        pub fn new(_config: MacosWindowBackendConfig) -> Self {
            Self
        }

        pub fn target_pid_handle(&self) -> MacosTargetPid {
            MacosTargetPid
        }
    }

    impl PollingBackend for MacosWindowBackend {
        fn capabilities(&self) -> Capabilities {
            Capabilities { xwayland: false }
        }

        fn display_config(&self) -> DisplayConfig {
            DisplayConfig::default()
        }

        fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
            bail!("macOS window backend is only supported on macOS")
        }

        fn poll(&mut self) -> Result<Vec<BackendObservation>> {
            bail!("macOS window backend is only supported on macOS")
        }

        fn handle_client_event(&mut self, _event: Event) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct MacosFullscreenBackendConfig {
        pub dpi: Option<u32>,
    }

    #[derive(Debug, Default)]
    pub struct MacosFullscreenBackend;

    impl MacosFullscreenBackend {
        pub fn new(_config: MacosFullscreenBackendConfig) -> Self {
            Self
        }
    }

    impl PollingBackend for MacosFullscreenBackend {
        fn capabilities(&self) -> Capabilities {
            Capabilities { xwayland: false }
        }

        fn display_config(&self) -> DisplayConfig {
            DisplayConfig::default()
        }

        fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
            bail!("macOS fullscreen backend is only supported on macOS")
        }

        fn poll(&mut self) -> Result<Vec<BackendObservation>> {
            bail!("macOS fullscreen backend is only supported on macOS")
        }

        fn handle_client_event(&mut self, _event: Event) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(not(target_os = "windows"))]
pub mod windows {
    use crate::prelude::*;
    use crate::protocols::wprs::types::Capabilities;
    use crate::protocols::wprs::types::DisplayConfig;
    use crate::protocols::wprs::types::Event;
    use crate::server::backend::BackendObservation;
    use crate::server::backend::PollingBackend;

    #[derive(Clone, Debug, Default)]
    pub struct WindowsTargetPid;

    impl WindowsTargetPid {
        pub fn new() -> Self {
            Self
        }

        pub fn get(&self) -> Option<u32> {
            None
        }

        pub fn set(&self, _pid: Option<u32>) {}
    }

    #[derive(Debug, Default)]
    pub struct WindowsWindowBackend;

    impl WindowsWindowBackend {
        pub fn new() -> Self {
            Self
        }

        pub fn target_pid_handle(&self) -> WindowsTargetPid {
            WindowsTargetPid
        }
    }

    impl PollingBackend for WindowsWindowBackend {
        fn capabilities(&self) -> Capabilities {
            Capabilities { xwayland: false }
        }

        fn display_config(&self) -> DisplayConfig {
            DisplayConfig::default()
        }

        fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
            bail!(Error::Unsupported(
                "Windows window backend is only supported on Windows".to_string()
            ))
        }

        fn poll(&mut self) -> Result<Vec<BackendObservation>> {
            bail!(Error::Unsupported(
                "Windows window backend is only supported on Windows".to_string()
            ))
        }

        fn handle_client_event(&mut self, _event: Event) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    pub struct WindowsFullscreenBackend;

    impl WindowsFullscreenBackend {
        pub fn new() -> Self {
            Self
        }
    }

    impl PollingBackend for WindowsFullscreenBackend {
        fn capabilities(&self) -> Capabilities {
            Capabilities { xwayland: false }
        }

        fn display_config(&self) -> DisplayConfig {
            DisplayConfig::default()
        }

        fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
            bail!(Error::Unsupported(
                "Windows fullscreen backend is only supported on Windows".to_string()
            ))
        }

        fn poll(&mut self) -> Result<Vec<BackendObservation>> {
            bail!(Error::Unsupported(
                "Windows fullscreen backend is only supported on Windows".to_string()
            ))
        }

        fn handle_client_event(&mut self, _event: Event) -> Result<()> {
            Ok(())
        }
    }
}
