mod commit;
mod generation;
mod manifest;

pub use generation::{FileFingerprint, GenerationFingerprint};

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, Transaction};

pub(crate) use commit::{install_noop_hardware_test, pending_transaction};

use self::manifest::{back_up_generation, write_staging_manifest};
use crate::{
    error::io_error,
    fs::read_limited,
    storage::{
        binary::{build_hashab_cbk, remove_tracks_from_cdb, verify_cbk},
        sqlite::inspect_sqlite_database,
    },
    BackendKind, Device, Error, IpodPath, Library, MountRoot, PersistentId, Result,
    SqliteLibraryFile,
};

/// Exact acknowledgement required by the Nano 7G no-op hardware write gate.
pub const NANO7_NOOP_HARDWARE_TEST_CONFIRMATION: &str = commit::NOOP_CONFIRMATION;

const ITLP_PATH: &str = "iPod_Control/iTunes/iTunes Library.itlp";
const MAX_LOCATIONS_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CDB_BYTES: u64 = 512 * 1024 * 1024;
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
        if self.removals.is_empty() {
            return Err(Error::Unsupported {
                feature: "empty edit preview",
                reason: "queue at least one removal before staging".to_owned(),
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
        if !self.removals.is_empty() {
            edit_staged_databases(&destination, &self.removals)?;
        }

        let guid = self
            .device
            .evidence()
            .firewire_guid()
            .ok_or_else(|| Error::Unsupported {
                feature: "HASHAB staging",
                reason: "the required signing identity is unavailable".to_owned(),
            })?;
        if self.removals.is_empty() {
            copy_unchanged_companions(&source, self.device, &destination)?;
        } else {
            write_and_verify_cbk(&destination, guid)?;
            write_cdb_preview(self.device, &destination, guid, &self.removals)?;
        }
        validate_staged_set(&destination)?;

        let after = Library::read_sqlite(
            &destination.join(SqliteLibraryFile::Library.file_name()),
            &destination.join(SqliteLibraryFile::Locations.file_name()),
        )?;
        validate_semantics(before, &after, &self.removals)?;
        self.device
            .generation()
            .verify_unchanged(self.device.mount(), self.device.profile())?;
        let manifest = write_staging_manifest(
            self.device,
            &destination,
            self.device.generation(),
            self.removals.len(),
        )?;

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
        Ok(StagedSqliteEdit {
            directory: destination,
            removed_tracks: self.removals.len(),
            remaining_tracks: after.track_count(),
            removed_media,
            removed_artwork_tracks,
            source_generation: self.device.generation().clone(),
            manifest,
        })
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
    remaining_tracks: usize,
    removed_media: Vec<IpodPath>,
    removed_artwork_tracks: usize,
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
    Ok(())
}

fn validate_semantics(
    before: &Library,
    after: &Library,
    removals: &BTreeSet<PersistentId>,
) -> Result<()> {
    let expected_tracks = before.track_count().saturating_sub(removals.len());
    if after.track_count() != expected_tracks
        || after
            .tracks()
            .iter()
            .any(|track| removals.contains(&track.id))
    {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "reparsed track set differs from the requested removal".to_owned(),
        });
    }
    if before.playlists().len() != after.playlists().len() {
        return Err(Error::Verification {
            format: "staged SQLite edit",
            reason: "playlist containers were not preserved".to_owned(),
        });
    }
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
            || expected != new.track_ids()
        {
            return Err(Error::Verification {
                format: "staged SQLite edit",
                reason: "playlist metadata or membership was not preserved".to_owned(),
            });
        }
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
