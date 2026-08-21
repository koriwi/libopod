use std::path::PathBuf;

/// The result type returned by libopod operations.
pub type Result<T> = std::result::Result<T, Error>;

/// An error produced while inspecting or managing an iPod.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A host filesystem operation failed.
    #[error("could not {operation} `{path}`")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A mount root is not suitable for use as an iPod volume.
    #[error("invalid mount root `{path}`: {reason}")]
    InvalidMount { path: PathBuf, reason: String },

    /// A mount-relative iPod path failed validation.
    #[error("invalid iPod-relative path `{path}`: {reason}")]
    InvalidIpodPath { path: String, reason: String },

    /// A file is too large for a bounded parser operation.
    #[error("{format} input `{path}` is {actual} bytes; limit is {limit} bytes")]
    InputTooLarge {
        format: &'static str,
        path: PathBuf,
        actual: u64,
        limit: u64,
    },

    /// An on-device format is malformed or truncated.
    #[error("malformed {format} at byte {offset}: {reason}")]
    Malformed {
        format: &'static str,
        offset: u64,
        reason: String,
    },

    /// Device evidence conflicts in a way that affects safe writing.
    #[error("conflicting device evidence: {reason}")]
    ConflictingEvidence { reason: String },

    /// The detected device or format is not supported yet.
    #[error("unsupported {feature}: {reason}")]
    Unsupported {
        feature: &'static str,
        reason: String,
    },

    /// A checksum or signed companion file did not verify.
    #[error("{format} verification failed: {reason}")]
    Verification {
        format: &'static str,
        reason: String,
    },

    /// An interrupted libopod transaction requires explicit recovery.
    #[error("an interrupted libopod transaction exists at `{path}`; recover it before opening the device")]
    RecoveryRequired { path: PathBuf },

    /// An edit referred to a track that is not in the opened library.
    #[error("the requested track is not present in the opened iPod library")]
    TrackNotFound,

    /// An edit referred to a playlist that is not in the opened library.
    #[error("the requested playlist is not present in the opened iPod library")]
    PlaylistNotFound,

    /// A playlist name is empty or otherwise invalid.
    #[error("invalid playlist name: {reason}")]
    InvalidPlaylistName { reason: String },

    /// A host staging directory is unsafe or unsuitable.
    #[error("invalid staging directory `{path}`: {reason}")]
    InvalidStagingDirectory { path: PathBuf, reason: String },

    /// `SQLite` rejected an inspection or staged-edit operation.
    #[error("SQLite operation `{operation}` failed for `{path}`")]
    Sqlite {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
}

pub(crate) fn io_error(
    operation: &'static str,
    path: impl Into<PathBuf>,
    source: std::io::Error,
) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
