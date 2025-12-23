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
pub mod transport;
pub mod tuple;
pub mod wayland;
pub mod xdg_shell;

pub mod client_sync;
pub mod handshake;

mod endpoint;
mod raw_buffer;
mod serializer;
mod server_core;
mod types;

pub use endpoint::ClientTransportGuard;
pub use endpoint::Endpoint;
pub use endpoint::SshDestination;
pub use endpoint::setup_client_transport;

pub use raw_buffer::RawBufferHeader;
pub use raw_buffer::RawBufferKind;
pub use raw_buffer::RawBufferMessage;
pub use raw_buffer::RawBufferPayload;

pub use serializer::MessageType;
pub use serializer::RecvType;
pub use serializer::SendType;
pub use serializer::Serializable;
pub use serializer::Serializer;
pub use serializer::SerializerClientOptions;
pub use serializer::new_inproc_serializer_pair;

pub use server_core::Backend;
pub use server_core::Core;
pub use server_core::dispatch_event;
pub use server_core::surface_request_from_state;

pub use types::Capabilities;
pub use types::ClientId;
pub use types::DisplayConfig;
pub use types::Event;
pub use types::ObjectId;
pub use types::Request;
pub use types::hash;
