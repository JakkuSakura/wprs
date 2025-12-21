use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::ensure;
use clap::Parser;
use clap::ValueEnum;
use serde_derive::Deserialize;
use serde_derive::Serialize;

#[cfg(all(unix, feature = "wayland"))]
use std::io::Write;

#[cfg(all(unix, feature = "wayland"))]
use calloop::EventLoop as CalloopEventLoop;
#[cfg(all(unix, feature = "wayland"))]
use calloop::channel::Event as CalloopChannelEvent;
#[cfg(all(unix, feature = "wayland"))]
use termwiz::render::RenderTty;
#[cfg(all(unix, feature = "wayland"))]
use termwiz::render::terminfo::TerminfoRenderer;
#[cfg(all(unix, feature = "wayland"))]
use termwiz::surface::Change;
#[cfg(all(unix, feature = "wayland"))]
use termwiz::terminal::ScreenSize;
#[cfg(all(unix, feature = "wayland"))]
use termwiz::terminal::Terminal as _;

use wprs::config;
#[cfg(all(unix, feature = "wayland"))]
use wprs::filtering;
use wprs::prelude::*;

#[cfg(any(all(unix, feature = "wayland"), target_os = "macos"))]
use wprs::protocols::wprs::Serializer;

#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs as proto;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::SendType;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::transport;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::wayland::{BufferAssignment, BufferData, Role, SurfaceRequestPayload};
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::{RecvType, Request};
use wprs::server::config::WprsdConfig;
#[cfg(all(unix, feature = "wayland"))]
use wprs::vec4u8::Vec4u8s;

#[cfg(feature = "wayland")]
use wprs::server::backends::wayland::backend::WaylandSmithayBackend;
#[cfg(feature = "wayland")]
use wprs::server::backends::wayland::backend::WaylandSmithayBackendConfig;
#[cfg(feature = "wayland")]
use wprs::server::config::XwaylandMode;
#[cfg(feature = "wayland")]
use wprs::server::runtime::backend::ServerBackend as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
enum CompositorMode {
    /// Use the compositor implied by the current environment/state.
    ///
    /// Behavior:
    /// - If a `wrun`-managed embedded instance is already running, reuse it.
    /// - Otherwise, try an external `wprsd` from the loaded config.
    /// - Otherwise, start an embedded `wprsd` and persist it for future runs.
    Inherited,
    /// Require an already-running external `wprsd` (from config).
    External,
    /// Ensure an embedded `wprsd` is running (start it if needed).
    Embedded,
}

impl Default for CompositorMode {
    fn default() -> Self {
        Self::Inherited
    }
}

#[derive(Parser, Debug)]
#[command(name = "wrun")]
#[command(trailing_var_arg = true)]
struct Args {
    /// Optional path to a `wprsd.ron` config file.
    #[arg(long, value_name = "PATH")]
    config_file: Option<PathBuf>,

    /// If set, runs an embedded `wprsd` (Wayland compositor backend) and renders the remote
    /// surfaces as inline images in the terminal.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    termwiz: bool,

    /// Socket path for the embedded server (primarily used on macOS).
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Which compositor instance to target.
    #[arg(long, value_name = "MODE", default_value = "inherited")]
    compositor_mode: CompositorMode,

    /// Force starting a new embedded compositor instance for this invocation.
    ///
    /// This mode does not `exec()` into the wrapped command so that `wrun` can
    /// terminate the temporary compositor when the command exits.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    standalone: bool,

    /// Disable setting `WAYLAND_DISPLAY` for the wrapped command.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    no_wayland: bool,

    /// Disable setting `DISPLAY` for the wrapped command.
    #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
    no_x11: bool,

    /// Command to run.
    #[arg(value_name = "CMD")]
    cmd: Vec<OsString>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WrunEmbeddedInstance {
    socket: PathBuf,
    wayland_display: String,
    xwayland_display: Option<u32>,
    wprsd_pid: Option<u32>,
}

#[cfg(unix)]
fn wrun_state_dir() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(whoami::username()));
    runtime.join("wprs").join("wrun")
}

#[cfg(unix)]
fn wrun_instance_state_file() -> PathBuf {
    wrun_state_dir().join("instance.ron")
}

#[cfg(unix)]
fn load_wrun_embedded_instance() -> Result<Option<WrunEmbeddedInstance>> {
    let path = wrun_instance_state_file();
    config::maybe_read_ron_file::<WrunEmbeddedInstance>(&path).location(loc!())
}

