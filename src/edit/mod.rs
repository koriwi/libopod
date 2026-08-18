mod add;
mod commit;
mod generation;
mod manifest;
pub(crate) mod sort;

pub use generation::{FileFingerprint, GenerationFingerprint};

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, Transaction};

pub(crate) use add::{
    add_tracks_to_staged_databases, ArtworkFrameOut, ArtworkLink, ResolvedAddition,
};
pub(crate) use commit::{
    install_artwork_addition_hardware_test, install_noop_hardware_test,
    install_single_addition_hardware_test, install_single_artwork_removal_hardware_test,
    install_single_removal_hardware_test, pending_transaction,
};

use self::generation::fingerprint_host_file;
use self::manifest::{back_up_generation, write_staging_manifest};
use crate::{
    error::io_error,
    fs::read_limited,
    storage::{
        binary::{
            add_track_to_cdb, build_hashab_cbk, remove_tracks_from_cdb, verify_cbk, CdbArtworkLink,
            CdbTrackAddition,
        },
        sqlite::inspect_sqlite_database,
    },
    BackendKind, Device, Error, IpodPath, Library, MountRoot, PersistentId, Result,
    SqliteLibraryFile,
};

/// Exact acknowledgement required by the Nano 7G no-op hardware write gate.
pub const NANO7_NOOP_HARDWARE_TEST_CONFIRMATION: &str = commit::NOOP_CONFIRMATION;

/// Exact acknowledgement required by the single no-artwork removal gate.
pub const NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION: &str = commit::REMOVAL_CONFIRMATION;

/// Exact acknowledgement required by the single artwork-bearing removal gate.
pub const NANO7_ARTWORK_REMOVAL_HARDWARE_TEST_CONFIRMATION: &str =
    commit::ARTWORK_REMOVAL_CONFIRMATION;

/// Exact acknowledgement required by the single no-artwork addition gate.
pub const NANO7_ADDITION_HARDWARE_TEST_CONFIRMATION: &str = commit::ADDITION_CONFIRMATION;

/// Exact acknowledgement required by the reused-album-art addition gate.
pub const NANO7_ARTWORK_REUSE_ADDITION_CONFIRMATION: &str =
    commit::ARTWORK_REUSE_ADDITION_CONFIRMATION;

/// Exact acknowledgement required by the new-cover-art addition gate.
pub const NANO7_NEW_ART_ADDITION_CONFIRMATION: &str = commit::NEW_ART_ADDITION_CONFIRMATION;

/// Metadata for one track to add to the staged library.
///
/// The source file is copied into the staging bundle and never modified.
#[derive(Clone, Debug)]
pub struct TrackToAdd {
    /// Host path of the audio file to stage (must exist and be readable).
    pub source_path: PathBuf,
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist: Option<String>,
    /// Album name.
    pub album: Option<String>,
    /// Album artist (falls back to `artist`).
    pub album_artist: Option<String>,
    /// Genre name.
    pub genre: Option<String>,
    /// Composer name.
    pub composer: Option<String>,
    /// Release year.
    pub year: u32,
    /// Track number within the album.
    pub track_number: u32,
    /// Total tracks on the album.
    pub total_tracks: u32,
    /// Disc number.
    pub disc_number: u32,
    /// Total discs.
    pub total_discs: u32,
    /// Bitrate in kbps.
    pub bitrate: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Duration in milliseconds.
    pub length_ms: u32,
    /// Whether this track belongs to a compilation album.
    pub compilation: bool,
    /// When true, a track joining an album that already has on-device artwork
    /// inherits that album's `.ithmb` slots (no image decoding or encoding).
    pub reuse_album_art: bool,
    /// When set, the JPEG/PNG file at this path is encoded into all four
    /// Nano 7G cover formats and written into new `.ithmb` slots.
    pub artwork_source: Option<PathBuf>,
}

const ITLP_PATH: &str = "iPod_Control/iTunes/iTunes Library.itlp";
const MAX_LOCATIONS_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CDB_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARTWORK_BYTES: u64 = 64 * 1024 * 1024;
const SQLITE_FILES: [SqliteLibraryFile; 5] = [
    SqliteLibraryFile::Library,
    SqliteLibraryFile::Locations,
    SqliteLibraryFile::Dynamic,
    SqliteLibraryFile::Extras,
    SqliteLibraryFile::Genius,
];

/// An in-memory set of requested library changes.
///
/// Sessions never modify the opened mount. The current preview staging method
/// writes only to a separate, empty host directory and does not create a
/// committable device update yet.
#[derive(Debug)]
pub struct EditSession<'device> {
    device: &'device Device,
    removals: BTreeSet<PersistentId>,
    additions: Vec<TrackToAdd>,
}

impl<'device> EditSession<'device> {
    pub(crate) fn new(device: &'device Device) -> Result<Self> {
        let profile = device.profile().ok_or_else(|| Error::Unsupported {
            feature: "edit sessions",
            reason: "the device profile is unknown".to_owned(),
        })?;
        if profile.capabilities().backend != BackendKind::SqliteWithBinaryCompanion {
            return Err(Error::Unsupported {
                feature: "edit sessions",
                reason: "only the staged SQLite preview backend is implemented".to_owned(),
            });
        }
        if device.library().is_none() {
            return Err(Error::Unsupported {
                feature: "edit sessions",
                reason: "the authoritative SQLite library is unavailable".to_owned(),
            });
        }
        if !device.evidence().has_firewire_guid() {
            return Err(Error::Unsupported {
                feature: "HASHAB staging",
                reason: "the required signing identity is unavailable".to_owned(),
            });
        }
        Ok(Self {
            device,
            removals: BTreeSet::new(),
            additions: Vec::new(),
        })
    }

    /// Queues a track for removal without changing any files.
    ///
    /// Returns `true` when this call added a new removal and `false` when the
    /// same track was already queued.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TrackNotFound`] when `id` is absent from the opened
    /// music library.
    pub fn remove_track(&mut self, id: PersistentId) -> Result<bool> {
        let exists = self
            .device
            .library()
            .is_some_and(|library| library.tracks().iter().any(|track| track.id == id));
        if !exists {
            return Err(Error::TrackNotFound);
        }
        Ok(self.removals.insert(id))
    }

    /// Returns the number of unique queued removals.
    #[must_use]
    pub fn removal_count(&self) -> usize {
        self.removals.len()
    }

    /// Queues a track for addition without changing any files.
    ///
    /// The host source file is only read during staging, when it is copied
    /// into the preview bundle. Queuing does not modify anything.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidStagingDirectory`] when the source path does
    /// not exist or is not a regular file.
    pub fn add_track(&mut self, track: TrackToAdd) -> Result<()> {
        let metadata = fs::metadata(&track.source_path)
            .map_err(|source| io_error("inspect track source", &track.source_path, source))?;
        if !metadata.is_file() {
            return Err(Error::InvalidStagingDirectory {
                path: track.source_path.clone(),
                reason: "track source must be a regular file".to_owned(),
            });
        }
        if track.title.trim().is_empty() || track.sample_rate == 0 {
            return Err(Error::InvalidStagingDirectory {
                path: track.source_path.clone(),
                reason: "track requires a non-empty title and a valid sample rate".to_owned(),
            });
        }
        self.additions.push(track);
        Ok(())
    }

