//! Standalone-build (no `oxideav-core` dep) error / result / I/O
//! shims.
//!
//! When the `registry` feature is OFF the crate must compile and
//! function without ever touching the `oxideav-core` types. The
//! per-module sources `use crate::standalone::{Error, Result}` then
//! and the shims here mirror the surface of `oxideav_core::Error`
//! closely enough that the parsing modules can stay branch-free
//! across the two builds.

use std::fmt;
use std::io;

/// Standalone error type. Mirrors the variants we actually construct
/// (`InvalidData` / `Unsupported` / `Eof` plus an I/O carrier) so the
/// parsing modules don't need feature gates around individual call sites.
#[derive(Debug)]
pub enum Error {
    /// Synthetic "unexpected end-of-stream" — produced by the demuxer
    /// when the sample queue is exhausted.
    Eof,
    /// Malformed input.
    InvalidData(String),
    /// Recognised input shape that this demuxer cannot fully consume
    /// (e.g. fragmented MP4, reference-movie aliases).
    Unsupported(String),
    /// Underlying I/O error (file truncation, OS read failures, …).
    Io(io::Error),
}

impl Error {
    /// Build an [`Error::InvalidData`] from any displayable message.
    /// Mirrors the `oxideav_core::Error::invalid` constructor used by
    /// the parsing modules.
    pub fn invalid<S: Into<String>>(msg: S) -> Self {
        Error::InvalidData(msg.into())
    }

    /// Build an [`Error::Unsupported`] — used for input that we
    /// recognise but deliberately do not implement (fragmented MP4
    /// boxes, reference-movie alias resolution).
    pub fn unsupported<S: Into<String>>(msg: S) -> Self {
        Error::Unsupported(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Eof => f.write_str("eof"),
            Error::InvalidData(m) => write!(f, "invalid data: {m}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Io(e) => write!(f, "I/O: {e}"),
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
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Result alias used by the standalone parsing modules.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `Read + Seek` trait object used as the demuxer's input handle in
/// the standalone build. This mirrors `oxideav_core::ReadSeek` so the
/// public `MovDemuxer::open` signature stays identical across builds.
pub trait ReadSeek: io::Read + io::Seek {}
impl<T: io::Read + io::Seek + ?Sized> ReadSeek for T {}
