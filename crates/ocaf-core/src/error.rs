//! Error types used throughout OCAF.

use std::fmt;
use std::io;

/// Result type used by all fallible OCAF operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while reading, writing or validating OCAF data.
#[derive(Debug)]
pub enum Error {
    /// An I/O operation failed.
    Io(io::Error),
    /// The input WAV file is malformed or uses an unsupported layout.
    InvalidWav(String),
    /// A raw PCM stream is malformed (for example, a partial sample frame).
    InvalidPcm(String),
    /// The OCAF file is malformed.
    InvalidOcaf(String),
    /// The file is valid but uses a feature this build does not support.
    Unsupported(String),
    /// Integrity verification failed.
    Corrupt(String),
    /// A caller supplied an invalid parameter.
    InvalidArgument(String),
    /// A defensive limit was exceeded (e.g. a hostile bitstream).
    Limit(String),
}

impl Error {
    pub fn invalid_wav(msg: impl Into<String>) -> Self {
        Error::InvalidWav(msg.into())
    }

    pub fn invalid_pcm(msg: impl Into<String>) -> Self {
        Error::InvalidPcm(msg.into())
    }

    pub fn invalid_ocaf(msg: impl Into<String>) -> Self {
        Error::InvalidOcaf(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Error::Unsupported(msg.into())
    }

    pub fn corrupt(msg: impl Into<String>) -> Self {
        Error::Corrupt(msg.into())
    }

    pub fn argument(msg: impl Into<String>) -> Self {
        Error::InvalidArgument(msg.into())
    }

    pub fn limit(msg: impl Into<String>) -> Self {
        Error::Limit(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::InvalidWav(m) => write!(f, "invalid WAV file: {m}"),
            Error::InvalidPcm(m) => write!(f, "invalid PCM input: {m}"),
            Error::InvalidOcaf(m) => write!(f, "invalid OCAF file: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported feature: {m}"),
            Error::Corrupt(m) => write!(f, "integrity check failed: {m}"),
            Error::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
            Error::Limit(m) => write!(f, "limit exceeded: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Error::Io(value)
    }
}
