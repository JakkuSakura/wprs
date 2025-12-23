use std::io::Write;

use anyhow::ensure;

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::utils::filtering;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::RecvType;
use crate::protocols::wprs::Request;
use crate::protocols::wprs::Serializer;

use calloop::channel::Event as CalloopChannelEvent;
use calloop::EventLoop as CalloopEventLoop;

use termwiz::render::RenderTty;
use termwiz::render::terminfo::TerminfoRenderer;
use termwiz::surface::Change;
use termwiz::terminal::ScreenSize;
use termwiz::terminal::Terminal as _;

pub struct TermwizImageClientBackend {
    _config: ClientBackendConfig,
}

impl TermwizImageClientBackend {
    pub fn new(config: ClientBackendConfig) -> Self {
        Self { _config: config }
    }

    pub fn new_for_wrun() -> Self {
        Self::new(ClientBackendConfig {
            title_prefix: String::new(),
            control_socket: std::env::temp_dir()
                .join(format!("wrun-termwiz-{}-ctrl.sock", std::process::id())),
            keyboard_mode: crate::client::config::KeyboardMode::default(),
            xkb_keymap_file: None,
            ui_scale_factor: 1.0,
            min_output_scale_factor: None,
        })
    }
}

impl ClientBackend for TermwizImageClientBackend {
    fn name(&self) -> &'static str {
        "termwiz-image"
    }

    fn run(self: Box<Self>, serializer: Serializer<proto::Event, proto::Request>) -> Result<()> {
        run_event_loop(serializer).location(loc!())
    }
}

struct StdoutRenderTty<'a> {
    out: &'a mut dyn Write,
    size: ScreenSize,
}

impl Write for StdoutRenderTty<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

impl RenderTty for StdoutRenderTty<'_> {
    fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
        Ok((self.size.cols, self.size.rows))
    }
}

fn bgra_to_rgba_in_place(buf: &mut [u8]) {
    for pixel in buf.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

struct TerminalPresenter {
    renderer: TerminfoRenderer,
    screen_size: ScreenSize,
    selected_surface: Option<proto::wayland::WlSurfaceId>,
}

impl TerminalPresenter {
    fn new() -> Result<Self> {
        let termwiz_caps = termwiz::caps::Capabilities::new_from_env().location(loc!())?;
        let mut term = termwiz::terminal::new_terminal(termwiz_caps.clone()).location(loc!())?;
        let size = term.get_screen_size().location(loc!())?;
        ensure!(
            size.cols > 0 && size.rows > 0,
            "terminal reported zero size"
        );

        Ok(Self {
            renderer: TerminfoRenderer::new(termwiz_caps),
            screen_size: ScreenSize {
                rows: size.rows,
                cols: size.cols,
                xpixel: size.xpixel,
                ypixel: size.ypixel,
            },
            selected_surface: None,
        })
    }

    fn handle_message(&mut self, msg: RecvType<Request>) -> Result<()> {
        match msg {
            RecvType::Object(Request::Surface(surface)) => {
                use proto::wayland::BufferAssignment;
                use proto::wayland::BufferData;
                use proto::wayland::Role;
                use proto::wayland::SurfaceRequestPayload;

                let SurfaceRequestPayload::Commit(mut state) = surface.payload else {
                    return Ok(());
                };

                if self.selected_surface.is_none() {
                    if matches!(state.role.as_ref(), Some(Role::XdgToplevel(_))) {
                        self.selected_surface = Some(surface.surface);
                    }
                }
                if Some(surface.surface) != self.selected_surface {
                    return Ok(());
                }

                let Some(BufferAssignment::New(buf)) = state.buffer.take() else {
                    return Ok(());
                };
                let filtered = match buf.data {
                    BufferData::Uncompressed(data) => data.0,
                    _ => return Ok(()),
                };

                let mut bgra = vec![0u8; buf.metadata.len()];
                filtering::unfilter(&filtered, &mut bgra);
                bgra_to_rgba_in_place(&mut bgra);

                let png =
                    crate::protocols::image::png::encode_png_rgba(&bgra, buf.metadata.width as u32, buf.metadata.height as u32)
                        .location(loc!())?;

                let cols = self.screen_size.cols;
                let rows = self.screen_size.rows;
                let image = termwiz::surface::Image {
                    width: cols,
                    height: rows,
                    top_left: termwiz::image::TextureCoordinate::new_f32(0.0, 0.0),
                    bottom_right: termwiz::image::TextureCoordinate::new_f32(1.0, 1.0),
                    image: std::sync::Arc::new(termwiz::image::ImageData::with_data(
                        termwiz::image::ImageDataType::EncodedFile(png),
                    )),
                };

                let mut out = std::io::stdout().lock();
                let mut tty = StdoutRenderTty {
                    out: &mut out,
                    size: self.screen_size,
                };
                self.renderer
                    .render_to(
                        &[
                            Change::ClearScreen(Default::default()),
                            Change::Image(image),
                        ],
                        &mut tty,
                    )
                    .location(loc!())?;
                tty.flush().location(loc!())?;
            },
            _ => {},
        }
        Ok(())
    }
}

fn run_event_loop(mut serializer: Serializer<proto::Event, proto::Request>) -> Result<()> {
    let reader = serializer.reader().location(loc!())?;

    struct State {
        presenter: TerminalPresenter,
        client_sync: crate::protocols::wprs::core::client_sync::ClientSync,
    }

    let mut loop_: CalloopEventLoop<State> = CalloopEventLoop::try_new().location(loc!())?;
    let mut state = State {
        presenter: TerminalPresenter::new().location(loc!())?,
        client_sync: crate::protocols::wprs::core::client_sync::ClientSync::new(),
    };

    loop_
        .handle()
        .insert_source(reader, move |event, _metadata, state| {
            if let CalloopChannelEvent::Msg(msg) = event {
                match state.client_sync.handle_message(msg).location(loc!()) {
                    Ok(Some(msg)) => state.presenter.handle_message(msg).log_and_ignore(loc!()),
                    Ok(None) => {},
                    Err(err) => warn!("client_sync failed: {err:?}"),
                }
            }
        })
        .map_err(|e| anyhow!("insert_source(serializer reader) failed: {e:?}"))?;

    // Hold onto the serializer so its transport threads stay alive.
    let _serializer = serializer;
    loop_.run(None, &mut state, |_| {}).location(loc!())
}
