use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use crate::prelude::*;
use crate::protocols::wctl::Request;
use crate::protocols::wctl::Response;
use crate::protocols::wctl::codec;
use crate::utils;

pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request) -> Response;
}

pub fn serve(socket: &Path, handler: Arc<dyn Handler>) -> Result<()> {
    let listener: UnixListener = utils::bind_user_socket(socket).location(loc!())?;
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(err) => {
                warn!("wctl: accept failed: {err}");
                continue;
            },
        };

        let handler = handler.clone();
        std::thread::spawn(move || {
            handle_connection(stream, handler).log_and_ignore(loc!());
        });
    }
    Ok(())
}

fn handle_connection(mut stream: UnixStream, handler: Arc<dyn Handler>) -> Result<()> {
    loop {
        let req = match codec::recv::<Request>(&mut stream) {
            Ok(req) => req,
            Err(err) => {
                debug!("wctl: recv failed: {err}");
                return Ok(());
            },
        };
        let resp = handler.handle(req);
        codec::send(&mut stream, &resp).location(loc!())?;
    }
}
