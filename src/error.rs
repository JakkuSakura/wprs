use std::error::Error as StdError;
use std::fmt;

use thiserror::Error;

use crate::utils::error::Location;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("missing required value: {0}")]
    Missing(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("{context}: {source}")]
    Context {
        context: String,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    #[error("{location}: {source}")]
    Location {
        location: Location,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)]
    TryFromInt(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error(transparent)]
    FromUtf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Nul(#[from] std::ffi::NulError),
    #[error(transparent)]
    Env(#[from] std::env::VarError),
    #[error(transparent)]
    AddrParse(#[from] std::net::AddrParseError),
    #[error(transparent)]
    StripPrefix(#[from] std::path::StripPrefixError),
    #[error(transparent)]
    SystemTime(#[from] std::time::SystemTimeError),
    #[error(transparent)]
    Recv(#[from] std::sync::mpsc::RecvError),
    #[error(transparent)]
    TryRecv(#[from] std::sync::mpsc::TryRecvError),
    #[error(transparent)]
    Fmt(#[from] std::fmt::Error),
}

impl Error {
    pub fn context(
        context: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self::Context {
            context: context.into(),
            source: Box::new(source),
        }
    }

    pub fn location(location: Location, source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Location {
            location,
            source: Box::new(source),
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    pub fn missing(message: impl Into<String>) -> Self {
        Self::Missing(message.into())
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

#[macro_export]
macro_rules! bail {
    ($err:expr $(,)?) => {
        return Err($err)
    };
}
pub use bail;

#[macro_export]
macro_rules! ensure {
    ($cond:expr, $err:expr $(,)?) => {
        if !$cond {
            return Err($err)
        }
    };
}
pub use ensure;

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}:{}", self.fname, self.file, self.line)
    }
}