    /// Returns the number of queued additions.
    #[must_use]
    pub fn addition_count(&self) -> usize {
        self.additions.len()
    }

    /// Builds a schema-preserving `SQLite` removal preview in an empty host
    /// directory.
    ///
    /// All five existing `SQLite` databases are copied before modification.
    /// Track rows, playlist memberships, direct companion references, derived
    /// counts, physical ordering, and `Locations.itdb.cbk` are updated on those
    /// copies. The matching `iTunesCDB` track, playlist, sorted-index, and jump
    /// table records are removed before recompression and HASHAB signing.
    /// Unknown `SQLite` schema objects and unrelated binary chunks remain in
    /// place. The result is reparsed and integrity checked.
    ///
    /// The host bundle includes verified copies of every generation input and
    /// a durable manifest. It deliberately excludes `ArtworkDB` mutation,
    /// media-file deletion, an on-device transaction journal, and recovery.
    /// It is therefore **not safe to install on an iPod**.
    ///
    /// # Errors
    ///
    /// Returns an error if the destination does not exist, is not empty, is
    /// inside the opened mount, required schemas differ, a database operation
    /// fails, or output validation fails.
    pub fn stage_sqlite_preview(&self, destination: impl AsRef<Path>) -> Result<StagedSqliteEdit> {
        if self.removals.is_empty() && self.additions.is_empty() {
            return Err(Error::Unsupported {
                feature: "empty edit preview",
                reason: "queue at least one removal or addition before staging".to_owned(),
            });
        }
        self.stage_preview(destination.as_ref())
    }

    pub(crate) fn stage_noop_preview(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<StagedSqliteEdit> {
        self.stage_preview(destination.as_ref())
    }

    fn stage_preview(&self, destination: &Path) -> Result<StagedSqliteEdit> {
        self.device
            .generation()
            .verify_unchanged(self.device.mount(), self.device.profile())?;
        let destination = validate_destination(self.device, destination)?;
        back_up_generation(self.device, &destination, self.device.generation())?;
        let source = self
            .device
            .mount()
            .resolve_existing(&IpodPath::new(ITLP_PATH)?)?;
        copy_sqlite_set(&source, &destination)?;

        let before = self.device.library().ok_or_else(|| Error::Unsupported {
            feature: "SQLite staging",
            reason: "the source library is unavailable".to_owned(),
        })?;
        let resolved = self.resolve_additions(&destination)?;
        if !self.removals.is_empty() {
            edit_staged_databases(&destination, &self.removals)?;
        }
        if !resolved.is_empty() {
            add_tracks_to_staged_databases(&destination, &resolved)?;
        }

        let guid = self
            .device
            .evidence()
            .firewire_guid()
            .ok_or_else(|| Error::Unsupported {
                feature: "HASHAB staging",
                reason: "the required signing identity is unavailable".to_owned(),
            })?;
        if self.removals.is_empty() && resolved.is_empty() {
            copy_unchanged_companions(&source, self.device, &destination)?;
        } else {
            write_and_verify_cbk(&destination, guid)?;
            if !self.removals.is_empty() {
                write_cdb_preview(self.device, &destination, guid, &self.removals)?;
                write_artwork_preview(self.device, &destination, &self.removals)?;
            }
            if !resolved.is_empty() {
                write_cdb_additions(self.device, &destination, guid, &resolved)?;
                write_artwork_additions(self.device, &destination, &resolved)?;
                write_artwork_frames(self.device, &destination, &resolved)?;
            }
        }
        validate_staged_set(&destination)?;

        let after = Library::read_sqlite(
            &destination.join(SqliteLibraryFile::Library.file_name()),
            &destination.join(SqliteLibraryFile::Locations.file_name()),
        )?;
        validate_semantics(before, &after, &self.removals, &resolved)?;
        self.device
            .generation()
            .verify_unchanged(self.device.mount(), self.device.profile())?;
        let removed_tracks: Vec<_> = before
            .tracks()
            .iter()
            .filter(|track| self.removals.contains(&track.id))
            .collect();
        let removed_media = removed_tracks
            .iter()
            .map(|track| track.location.clone())
            .collect();
        let removed_artwork_tracks = removed_tracks
            .iter()
            .filter(|track| track.has_artwork)
            .count();
        let added_targets: Vec<IpodPath> = resolved
            .iter()
            .map(|addition| {
                IpodPath::new(format!("iPod_Control/Music/{}", addition.media_relative))
            })
            .collect::<Result<_>>()?;
        let manifest = write_staging_manifest(
            self.device,
            &destination,
            self.device.generation(),
            self.removals.len(),
            &added_targets,
        )?;
        Ok(StagedSqliteEdit {
            directory: destination,
            removed_tracks: self.removals.len(),
            added_tracks: resolved.len(),
            added_artwork_tracks: resolved.iter().filter(|add| add.artwork.is_some()).count(),
            added_ithmb: added_ithmb_targets(&resolved)?,
            remaining_tracks: after.track_count(),
            removed_media,
            removed_artwork_tracks,
            added_media: added_targets,
            source_generation: self.device.generation().clone(),
            manifest,
        })
    }

    /// Allocates media paths, stages copies of each source file, and assigns
    /// persistent IDs and timestamps for every queued addition.
    fn resolve_additions(&self, destination: &Path) -> Result<Vec<ResolvedAddition>> {
        if self.additions.is_empty() {
            return Ok(Vec::new());
        }
        let existing_pids: BTreeSet<PersistentId> =
            self.device.library().map_or_else(BTreeSet::new, |library| {
                library.tracks().iter().map(|track| track.id).collect()
            });
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let date_coredata = i64::try_from(now.saturating_add(978_307_200)).unwrap_or(i64::MAX);
        let date_mac = u32::try_from(now.saturating_add(2_082_844_800)).unwrap_or(u32::MAX);
        let mut resolved = Vec::with_capacity(self.additions.len());
        let mut running_sizes = std::collections::HashMap::new();
        let mut next_image_id = base_image_id(self.device)?;
        for track in &self.additions {
            let (media_relative, _staged_path) =
                allocate_media_path(self.device, destination, &track.source_path)?;
            let metadata = fs::metadata(&track.source_path)
                .map_err(|source| io_error("inspect track source", &track.source_path, source))?;
            let pid = generate_unique_pid(&existing_pids);
            let artwork = if track.reuse_album_art && track.artwork_source.is_some() {
                return Err(Error::InvalidStagingDirectory {
                    path: track.source_path.clone(),
                    reason: "reuse_album_art and artwork_source are mutually exclusive".to_owned(),
                });
            } else if track.reuse_album_art {
                resolve_reused_artwork(self.device, track, &mut next_image_id)?
            } else if track.artwork_source.is_some() {
                resolve_new_artwork(self.device, track, &mut running_sizes, &mut next_image_id)?
            } else {
                None
            };
            resolved.push(ResolvedAddition {
                pid,
                title: track.title.trim().to_owned(),
                artist: track.artist.as_ref().map(|value| value.trim().to_owned()),
                album: track.album.as_ref().map(|value| value.trim().to_owned()),
                album_artist: track
                    .album_artist
                    .as_ref()
                    .map(|value| value.trim().to_owned()),
                genre: track.genre.as_ref().map(|value| value.trim().to_owned()),
                composer: track.composer.as_ref().map(|value| value.trim().to_owned()),
                year: track.year,
                track_number: track.track_number,
                total_tracks: track.total_tracks,
                disc_number: track.disc_number,
                total_discs: track.total_discs,
                bitrate: track.bitrate,
                sample_rate: track.sample_rate,
                length_ms: track.length_ms,
                compilation: track.compilation,
                file_size: metadata.len(),
                date_coredata,
                media_relative,
                artwork,
                date_mac,
            });
        }
        Ok(resolved)
    }
}

/// Validated output from a SQLite/CDB edit preview.
///
/// This output is intentionally incomplete and cannot be committed to a
/// device until artwork and on-device recovery support is added.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedSqliteEdit {
    directory: PathBuf,
    removed_tracks: usize,
    added_tracks: usize,
    added_artwork_tracks: usize,
    added_ithmb: Vec<IpodPath>,
    remaining_tracks: usize,
    removed_media: Vec<IpodPath>,
    removed_artwork_tracks: usize,
    added_media: Vec<IpodPath>,
    source_generation: GenerationFingerprint,
    manifest: PathBuf,
}

