use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::ensure;
use clap::Parser;
use clap::ValueEnum;
use serde_derive::Deserialize;
use serde_derive::Serialize;

use wprs::config;
use wprs::prelude::*;
use wprs::server::config::WprsdConfig;

#[cfg(any(all(unix, feature = "wayland"), target_os = "macos"))]
use wprs::protocols::wprs::Serializer;

#[cfg(all(unix, feature = "wayland"))]
use wprs::client::ClientBackend as _;
#[cfg(all(unix, feature = "wayland"))]
use wprs::client::backends::termwiz_image::TermwizImageClientBackend;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs as proto;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::SendType;
#[cfg(all(unix, feature = "wayland"))]
use wprs::protocols::wprs::transport;

#[cfg(feature = "wayland")]
use wprs::server::backends::wayland::backend::WaylandSmithayBackend;
#[cfg(feature = "wayland")]
use wprs::server::backends::wayland::backend::WaylandSmithayBackendConfig;
#[cfg(feature = "wayland")]
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
    let s =
        ron::ser::to_string_pretty(state, ron::ser::PrettyConfig::default()).location(loc!())?;
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
        &cfg.wayland.display,
        cfg.wayland.xwayland.as_ref().and_then(|x| x.display),
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
            Ok((
                cfg.wayland.display.clone(),
                cfg.wayland.xwayland.as_ref().and_then(|x| x.display),
            ))
        },
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
        },
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
                    cfg.socket, cfg.wayland.display
                );
                return Ok((
                    cfg.wayland.display.clone(),
                    cfg.wayland.xwayland.as_ref().and_then(|x| x.display),
                ));
            }

            let mut state = default_embedded_instance();
            start_embedded_wprsd_persistent(&mut state).location(loc!())?;
            save_wrun_embedded_instance(&state).location(loc!())?;
            Ok((state.wayland_display, state.xwayland_display))
        },
    }
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
                xwayland: None,
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
                supported_codecs: {
                    let mut codecs = vec![
                        transport::TransportCodec::ShardedZstd { level: 1 },
                        transport::TransportCodec::ShardedLz4,
                        transport::TransportCodec::ShardedRaw,
                    ];
                    #[cfg(feature = "video-h264")]
                    codecs.insert(0, transport::TransportCodec::H264);
                    codecs
                },
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

    let render_thread = std::thread::spawn(move || {
        let backend = TermwizImageClientBackend::new_for_wrun();
        if let Err(err) = Box::new(backend).run(serializer) {
            warn!("termwiz-image backend terminated: {err:?}");
        }
    });

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
