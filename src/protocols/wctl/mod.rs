use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(derive(Debug))]
pub enum Request {
    Ping,
    ServerInfo,
    /// Starts a wrun-managed session.
    ///
    /// This is a generic hook for server backends that need to bind themselves
    /// to the lifecycle of the launched child process.
    ///
    /// Example: the macOS seamless capture backend uses this to filter capture
    /// to the launched app.
    StartSession {
        child_pid: u32,
    },
    /// Stops a wrun-managed session.
    StopSession,
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
