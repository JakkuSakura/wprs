use std::error::Error as StdError;
use std::fmt;

use crate::utils::error::Location;

pub type Result<T> = std::result::Result<T, Error>;

pub type StdErrorSource = Box<dyn StdError + Send + Sync>;

pub enum Error {
    InvalidArgument(String),
    Missing(String),
    Unsupported(String),
    Config(String),
    Internal(String),
    Context {
        context: String,
        source: StdErrorSource,
    },
    Location {
        location: Location,
        source: StdErrorSource,
    },
    Io(std::io::Error),
    ParseInt(std::num::ParseIntError),
    TryFromInt(std::num::TryFromIntError),
    Utf8(std::str::Utf8Error),
    FromUtf8(std::string::FromUtf8Error),
    Nul(std::ffi::NulError),
    Env(std::env::VarError),
    AddrParse(std::net::AddrParseError),
    StripPrefix(std::path::StripPrefixError),
    SystemTime(std::time::SystemTimeError),
    Recv(std::sync::mpsc::RecvError),
    TryRecv(std::sync::mpsc::TryRecvError),
    Fmt(std::fmt::Error),
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

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", display_message(self))
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            return format_structured_debug(self, f);
        }
        write!(f, "{}", format_error_chain(self))
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Context { source, .. } | Error::Location { source, .. } => Some(source.as_ref()),
            Error::Io(err) => Some(err),
            Error::ParseInt(err) => Some(err),
            Error::TryFromInt(err) => Some(err),
            Error::Utf8(err) => Some(err),
            Error::FromUtf8(err) => Some(err),
            Error::Nul(err) => Some(err),
            Error::Env(err) => Some(err),
            Error::AddrParse(err) => Some(err),
            Error::StripPrefix(err) => Some(err),
            Error::SystemTime(err) => Some(err),
            Error::Recv(err) => Some(err),
            Error::TryRecv(err) => Some(err),
            Error::Fmt(err) => Some(err),
            Error::InvalidArgument(_)
            | Error::Missing(_)
            | Error::Unsupported(_)
            | Error::Config(_)
            | Error::Internal(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(err: std::num::ParseIntError) -> Self {
        Error::ParseInt(err)
    }
}

impl From<std::num::TryFromIntError> for Error {
    fn from(err: std::num::TryFromIntError) -> Self {
        Error::TryFromInt(err)
    }
}

impl From<std::str::Utf8Error> for Error {
    fn from(err: std::str::Utf8Error) -> Self {
        Error::Utf8(err)
    }
}

impl From<std::string::FromUtf8Error> for Error {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Error::FromUtf8(err)
    }
}

impl From<std::ffi::NulError> for Error {
    fn from(err: std::ffi::NulError) -> Self {
        Error::Nul(err)
    }
}

impl From<std::env::VarError> for Error {
    fn from(err: std::env::VarError) -> Self {
        Error::Env(err)
    }
}

impl From<std::net::AddrParseError> for Error {
    fn from(err: std::net::AddrParseError) -> Self {
        Error::AddrParse(err)
    }
}

impl From<std::path::StripPrefixError> for Error {
    fn from(err: std::path::StripPrefixError) -> Self {
        Error::StripPrefix(err)
    }
}

impl From<std::time::SystemTimeError> for Error {
    fn from(err: std::time::SystemTimeError) -> Self {
        Error::SystemTime(err)
    }
}

impl From<std::sync::mpsc::RecvError> for Error {
    fn from(err: std::sync::mpsc::RecvError) -> Self {
        Error::Recv(err)
    }
}

impl From<std::sync::mpsc::TryRecvError> for Error {
    fn from(err: std::sync::mpsc::TryRecvError) -> Self {
        Error::TryRecv(err)
    }
}

impl From<std::fmt::Error> for Error {
    fn from(err: std::fmt::Error) -> Self {
        Error::Fmt(err)
    }
}

