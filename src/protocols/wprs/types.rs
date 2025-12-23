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

use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

#[cfg(feature = "wayland")]
use smithay::reexports::wayland_server::Client;
#[cfg(feature = "wayland")]
use smithay::reexports::wayland_server::backend;

use crate::protocols::wprs::transport;
use crate::protocols::wprs::wayland;
use crate::protocols::wprs::xdg_shell;

#[derive(Archive, Deserialize, Serialize, Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub struct ClientId(pub u64);

impl ClientId {
    #[cfg(feature = "wayland")]
    pub fn new(client: &Client) -> Self {
        Self(hash(&client.id()))
    }
}

#[cfg(feature = "wayland")]
impl From<backend::ClientId> for ClientId {
    fn from(client_id: backend::ClientId) -> Self {
        (&client_id).into()
    }
}

#[cfg(feature = "wayland")]
impl From<&backend::ClientId> for ClientId {
    fn from(client_id: &backend::ClientId) -> Self {
        Self(hash(client_id))
    }
}

#[derive(Archive, Deserialize, Serialize, Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub enum ObjectId {
    WlSurface(wayland::WlSurfaceId),
    XdgSurface(xdg_shell::XdgSurfaceId),
    XdgToplevel(xdg_shell::XdgToplevelId),
    XdgPopup(xdg_shell::XdgPopupId),
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize, serde_derive::Serialize)]
pub struct Capabilities {
    pub xwayland: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Archive, Deserialize, Serialize)]
pub struct DisplayConfig {
    /// Suggested server-side output scale (pixels per logical point).
    pub scale_factor: i32,
    /// Suggested server-side DPI (if known).
    pub dpi: Option<u32>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            scale_factor: 1,
            dpi: None,
        }
    }
}

// TODO: https://github.com/rust-lang/rfcs/pull/2593 - simplify all the enums.

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub enum Request {
    Surface(wayland::SurfaceRequest),
    CursorImage(wayland::CursorImage),
    Toplevel(xdg_shell::ToplevelRequest),
    Popup(xdg_shell::PopupRequest),
    Data(wayland::DataRequest),
    ClientDisconnected(ClientId),
    Capabilities(Capabilities),
    DisplayConfig(DisplayConfig),
    Transport(transport::TransportRequest),
}

#[derive(Debug, Clone, PartialEq, Archive, Deserialize, Serialize)]
pub enum Event {
    WprsClientConnect,
    Transport(transport::TransportEvent),
    Output(wayland::OutputEvent),
    PointerFrame(Vec<wayland::PointerEvent>),
    PointerGesture(wayland::PointerGestureEvent),
    KeyboardEvent(wayland::KeyboardEvent),
    Toplevel(xdg_shell::ToplevelEvent),
    Popup(xdg_shell::PopupEvent),
    Data(wayland::DataEvent),
    Surface(wayland::SurfaceEvent),
}

// TODO: test that object ids with same value from different clients hash
// differently.
pub fn hash<T: Hash>(t: &T) -> u64 {
    let mut s = DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}
