use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub enum Request {
    Ping,
    ServerInfo,
    /// Best-effort capture filtering.
    ///
    /// For macOS window/seamless capture, this limits capture to windows owned
    /// by `pid`.
    SetCaptureTargetPid {
        pid: Option<u32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub enum Response {
    Pong,
    ServerInfo(ServerInfo),
    Ok,
    Error { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub struct ServerInfo {
    pub wprs_endpoint: String,
    pub wayland_display: Option<String>,
    pub xwayland_display: Option<u32>,
}

pub mod client;
pub mod codec;
pub mod endpoint;
pub mod server;

pub use endpoint::Endpoint;

#[cfg(unix)]
pub mod unix;

pub mod tcp;
