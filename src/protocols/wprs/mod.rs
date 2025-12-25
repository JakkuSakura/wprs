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

pub mod framing;
pub mod geometry;
pub mod capabilities;
pub mod codecs;
pub mod transport;
pub mod tuple;
pub mod wayland;
pub mod xdg_shell;

pub mod handshake;

pub mod endpoint;
pub mod raw_buffer;
pub mod serializer;
pub mod server_core;
pub mod types;
