//! Error type.

use std::fmt;

/// An I/O, protocol or configuration error.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Operating-system I/O failure.
    Io(std::io::Error),
    /// TLS failure reported by rustls.
    Tls(rustls::Error),
    /// Record-layer failure (bad record, AEAD failure, unexpected content).
    Record(&'static str),
    /// WebSocket protocol violation.
    Ws(&'static str),
    /// HTTP failure (bad response, upgrade refused).
    Http(String),
    /// The peer closed the connection.
    Closed,
    /// Bad configuration input.
    Config(String),
    /// The backend is not available on this platform or build.
    Unsupported(&'static str),
    /// Name resolution failed.
    Dns(String),
    /// An operation did not finish in time.
    Timeout(&'static str),
    /// A limit was reached; retry later.
    Busy(&'static str),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Tls(e) => write!(f, "tls: {e}"),
            Self::Record(e) => write!(f, "tls record: {e}"),
            Self::Ws(e) => write!(f, "websocket: {e}"),
            Self::Http(e) => write!(f, "http: {e}"),
            Self::Closed => write!(f, "peer closed the connection"),
            Self::Config(e) => write!(f, "config: {e}"),
            Self::Unsupported(e) => write!(f, "unsupported: {e}"),
            Self::Dns(e) => write!(f, "dns: {e}"),
            Self::Timeout(e) => write!(f, "timed out: {e}"),
            Self::Busy(e) => write!(f, "busy: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Tls(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Self::Tls(e)
    }
}
