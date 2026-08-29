//! Error type for the engine.

use std::path::PathBuf;
use thiserror::Error;

pub type EngineResult<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("refused to traverse outside of {root}: {attempted}")]
    PathTraversal { root: PathBuf, attempted: PathBuf },

    #[error("refused to follow symlink {link} pointing outside of {root}")]
    SymlinkEscape { root: PathBuf, link: PathBuf },

    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("source and destination are the same path: {0}")]
    SameSourceDest(PathBuf),

    #[error("operation cancelled")]
    Cancelled,

    #[error("configuration error: {0}")]
    Config(String),

    #[error("internal: {0}")]
    Internal(String),
}

impl EngineError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            path: PathBuf::from("<unspecified>"),
            source: e,
        }
    }
}
