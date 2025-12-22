use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

use wprs::client::config::ClientBackend;
use wprs::launcher;
use wprs::prelude::*;

#[derive(Parser, Debug)]
#[command(name = "wrun")]
#[command(trailing_var_arg = true)]
struct Args {
    /// Optional path to a `wprsd.ron` config file.
    #[arg(long, value_name = "PATH")]
    config_file: Option<PathBuf>,

    /// Which client backend to use for presenting remote surfaces.
    ///
    /// If omitted, `wrun` will print the WPRS endpoint to stdout.
    #[arg(long, value_name = "BACKEND")]
    client_backend: Option<ClientBackend>,

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
    let exit_code = launcher::run(launcher::RunConfig {
        wprsd_config_file: args.config_file,
        client_backend: args.client_backend,
        no_wayland: args.no_wayland,
        no_x11: args.no_x11,
        cmd: args.cmd,
    })
    .location(loc!())?;

    std::process::exit(exit_code)
}
