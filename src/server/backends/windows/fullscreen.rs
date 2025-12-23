use crate::prelude::*;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::Event;
use crate::server::backend::BackendObservation;
use crate::server::backend::PollingBackend;

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

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>> {
        bail!("Windows fullscreen capture backend is not implemented yet")
    }

    fn poll(&mut self) -> Result<Vec<BackendObservation>> {
        bail!("Windows fullscreen capture backend is not implemented yet")
    }

    fn handle_client_event(&mut self, _event: Event) -> Result<()> {
        Ok(())
    }
}
