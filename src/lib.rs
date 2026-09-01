#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod artwork;
pub mod crypto;
pub mod device;
pub mod edit;
mod error;
pub mod fs;
pub mod library;
mod random;
mod storage;

pub use artwork::{
    parse_artwork_records, ArtworkDatabaseInfo, ArtworkDatasetInfo, ArtworkFormatRef,
    ArtworkFrameInfo, ArtworkRecord,
};
pub use device::{
    ArtworkFormatProfile, BackendKind, ChecksumKind, Device, DeviceCapabilities, DeviceInspection,
    DeviceProfile, EvidenceSource, IdentityEvidence, Sourced, VolumeFormat,
};
pub use edit::{
    recover_interrupted_transaction, EditSession, FileFingerprint, GenerationFingerprint,
    MediaDeletionPolicy, StagedSqliteEdit, TrackToAdd, NANO7_ADDITION_HARDWARE_TEST_CONFIRMATION,
    NANO7_ARTWORK_REMOVAL_DELETE_HARDWARE_TEST_CONFIRMATION,
    NANO7_ARTWORK_REMOVAL_HARDWARE_TEST_CONFIRMATION, NANO7_ARTWORK_REUSE_ADDITION_CONFIRMATION,
    NANO7_NEW_ART_ADDITION_CONFIRMATION, NANO7_NOOP_HARDWARE_TEST_CONFIRMATION,
    NANO7_REMOVAL_DELETE_HARDWARE_TEST_CONFIRMATION, NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION,
};
pub use error::{Error, Result};
pub use fs::{IpodPath, MountRoot};
pub use library::{Library, MediaKind, PersistentId, Playlist, Track};
pub use storage::{CbkInfo, CdbDatasetInfo, CdbInfo, SqliteDatabaseInfo, SqliteLibraryFile};
