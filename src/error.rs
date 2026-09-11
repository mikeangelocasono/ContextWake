use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentDeckError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("state store error: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("profile not found: {0}")]
    ProfileNotFound(String),
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("checkpoint not found: {0}")]
    CheckpointNotFound(String),
    #[error("handoff not found: {0}")]
    HandoffNotFound(String),
    #[error("coding-agent capability unavailable: {0}")]
    CapabilityUnavailable(String),
    #[error("coding-agent command failed: {0}")]
    Provider(String),
    #[error("Git command failed: {0}")]
    Git(String),
    #[error("invalid or unsafe path: {0}")]
    UnsafePath(String),
    #[error("untrusted project configuration: {0}")]
    UntrustedConfiguration(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, AgentDeckError>;

pub trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        let path = path.into();
        self.map_err(|source| AgentDeckError::Io { path, source })
    }
}