#[cfg(unix)]
fn save_wrun_embedded_instance(state: &WrunEmbeddedInstance) -> Result<()> {
    let path = wrun_instance_state_file();
    std::fs::create_dir_all(path.parent().location(loc!())?).location(loc!())?;
    let s = ron::ser::to_string_pretty(state, ron::ser::PrettyConfig::default()).location(loc!())?;
    std::fs::write(&path, s).location(loc!())?;
    Ok(())
}

#[cfg(unix)]
fn unix_socket_is_listening(path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

fn apply_linux_env_values(
    cmd: &mut Command,
    wayland_display: &str,
    xwayland_display: Option<u32>,
    no_wayland: bool,
    no_x11: bool,
) {
    #[cfg(target_os = "linux")]
    {
        if !no_wayland {
            cmd.env("WAYLAND_DISPLAY", wayland_display);
        }
        if !no_x11 {
            if let Some(display) = xwayland_display {
                cmd.env("DISPLAY", format!(":{display}"));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cmd, wayland_display, xwayland_display, no_wayland, no_x11);
    }
}

fn load_wprsd_config(config_file: Option<PathBuf>) -> Result<WprsdConfig> {
    let config_file = config_file.unwrap_or_else(|| config::default_config_file("wprsd"));
    let mut cfg = WprsdConfig::default();
    if let Some(from_file) =
        config::maybe_read_ron_file::<WprsdConfig>(&config_file).location(loc!())?
    {
        cfg = from_file;
    }
    Ok(cfg)
}

#[cfg(not(unix))]
fn apply_linux_env(cmd: &mut Command, cfg: &WprsdConfig, no_wayland: bool, no_x11: bool) {
    apply_linux_env_values(
        cmd,
        &cfg.wayland_display,
        cfg.xwayland_display,
        no_wayland,
        no_x11,
    );
}

#[cfg(target_os = "macos")]
fn default_wrun_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(whoami::username()));
    dir.join("wrun.sock")
}

#[cfg(not(unix))]
fn run_wrapped_command(
    cfg: &WprsdConfig,
    no_wayland: bool,
    no_x11: bool,
    cmd: &[OsString],
) -> Result<()> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command; try: wrun -- <cmd> [args...]"))
        .location(loc!())?;

    let mut c = Command::new(program);
    c.args(args);

    #[cfg(target_os = "linux")]
    apply_linux_env(&mut c, cfg, no_wayland, no_x11);

    // Replace the current process so signals/exit code match the wrapped app.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = c.exec();
        Err(anyhow!("exec failed: {err}"))
    }

    #[cfg(not(unix))]
    {
        let status = c.status().location(loc!())?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(unix)]
fn run_wrapped_command_with_env(
    wayland_display: &str,
    xwayland_display: Option<u32>,
    no_wayland: bool,
    no_x11: bool,
    cmd: &[OsString],
) -> Result<()> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command; try: wrun -- <cmd> [args...]"))
        .location(loc!())?;

    let mut c = Command::new(program);
    c.args(args);

    apply_linux_env_values(
        &mut c,
        wayland_display,
        xwayland_display,
        no_wayland,
        no_x11,
    );

    use std::os::unix::process::CommandExt as _;
    let err = c.exec();
    Err(anyhow!("exec failed: {err}"))
}

#[cfg(unix)]
fn run_wrapped_command_standalone(
    wayland_display: &str,
    xwayland_display: Option<u32>,
    no_wayland: bool,
    no_x11: bool,
    cmd: &[OsString],
) -> Result<std::process::ExitStatus> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command; try: wrun -- <cmd> [args...]"))
        .location(loc!())?;

    let mut c = Command::new(program);
    c.args(args);
    apply_linux_env_values(
        &mut c,
        wayland_display,
        xwayland_display,
        no_wayland,
        no_x11,
    );
    c.status().location(loc!())
}

#[cfg(unix)]
fn find_wprsd_exe() -> PathBuf {
    let exe_name = if cfg!(windows) { "wprsd.exe" } else { "wprsd" };
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(exe_name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("wprsd"))
}

#[cfg(unix)]
fn default_embedded_instance() -> WrunEmbeddedInstance {
    let dir = wrun_state_dir();
    WrunEmbeddedInstance {
        socket: dir.join("wprsd.sock"),
        wayland_display: "wprs-wrun".to_string(),
        xwayland_display: Some(40_100),
        wprsd_pid: None,
    }
}

#[cfg(unix)]
fn wait_for_wprsd_socket(socket: &std::path::Path) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        if unix_socket_is_listening(socket) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    bail!("wprsd did not become ready in time (socket={socket:?})")
}

