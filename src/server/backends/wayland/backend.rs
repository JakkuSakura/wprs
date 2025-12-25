use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::Interest;
use smithay::reexports::calloop::Mode;
use smithay::reexports::calloop::PostAction;
use smithay::reexports::calloop::channel::Event as CalloopEvent;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;

use super::WprsServerState;
use crate::prelude::*;
use crate::protocols::wprs::serializer::Serializer;
use crate::protocols::wprs::types::Event;
use crate::protocols::wprs::types::Request;
use crate::server::backends::wayland::smithay_handlers::ClientState;

#[derive(Debug, Clone)]
pub struct WaylandSmithayBackendConfig {
    pub wayland_display: String,
    pub framerate: u32,
    pub xwayland: Option<crate::server::config::XwaylandConfig>,
    pub kde_server_side_decorations: bool,
}

#[derive(Debug)]
pub struct WaylandSmithayBackend {
    config: WaylandSmithayBackendConfig,
}

impl WaylandSmithayBackend {
    pub fn new(config: WaylandSmithayBackendConfig) -> Self {
        Self { config }
    }
}

fn init_wayland_listener(
    wayland_display: &str,
    mut display: Display<WprsServerState>,
    state: &mut WprsServerState,
    event_loop: &EventLoop<WprsServerState>,
) -> Result<()> {
    let listening_socket = ListeningSocketSource::with_name(wayland_display).location(loc!())?;
    let writer = state.serializer.writer().into_inner();
    let mut dh = display.handle();

    event_loop
        .handle()
        .insert_source(listening_socket, move |stream, _, _| {
            dh.insert_client(stream, Arc::new(ClientState::new(writer.clone())))
                .unwrap();
        })
        .location(loc!())?;

    event_loop
        .handle()
        .insert_source(
            Generic::new(
                display.backend().poll_fd().try_clone_to_owned().unwrap(),
                Interest::READ,
                Mode::Level,
            ),
            move |_, _, state| {
                display.dispatch_clients(state).unwrap();
                Ok(PostAction::Continue)
            },
        )
        .location(loc!())?;

    Ok(())
}

fn ensure_runtime_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if path.as_os_str().is_empty() {
            bail!(Error::Config(
                "XDG_RUNTIME_DIR is set but empty".to_string()
            ))
        }
        return Ok(path);
    }

    let runtime_dir = env::temp_dir().join(format!("wprs-runtime-{}", std::process::id()));
    fs::create_dir_all(&runtime_dir).location(loc!())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).location(loc!())?;
    }
    // safety: it's during startup phase, single thread
    unsafe {
        env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    }
    info!("XDG_RUNTIME_DIR not set; using temporary directory {runtime_dir:?}");
    Ok(runtime_dir)
}

impl crate::server::backend::ServerBackend for WaylandSmithayBackend {
    fn tick_mode(&self) -> crate::server::backend::TickMode {
        crate::server::backend::TickMode::EventDriven
    }

    fn run(
        self: Box<Self>,
        mut serializer: Serializer<Request, Event>,
        _tick_interval: Option<Duration>,
    ) -> Result<()> {
        let config = self.config;

        ensure_runtime_dir().location(loc!())?;

        let reader = serializer
            .reader()
            .ok_or_else(|| Error::Internal("serializer reader already taken".to_string()))
            .location(loc!())?;

        let mut event_loop = EventLoop::try_new().location(loc!())?;
        let display: Display<WprsServerState> = Display::new().location(loc!())?;

        let frame_interval = Duration::from_secs_f64(1.0 / (config.framerate.max(1) as f64));
        let dh = display.handle();

        let mut state = WprsServerState::new(
            &dh,
            event_loop.handle(),
            serializer,
            config.xwayland.is_some(),
            frame_interval,
            config.kde_server_side_decorations,
        );

        init_wayland_listener(&config.wayland_display, display, &mut state, &event_loop)
            .location(loc!())?;

        if let Some(xwayland_cfg) = config.xwayland {
            #[cfg(feature = "xwayland")]
            {
                state
                    .start_xwayland(xwayland_cfg.wayland_debug, xwayland_cfg.display)
                    .location(loc!())?;
            }

            #[cfg(not(feature = "xwayland"))]
            {
                let _ = xwayland_cfg;
                let _ = &mut state;
                bail!(Error::Unsupported(
                    "wayland.xwayland is set but wprsd was built without `--features xwayland`"
                        .to_string(),
                ));
            }
        }

        let _keyboard = state
            .seat
            .add_keyboard(Default::default(), 200, 200)
            .location(loc!())?;
        let _pointer = state.seat.add_pointer();

        event_loop
            .handle()
            .insert_source(reader, |event, _metadata, state| {
                if let CalloopEvent::Msg(msg) = event {
                    state.handle_event(msg);
                }
            })
            .map_err(|e| {
                Error::Internal(format!("insert_source(serializer reader) failed: {e:?}"))
            })?;

        event_loop
            .run(None, &mut state, move |state| {
                state.dh.flush_clients().unwrap();
            })
            .location(loc!())?;

        Ok(())
    }
}
