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

pub mod client;
pub mod config;
pub mod constants;
pub mod error;
pub mod launcher;
pub mod models;
pub mod prelude;
pub mod protocols;
pub mod server;
pub mod utils;

// Backward-compatible re-export for existing code/tests.
pub use crate::utils::buffer_pointer;

#[cfg(feature = "rdp")]
pub use crate::protocols::rdp;

#[cfg(all(
    feature = "wayland-client",
    any(target_os = "macos", target_os = "ios")
))]
compile_error!(
    "The `wayland-client` feature (SCTK/Wayland backend) is not supported on Apple platforms. Use `--features winit-wgpu-client` instead."
);
