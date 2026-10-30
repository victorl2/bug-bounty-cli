//! One error type for every layer; the variant decides how callers recover.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A request failed; `status` is the HTTP status when there was one.
    Fetch { message: String, status: Option<u16> },
    /// The resource does not exist (HTTP 404, or a page that says so).
    NotFound(String),
    /// Still throttled after retrying: transient, so never cached as a result.
    RateLimited(String),
    /// A chain, explorer or node could not answer.
    Chain(String),
    /// An eth_call reverted.
    Reverted(String),
    /// A page did not contain the data it should.
    Payload(String),
    /// A target names the other kind of opportunity; the message holds the right command.
    Lookup(String),
    /// Bad user input or an unexpected value.
    Value(String),
    /// Filesystem trouble.
    Io(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn value(message: impl Into<String>) -> Self {
        Error::Value(message.into())
    }

    pub fn fetch(message: impl Into<String>, status: Option<u16>) -> Self {
        Error::Fetch { message: message.into(), status }
    }

    pub fn chain(message: impl Into<String>) -> Self {
        Error::Chain(message.into())
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Fetch { status, .. } => *status,
            Error::NotFound(_) => Some(404),
            Error::RateLimited(_) => Some(429),
            _ => None,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Error::Fetch { message, .. } => message,
            Error::NotFound(m)
            | Error::RateLimited(m)
            | Error::Chain(m)
            | Error::Reverted(m)
            | Error::Payload(m)
            | Error::Lookup(m)
            | Error::Value(m)
            | Error::Io(m) => m,
        }
    }

    /// Errors that mean "the network or provider failed" rather than "the input is wrong".
    pub fn is_chain_failure(&self) -> bool {
        matches!(self, Error::Chain(_) | Error::RateLimited(_) | Error::Reverted(_) | Error::Fetch { .. } | Error::NotFound(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Payload(format!("invalid JSON: {e}"))
    }
}
