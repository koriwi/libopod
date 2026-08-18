mod evidence;
mod inspect;
mod profile;

use std::path::{Path, PathBuf};

pub use evidence::{EvidenceSource, IdentityEvidence, Sourced, VolumeFormat};
pub use inspect::DeviceInspection;
pub use profile::{
    ArtworkFormatProfile, BackendKind, ChecksumKind, DeviceCapabilities, DeviceProfile,
};

use crate::{EditSession, GenerationFingerprint, IpodPath, Library, MountRoot, Result, Track};

/// A read-only handle to a mounted iPod.
///
/// Mutating edit sessions are intentionally not exposed until staged commit
/// and recovery guarantees are implemented.
#[derive(Debug)]
pub struct Device {
    mount: MountRoot,
    evidence: IdentityEvidence,
    profile: Option<DeviceProfile>,
    inspection: DeviceInspection,
    library: Option<Library>,
    generation: GenerationFingerprint,
}

impl Device {
    /// Opens a mounted iPod without modifying any device file.
    ///
    /// # Errors
    ///
    /// Returns an error when the mount cannot be accessed, identity evidence
    /// is malformed or contradictory, or a detected database fails structural
    /// or integrity validation.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_impl(path.as_ref(), false)
    }

    pub(crate) fn open_during_transaction(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_impl(path.as_ref(), true)
    }

    fn open_impl(path: &Path, allow_transaction: bool) -> Result<Self> {
        let mount = MountRoot::open(path)?;
        if !allow_transaction {
            if let Some(path) = crate::edit::pending_transaction(&mount)? {
                return Err(crate::Error::RecoveryRequired { path });
            }
        }
        let evidence = IdentityEvidence::read_from(&mount)?;
        let profile = profile::resolve(&evidence)?;
        let signing_guid = evidence.firewire_guid();
        let inspection =
            DeviceInspection::read_from(&mount, profile.as_ref(), signing_guid.as_ref())?;
        let library = read_library(&mount, profile.as_ref())?;
        let generation = GenerationFingerprint::capture(&mount, profile.as_ref())?;
        Ok(Self {
            mount,
            evidence,
            profile,
            inspection,
            library,
            generation,
        })
    }

    /// Returns the canonical mount root.
    #[must_use]
    pub fn mount(&self) -> &MountRoot {
        &self.mount
    }

    /// Returns redaction-safe identity and capability evidence.
    #[must_use]
    pub fn evidence(&self) -> &IdentityEvidence {
        &self.evidence
    }

    /// Returns a known device profile, or `None` when evidence is incomplete.
    #[must_use]
    pub fn profile(&self) -> Option<&DeviceProfile> {
        self.profile.as_ref()
    }

    /// Returns read-only structural database and artwork information.
    #[must_use]
    pub fn inspection(&self) -> &DeviceInspection {
        &self.inspection
    }

    /// Returns the normalized library when a read adapter is available.
    #[must_use]
    pub fn library(&self) -> Option<&Library> {
        self.library.as_ref()
    }

    /// Returns the database and artwork generation captured at open time.
    #[must_use]
    pub const fn generation(&self) -> &GenerationFingerprint {
        &self.generation
    }

    /// Builds a byte-identical host bundle for exercising transaction safety.
    ///
    /// This method never modifies the device, and its output must not be
    /// installed manually.
    ///
    /// # Errors
    ///
    /// Returns an error if staging preconditions, backups, fingerprints, or
    /// output validation fail.
    pub fn stage_noop_preview(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<crate::StagedSqliteEdit> {
        EditSession::new(self)?.stage_noop_preview(destination)
    }

    /// Installs a byte-identical Nano 7G bundle as a hardware safety test.
    ///
    /// This qualification gate creates verified on-device backups and a
    /// transaction journal, replaces only the seven database companion files
    /// with byte-identical copies, reads the library back, and rolls back a
    /// reported failure. It does not add, remove, or alter music or artwork.
    /// Use [`crate::recover_interrupted_transaction`] after a power loss.
    ///
    /// # Errors
    ///
    /// Returns an error before mutation unless `staged` is a verified no-op
    /// bundle for this exact open generation and `confirmation` exactly equals
    /// [`crate::NANO7_NOOP_HARDWARE_TEST_CONFIRMATION`].
    pub fn install_noop_hardware_test(
        &self,
        staged: &crate::StagedSqliteEdit,
        confirmation: &str,
    ) -> Result<()> {
        crate::edit::install_noop_hardware_test(self, staged, confirmation)
    }

    /// Installs one staged no-artwork removal as a Nano 7G hardware test.
    ///
    /// The database transaction is recoverable and read back before success.
    /// The corresponding media file remains on the device as an orphan, and
    /// no artwork file changes. This is a qualification gate, not general sync.
    ///
    /// # Errors
    ///
    /// Returns an error before mutation unless the bundle removes exactly one
    /// track without artwork from this generation and `confirmation` exactly
    /// equals [`crate::NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION`].
    pub fn install_single_removal_hardware_test(
        &self,
        staged: &crate::StagedSqliteEdit,
        confirmation: &str,
    ) -> Result<()> {
        crate::edit::install_single_removal_hardware_test(self, staged, confirmation)
    }

    /// Starts an in-memory edit session without modifying the device.
    ///
    /// # Errors
    ///
    /// Returns an error when the detected backend or required signing evidence
    /// is not supported by the current staged-preview implementation.
    pub fn edit(&self) -> Result<EditSession<'_>> {
        EditSession::new(self)
    }

    /// Resolves a track location while rejecting symlink escapes.
    ///
    /// # Errors
    ///
    /// Returns an error if the media file is missing, inaccessible, or resolves
    /// outside the opened mount root.
    pub fn track_path(&self, track: &Track) -> Result<PathBuf> {
        self.mount.resolve_existing(&track.location)
    }
}

fn read_library(mount: &MountRoot, profile: Option<&DeviceProfile>) -> Result<Option<Library>> {
    if !profile.is_some_and(|profile| {
        profile.capabilities().backend == BackendKind::SqliteWithBinaryCompanion
    }) {
        return Ok(None);
    }
    let library = IpodPath::new("iPod_Control/iTunes/iTunes Library.itlp/Library.itdb")?;
    let locations = IpodPath::new("iPod_Control/iTunes/iTunes Library.itlp/Locations.itdb")?;
    if !mount.contains(&library)? || !mount.contains(&locations)? {
        return Ok(None);
    }
    Library::read_sqlite(
        &mount.resolve_existing(&library)?,
        &mount.resolve_existing(&locations)?,
    )
    .map(Some)
}