#[cfg(unix)]
fn spawn_embedded_wprsd(
    state: &WrunEmbeddedInstance,
    log_file: &std::path::Path,
) -> Result<std::process::Child> {
    std::fs::create_dir_all(wrun_state_dir()).location(loc!())?;
    let wprsd = find_wprsd_exe();

    info!(
        "wrun: starting embedded wprsd: {wprsd:?} socket={sock:?} wayland_display={wl:?}",
        sock = state.socket,
        wl = state.wayland_display
    );

    let mut cmd = Command::new(wprsd);
    cmd.arg("--backend")
        .arg("wayland")
        .arg("--socket")
        .arg(&state.socket)
        .arg("--wayland-display")
        .arg(&state.wayland_display)
        .arg("--stderr-log-level")
        .arg("warn")
        .arg("--log-file")
        .arg(&log_file)
        .arg("--file-log-level")
        .arg("info");
    if let Some(x11) = state.xwayland_display {
        cmd.arg("--xwayland-display").arg(x11.to_string());
    }

    cmd.spawn().location(loc!())
}

#[cfg(unix)]
fn start_embedded_wprsd_persistent(state: &mut WrunEmbeddedInstance) -> Result<()> {
    std::fs::create_dir_all(wrun_state_dir()).location(loc!())?;
    let log_file = wrun_state_dir().join("wprsd.log");
    let child = spawn_embedded_wprsd(state, &log_file).location(loc!())?;
    state.wprsd_pid = Some(child.id());
    drop(child);
    wait_for_wprsd_socket(&state.socket).location(loc!())?;
    info!("wrun: embedded wprsd is ready");
    Ok(())
}

#[cfg(unix)]
fn resolve_compositor(mode: CompositorMode, cfg: &WprsdConfig) -> Result<(String, Option<u32>)> {
    match mode {
        CompositorMode::External => {
            ensure!(
                unix_socket_is_listening(&cfg.socket),
                "external mode requested, but wprsd is not listening on socket={:?}",
                cfg.socket
            );
            Ok((cfg.wayland_display.clone(), cfg.xwayland_display))
        }
        CompositorMode::Embedded => {
            let mut state = load_wrun_embedded_instance()
                .location(loc!())?
                .unwrap_or_else(default_embedded_instance);
            if !unix_socket_is_listening(&state.socket) {
                start_embedded_wprsd_persistent(&mut state).location(loc!())?;
                save_wrun_embedded_instance(&state).location(loc!())?;
            } else {
                info!(
                    "wrun: reusing embedded wprsd: socket={:?} wayland_display={:?}",
                    state.socket, state.wayland_display
                );
            }
            Ok((state.wayland_display, state.xwayland_display))
        }
        CompositorMode::Inherited => {
            if let Some(state) = load_wrun_embedded_instance().location(loc!())? {
                if unix_socket_is_listening(&state.socket) {
                    info!(
                        "wrun: inherited embedded wprsd: socket={:?} wayland_display={:?}",
                        state.socket, state.wayland_display
                    );
                    return Ok((state.wayland_display, state.xwayland_display));
                }
            }

            if unix_socket_is_listening(&cfg.socket) {
                info!(
                    "wrun: using external wprsd from config: socket={:?} wayland_display={:?}",
                    cfg.socket, cfg.wayland_display
                );
                return Ok((cfg.wayland_display.clone(), cfg.xwayland_display));
            }

            let mut state = default_embedded_instance();
            start_embedded_wprsd_persistent(&mut state).location(loc!())?;
            save_wrun_embedded_instance(&state).location(loc!())?;
            Ok((state.wayland_display, state.xwayland_display))
        }
    }
}

#[cfg(all(unix, feature = "wayland"))]
struct StdoutRenderTty<'a> {
    out: &'a mut dyn Write,
    size: ScreenSize,
}

#[cfg(all(unix, feature = "wayland"))]
impl Write for StdoutRenderTty<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.out.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(all(unix, feature = "wayland"))]
impl RenderTty for StdoutRenderTty<'_> {
    fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
        Ok((self.size.cols, self.size.rows))
    }
}

#[cfg(all(unix, feature = "wayland"))]
fn bgra_to_rgba_in_place(buf: &mut [u8]) {
    for p in buf.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
}

#[cfg(all(unix, feature = "wayland"))]
struct TerminalPresenter {
    renderer: TerminfoRenderer,
    screen_size: ScreenSize,
    buffer_cache: Option<Vec4u8s>,
    selected_surface: Option<proto::wayland::WlSurfaceId>,
}

