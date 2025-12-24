use crate::prelude::*;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::Event;
use crate::server::backend::BackendObservation;
use crate::server::backend::PollingBackend;

#[derive(Clone, Debug)]
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

#[derive(Debug)]
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

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        bail!(Error::Unsupported(
            "Windows fullscreen capture backend is only supported on Windows".to_string(),
        ))
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        bail!(Error::Unsupported(
            "Windows fullscreen capture backend is only supported on Windows".to_string(),
        ))
    }

    fn handle_client_event(&mut self, _event: Event) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct WindowsWindowBackend;

impl WindowsWindowBackend {
    pub fn new() -> Self {
        Self
    }

    pub fn target_pid_handle(&self) -> WindowsTargetPid {
        WindowsTargetPid::new()
    }
}

impl PollingBackend for WindowsWindowBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities { xwayland: false }
    }

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        bail!(Error::Unsupported(
            "Windows window capture backend is only supported on Windows".to_string(),
        ))
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        bail!(Error::Unsupported(
            "Windows window capture backend is only supported on Windows".to_string(),
        ))
    }

    fn handle_client_event(&mut self, _event: Event) -> Result<()> {
        Ok(())
    }
}