impl StagedSqliteEdit {
    /// Returns the host directory containing staged database files.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the number of removed tracks.
    #[must_use]
    pub const fn removed_tracks(&self) -> usize {
        self.removed_tracks
    }

    /// Returns the number of queued additions in this preview.
    #[must_use]
    pub const fn added_tracks(&self) -> usize {
        self.added_tracks
    }

    /// Returns how many additions inherit reused album artwork slots.
    #[must_use]
    pub const fn added_artwork_tracks(&self) -> usize {
        self.added_artwork_tracks
    }

    /// Returns the `.ithmb` frame files that a completed commit would extend.
    #[must_use]
    pub fn added_ithmb(&self) -> &[IpodPath] {
        &self.added_ithmb
    }

    /// Returns media targets that a future completed commit would install.
    #[must_use]
    pub fn added_media(&self) -> &[IpodPath] {
        &self.added_media
    }

    /// Returns the remaining music-track count after reparsing.
    #[must_use]
    pub const fn remaining_tracks(&self) -> usize {
        self.remaining_tracks
    }

    /// Returns media paths that a future completed commit could delete only
    /// after installing and validating all database companions.
    #[must_use]
    pub fn removed_media(&self) -> &[IpodPath] {
        &self.removed_media
    }

    /// Returns how many removals still require `ArtworkDB` handling.
    #[must_use]
    pub const fn removed_artwork_tracks(&self) -> usize {
        self.removed_artwork_tracks
    }

    /// Returns the exact source generation that must still match before any
    /// future installation attempt.
    #[must_use]
    pub const fn source_generation(&self) -> &GenerationFingerprint {
        &self.source_generation
    }

    /// Returns the durable, self-verified host staging manifest.
    #[must_use]
    pub fn manifest(&self) -> &Path {
        &self.manifest
    }
}

/// Recovers or cleans up an interrupted libopod transaction.
///
/// This operation restores verified on-device backups when installation did
/// not reach its committed state. A committed but interrupted cleanup only
/// removes the verified transaction directory.
///
/// # Errors
///
/// Returns an error without changing live files if the journal, volume
/// identity inputs, backups, or current interrupted state do not verify.
pub fn recover_interrupted_transaction(path: impl AsRef<Path>) -> Result<bool> {
    let mount = MountRoot::open(path)?;
    if commit::pending_transaction(&mount)?.is_none() {
        return Ok(false);
    }
    commit::recover_transaction(&mount)?;
    Ok(true)
}

#[allow(clippy::too_many_lines)]
/// Chooses the least-populated `Music/Fxx` directory, stages a verified copy
/// of the source MP3 inside the bundle, and returns the relative media path
/// (`Fxx/NAME.mp3`) plus the bundle-relative staged path.
fn allocate_media_path(
    device: &Device,
    destination: &Path,
    source_path: &Path,
) -> Result<(String, String)> {
    let music_relative = IpodPath::new("iPod_Control/Music")?;
    let music = device.mount().resolve_existing(&music_relative)?;
    let mut directories = Vec::new();
    for entry in
        fs::read_dir(&music).map_err(|source| io_error("read Music directory", &music, source))?
    {
        let entry = entry.map_err(|source| io_error("read Music entry", &music, source))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && name.len() == 3
            && name.starts_with('F')
            && name[1..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            directories.push(entry.path());
        }
    }
    directories.sort();
    if directories.is_empty() {
        return Err(Error::Unsupported {
            feature: "media allocation",
            reason: "the device has no Music/Fxx media directories".to_owned(),
        });
    }
    let directory = directories
        .into_iter()
        .min_by_key(|path| fs::read_dir(path).map_or(usize::MAX, std::iter::Iterator::count))
        .ok_or_else(|| Error::Verification {
            format: "media allocation",
            reason: "no media directory could be selected".to_owned(),
        })?;
    let folder = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Verification {
            format: "media allocation",
            reason: "media directory name is not UTF-8".to_owned(),
        })?
        .to_owned();
    let existing: BTreeSet<String> = fs::read_dir(&directory)
        .map_err(|source| io_error("read media directory", &directory, source))?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| name.to_ascii_uppercase())
        .collect();
    let extension = source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    if extension != "mp3" {
        return Err(Error::Unsupported {
            feature: "media allocation",
            reason: "only MP3 sources are supported by the first addition gate".to_owned(),
        });
    }
    let mut name = None;
    for _ in 0..64 {
        let candidate = random_media_name();
        if !existing.contains(&candidate) {
            name = Some(candidate);
            break;
        }
    }
    let name = name.ok_or_else(|| Error::Verification {
        format: "media allocation",
        reason: "could not find a free media filename".to_owned(),
    })?;
    let staged_dir = destination.join("iPod_Control").join("Music").join(&folder);
    fs::create_dir_all(&staged_dir)
        .map_err(|source| io_error("create staged media directory", &staged_dir, source))?;
    let file_name = format!("{name}.{extension}");
    let staged_file = staged_dir.join(&file_name);
    fs::copy(source_path, &staged_file)
        .map_err(|source| io_error("stage media file", &staged_file, source))?;
    let (expected_bytes, expected_digest) = fingerprint_host_file(source_path)?;
    let (actual_bytes, actual_digest) = fingerprint_host_file(&staged_file)?;
    if actual_bytes != expected_bytes || actual_digest != expected_digest {
        let _ = fs::remove_file(&staged_file);
        return Err(Error::Verification {
            format: "staged media file",
            reason: "staged copy did not verify against its source".to_owned(),
        });
    }
    if actual_bytes == 0 {
        let _ = fs::remove_file(&staged_file);
        return Err(Error::Verification {
            format: "staged media file",
            reason: "the source audio file is empty".to_owned(),
        });
    }
    Ok((
        format!("{folder}/{file_name}"),
        format!("iPod_Control/Music/{folder}/{file_name}"),
    ))
}

