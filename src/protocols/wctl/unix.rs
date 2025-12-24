use std::error::Error as StdError;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use crate::prelude::*;
use crate::protocols::wctl::Request;
use crate::protocols::wctl::codec;
use crate::protocols::wctl::server::Handler;
use crate::utils;

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
                if is_disconnect_error(&err) {
                    debug!("wctl: client disconnected");
                } else {
                    error!("wctl: recv failed: {err:?}");
                }
                return Ok(());
            },
        };
        let resp = handler.handle(req);
        codec::send(&mut stream, &resp).location(loc!())?;
    }
}

fn is_disconnect_error(err: &(dyn StdError + 'static)) -> bool {
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(cause) = current {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            ) {
                return true;
            }
        }
        current = cause.source();
    }
    false
}
