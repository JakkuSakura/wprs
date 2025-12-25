use std::io::Write;

use calloop::EventLoop as CalloopEventLoop;

use termwiz::caps::Capabilities;
use termwiz::input::InputEvent;
use termwiz::terminal::ScreenSize;
use termwiz::terminal::Terminal as _;

use rasteroid::InlineEncoder;
use rasteroid::term_misc::EnvIdentifiers;

use crate::client::backend::ClientBackend;
use crate::client::backend::ClientBackendConfig;
use crate::client::backend::ClientContext;
use crate::client::state::ClientState;
use crate::client::state::drain_client_updates;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::geometry::Point;
use crate::protocols::wprs::serializer::SendType;
use crate::protocols::wprs::wayland::PointerEvent;
use crate::protocols::wprs::wayland::PointerEventKind;

pub struct TerminalClientBackend {
    _config: ClientBackendConfig,
}

impl TerminalClientBackend {
    pub fn new(config: ClientBackendConfig) -> Self {
        Self { _config: config }
    }

    pub fn new_for_wrun() -> Self {
        Self::new(ClientBackendConfig {
            title_prefix: String::new(),
            control_socket: std::env::temp_dir()
                .join(format!("wrun-terminal-{}-ctrl.sock", std::process::id())),
            keyboard_mode: crate::client::config::KeyboardMode::default(),
            xkb_keymap_file: None,
            ui_scale_factor: 1.0,
            min_output_scale_factor: None,
            html_bind_addr: crate::client::config::default_html_bind_addr(),
        })
    }
}

impl ClientBackend for TerminalClientBackend {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn run(self: Box<Self>, ctx: ClientContext) -> Result<()> {
        run_event_loop(ctx).location(loc!())
    }
}

struct TerminalPresenter {
    terminal: Box<dyn termwiz::terminal::Terminal>,
    screen_size: ScreenSize,
    selected_surface: Option<proto::wayland::WlSurfaceId>,
    selected_size: Option<(u32, u32)>,
    encoder: InlineEncoder,
}

impl TerminalPresenter {
    fn new() -> Result<Self> {
        let termwiz_caps = Capabilities::new_from_env().location(loc!())?;
        let mut terminal = Box::new(termwiz::terminal::new_terminal(termwiz_caps).location(loc!())?);
        terminal.set_raw_mode().location(loc!())?;
        terminal.enter_alternate_screen().location(loc!())?;

        let size = terminal.get_screen_size().location(loc!())?;
        ensure!(
            size.cols > 0 && size.rows > 0,
            Error::InvalidArgument("terminal reported zero size".to_string()),
        );

        let mut env = EnvIdentifiers::new();
        let encoder = InlineEncoder::auto_detect(false, false, false, false, &mut env);

        Ok(Self {
            terminal,
            screen_size: ScreenSize {
                rows: size.rows,
                cols: size.cols,
                xpixel: size.xpixel,
                ypixel: size.ypixel,
            },
            selected_surface: None,
            selected_size: None,
            encoder,
        })
    }

    fn refresh_size(&mut self) -> Result<()> {
        let size = self.terminal.get_screen_size().location(loc!())?;
        ensure!(
            size.cols > 0 && size.rows > 0,
            Error::InvalidArgument("terminal reported zero size".to_string()),
        );
        self.screen_size = ScreenSize {
            rows: size.rows,
            cols: size.cols,
            xpixel: size.xpixel,
            ypixel: size.ypixel,
        };
        Ok(())
    }

    fn apply_state(&mut self, state: &ClientState) -> Result<()> {
        use proto::wayland::BitmapAssignment;
        use proto::wayland::Role;

        let surfaces = state.snapshot_surfaces();
        for surface_state in surfaces {
            if self.selected_surface.is_none()
                && matches!(surface_state.role.as_ref(), Some(Role::XdgToplevel(_)))
            {
                self.selected_surface = Some(surface_state.id);
            }
            if Some(surface_state.id) != self.selected_surface {
                continue;
            }

            let Some(BitmapAssignment::New(buf)) = surface_state.bitmap else {
                continue;
            };
            let metadata = buf.metadata;
            if buf.data.len() < metadata.len() {
                continue;
            }
            let rgba = bitmap_to_rgba(&buf, metadata.format).location(loc!())?;
            self.selected_size = Some((metadata.width as u32, metadata.height as u32));

            self.refresh_size().location(loc!())?;
            self.render_rgba(
                &rgba,
                buf.metadata.width as usize,
                buf.metadata.height as usize,
            )
            .location(loc!())?;
        }
        Ok(())
    }
    fn render_rgba(&mut self, rgba: &[u8], src_width: usize, src_height: usize) -> Result<()> {
        let out_cols = self.screen_size.cols;
        let out_rows = self.screen_size.rows;
        if out_cols == 0 || out_rows == 0 || src_width == 0 || src_height == 0 {
            return Ok(());
        }

        let png = encode_rgba_png(rgba, src_width as u32, src_height as u32).location(loc!())?;
        let mut out = std::io::stdout().lock();
        out.write_all(b"\x1b[2J\x1b[H").location(loc!())?;
        rasteroid::inline_an_image(&png, &mut out, None, None, &self.encoder)
            .map_err(|e| Error::Internal(format!("inline image failed: {e:?}")))?;
        out.flush().location(loc!())?;
        
        Ok(())
    }

    fn poll_input(&mut self) -> Result<Option<InputEvent>> {
        self.terminal.poll_input(Some(std::time::Duration::ZERO)).location(loc!())
    }

