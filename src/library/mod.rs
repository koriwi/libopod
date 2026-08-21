use std::{collections::HashMap, path::Path};

use rusqlite::{Connection, OpenFlags};

use crate::{Error, IpodPath, Result};

/// A stable 64-bit persistent identifier stored by the iPod databases.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PersistentId(u64);

impl PersistentId {
    /// Creates an ID from its complete on-disk bit pattern.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the complete on-disk bit pattern.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.0
    }
}

/// Read-only normalized track metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub id: PersistentId,
    pub location: IpodPath,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub size: u64,
    pub duration_ms: u32,
    pub track_number: u32,
    pub disc_number: u32,
    pub has_artwork: bool,
}

/// A playlist and its ordered track membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    pub id: PersistentId,
    pub name: String,
    pub parent_id: Option<PersistentId>,
    pub distinguished_kind: i64,
    pub is_hidden: bool,
    pub is_smart: bool,
    track_ids: Vec<PersistentId>,
}

impl Playlist {
    /// Returns track IDs in playlist order.
    #[must_use]
    pub fn track_ids(&self) -> &[PersistentId] {
        &self.track_ids
    }
}

/// A read-only normalized iPod music library.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Library {
    tracks: Vec<Track>,
    playlists: Vec<Playlist>,
}

impl Library {
    /// Returns all music tracks in stable database order.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Returns the number of music tracks.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Returns playlists and other containers in stable database order.
    #[must_use]
    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    /// Builds the normalized library from an uncompressed classic iTunesDB.
    ///
    /// `artworkdb` optionally resolves per-track artwork presence from the
    /// device's `ArtworkDB`; track locations and playlist membership come from
    /// the binary itself.
    pub(crate) fn read_binary(itunesdb: &[u8], artworkdb: Option<&[u8]>) -> Result<Self> {
        let classic = crate::storage::binary::parse_library(itunesdb, artworkdb)?;
        let track_id_to_persistent: HashMap<u32, PersistentId> = classic
            .tracks
            .iter()
            .map(|track| (track.track_id, track.persistent_id))
            .collect();
        let tracks = classic
            .tracks
            .iter()
            .map(|track| {
                let location =
                    IpodPath::new(track.location.clone()).map_err(|error| Error::Malformed {
                        format: "classic iTunesDB",
                        offset: 0,
                        reason: format!("invalid track location: {error}"),
                    })?;
                Ok(Track {
                    id: track.persistent_id,
                    location,
                    title: track.title.clone(),
                    album: track.album.clone(),
                    artist: track.artist.clone(),
                    album_artist: track.album_artist.clone(),
                    size: track.size,
                    duration_ms: track.duration_ms,
                    track_number: track.track_number,
                    disc_number: track.disc_number,
                    has_artwork: track.has_artwork,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let playlists = classic
            .playlists
            .iter()
            .map(|playlist| Playlist {
                id: playlist.id,
                name: playlist.name.clone(),
                parent_id: None,
                distinguished_kind: 0,
                is_hidden: playlist.is_hidden,
                is_smart: playlist.is_smart,
                track_ids: playlist
                    .track_ids
                    .iter()
                    .filter_map(|track_id| track_id_to_persistent.get(track_id).copied())
                    .collect(),
            })
            .collect();
        Ok(Library { tracks, playlists })
    }

    pub(crate) fn read_sqlite(library_path: &Path, locations_path: &Path) -> Result<Self> {
        let locations = read_locations(locations_path)?;
        let connection = open_read_only(library_path)?;
        let mut statement = connection
            .prepare(
                "SELECT pid, COALESCE(title, ''), COALESCE(album, ''), \
                 COALESCE(artist, ''), COALESCE(album_artist, ''), \
                 COALESCE(total_time_ms, 0), COALESCE(track_number, 0), \
                 COALESCE(disc_number, 0), COALESCE(artwork_status, 0), \
                 COALESCE(artwork_cache_id, 0) \
                 FROM item WHERE is_song = 1 OR media_kind = 1 \
                 ORDER BY COALESCE(physical_order, 0), pid",
            )
            .map_err(|source| sqlite_error("prepare track query", library_path, source))?;
        let rows = statement
            .query_map([], |row| {
                Ok(RawTrack {
                    pid: row.get(0)?,
                    title: row.get(1)?,
                    album: row.get(2)?,
                    artist: row.get(3)?,
                    album_artist: row.get(4)?,
                    duration_ms: row.get(5)?,
                    track_number: row.get(6)?,
                    disc_number: row.get(7)?,
                    artwork_status: row.get(8)?,
                    artwork_cache_id: row.get(9)?,
                })
            })
            .map_err(|source| sqlite_error("query tracks", library_path, source))?;

        let mut tracks = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| sqlite_error("read track row", library_path, source))?;
            let location = locations.get(&raw.pid).ok_or_else(|| Error::Malformed {
                format: "SQLite iPod library",
                offset: 0,
                reason: "a song item has no primary location".to_owned(),
            })?;
            tracks.push(Track {
                id: PersistentId::from_bits(u64::from_ne_bytes(raw.pid.to_ne_bytes())),
                location: location.path.clone(),
                title: raw.title,
                album: raw.album,
                artist: raw.artist,
                album_artist: raw.album_artist,
                size: location.size,
                duration_ms: clamp_u32(raw.duration_ms),
                track_number: clamp_u32(raw.track_number),
                disc_number: clamp_u32(raw.disc_number),
                has_artwork: raw.artwork_status != 0 || raw.artwork_cache_id != 0,
            });
        }
        let playlists = read_playlists(&connection, library_path)?;
        Ok(Self { tracks, playlists })
    }
}

fn read_playlists(connection: &Connection, path: &Path) -> Result<Vec<Playlist>> {
    let mut memberships = HashMap::<i64, Vec<PersistentId>>::new();
    let mut membership_statement = connection
        .prepare(
            "SELECT container_pid, item_pid FROM item_to_container \
             ORDER BY container_pid, COALESCE(physical_order, 0), rowid",
        )
        .map_err(|source| sqlite_error("prepare playlist membership query", path, source))?;
    let membership_rows = membership_statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|source| sqlite_error("query playlist memberships", path, source))?;
    for row in membership_rows {
        let (container_pid, item_pid) =
            row.map_err(|source| sqlite_error("read playlist membership", path, source))?;
        memberships
            .entry(container_pid)
            .or_default()
            .push(PersistentId::from_bits(u64::from_ne_bytes(
                item_pid.to_ne_bytes(),
            )));
    }
    drop(membership_statement);

    let mut statement = connection
        .prepare(
            "SELECT pid, COALESCE(name, ''), COALESCE(parent_pid, 0), \
             COALESCE(distinguished_kind, 0), COALESCE(is_hidden, 0), \
             smart_criteria IS NOT NULL OR COALESCE(smart_is_dynamic, 0) != 0 \
             OR COALESCE(smart_is_filtered, 0) != 0 \
             FROM container ORDER BY COALESCE(name_order, 0), pid",
        )
        .map_err(|source| sqlite_error("prepare playlist query", path, source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
            ))
        })
        .map_err(|source| sqlite_error("query playlists", path, source))?;
    let mut playlists = Vec::new();
    for row in rows {
        let (pid, name, parent_pid, distinguished_kind, is_hidden, is_smart) =
            row.map_err(|source| sqlite_error("read playlist", path, source))?;
        playlists.push(Playlist {
            id: PersistentId::from_bits(u64::from_ne_bytes(pid.to_ne_bytes())),
            name,
            parent_id: (parent_pid != 0)
                .then(|| PersistentId::from_bits(u64::from_ne_bytes(parent_pid.to_ne_bytes()))),
            distinguished_kind,
            is_hidden: is_hidden != 0,
            is_smart,
            track_ids: memberships.remove(&pid).unwrap_or_default(),
        });
    }
    Ok(playlists)
}

