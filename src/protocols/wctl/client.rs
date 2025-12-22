use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;
use std::time::Instant;

use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wctl::Endpoint;

#[derive(Clone, Debug)]
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    pub fn ping(&self) -> Result<()> {
        let resp = self.request(wctl::Request::Ping).location(loc!())?;
        match resp {
            wctl::Response::Pong => Ok(()),
            wctl::Response::Error { message } => bail!("wctl ping failed: {message}"),
            other => bail!("unexpected wctl response: {other:?}"),
        }
    }

    pub fn server_info(&self) -> Result<wctl::ServerInfo> {
        let resp = self.request(wctl::Request::ServerInfo).location(loc!())?;
        match resp {
            wctl::Response::ServerInfo(info) => Ok(info),
            wctl::Response::Error { message } => bail!("wctl server_info failed: {message}"),
            other => bail!("unexpected wctl response: {other:?}"),
        }
    }

    pub fn start_session(&self, child_pid: u32) -> Result<()> {
        let resp = self
            .request(wctl::Request::StartSession { child_pid })
            .location(loc!())?;
        match resp {
            wctl::Response::Ok => Ok(()),
            wctl::Response::Error { message } => {
                bail!("wctl start session failed: {message}")
            },
            other => bail!("unexpected wctl response: {other:?}"),
        }
    }

    pub fn stop_session(&self) -> Result<()> {
        let resp = self.request(wctl::Request::StopSession).location(loc!())?;
        match resp {
            wctl::Response::Ok => Ok(()),
            wctl::Response::Error { message } => {
                bail!("wctl stop session failed: {message}")
            },
            other => bail!("unexpected wctl response: {other:?}"),
        }
    }

    pub fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.ping() {
                Ok(()) => return Ok(()),
                Err(err) => {
                    if Instant::now() >= deadline {
                        return Err(err);
                    }
                    std::thread::sleep(Duration::from_millis(25));
                },
            }
        }
    }

    fn request(&self, req: wctl::Request) -> Result<wctl::Response> {
        self.endpoint.ensure_localhost().location(loc!())?;
        match &self.endpoint {
            #[cfg(unix)]
            Endpoint::Unix { path } => {
                use std::os::unix::net::UnixStream;
                let mut stream = UnixStream::connect(path).location(loc!())?;
                self.send_and_recv(&mut stream, req).location(loc!())
            },
            Endpoint::Tcp { addr } => {
                let mut stream = TcpStream::connect(addr).location(loc!())?;
                self.send_and_recv(&mut stream, req).location(loc!())
            },
        }
    }

    fn send_and_recv(
        &self,
        stream: &mut (impl Read + Write),
        req: wctl::Request,
    ) -> Result<wctl::Response> {
        wctl::codec::send(stream, &req).location(loc!())?;
        wctl::codec::recv::<wctl::Response>(stream).location(loc!())
    }
}
