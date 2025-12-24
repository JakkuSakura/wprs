use calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::ConnectError;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::client::backend::ClientContext;
use crate::client::backends::wayland::ClientOptions;
use crate::client::backends::wayland::WprsClientState;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::serializer::Serializer;

#[derive(Debug)]
pub struct WaylandClientBackend {
    config: ClientBackendConfig,
    conn: Connection,
}

impl WaylandClientBackend {
    pub fn new(config: ClientBackendConfig, conn: Connection) -> Self {
        Self { config, conn }
    }

    pub fn connect_to_env(config: ClientBackendConfig) -> Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| Error::context("connect_to_env failed", e))
            .location(loc!())?;
        Ok(Self::new(config, conn))
    }

    pub fn try_connect_to_env(config: ClientBackendConfig) -> Result<Option<Self>> {
        match Connection::connect_to_env() {
            Ok(conn) => Ok(Some(Self::new(config, conn))),
            Err(ConnectError::NoCompositor) => Ok(None),
            Err(e) => Err(Error::context("connect_to_env failed", e)),
        }
    }
}

impl ClientBackend for WaylandClientBackend {
    fn name(&self) -> &'static str {
        "wayland"
    }

    fn run(self: Box<Self>, ctx: ClientContext) -> Result<()> {
        run_wayland(ctx, self.config, self.conn).location(loc!())
    }
}

fn run_wayland(ctx: ClientContext, config: ClientBackendConfig, conn: Connection) -> Result<()> {
    let (globals, event_queue) = registry_queue_init(&conn)?;

    info!(
        "wprsc(wayland): starting (title_prefix={:?} ui_scale_factor={})",
        config.title_prefix, config.ui_scale_factor
    );

    let options = ClientOptions {
        title_prefix: config.title_prefix,
    };

    let mut state = WprsClientState::new(
        event_queue.handle(),
        globals,
        conn.clone(),
        ctx.serializer,
        options,
        ctx.state,
        ctx.notify_rx,
    )
    .location(loc!())?;

    info!(
        "wprsc(wayland): globals: wp_viewporter={} wp_pointer_gestures={}",
        state.wp_viewporter.is_some(),
        state.wp_pointer_gestures.is_some()
    );

    let mut event_loop = EventLoop::try_new()?;
    let mut timer = calloop::timer::Timer::from_duration(std::time::Duration::from_millis(50));
    event_loop
        .handle()
        .insert_source(timer, |_, _, state: &mut WprsClientState| {
            state.apply_client_state_updates().log_and_ignore(loc!());
            calloop::timer::TimeoutAction::ToDuration(std::time::Duration::from_millis(50))
        })
        .map_err(|e| Error::Internal(format!("insert_source(refresh timer) failed: {e:?}")))?;

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| Error::Internal(format!("insert_source(wayland) failed: {e}")))
        .location(loc!())?;

    event_loop.run(None, &mut state, |_| {}).location(loc!())
}