    fn pointer_event_for_mouse(&self, mouse: termwiz::input::MouseEvent) -> Option<PointerEvent> {
        let surface_id = self.selected_surface?;
        let (width, height) = self.selected_size?;
        let cols = self.screen_size.cols as f64;
        let rows = self.screen_size.rows as f64;
        if cols <= 0.0 || rows <= 0.0 || width == 0 || height == 0 {
            return None;
        }

        let x_cell = mouse.x.saturating_sub(1) as f64;
        let y_cell = mouse.y.saturating_sub(1) as f64;
        let x = (x_cell / cols) * width as f64;
        let y = (y_cell / rows) * height as f64;
        let max_x = width.saturating_sub(1) as f64;
        let max_y = height.saturating_sub(1) as f64;
        let x = x.clamp(0.0, max_x);
        let y = y.clamp(0.0, max_y);

        Some(PointerEvent {
            surface_id,
            position: Point { x, y },
            kind: PointerEventKind::Motion,
        })
    }
}

impl Drop for TerminalPresenter {
    fn drop(&mut self) {
        let _ = self.terminal.set_cooked_mode();
        let _ = self.terminal.exit_alternate_screen();
    }
}

fn encode_rgba_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let expected = width as usize * height as usize * 4;
    ensure!(
        rgba.len() == expected,
        Error::InvalidArgument("rgba buffer size mismatch".to_string()),
    );
    let mut data = Vec::new();
    let mut encoder = png::Encoder::new(&mut data, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    {
        let mut writer = encoder.write_header().location(loc!())?;
        writer.write_image_data(rgba).location(loc!())?;
    }
    Ok(data)
}

fn bitmap_to_rgba(
    bitmap: &proto::wayland::Bitmap,
    format: proto::wayland::BufferFormat,
) -> Result<Vec<u8>> {
    let meta = bitmap.metadata;
    let width = meta.width as usize;
    let height = meta.height as usize;
    let stride = meta.stride as usize;
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    if stride < width * 4 {
        bail!(Error::InvalidArgument("bitmap stride too small".to_string()));
    }
    let bytes = bitmap.bytes();
    if bytes.len() < height * stride {
        bail!(Error::InvalidArgument("bitmap data too short".to_string()));
    }

    let mut out = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let row_data = &bytes[row * stride..row * stride + width * 4];
        for px in row_data.chunks_exact(4) {
            let b = px[0];
            let g = px[1];
            let r = px[2];
            let a = if matches!(format, proto::wayland::BufferFormat::Xrgb8888) {
                0xff
            } else {
                px[3]
            };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(out)
}

struct TerminalState {
    presenter: TerminalPresenter,
    client_state: std::sync::Arc<ClientState>,
    notify_rx: std::sync::mpsc::Receiver<()>,
    serializer: proto::serializer::Serializer<proto::types::Event, proto::types::Request>,
}

fn run_event_loop(ctx: ClientContext) -> Result<()> {
    let mut loop_: CalloopEventLoop<TerminalState> = CalloopEventLoop::try_new().location(loc!())?;
    let mut state = TerminalState {
        presenter: TerminalPresenter::new().location(loc!())?,
        client_state: ctx.state,
        notify_rx: ctx.notify_rx,
        serializer: ctx.serializer,
    };

    let timer = calloop::timer::Timer::from_duration(std::time::Duration::from_millis(200));
    loop_
        .handle()
        .insert_source(timer, move |_, _, state| {
            drain_client_updates(&state.notify_rx, &state.client_state)
                .log_and_ignore(loc!());
            state
                .presenter
                .apply_state(&state.client_state)
                .log_and_ignore(loc!());
            poll_terminal_input(state).log_and_ignore(loc!());
            calloop::timer::TimeoutAction::ToDuration(std::time::Duration::from_millis(200))
        })
        .map_err(|e| Error::Internal(format!("insert_source(refresh timer) failed: {e:?}")))?;

    loop_.run(None, &mut state, |_| {}).location(loc!())
}

fn poll_terminal_input(state: &mut TerminalState) -> Result<()> {
    loop {
        let event = match state.presenter.poll_input().location(loc!())? {
            Some(ev) => ev,
            None => break,
        };

        match event {
            InputEvent::Mouse(mouse) => {
                if let Some(pointer) = state.presenter.pointer_event_for_mouse(mouse) {
                    state
                        .serializer
                        .writer()
                        .send(SendType::Object(proto::types::Event::PointerFrame(vec![
                            pointer,
                        ])));
                }
            }
            InputEvent::PixelMouse(mouse) => {
                let surface_id = match state.presenter.selected_surface {
                    Some(surface) => surface,
                    None => continue,
                };
                let (width, height) = match state.presenter.selected_size {
                    Some(size) => size,
                    None => continue,
                };
                let xpixel = state.presenter.screen_size.xpixel as f64;
                let ypixel = state.presenter.screen_size.ypixel as f64;
                if xpixel <= 0.0 || ypixel <= 0.0 {
                    continue;
                }
                let x = (mouse.x_pixels as f64 / xpixel) * width as f64;
                let y = (mouse.y_pixels as f64 / ypixel) * height as f64;
                let pointer = PointerEvent {
                    surface_id,
                    position: Point { x, y },
                    kind: PointerEventKind::Motion,
                };
                state
                    .serializer
                    .writer()
                    .send(SendType::Object(proto::types::Event::PointerFrame(vec![
                        pointer,
                    ])));
            }
            InputEvent::Resized { .. } => {
                state.presenter.refresh_size().log_and_ignore(loc!());
            }
            _ => {}
        }
    }
    Ok(())
}