fn random_media_name() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut state = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    )
    .unwrap_or(u64::MAX)
        ^ 0x9e37_79b9_7f4a_7c15;
    let mut output = String::with_capacity(4);
    for _ in 0..4 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        output.push(char::from(ALPHABET[(state % 36) as usize]));
    }
    output
}

fn generate_unique_pid(existing: &BTreeSet<PersistentId>) -> PersistentId {
    let mut state = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    )
    .unwrap_or(u64::MAX)
        ^ 0xd1b5_4a32_d192_ed03;
    loop {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let candidate = PersistentId::from_bits(state);
        if candidate.to_bits() != 0 && !existing.contains(&candidate) {
            return candidate;
        }
    }
}

/// Finds an album-mate with on-device artwork and builds a reused-artwork
/// link for a new track: a fresh `mhii` image ID plus the album-mate's slot
/// references copied verbatim.
fn resolve_reused_artwork(
    device: &Device,
    track: &TrackToAdd,
    next_image_id: &mut u32,
) -> Result<Option<ArtworkLink>> {
    let album = track.album.as_deref().unwrap_or("");
    let album_artist = track
        .album_artist
        .as_deref()
        .or(track.artist.as_deref())
        .unwrap_or("");
    let mate = device
        .library()
        .and_then(|library| {
            library.tracks().iter().find(|candidate| {
                candidate.has_artwork
                    && candidate.album == album
                    && (if candidate.album_artist.is_empty() {
                        candidate.artist.clone()
                    } else {
                        candidate.album_artist.clone()
                    }) == album_artist
            })
        })
        .cloned();
    let Some(mate) = mate else {
        return Ok(None);
    };
    let relative = IpodPath::new("iPod_Control/Artwork/ArtworkDB")?;
    let artwork_path = device.mount().resolve_existing(&relative)?;
    let bytes = read_limited(&artwork_path, MAX_ARTWORK_BYTES, "ArtworkDB")?;
    let records = crate::artwork::parse_artwork_records(&bytes)?;
    let record = records
        .iter()
        .find(|record| record.track_id == mate.id)
        .ok_or_else(|| Error::Verification {
            format: "artwork reuse",
            reason: "album-mate track has no ArtworkDB record".to_owned(),
        })?;
    let (mhod_children, child_count) = crate::artwork::build_reused_children(&record.formats);
    let image_id = *next_image_id;
    *next_image_id = next_image_id.saturating_add(1);
    Ok(Some(ArtworkLink {
        image_id,
        src_img_size: record.src_img_size,
        child_count,
        mhod_children,
        frames: Vec::new(),
    }))
}

/// The next free `mhii` image ID on the device.
fn base_image_id(device: &Device) -> Result<u32> {
    let relative = IpodPath::new("iPod_Control/Artwork/ArtworkDB")?;
    let artwork_path = device.mount().resolve_existing(&relative)?;
    let bytes = read_limited(&artwork_path, MAX_ARTWORK_BYTES, "ArtworkDB")?;
    let records = crate::artwork::parse_artwork_records(&bytes)?;
    records
        .iter()
        .map(|record| record.image_id)
        .max()
        .unwrap_or(99)
        .checked_add(1)
        .ok_or_else(|| Error::Verification {
            format: "artwork encoding",
            reason: "image ID overflow".to_owned(),
        })
}

/// Decodes a source image, encodes the four Nano 7G cover formats, and
/// allocates a fresh `.ithmb` slot after the running end of each format file.
fn resolve_new_artwork(
    device: &Device,
    track: &TrackToAdd,
    running_sizes: &mut std::collections::HashMap<String, u64>,
    next_image_id: &mut u32,
) -> Result<Option<ArtworkLink>> {
    let source = track.artwork_source.as_ref().expect("checked by caller");
    let source_bytes =
        fs::read(source).map_err(|error| io_error("read artwork source", source, error))?;
    if source_bytes.is_empty() {
        return Err(Error::InvalidStagingDirectory {
            path: source.clone(),
            reason: "artwork source is empty".to_owned(),
        });
    }
    let frames = crate::artwork::encode_new_frames(&source_bytes)?;
    let mut out_frames = Vec::with_capacity(frames.len());
    let mut refs = Vec::with_capacity(frames.len());
    for frame in &frames {
        let relative = IpodPath::new(format!("iPod_Control/Artwork/{}", frame.filename))?;
        let path = device.mount().resolve_existing(&relative)?;
        let device_size = fs::metadata(&path)
            .map_err(|error| io_error("inspect artwork frame file", &path, error))?
            .len();
        let slot_bytes = frame.slot_bytes();
        if device_size % slot_bytes != 0 {
            return Err(Error::Verification {
                format: "artwork encoding",
                reason: format!("{} is not a whole number of slots", frame.filename),
            });
        }
        let running = running_sizes
            .entry(frame.filename.to_owned())
            .or_insert(device_size);
        let offset = u32::try_from(*running).map_err(|_| Error::Verification {
            format: "artwork encoding",
            reason: "ithmb file exceeds 4 GiB".to_owned(),
        })?;
        *running = running.saturating_add(slot_bytes);
        refs.push(crate::artwork::ArtworkFormatRef {
            format_id: frame.format_id,
            ithmb_offset: offset,
            image_size: u32::try_from(slot_bytes).unwrap_or(u32::MAX),
            width: u16::try_from(frame.width).unwrap_or(u16::MAX),
            height: u16::try_from(frame.height).unwrap_or(u16::MAX),
            filename: format!(":{}", frame.filename),
        });
        out_frames.push(ArtworkFrameOut {
            filename: frame.filename.to_owned(),
            ithmb_offset: offset,
            frame: frame.rgb565.clone(),
        });
    }
    let (mhod_children, child_count) = crate::artwork::build_reused_children(&refs);
    let image_id = *next_image_id;
    *next_image_id = next_image_id.saturating_add(1);
    Ok(Some(ArtworkLink {
        image_id,
        src_img_size: u32::try_from(source_bytes.len()).unwrap_or(u32::MAX),
        child_count,
        mhod_children,
        frames: out_frames,
    }))
}

