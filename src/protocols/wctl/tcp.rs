use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;

use crate::prelude::*;
use crate::protocols::wctl::Request;
use crate::protocols::wctl::codec;
use crate::protocols::wctl::server::Handler;

pub fn serve(addr: SocketAddr, handler: Arc<dyn Handler>) -> Result<()> {
    ensure!(
        addr.ip().is_loopback(),
        Error::InvalidArgument(format!(
            "wctl tcp endpoint must use a loopback address: {addr}"
        )),
    );

    let listener = TcpListener::bind(addr).location(loc!())?;
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(err) => {
                warn!("wctl(tcp): accept failed: {err}");
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

fn handle_connection(mut stream: TcpStream, handler: Arc<dyn Handler>) -> Result<()> {
    loop {
        let req = match codec::recv::<Request>(&mut stream) {
            Ok(req) => req,
            Err(err) => {
                debug!("wctl(tcp): recv failed: {err}");
                return Ok(());
            },
        };
        let resp = handler.handle(req);
        codec::send(&mut stream, &resp).location(loc!())?;
    }
}
