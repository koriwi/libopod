#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod artwork;
pub mod crypto;
pub mod device;
pub mod edit;
mod error;
pub mod fs;
pub mod library;
mod storage;

pub use artwork::{ArtworkDatabaseInfo, ArtworkDatasetInfo, ArtworkFrameInfo};
pub use device::{
    ArtworkFormatProfile, BackendKind, ChecksumKind, Device, DeviceCapabilities, DeviceInspection,
    DeviceProfile, EvidenceSource, IdentityEvidence, Sourced, VolumeFormat,
};
pub use edit::{EditSession, FileFingerprint, GenerationFingerprint, StagedSqliteEdit};
pub use error::{Error, Result};
pub use fs::{IpodPath, MountRoot};
pub use library::{Library, PersistentId, Playlist, Track};
pub use storage::{CbkInfo, CdbDatasetInfo, CdbInfo, SqliteDatabaseInfo, SqliteLibraryFile};
