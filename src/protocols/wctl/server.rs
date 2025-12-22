use std::sync::Arc;

use crate::prelude::*;
use crate::protocols::wctl::Endpoint;
use crate::protocols::wctl::Request;
use crate::protocols::wctl::Response;

pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: Request) -> Response;
}

pub fn serve(endpoint: &Endpoint, handler: Arc<dyn Handler>) -> Result<()> {
    endpoint.ensure_localhost().location(loc!())?;
    match endpoint {
        #[cfg(unix)]
        Endpoint::Unix { path } => crate::protocols::wctl::unix::serve(path, handler),
        Endpoint::Tcp { addr } => crate::protocols::wctl::tcp::serve(*addr, handler),
    }
}
