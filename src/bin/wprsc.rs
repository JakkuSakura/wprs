// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use clap::Parser;
use wprs::client::config::WprscArgs;
use wprs::client::run_wprsc;
use wprs::config;
use wprs::prelude::*;
use wprs::utils;

fn main() -> Result<()> {
    let args = WprscArgs::parse();
    let config = args.load_config().location(loc!())?;

    config::set_log_priv_data(config.log_priv_data);
    utils::configure_tracing(
        config.stderr_log_level.0,
        config.log_file.clone(),
        config.file_log_level.0,
    )
    .location(loc!())?;
    utils::exit_on_thread_panic();

    info!(
        "wprsc config: role={:?} present_backend={:?} keyboard_mode={:?} ui_scale_factor={} auto_reconnect={} endpoint_present={}",
        config.role,
        config.present_backend,
        config.keyboard_mode,
        config.ui_scale_factor,
        config.auto_reconnect,
        config.endpoint.is_some()
    );

    info!(
        "wprsc endpoints: socket={:?} control_socket={:?} endpoint={:?}",
        config.socket, config.control_socket, config.endpoint
    );
    run_wprsc(config).location(loc!())
}
