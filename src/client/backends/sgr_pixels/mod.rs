use std::fmt::Write as _;
use std::io::Write;

use anyhow::ensure;

use calloop::channel::Event as CalloopChannelEvent;
use calloop::EventLoop as CalloopEventLoop;

use termwiz::terminal::ScreenSize;
use termwiz::terminal::Terminal as _;

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::serializer::RecvType;
use crate::protocols::wprs::types::Request;
use crate::protocols::wprs::serializer::Serializer;
use crate::utils::filtering;

const UPPER_HALF_BLOCK: &str = "▀";

pub struct SgrPixelsClientBackend {
    _config: ClientBackendConfig,
}

impl SgrPixelsClientBackend {
    pub fn new(config: ClientBackendConfig) -> Self {
        Self { _config: config }
    }

    pub fn new_for_wrun() -> Self {
        Self::new(ClientBackendConfig {
            title_prefix: String::new(),
            control_socket: std::env::temp_dir()
                .join(format!("wrun-sgr-pixels-{}-ctrl.sock", std::process::id())),
            keyboard_mode: crate::client::config::KeyboardMode::default(),
            xkb_keymap_file: None,
            ui_scale_factor: 1.0,
            min_output_scale_factor: None,
        })
    }
}

impl ClientBackend for SgrPixelsClientBackend {
    fn name(&self) -> &'static str {
        "sgr-pixels"
    }

    fn run(self: Box<Self>, serializer: Serializer<proto::types::Event, proto::types::Request>) -> Result<()> {
        run_event_loop(serializer).location(loc!())
    }
}

fn bgra_to_rgba_in_place(buf: &mut [u8]) {
    for pixel in buf.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

struct TerminalPresenter {
    terminal: Box<dyn termwiz::terminal::Terminal>,
    screen_size: ScreenSize,
    selected_surface: Option<proto::wayland::WlSurfaceId>,
}

impl TerminalPresenter {
    fn new() -> Result<Self> {
        let termwiz_caps = termwiz::caps::Capabilities::new_from_env().location(loc!())?;
        let mut terminal = Box::new(
            termwiz::terminal::new_terminal(termwiz_caps).location(loc!())?,
        );
        let size = terminal.get_screen_size().location(loc!())?;
        ensure!(
            size.cols > 0 && size.rows > 0,
            "terminal reported zero size"
        );

        Ok(Self {
            terminal,
            screen_size: ScreenSize {
                rows: size.rows,
                cols: size.cols,
                xpixel: size.xpixel,
                ypixel: size.ypixel,
            },
            selected_surface: None,
        })
    }

    fn refresh_size(&mut self) -> Result<()> {
        let size = self.terminal.get_screen_size().location(loc!())?;
        ensure!(
            size.cols > 0 && size.rows > 0,
            "terminal reported zero size"
        );
        self.screen_size = ScreenSize {
            rows: size.rows,
            cols: size.cols,
            xpixel: size.xpixel,
            ypixel: size.ypixel,
        };
        Ok(())
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

                if self.selected_surface.is_none()
                    && matches!(state.role.as_ref(), Some(Role::XdgToplevel(_)))
                {
                    self.selected_surface = Some(surface.surface);
                }
                if Some(surface.surface) != self.selected_surface {
                    return Ok(());
                }

                let Some(BufferAssignment::New(buf)) = state.buffer.take() else {
                    return Ok(());
                };
                let filtered = match buf.data {
                    BufferData::Uncompressed(data) => data.0.clone(),
                    _ => return Ok(()),
                };

                let mut rgba = vec![0u8; buf.metadata.len()];
                filtering::unfilter(filtered.as_ref(), &mut rgba);
                bgra_to_rgba_in_place(&mut rgba);

                self.refresh_size().location(loc!())?;
                self.render_rgba(
                    &rgba,
                    buf.metadata.width as usize,
                    buf.metadata.height as usize,
                )
                .location(loc!())?;
            },
            _ => {},
        }
        Ok(())
    }

    fn render_rgba(&self, rgba: &[u8], src_width: usize, src_height: usize) -> Result<()> {
        let out_cols = self.screen_size.cols as usize;
        let out_rows = self.screen_size.rows as usize;
        if out_cols == 0 || out_rows == 0 || src_width == 0 || src_height == 0 {
            return Ok(());
        }

        let out_pixel_height = out_rows.saturating_mul(2);
        if out_pixel_height == 0 {
            return Ok(());
        }

        let mut output = String::new();
        output.push_str("\x1b[2J\x1b[H\x1b[0m");

        let mut last_fg: Option<[u8; 3]> = None;
        let mut last_bg: Option<[u8; 3]> = None;

        for row in 0..out_rows {
            let y_top = row * 2;
            let y_bottom = y_top + 1;

            for col in 0..out_cols {
                let src_x = col * src_width / out_cols;
                let src_y_top = y_top * src_height / out_pixel_height;
                let src_y_bottom = y_bottom * src_height / out_pixel_height;

                let fg = read_pixel(rgba, src_width, src_x, src_y_top);
                let bg = read_pixel(rgba, src_width, src_x, src_y_bottom);

                if last_fg != Some(fg) {
                    write!(output, "\x1b[38;2;{};{};{}m", fg[0], fg[1], fg[2])?;
                    last_fg = Some(fg);
                }
                if last_bg != Some(bg) {
                    write!(output, "\x1b[48;2;{};{};{}m", bg[0], bg[1], bg[2])?;
                    last_bg = Some(bg);
                }

                output.push_str(UPPER_HALF_BLOCK);
            }

            output.push_str("\x1b[0m");
            if row + 1 < out_rows {
                output.push('\n');
            }
            last_fg = None;
            last_bg = None;
        }

        let mut out = std::io::stdout().lock();
        out.write_all(output.as_bytes()).location(loc!())?;
        out.flush().location(loc!())?;

        Ok(())
    }
}

fn read_pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 3] {
    let idx = (y * width + x) * 4;
    [rgba[idx], rgba[idx + 1], rgba[idx + 2]]
}

fn run_event_loop(mut serializer: Serializer<proto::types::Event, proto::types::Request>) -> Result<()> {
    let reader = serializer.reader().location(loc!())?;

    struct State {
        presenter: TerminalPresenter,
        client_sync: crate::protocols::wprs::client_sync::ClientSync,
    }

    let mut loop_: CalloopEventLoop<State> = CalloopEventLoop::try_new().location(loc!())?;
    let mut state = State {
        presenter: TerminalPresenter::new().location(loc!())?,
            client_sync: crate::protocols::wprs::client_sync::ClientSync::new(),
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

    let _serializer = serializer;
    loop_.run(None, &mut state, |_| {}).location(loc!())
}