fn display_message(err: &Error) -> String {
    match err {
        Error::InvalidArgument(message) => format!("invalid argument: {message}"),
        Error::Missing(message) => format!("missing required value: {message}"),
        Error::Unsupported(message) => format!("unsupported operation: {message}"),
        Error::Config(message) => format!("configuration error: {message}"),
        Error::Internal(message) => format!("internal error: {message}"),
        Error::Context { context, source } => format!("{context}: {source}"),
        Error::Location { source, .. } => source.to_string(),
        Error::Io(err) => err.to_string(),
        Error::ParseInt(err) => err.to_string(),
        Error::TryFromInt(err) => err.to_string(),
        Error::Utf8(err) => err.to_string(),
        Error::FromUtf8(err) => err.to_string(),
        Error::Nul(err) => err.to_string(),
        Error::Env(err) => err.to_string(),
        Error::AddrParse(err) => err.to_string(),
        Error::StripPrefix(err) => err.to_string(),
        Error::SystemTime(err) => err.to_string(),
        Error::Recv(err) => err.to_string(),
        Error::TryRecv(err) => err.to_string(),
        Error::Fmt(err) => err.to_string(),
    }
}

fn format_error_chain(err: &(dyn StdError + 'static)) -> String {
    let mut out = format_error_summary(err);
    let mut source = err.source();
    let mut depth = 0;
    while let Some(cause) = source {
        depth += 1;
        out.push_str(&format!("\n  caused by ({depth}): {}", format_error_summary(cause)));
        source = cause.source();
    }
    out
}

fn format_error_summary(err: &(dyn StdError + 'static)) -> String {
    if let Some(wprs_err) = err.downcast_ref::<Error>() {
        return format_wprs_error(wprs_err);
    }
    err.to_string()
}

fn format_wprs_error(err: &Error) -> String {
    match err {
        Error::Location { location, source } => {
            format!("{location}: {}", format_error_summary(source.as_ref()))
        }
        Error::Context { context, source } => {
            format!("{context}: {}", format_error_summary(source.as_ref()))
        }
        _ => display_message(err),
    }
}

fn format_structured_debug(err: &Error, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match err {
        Error::InvalidArgument(message) => f.debug_tuple("InvalidArgument").field(message).finish(),
        Error::Missing(message) => f.debug_tuple("Missing").field(message).finish(),
        Error::Unsupported(message) => f.debug_tuple("Unsupported").field(message).finish(),
        Error::Config(message) => f.debug_tuple("Config").field(message).finish(),
        Error::Internal(message) => f.debug_tuple("Internal").field(message).finish(),
        Error::Context { context, source } => f
            .debug_struct("Context")
            .field("context", context)
            .field("source", source)
            .finish(),
        Error::Location { location, source } => f
            .debug_struct("Location")
            .field("location", location)
            .field("source", source)
            .finish(),
        Error::Io(err) => f.debug_tuple("Io").field(err).finish(),
        Error::ParseInt(err) => f.debug_tuple("ParseInt").field(err).finish(),
        Error::TryFromInt(err) => f.debug_tuple("TryFromInt").field(err).finish(),
        Error::Utf8(err) => f.debug_tuple("Utf8").field(err).finish(),
        Error::FromUtf8(err) => f.debug_tuple("FromUtf8").field(err).finish(),
        Error::Nul(err) => f.debug_tuple("Nul").field(err).finish(),
        Error::Env(err) => f.debug_tuple("Env").field(err).finish(),
        Error::AddrParse(err) => f.debug_tuple("AddrParse").field(err).finish(),
        Error::StripPrefix(err) => f.debug_tuple("StripPrefix").field(err).finish(),
        Error::SystemTime(err) => f.debug_tuple("SystemTime").field(err).finish(),
        Error::Recv(err) => f.debug_tuple("Recv").field(err).finish(),
        Error::TryRecv(err) => f.debug_tuple("TryRecv").field(err).finish(),
        Error::Fmt(err) => f.debug_tuple("Fmt").field(err).finish(),
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
