use std::sync::Arc;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::time::Duration;
use std::time::Instant;

use crate::prelude::*;
use crate::protocols::wctl;
use crate::protocols::wctl::Request;
use crate::protocols::wctl::Response;
use crate::protocols::wctl::server::Handler;

struct RequestEnvelope {
    req: Request,
    resp_tx: Sender<Response>,
}

#[derive(Clone, Debug)]
pub struct Client {
    tx: Sender<RequestEnvelope>,
}

impl Client {
    fn new(tx: Sender<RequestEnvelope>) -> Self {
        Self { tx }
    }

    pub fn ping(&self) -> Result<()> {
        let resp = self.request(wctl::Request::Ping).location(loc!())?;
        match resp {
            wctl::Response::Pong => Ok(()),
            wctl::Response::Error { message } => {
                bail!(Error::Internal(format!("wctl ping failed: {message}")))
            }
            other => bail!(Error::Internal(format!(
                "unexpected wctl response: {other:?}"
            ))),
        }
    }

    pub fn server_info(&self) -> Result<wctl::ServerInfo> {
        let resp = self.request(wctl::Request::ServerInfo).location(loc!())?;
        match resp {
            wctl::Response::ServerInfo(info) => Ok(info),
            wctl::Response::Error { message } => {
                bail!(Error::Internal(format!("wctl server_info failed: {message}")))
            }
            other => bail!(Error::Internal(format!(
                "unexpected wctl response: {other:?}"
            ))),
        }
    }

    pub fn start_session(&self, child_pid: u32) -> Result<()> {
        let resp = self
            .request(wctl::Request::StartSession { child_pid })
            .location(loc!())?;
        match resp {
            wctl::Response::Ok => Ok(()),
            wctl::Response::Error { message } => {
                bail!(Error::Internal(format!("wctl start session failed: {message}")))
            }
            other => bail!(Error::Internal(format!(
                "unexpected wctl response: {other:?}"
            ))),
        }
    }

    pub fn stop_session(&self) -> Result<()> {
        let resp = self.request(wctl::Request::StopSession).location(loc!())?;
        match resp {
            wctl::Response::Ok => Ok(()),
            wctl::Response::Error { message } => {
                bail!(Error::Internal(format!("wctl stop session failed: {message}")))
            }
            other => bail!(Error::Internal(format!(
                "unexpected wctl response: {other:?}"
            ))),
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
                }
            }
        }
    }

    fn request(&self, req: Request) -> Result<Response> {
        let (resp_tx, resp_rx) = mpsc::channel();
        self.tx
            .send(RequestEnvelope { req, resp_tx })
            .map_err(|_| Error::Internal("wctl inproc channel closed".to_string()))?;
        resp_rx
            .recv()
            .map_err(|_| Error::Internal("wctl inproc response channel closed".to_string()))
    }
}

pub struct Server {
    rx: Receiver<RequestEnvelope>,
}

impl Server {
    fn new(rx: Receiver<RequestEnvelope>) -> Self {
        Self { rx }
    }

    pub fn serve(self, handler: Arc<dyn Handler>) -> Result<()> {
        for env in self.rx {
            let resp = handler.handle(env.req);
            let _ = env.resp_tx.send(resp);
        }
        Ok(())
    }
}

pub fn channel_pair() -> (Client, Server) {
    let (tx, rx) = mpsc::channel();
    (Client::new(tx), Server::new(rx))
}