fn added_ithmb_targets(resolved: &[ResolvedAddition]) -> Result<Vec<IpodPath>> {
    let mut targets = Vec::new();
    for addition in resolved {
        if let Some(artwork) = &addition.artwork {
            for frame in &artwork.frames {
                let target = IpodPath::new(format!("iPod_Control/Artwork/{}", frame.filename))?;
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
    }
    Ok(targets)
}

fn validate_destination(device: &Device, supplied: &Path) -> Result<PathBuf> {
    let destination = fs::canonicalize(supplied)
        .map_err(|source| io_error("canonicalize staging directory", supplied, source))?;
    let metadata = fs::metadata(&destination)
        .map_err(|source| io_error("inspect staging directory", &destination, source))?;
    if !metadata.is_dir() {
        return invalid_staging(supplied, "path is not a directory");
    }
    if destination.starts_with(device.mount().as_path()) {
        return invalid_staging(
            supplied,
            "directory is inside the opened iPod mount; use separate host storage",
        );
    }
    let mut entries = fs::read_dir(&destination)
        .map_err(|source| io_error("read staging directory", &destination, source))?;
    if entries
        .next()
        .transpose()
        .map_err(|source| {
            io_error(
                "inspect staging directory entry",
                destination.clone(),
                source,
            )
        })?
        .is_some()
    {
        return invalid_staging(supplied, "directory must be empty");
    }
    Ok(destination)
}

fn invalid_staging<T>(path: &Path, reason: &str) -> Result<T> {
    Err(Error::InvalidStagingDirectory {
        path: path.to_path_buf(),
        reason: reason.to_owned(),
    })
}

fn copy_sqlite_set(source: &Path, destination: &Path) -> Result<()> {
    for file in SQLITE_FILES {
        let source_file = source.join(file.file_name());
        let destination_file = destination.join(file.file_name());
        fs::copy(&source_file, &destination_file)
            .map_err(|error| io_error("copy SQLite file to staging", source_file, error))?;
    }
    Ok(())
}

fn copy_unchanged_companions(
    itlp_source: &Path,
    device: &Device,
    destination: &Path,
) -> Result<()> {
    let cbk_source = itlp_source.join("Locations.itdb.cbk");
    let cbk_output = destination.join("Locations.itdb.cbk");
    fs::copy(&cbk_source, &cbk_output)
        .map_err(|source| io_error("copy unchanged CBK to staging", &cbk_source, source))?;
    let cdb_relative = IpodPath::new("iPod_Control/iTunes/iTunesCDB")?;
    let cdb_source = device.mount().resolve_existing(&cdb_relative)?;
    let cdb_output = destination.join("iTunesCDB");
    fs::copy(&cdb_source, &cdb_output)
        .map_err(|source| io_error("copy unchanged CDB to staging", &cdb_source, source))?;
    Ok(())
}

fn edit_staged_databases(directory: &Path, removals: &BTreeSet<PersistentId>) -> Result<()> {
    let library_path = directory.join(SqliteLibraryFile::Library.file_name());
    let mut connection = Connection::open_with_flags(
        &library_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| sqlite_error("open staged library", &library_path, source))?;
    attach(
        &connection,
        directory,
        SqliteLibraryFile::Locations,
        "locations",
    )?;
    attach(
        &connection,
        directory,
        SqliteLibraryFile::Dynamic,
        "dynamic",
    )?;
    attach(&connection, directory, SqliteLibraryFile::Extras, "extras")?;
    attach(&connection, directory, SqliteLibraryFile::Genius, "genius")?;

    let transaction = connection
        .transaction()
        .map_err(|source| sqlite_error("begin staged edit", &library_path, source))?;
    prepare_removal_tables(&transaction, removals, &library_path)?;
    delete_direct_references(&transaction, &library_path)?;
    update_derived_rows(&transaction, &library_path)?;
    validate_relational_invariants(&transaction, &library_path)?;
    transaction
        .commit()
        .map_err(|source| sqlite_error("commit staged edit", &library_path, source))?;
    Ok(())
}

fn attach(
    connection: &Connection,
    directory: &Path,
    file: SqliteLibraryFile,
    schema: &'static str,
) -> Result<()> {
    let path = directory.join(file.file_name());
    let text = path
        .to_str()
        .ok_or_else(|| Error::InvalidStagingDirectory {
            path: directory.to_path_buf(),
            reason: "SQLite preview currently requires a UTF-8 host path".to_owned(),
        })?;
    let sql = format!("ATTACH DATABASE ?1 AS {schema}");
    connection
        .execute(&sql, [text])
        .map_err(|source| sqlite_error("attach staged companion", &path, source))?;
    Ok(())
}

fn prepare_removal_tables(
    transaction: &Transaction<'_>,
    removals: &BTreeSet<PersistentId>,
    path: &Path,
) -> Result<()> {
    transaction
        .execute_batch(
            "CREATE TEMP TABLE opod_removed (pid INTEGER PRIMARY KEY);\
             CREATE TEMP TABLE opod_album AS SELECT album_pid AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_artist AS SELECT artist_pid AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_track_artist AS SELECT track_artist_pid AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_composer AS SELECT composer_pid AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_genre AS SELECT genre_id AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_category AS SELECT category_id AS id FROM item WHERE 0;\
             CREATE TEMP TABLE opod_genius AS SELECT genius_id AS id FROM item WHERE 0;",
        )
        .map_err(|source| sqlite_error("create staged removal tables", path, source))?;
    for id in removals {
        let stored = i64::from_ne_bytes(id.to_bits().to_ne_bytes());
        transaction
            .execute("INSERT INTO temp.opod_removed (pid) VALUES (?1)", [stored])
            .map_err(|source| sqlite_error("queue staged removal ID", path, source))?;
    }
    let found: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM item JOIN temp.opod_removed USING (pid)",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("validate staged removal IDs", path, source))?;
    if usize::try_from(found).ok() != Some(removals.len()) {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "one or more queued tracks disappeared from the source generation".to_owned(),
        });
    }
    transaction
        .execute_batch(
            "INSERT INTO temp.opod_album SELECT DISTINCT album_pid FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_artist SELECT DISTINCT artist_pid FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_track_artist SELECT DISTINCT track_artist_pid FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_composer SELECT DISTINCT composer_pid FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_genre SELECT DISTINCT genre_id FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_category SELECT DISTINCT category_id FROM item JOIN temp.opod_removed USING(pid);\
             INSERT INTO temp.opod_genius SELECT DISTINCT genius_id FROM item JOIN temp.opod_removed USING(pid);",
        )
        .map_err(|source| sqlite_error("capture affected aggregate IDs", path, source))?;
    Ok(())
}

