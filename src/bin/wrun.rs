use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

use wprs::client::config::ClientBackend;
use wprs::config;
use wprs::config::SerializableLevel;
use wprs::launcher;
use wprs::prelude::*;
use wprs::server::config::WprsdConfig;
use wprs::utils;

#[derive(Parser, Debug)]
#[command(name = "wrun")]
#[command(trailing_var_arg = true)]
struct Args {
    /// Optional path to a `wprsd.ron` config file.
    #[arg(long, value_name = "PATH")]
    config_file: Option<PathBuf>,

    /// Which backend to use for presenting remote surfaces.
    ///
    /// If omitted, `wrun` will print the WPRS endpoint to stdout.
    #[arg(long = "backend", value_name = "BACKEND")]
    backend: Option<ClientBackend>,

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

fn main() -> Result<()> {
    let args = Args::parse();
    let config_file = args
        .config_file
        .clone()
        .unwrap_or_else(|| config::default_config_file("wprsd"));
    let wprsd_config_from_file =
        config::maybe_read_ron_file::<WprsdConfig>(&config_file).location(loc!())?;
    let config_from_file_missing = wprsd_config_from_file.is_none();
    let mut wprsd_config = wprsd_config_from_file.clone().unwrap_or_default();

    config::set_log_priv_data(wprsd_config.log_priv_data);
    utils::configure_tracing(
        wprsd_config.stderr_log_level.0,
        wprsd_config.log_file.clone(),
        wprsd_config.file_log_level.0,
    )
    .location(loc!())?;
    if config_from_file_missing {
        error!("config file does not exist at {config_file:?}");
    }
    utils::exit_on_thread_panic();

    let exit_code = launcher::run(launcher::RunConfig {
        wprsd_config_from_file,
        backend: args.backend,
        no_wayland: args.no_wayland,
        no_x11: args.no_x11,
        cmd: args.cmd,
    })
    .location(loc!())?;

    std::process::exit(exit_code)
}
