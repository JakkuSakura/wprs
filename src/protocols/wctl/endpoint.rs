use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::ensure;
use serde_derive::Deserialize;
use serde_derive::Serialize;

use crate::prelude::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Endpoint {
    #[cfg(unix)]
    Unix {
        path: PathBuf,
    },
    Tcp {
        addr: SocketAddr,
    },
}

impl Endpoint {
    pub fn ensure_localhost(&self) -> Result<()> {
        match self {
            #[cfg(unix)]
            Endpoint::Unix { .. } => Ok(()),
            Endpoint::Tcp { addr } => {
                ensure!(
                    addr.ip().is_loopback(),
                    "wctl tcp endpoint must use a loopback address: {addr}"
                );
                Ok(())
            },
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            Endpoint::Unix { path } => write!(f, "unix://{}", path.display()),
            Endpoint::Tcp { addr } => write!(f, "tcp://{addr}"),
        }
    }
}

impl FromStr for Endpoint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        #[cfg(unix)]
        if let Some(rest) = s.strip_prefix("unix://") {
            return Ok(Self::Unix {
                path: PathBuf::from(rest),
            });
        }

        if let Some(rest) = s.strip_prefix("tcp://") {
            let addr: SocketAddr = rest.parse().location(loc!())?;
            return Ok(Self::Tcp { addr });
        }

        #[cfg(unix)]
        {
            // Convenience: treat a bare path as a Unix socket path.
            if s.starts_with('/') {
                return Ok(Self::Unix {
                    path: PathBuf::from(s),
                });
            }
        }

        bail!(
            "invalid wctl endpoint {s:?} (expected: tcp://127.0.0.1:PORT{} )",
            if cfg!(unix) { " or unix:///path" } else { "" }
        )
    }
}