fn delete_direct_references(transaction: &Transaction<'_>, path: &Path) -> Result<()> {
    transaction
        .execute_batch(
            "DELETE FROM avformat_info WHERE item_pid IN temp.opod_removed;\
             DELETE FROM item_to_container WHERE item_pid IN temp.opod_removed;\
             DELETE FROM container_seed WHERE item_pid IN temp.opod_removed;\
             DELETE FROM video_info WHERE item_pid IN temp.opod_removed;\
             DELETE FROM video_characteristics WHERE item_pid IN temp.opod_removed;\
             DELETE FROM podcast_info WHERE item_pid IN temp.opod_removed;\
             DELETE FROM store_info WHERE item_pid IN temp.opod_removed;\
             DELETE FROM locations.location WHERE item_pid IN temp.opod_removed;\
             DELETE FROM dynamic.item_stats WHERE item_pid IN temp.opod_removed;\
             DELETE FROM dynamic.rental_info WHERE item_pid IN temp.opod_removed;\
             DELETE FROM extras.chapter WHERE item_pid IN temp.opod_removed;\
             DELETE FROM extras.lyrics WHERE item_pid IN temp.opod_removed;\
             DELETE FROM item WHERE pid IN temp.opod_removed;\
             DELETE FROM genius.genius_metadata WHERE genius_id IN temp.opod_genius \
               AND genius_id != 0 AND NOT EXISTS (SELECT 1 FROM item WHERE item.genius_id=genius_metadata.genius_id);\
             DELETE FROM genius.genius_similarities WHERE genius_id IN temp.opod_genius \
               AND genius_id != 0 AND NOT EXISTS (SELECT 1 FROM item WHERE item.genius_id=genius_similarities.genius_id);",
        )
        .map_err(|source| sqlite_error("delete staged track references", path, source))?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn update_derived_rows(transaction: &Transaction<'_>, path: &Path) -> Result<()> {
    transaction
        .execute_batch(
            "UPDATE album SET \
               item_count=(SELECT COUNT(*) FROM item WHERE item.album_pid=album.pid),\
               has_songs=EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND is_song!=0),\
               has_music_videos=EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND is_music_video!=0),\
               has_movies=EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND is_movie!=0),\
               has_any_compilations=EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND is_compilation!=0),\
               all_compilations=NOT EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND COALESCE(is_compilation,0)=0),\
               artwork_item_pid=COALESCE((SELECT pid FROM item WHERE item.album_pid=album.pid AND COALESCE(artwork_status,0)!=0 ORDER BY COALESCE(physical_order,0),pid LIMIT 1),0),\
               artwork_status=EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid AND COALESCE(artwork_status,0)!=0),\
               min_volume_normalization_energy=COALESCE((SELECT MIN(volume_normalization_energy) FROM avformat_info JOIN item ON item.pid=avformat_info.item_pid WHERE item.album_pid=album.pid),0)\
             WHERE pid IN temp.opod_album AND EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid);\
             DELETE FROM album WHERE pid IN temp.opod_album AND pid!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.album_pid=album.pid);\
             UPDATE artist SET \
               has_songs=EXISTS(SELECT 1 FROM item WHERE item.artist_pid=artist.pid AND is_song!=0),\
               has_music_videos=EXISTS(SELECT 1 FROM item WHERE item.artist_pid=artist.pid AND is_music_video!=0),\
               has_non_compilation_tracks=EXISTS(SELECT 1 FROM item WHERE item.artist_pid=artist.pid AND COALESCE(is_compilation,0)=0),\
               album_count=(SELECT COUNT(DISTINCT album_pid) FROM item WHERE item.artist_pid=artist.pid),\
               artwork_album_pid=COALESCE((SELECT pid FROM album WHERE album.artist_pid=artist.pid AND COALESCE(artwork_status,0)!=0 ORDER BY COALESCE(name_order,0),pid LIMIT 1),0),\
               artwork_status=EXISTS(SELECT 1 FROM album WHERE album.artist_pid=artist.pid AND COALESCE(artwork_status,0)!=0)\
             WHERE pid IN temp.opod_artist AND EXISTS(SELECT 1 FROM item WHERE item.artist_pid=artist.pid);\
             DELETE FROM artist WHERE pid IN temp.opod_artist AND pid!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.artist_pid=artist.pid);\
             UPDATE track_artist SET \
               has_songs=EXISTS(SELECT 1 FROM item WHERE item.track_artist_pid=track_artist.pid AND is_song!=0),\
               has_music_videos=EXISTS(SELECT 1 FROM item WHERE item.track_artist_pid=track_artist.pid AND is_music_video!=0),\
               has_non_compilation_tracks=EXISTS(SELECT 1 FROM item WHERE item.track_artist_pid=track_artist.pid AND COALESCE(is_compilation,0)=0)\
             WHERE pid IN temp.opod_track_artist AND EXISTS(SELECT 1 FROM item WHERE item.track_artist_pid=track_artist.pid);\
             DELETE FROM track_artist WHERE pid IN temp.opod_track_artist AND pid!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.track_artist_pid=track_artist.pid);\
             UPDATE composer SET has_music=EXISTS(SELECT 1 FROM item WHERE item.composer_pid=composer.pid)\
             WHERE pid IN temp.opod_composer;\
             DELETE FROM composer WHERE pid IN temp.opod_composer AND pid!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.composer_pid=composer.pid);\
             UPDATE genre_map SET \
               has_music=EXISTS(SELECT 1 FROM item WHERE item.genre_id=genre_map.id),\
               artist_count_calc=(SELECT COUNT(DISTINCT artist_pid) FROM item WHERE item.genre_id=genre_map.id),\
               album_artist_count_calc=(SELECT COUNT(DISTINCT artist_pid) FROM item WHERE item.genre_id=genre_map.id),\
               album_count_calc=(SELECT COUNT(DISTINCT album_pid) FROM item WHERE item.genre_id=genre_map.id),\
               compilation_count_calc=(SELECT COUNT(DISTINCT album_pid) FROM item WHERE item.genre_id=genre_map.id AND is_compilation!=0)\
             WHERE id IN temp.opod_genre;\
             DELETE FROM genre_map WHERE id IN temp.opod_genre AND id!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.genre_id=genre_map.id);\
             DELETE FROM category_map WHERE id IN temp.opod_category AND id!=0 AND NOT EXISTS(SELECT 1 FROM item WHERE item.category_id=category_map.id);\
             UPDATE track_size_calc SET size=CASE kind \
               WHEN 'audio' THEN COALESCE((SELECT SUM(file_size) FROM item JOIN locations.location ON location.item_pid=item.pid AND location.sub_id=0 WHERE media_kind NOT IN (2,32,64)),0)\
               WHEN 'video' THEN COALESCE((SELECT SUM(file_size) FROM item JOIN locations.location ON location.item_pid=item.pid AND location.sub_id=0 WHERE media_kind IN (2,64)),0)\
               WHEN 'music_video' THEN COALESCE((SELECT SUM(file_size) FROM item JOIN locations.location ON location.item_pid=item.pid AND location.sub_id=0 WHERE media_kind=32),0)\
               ELSE size END;\
             WITH ranked AS (SELECT pid,ROW_NUMBER() OVER (ORDER BY COALESCE(physical_order,0),pid)-1 AS value FROM item)\
             UPDATE item SET physical_order=(SELECT value FROM ranked WHERE ranked.pid=item.pid);\
             WITH ranked AS (SELECT rowid AS rid,ROW_NUMBER() OVER (PARTITION BY container_pid ORDER BY COALESCE(physical_order,0),rowid)-1 AS value FROM item_to_container)\
             UPDATE item_to_container SET physical_order=(SELECT value FROM ranked WHERE ranked.rid=item_to_container.rowid);\
             WITH ranked AS (SELECT rowid AS rid,ROW_NUMBER() OVER (PARTITION BY container_pid ORDER BY shuffle_order,rowid)-1 AS value FROM item_to_container WHERE shuffle_order IS NOT NULL)\
             UPDATE item_to_container SET shuffle_order=(SELECT value FROM ranked WHERE ranked.rid=item_to_container.rowid) WHERE shuffle_order IS NOT NULL;",
        )
        .map_err(|source| sqlite_error("update staged derived rows", path, source))?;
    Ok(())
}

fn validate_relational_invariants(transaction: &Transaction<'_>, path: &Path) -> Result<()> {
    let violations: i64 = transaction
        .query_row(
            "SELECT\
               (SELECT COUNT(*) FROM item JOIN temp.opod_removed USING(pid)) +\
               (SELECT COUNT(*) FROM locations.location WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM dynamic.item_stats WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM extras.chapter WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM extras.lyrics WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM item_to_container WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM container_seed WHERE item_pid IN temp.opod_removed) +\
               (SELECT COUNT(*) FROM (SELECT COUNT(*) n,MIN(physical_order) lo,MAX(physical_order) hi,COUNT(DISTINCT physical_order) d FROM item HAVING lo!=0 OR hi!=n-1 OR d!=n)) +\
               (SELECT COUNT(*) FROM (SELECT container_pid,COUNT(*) n,MIN(physical_order) lo,MAX(physical_order) hi,COUNT(DISTINCT physical_order) d FROM item_to_container GROUP BY container_pid HAVING lo!=0 OR hi!=n-1 OR d!=n))",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("validate staged relational invariants", path, source))?;
    if violations != 0 {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "removed references or ordering invariant violations remain".to_owned(),
        });
    }
    Ok(())
}