#[cfg(all(unix, feature = "wayland"))]
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
            buffer_cache: None,
            selected_surface: None,
        })
    }

    fn handle_message(&mut self, msg: RecvType<Request>) -> Result<()> {
        match msg {
            RecvType::RawBuffer(buf) => {
                self.buffer_cache = Some(Vec4u8s::from(buf));
            },
            RecvType::Object(Request::Surface(surface)) => {
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

                let Some(BufferAssignment::New(mut buf)) = state.buffer.take() else {
                    return Ok(());
                };
                if buf.data.is_external() {
                    if let Some(cache) = self.buffer_cache.take() {
                        buf.data =
                            BufferData::Uncompressed(proto::wayland::UncompressedBufferData(cache));
                    }
                }
                let filtered = match buf.data {
                    BufferData::Uncompressed(data) => data.0,
                    _ => return Ok(()),
                };

                let mut bgra = vec![0u8; buf.metadata.len()];
                filtering::unfilter(&filtered, &mut bgra);
                bgra_to_rgba_in_place(&mut bgra);

                let png =
                    encode_png_rgba(&bgra, buf.metadata.width as u32, buf.metadata.height as u32)
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

#[cfg(all(unix, feature = "wayland"))]
fn render_termwiz(mut serializer: Serializer<proto::Event, proto::Request>) -> Result<()> {
    let reader = serializer.reader().location(loc!())?;

    struct State {
        presenter: TerminalPresenter,
    }

    let mut loop_: CalloopEventLoop<State> = CalloopEventLoop::try_new().location(loc!())?;
    let mut state = State {
        presenter: TerminalPresenter::new().location(loc!())?,
    };
    loop_
        .handle()
        .insert_source(reader, move |event, _metadata, state| {
            if let CalloopChannelEvent::Msg(msg) = event {
                state.presenter.handle_message(msg).log_and_ignore(loc!());
            }
        })
        .map_err(|e| anyhow!("insert_source(serializer reader) failed: {e:?}"))?;
    loop_.run(None, &mut state, |_| {}).location(loc!())
}

#[cfg(all(unix, feature = "wayland"))]
fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use png::{BitDepth, ColorType, Encoder};

    let mut buf = Vec::new();
    {
        let mut encoder = Encoder::new(&mut buf, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().location(loc!())?;
        writer.write_image_data(rgba).location(loc!())?;
        writer.finish().location(loc!())?;
    }
    Ok(buf)
}

#[cfg(all(unix, feature = "wayland"))]
fn run_termwiz_embedded(cmd: &[OsString]) -> Result<()> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command; try: wrun --termwiz -- <cmd> [args...]"))
        .location(loc!())?;

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    let wayland_display = format!("wrun-{}", std::process::id());
    let socket_path = runtime_dir.join(&wayland_display);

    info!(
        "wrun(termwiz) starting embedded compositor: WAYLAND_DISPLAY={wayland_display:?} socket={socket_path:?}"
    );

    let internal_dir = std::env::temp_dir().join(format!(
        "wrun-internal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&internal_dir).location(loc!())?;
    let internal_socket = internal_dir.join("wprs.sock");

    // Embedded server.
    {
        let server_socket = internal_socket.clone();
        let server_wayland_display = wayland_display.clone();
        std::thread::spawn(move || {
            let serializer: Serializer<proto::Request, proto::Event> =
                warn_and_return!(Serializer::new_server(&server_socket));
            let backend = WaylandSmithayBackend::new(WaylandSmithayBackendConfig {
                wayland_display: server_wayland_display,
                framerate: 60,
                enable_xwayland: false,
                xwayland_mode: XwaylandMode::External,
                xwayland_display: None,
                xwayland_xdg_shell_path: "xwayland-xdg-shell".to_string(),
                xwayland_xdg_shell_wayland_debug: false,
                xwayland_xdg_shell_args: Vec::new(),
                kde_server_side_decorations: false,
            });

            if let Err(err) = Box::new(backend).run(serializer, None) {
                warn!("embedded compositor terminated: {err:?}");
            }
        });
    }

    // Spawn the wrapped app as a Wayland client of the embedded compositor.
    let mut child = Command::new(program);
    child.args(args).env("WAYLAND_DISPLAY", &wayland_display);
    let mut child = child.spawn().location(loc!())?;

    // Embedded terminal client.
    let serializer_options = proto::SerializerClientOptions {
        auto_reconnect: false,
        on_connect: vec![SendType::Object(proto::Event::WprsClientConnect)],
    };
    let serializer: Serializer<proto::Event, proto::Request> =
        Serializer::new_client_with_options(&internal_socket, serializer_options)
            .location(loc!())?;

    // Tell the server we do NOT support patches; use full frames.
    serializer
        .writer()
        .send(SendType::Object(proto::Event::Transport(
            transport::TransportEvent::ClientHello(transport::ClientHello {
                supported_codecs: vec![
                    transport::TransportCodec::ShardedZstd { level: 1 },
                    transport::TransportCodec::ShardedLz4,
                    transport::TransportCodec::ShardedRaw,
                ],
                supports_buffer_patches: false,
                cpu: transport::CpuFeatures::default(),
                gpu: transport::GpuFeatures::default(),
                preferences: transport::TransportPreferences {
                    latency_weight: 10,
                    bandwidth_weight: 30,
                    cpu_weight: 30,
                    clarity_weight: 30,
                    ..Default::default()
                },
            }),
        )));

    let render_thread = std::thread::spawn(move || render_termwiz(serializer));

    let status = child.wait().location(loc!())?;
    warn!("wrapped app exited: {status}");
    drop(render_thread);
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.cmd.is_empty(),
        "missing command; try: wrun -- <cmd> [args...]"
    );

    if args.termwiz {
        #[cfg(all(unix, feature = "wayland"))]
        return run_termwiz_embedded(&args.cmd).location(loc!());

        #[cfg(not(all(unix, feature = "wayland")))]
        {
            bail!("--termwiz requires building with `--features wayland` on a Unix platform")
        }
    }
    #[cfg(target_os = "macos")]
    {
        return run_macos_forward(args.socket, &args.cmd).location(loc!());
    }

    let cfg = load_wprsd_config(args.config_file).location(loc!())?;

    #[cfg(unix)]
    {
        if args.standalone {
            ensure!(
                args.compositor_mode != CompositorMode::External,
                "--standalone is only supported with embedded/inherited modes"
            );

            let unique = format!(
                "wprs-wrun-standalone-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let dir = wrun_state_dir().join(unique);
            std::fs::create_dir_all(&dir).location(loc!())?;
            let mut state = WrunEmbeddedInstance {
                socket: dir.join("wprsd.sock"),
                wayland_display: format!("wprs-wrun-standalone-{}", std::process::id()),
                xwayland_display: Some(40_200 + (std::process::id() as u32 % 1000)),
                wprsd_pid: None,
            };
            let log_file = dir.join("wprsd.log");
            let mut wprsd = spawn_embedded_wprsd(&state, &log_file).location(loc!())?;
            state.wprsd_pid = Some(wprsd.id());
            wait_for_wprsd_socket(&state.socket).location(loc!())?;

            let status = run_wrapped_command_standalone(
                &state.wayland_display,
                state.xwayland_display,
                args.no_wayland,
                args.no_x11,
                &args.cmd,
            )
            .location(loc!())?;

            let _ = wprsd.kill();
            let _ = wprsd.wait();
            std::process::exit(status.code().unwrap_or(1));
        }

        let (wayland_display, xwayland_display) =
            resolve_compositor(args.compositor_mode, &cfg).location(loc!())?;
        return run_wrapped_command_with_env(
            &wayland_display,
            xwayland_display,
            args.no_wayland,
            args.no_x11,
            &args.cmd,
        )
        .location(loc!());
    }

    #[cfg(not(unix))]
    {
        run_wrapped_command(&cfg, args.no_wayland, args.no_x11, &args.cmd).location(loc!())
    }
}

#[cfg(target_os = "macos")]
fn run_macos_forward(socket: Option<PathBuf>, cmd: &[OsString]) -> Result<()> {
    use std::time::Duration;
    use wprs::protocols::wprs as proto;
    use wprs::server::backends::macos::MacosWindowBackend;
    use wprs::server::backends::macos::MacosWindowBackendConfig;
    use wprs::server::runtime::run_loop;

    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| anyhow!("missing command"))
        .location(loc!())?;

    let mut child = Command::new(program).args(args).spawn().location(loc!())?;
    let pid = child.id();
    info!("wrun(macos): spawned pid={pid}");

    let sock = socket.unwrap_or_else(default_wrun_socket_path);
    std::fs::create_dir_all(sock.parent().location(loc!())?).location(loc!())?;
    let serializer: Serializer<proto::Request, proto::Event> =
        Serializer::new_server(&sock).location(loc!())?;
    println!("unix://{}", sock.display());

    let backend = MacosWindowBackend::new(MacosWindowBackendConfig {
        dpi: None,
        target_pid: Some(pid),
    });
    // Polling tick: 30 FPS.
    std::thread::spawn(move || {
        run_loop::run(backend, serializer, Duration::from_secs_f64(1.0 / 30.0))
            .log_and_ignore(loc!());
    });

    let status = child.wait().location(loc!())?;
    info!("wrun(macos): wrapped app exited: {status}");
    Ok(())
}