struct RawTrack {
    pid: i64,
    title: String,
    album: String,
    artist: String,
    album_artist: String,
    duration_ms: f64,
    track_number: i64,
    disc_number: i64,
    artwork_status: i64,
    artwork_cache_id: i64,
}

struct Location {
    path: IpodPath,
    size: u64,
}

fn read_locations(path: &Path) -> Result<HashMap<i64, Location>> {
    let connection = open_read_only(path)?;
    let mut statement = connection
        .prepare(
            "SELECT location.item_pid, COALESCE(base_location.path, ''), \
             COALESCE(location.location, ''), COALESCE(location.file_size, 0) \
             FROM location LEFT JOIN base_location \
             ON base_location.id = location.base_location_id \
             WHERE location.sub_id = 0",
        )
        .map_err(|source| sqlite_error("prepare location query", path, source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|source| sqlite_error("query locations", path, source))?;

    let mut locations = HashMap::new();
    for row in rows {
        let (pid, base, relative, size) =
            row.map_err(|source| sqlite_error("read location row", path, source))?;
        let combined = match (base.trim_matches('/'), relative.trim_matches('/')) {
            ("", "") => String::new(),
            ("", relative) => relative.to_owned(),
            (base, "") => base.to_owned(),
            (base, relative) => format!("{base}/{relative}"),
        };
        let location = IpodPath::new(combined).map_err(|error| Error::Malformed {
            format: "Locations.itdb",
            offset: 0,
            reason: format!("invalid track location: {error}"),
        })?;
        if locations
            .insert(
                pid,
                Location {
                    path: location,
                    size: u64::try_from(size).unwrap_or(0),
                },
            )
            .is_some()
        {
            return Err(Error::Malformed {
                format: "Locations.itdb",
                offset: 0,
                reason: "duplicate primary location for one item".to_owned(),
            });
        }
    }
    Ok(locations)
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| sqlite_error("open read-only database", path, source))
}

fn sqlite_error(operation: &'static str, path: &Path, source: rusqlite::Error) -> Error {
    Error::Sqlite {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn clamp_u32<T>(value: T) -> u32
where
    T: ClampU32,
{
    value.clamp_u32()
}

trait ClampU32 {
    fn clamp_u32(self) -> u32;
}

impl ClampU32 for i64 {
    fn clamp_u32(self) -> u32 {
        u32::try_from(self.max(0)).unwrap_or(u32::MAX)
    }
}

impl ClampU32 for f64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn clamp_u32(self) -> u32 {
        if !self.is_finite() || self <= 0.0 {
            0
        } else if self >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            self as u32
        }
    }
}
