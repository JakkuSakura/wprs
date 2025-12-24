use crate::prelude::*;
use crate::protocols::wprs::types::Capabilities;
use crate::protocols::wprs::types::ClientId;
use crate::protocols::wprs::types::DisplayConfig;
use crate::protocols::wprs::types::Event;
use crate::protocols::wprs::types::Request;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::wayland::BufferMetadata;
use crate::protocols::wprs::wayland::WlSurfaceId;
use crate::protocols::wprs::xdg_shell;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum BackendSurfaceRole {
    XdgToplevel {
        id: xdg_shell::XdgToplevelId,
        title: Option<String>,
        app_id: Option<String>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BackendSurfaceDescriptor {
    pub client: ClientId,
    pub id: WlSurfaceId,
    pub role: BackendSurfaceRole,
    pub buffer_scale: i32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BackendBgraFrame {
    pub metadata: BufferMetadata,
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum BackendObservation {
    /// A surface commit, optionally carrying a full BGRA frame to be sent.
    SurfaceCommit {
        surface: BackendSurfaceDescriptor,
        frame: Option<BackendBgraFrame>,
    },

    /// Destroy a previously-advertised surface.
    SurfaceDestroyed {
        client: ClientId,
        surface: WlSurfaceId,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TickMode {
    /// Backend is driven by a periodic timer.
    Polling,
    /// Backend runs its own event loop and does not require polling.
    EventDriven,
}

/// Polling-style backend.
///
/// These backends are driven by the shared `server::run_loop` and are
/// polled on a fixed interval.
pub trait PollingBackend {
    fn capabilities(&self) -> Capabilities;

    fn display_config(&self) -> DisplayConfig {
        DisplayConfig::default()
    }

    fn initial_snapshot(&mut self) -> Result<Vec<BackendObservation>>;

    fn poll(&mut self) -> Result<Vec<BackendObservation>>;

    fn handle_client_event(&mut self, event: Event) -> Result<()>;
}

/// Unified server backend interface.
///
/// - Polling/capture backends should implement `PollingBackend`.
/// - Event-driven backends (eg. a Wayland compositor) should implement `ServerBackend`
///   directly.
pub trait ServerBackend {
    fn tick_mode(&self) -> TickMode;

    fn run(
        self: Box<Self>,
        serializer: Serializer<Request, Event>,
        tick_interval: Option<std::time::Duration>,
    ) -> Result<()>;
}

impl<T: PollingBackend + 'static> ServerBackend for T {
    fn tick_mode(&self) -> TickMode {
        TickMode::Polling
    }

    fn run(
        self: Box<Self>,
        serializer: Serializer<Request, Event>,
        tick_interval: Option<std::time::Duration>,
    ) -> Result<()> {
        let tick_interval = tick_interval
            .ok_or_else(|| Error::Config("polling backend requires tick_interval".to_string()))
            .location(loc!())?;
        if serializer.is_inproc() {
            crate::server::inproc_run_loop::run(*self, serializer, tick_interval).location(loc!())
        } else {
            crate::server::run_loop::run(*self, serializer, tick_interval).location(loc!())
        }
    }
}
