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
pub use edit::{
    recover_interrupted_transaction, EditSession, FileFingerprint, GenerationFingerprint,
    StagedSqliteEdit, NANO7_NOOP_HARDWARE_TEST_CONFIRMATION,
    NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION,
};
pub use error::{Error, Result};
pub use fs::{IpodPath, MountRoot};
pub use library::{Library, PersistentId, Playlist, Track};
pub use storage::{CbkInfo, CdbDatasetInfo, CdbInfo, SqliteDatabaseInfo, SqliteLibraryFile};