fn write_and_verify_cbk(directory: &Path, guid: [u8; 8]) -> Result<()> {
    let locations_path = directory.join(SqliteLibraryFile::Locations.file_name());
    let locations = read_limited(&locations_path, MAX_LOCATIONS_BYTES, "Locations.itdb")?;
    let cbk = build_hashab_cbk(&locations, guid);
    let cbk_path = directory.join("Locations.itdb.cbk");
    let mut file = File::create(&cbk_path)
        .map_err(|source| io_error("create staged Locations CBK", &cbk_path, source))?;
    file.write_all(&cbk)
        .map_err(|source| io_error("write staged Locations CBK", &cbk_path, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staged Locations CBK", &cbk_path, source))?;
    let info = verify_cbk(&locations, &cbk, Some(&guid))?;
    if !info.digests_match() || info.hashab_signature_matches != Some(true) {
        return Err(Error::Verification {
            format: "Locations.itdb.cbk",
            reason: "generated CBK did not verify".to_owned(),
        });
    }
    Ok(())
}

fn write_cdb_preview(
    device: &Device,
    directory: &Path,
    guid: [u8; 8],
    removals: &BTreeSet<PersistentId>,
) -> Result<()> {
    let relative = IpodPath::new("iPod_Control/iTunes/iTunesCDB")?;
    let source = device.mount().resolve_existing(&relative)?;
    let bytes = read_limited(&source, MAX_CDB_BYTES, "iTunesCDB")?;
    let requested: Vec<_> = removals.iter().copied().collect();
    let rewritten = remove_tracks_from_cdb(&bytes, guid, &requested)?;
    let output = directory.join("iTunesCDB");
    let mut file = File::create(&output)
        .map_err(|source| io_error("create staged iTunesCDB", &output, source))?;
    file.write_all(&rewritten)
        .map_err(|source| io_error("write staged iTunesCDB", &output, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staged iTunesCDB", &output, source))?;
    Ok(())
}

fn write_cdb_additions(
    device: &Device,
    directory: &Path,
    guid: [u8; 8],
    additions: &[ResolvedAddition],
) -> Result<()> {
    let relative = IpodPath::new("iPod_Control/iTunes/iTunesCDB")?;
    let source = device.mount().resolve_existing(&relative)?;
    let mut bytes = read_limited(&source, MAX_CDB_BYTES, "iTunesCDB")?;
    for addition in additions {
        let cdb_addition = CdbTrackAddition {
            persistent_id: addition.pid,
            location: format!(":iPod_Control:Music:{}", addition.media_relative),
            title: addition.title.clone(),
            artist: addition.artist.clone(),
            album: addition.album.clone(),
            album_artist: addition.album_artist.clone(),
            genre: addition.genre.clone(),
            composer: addition.composer.clone(),
            file_size: u32::try_from(addition.file_size).map_err(|_| Error::Verification {
                format: "staged CDB addition",
                reason: "media file exceeds 4 GiB".to_owned(),
            })?,
            length_ms: addition.length_ms,
            bitrate: addition.bitrate,
            sample_rate: addition.sample_rate,
            track_number: addition.track_number,
            total_tracks: addition.total_tracks,
            disc_number: addition.disc_number,
            total_discs: addition.total_discs,
            year: addition.year,
            compilation: addition.compilation,
            date_mac: addition.date_mac,
            artwork: addition.artwork.as_ref().map(|art| CdbArtworkLink {
                image_id: art.image_id,
                src_img_size: art.src_img_size,
            }),
        };
        bytes = add_track_to_cdb(&bytes, guid, &cdb_addition)?;
    }
    let output = directory.join("iTunesCDB");
    let mut file = File::create(&output)
        .map_err(|source| io_error("create staged iTunesCDB", &output, source))?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write staged iTunesCDB", &output, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staged iTunesCDB", &output, source))?;
    Ok(())
}

/// Removes the `ArtworkDB` records of every removed track that has one.
///
/// A no-op when none of the removals reference artwork, so no-artwork
/// removals keep an unchanged `ArtworkDB`. `.ithmb` slot payloads are left in
/// place as unreferenced data.
fn write_artwork_preview(
    device: &Device,
    directory: &Path,
    removals: &BTreeSet<PersistentId>,
) -> Result<()> {
    let relative = IpodPath::new("iPod_Control/Artwork/ArtworkDB")?;
    let source = device.mount().resolve_existing(&relative)?;
    let bytes = read_limited(&source, MAX_ARTWORK_BYTES, "ArtworkDB")?;
    let records = crate::artwork::parse_artwork_records(&bytes)?;
    let requested: Vec<PersistentId> = records
        .iter()
        .map(|record| record.track_id)
        .filter(|track_id| removals.contains(track_id))
        .collect();
    if requested.is_empty() {
        return Ok(());
    }
    let rewritten = crate::artwork::remove_tracks_from_artworkdb(&bytes, &requested)?;
    let remaining = crate::artwork::parse_artwork_records(&rewritten)?;
    if remaining
        .iter()
        .any(|record| removals.contains(&record.track_id))
    {
        return Err(Error::Verification {
            format: "staged ArtworkDB",
            reason: "removed artwork records remain in the rewritten ArtworkDB".to_owned(),
        });
    }
    let output = directory.join("ArtworkDB");
    let mut file = File::create(&output)
        .map_err(|source| io_error("create staged ArtworkDB", &output, source))?;
    file.write_all(&rewritten)
        .map_err(|source| io_error("write staged ArtworkDB", &output, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staged ArtworkDB", &output, source))?;
    Ok(())
}

/// Appends a reused-artwork `mhii` record for every added track that
/// inherits an existing album's slots. A no-op when nothing was reused.
fn write_artwork_additions(
    device: &Device,
    directory: &Path,
    additions: &[ResolvedAddition],
) -> Result<()> {
    let linked: Vec<&ResolvedAddition> = additions
        .iter()
        .filter(|addition| addition.artwork.is_some())
        .collect();
    if linked.is_empty() {
        return Ok(());
    }
    let relative = IpodPath::new("iPod_Control/Artwork/ArtworkDB")?;
    let source = device.mount().resolve_existing(&relative)?;
    let bytes = read_limited(&source, MAX_ARTWORK_BYTES, "ArtworkDB")?;
    let records: Vec<crate::artwork::NewArtworkRecord> = linked
        .iter()
        .map(|addition| {
            let artwork = addition.artwork.as_ref().expect("filtered above");
            crate::artwork::NewArtworkRecord {
                image_id: artwork.image_id,
                track_id: addition.pid,
                src_img_size: artwork.src_img_size,
                child_count: artwork.child_count,
                mhod_children: artwork.mhod_children.clone(),
            }
        })
        .collect();
    let rewritten = crate::artwork::append_artwork_records(&bytes, &records)?;
    let remaining = crate::artwork::parse_artwork_records(&rewritten)?;
    for addition in &linked {
        let artwork = addition.artwork.as_ref().expect("filtered above");
        if !remaining
            .iter()
            .any(|record| record.track_id == addition.pid)
        {
            return Err(Error::Verification {
                format: "staged ArtworkDB",
                reason: "an added artwork record is missing from the rewritten ArtworkDB"
                    .to_owned(),
            });
        }
        let record = remaining
            .iter()
            .find(|record| record.track_id == addition.pid)
            .expect("checked above");
        if record.image_id != artwork.image_id {
            return Err(Error::Verification {
                format: "staged ArtworkDB",
                reason: "an added artwork record has the wrong image ID".to_owned(),
            });
        }
    }
    let output = directory.join("ArtworkDB");
    let mut file = File::create(&output)
        .map_err(|source| io_error("create staged ArtworkDB", &output, source))?;
    file.write_all(&rewritten)
        .map_err(|source| io_error("write staged ArtworkDB", &output, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staged ArtworkDB", &output, source))?;
    Ok(())
}

/// Appends the encoded `.ithmb` frames of every addition with new artwork.
/// Each frame lands at the slot offset fixed during resolution.
fn write_artwork_frames(
    device: &Device,
    directory: &Path,
    additions: &[ResolvedAddition],
) -> Result<()> {
    let mut frames = Vec::new();
    for addition in additions {
        if let Some(artwork) = &addition.artwork {
            frames.extend(artwork.frames.iter().cloned());
        }
    }
    if frames.is_empty() {
        return Ok(());
    }
    for frame in frames {
        let relative = IpodPath::new(format!("iPod_Control/Artwork/{}", frame.filename))?;
        let output = directory
            .join("iPod_Control")
            .join("Artwork")
            .join(&frame.filename);
        // Chain against the already-staged file when present so multiple
        // additions append into distinct slots.
        let base = if output.exists() {
            fs::read(&output)
                .map_err(|error| io_error("read staged artwork frame", &output, error))?
        } else {
            let path = device.mount().resolve_existing(&relative)?;
            fs::read(&path).map_err(|error| io_error("read artwork frame file", &path, error))?
        };
        if u64::try_from(base.len()).unwrap_or(u64::MAX) != u64::from(frame.ithmb_offset) {
            return Err(Error::Verification {
                format: "staged artwork frames",
                reason: format!("{} changed since slot allocation", frame.filename),
            });
        }
        let mut updated = base;
        updated.extend_from_slice(&frame.frame);
        let output = directory
            .join("iPod_Control")
            .join("Artwork")
            .join(&frame.filename);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error("create staged artwork directory", parent, error))?;
        }
        let mut file = File::create(&output)
            .map_err(|error| io_error("create staged artwork frame", &output, error))?;
        file.write_all(&updated)
            .map_err(|error| io_error("write staged artwork frame", &output, error))?;
        file.sync_all()
            .map_err(|error| io_error("flush staged artwork frame", &output, error))?;
    }
    Ok(())
}

fn validate_staged_set(directory: &Path) -> Result<()> {
    for file in SQLITE_FILES {
        let info = inspect_sqlite_database(&directory.join(file.file_name()), file)?;
        if !info.integrity_ok {
            return Err(Error::Verification {
                format: "staged SQLite database",
                reason: format!("{} failed integrity_check", file.file_name()),
            });
        }
    }
    let artwork = directory.join("ArtworkDB");
    if artwork.exists() {
        let bytes = read_limited(&artwork, MAX_ARTWORK_BYTES, "ArtworkDB")?;
        crate::artwork::inspect_artwork_db(&bytes)?;
    }
    Ok(())
}

fn validate_semantics(
    before: &Library,
    after: &Library,
    removals: &BTreeSet<PersistentId>,
    additions: &[ResolvedAddition],
) -> Result<()> {
    let expected_tracks = before
        .track_count()
        .saturating_sub(removals.len())
        .saturating_add(additions.len());
    if after.track_count() != expected_tracks
        || after
            .tracks()
            .iter()
            .any(|track| removals.contains(&track.id))
    {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "reparsed track set differs from the requested edit".to_owned(),
        });
    }
    for addition in additions {
        let track = after.tracks().iter().find(|track| track.id == addition.pid);
        let Some(track) = track else {
            return Err(Error::Verification {
                format: "staged SQLite edit",
                reason: "an added track is missing from the reparsed library".to_owned(),
            });
        };
        if track.location.as_str() != format!("iPod_Control/Music/{}", addition.media_relative)
            || track.size != addition.file_size
        {
            return Err(Error::Verification {
                format: "staged SQLite edit",
                reason: "an added track's location or size does not match its staging".to_owned(),
            });
        }
    }
    if before.playlists().len() != after.playlists().len() {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "playlist containers were not preserved".to_owned(),
        });
    }
    let mut added_in_playlists = 0_usize;
    for (old, new) in before.playlists().iter().zip(after.playlists()) {
        let expected: Vec<_> = old
            .track_ids()
            .iter()
            .copied()
            .filter(|id| !removals.contains(id))
            .collect();
        if old.id != new.id
            || old.name != new.name
            || old.parent_id != new.parent_id
            || old.distinguished_kind != new.distinguished_kind
            || old.is_hidden != new.is_hidden
            || old.is_smart != new.is_smart
        {
            return Err(Error::Verification {
                format: "staged SQLite edit",
                reason: "playlist metadata was not preserved".to_owned(),
            });
        }
        let mut expected_with_additions = expected.clone();
        expected_with_additions.extend(additions.iter().map(|addition| addition.pid));
        if new.track_ids() == expected_with_additions {
            added_in_playlists += additions.len();
        } else if new.track_ids() != expected {
            return Err(Error::Verification {
                format: "staged SQLite edit",
                reason: "playlist membership was not preserved".to_owned(),
            });
        }
    }
    if added_in_playlists != additions.len() {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "added tracks were not joined to the master playlist".to_owned(),
        });
    }
    Ok(())
}

fn sqlite_error(operation: &'static str, path: &Path, source: rusqlite::Error) -> Error {
    Error::Sqlite {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

include!("tests.rs");
