//! Schema-preserving track insertion into the staged `SQLite` databases.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};

use super::sort::sort_key;
use crate::{Error, PersistentId, Result, SqliteLibraryFile};

/// A fully resolved staged track addition: metadata, persistent ID, media
/// target, and timestamps are fixed before any database write.
#[derive(Clone, Debug)]
pub(crate) struct ResolvedAddition {
    pub pid: PersistentId,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub year: u32,
    pub track_number: u32,
    pub total_tracks: u32,
    pub disc_number: u32,
    pub total_discs: u32,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub length_ms: u32,
    pub compilation: bool,
    pub file_size: u64,
    pub date_coredata: i64,
    pub date_mac: u32,
    pub media_relative: String,
    /// Set when the added track inherits an existing album's artwork slots.
    pub artwork: Option<ArtworkLink>,
}

/// A reused artwork link: a fresh `mhii` image ID plus the album-mate's slot
/// references and source image size.
#[derive(Clone, Debug)]
pub(crate) struct ArtworkLink {
    pub image_id: u32,
    pub src_img_size: u32,
    pub child_count: u32,
    pub mhod_children: Vec<u8>,
}

pub(crate) fn add_tracks_to_staged_databases(
    directory: &Path,
    additions: &[ResolvedAddition],
) -> Result<()> {
    if additions.is_empty() {
        return Ok(());
    }
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
    let transaction = connection
        .transaction()
        .map_err(|source| sqlite_error("begin staged add", &library_path, source))?;
    for addition in additions {
        insert_track(&transaction, addition, &library_path)?;
    }
    validate_added_invariants(&transaction, additions, &library_path)?;
    transaction
        .commit()
        .map_err(|source| sqlite_error("commit staged add", &library_path, source))?;
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

fn insert_track(
    transaction: &Transaction<'_>,
    addition: &ResolvedAddition,
    path: &Path,
) -> Result<()> {
    let pid = i64::from_ne_bytes(addition.pid.to_bits().to_ne_bytes());
    if pid == 0 {
        return Err(Error::Verification {
            format: "staged SQLite add",
            reason: "a persistent ID of zero is invalid".to_owned(),
        });
    }
    let duplicates: i64 = transaction
        .query_row("SELECT COUNT(*) FROM item WHERE pid = ?1", [pid], |row| {
            row.get(0)
        })
        .map_err(|source| sqlite_error("check staged persistent ID", path, source))?;
    if duplicates != 0 {
        return Err(Error::Verification {
            format: "staged SQLite add",
            reason: "the generated persistent ID collides with an existing track".to_owned(),
        });
    }

    let mut next_pid = next_pid(transaction, path)?;
    let album_artist = addition
        .album_artist
        .clone()
        .or_else(|| addition.artist.clone());
    let artist_pid = resolve_named_entity(
        transaction,
        path,
        "artist",
        album_artist.as_deref(),
        &mut next_pid,
        &[
            "pid, kind, artwork_status, artwork_album_pid, name, name_order, sort_name, \
             is_unknown, has_songs, has_music_videos, has_non_compilation_tracks, album_count",
            "VALUES (?1, 2, 0, 0, ?2, ?3, ?4, 0, 1, 0, 1, 1)",
        ],
    )?;
    let album_pid = resolve_album(
        transaction,
        path,
        addition.album.as_deref(),
        artist_pid,
        &mut next_pid,
    )?;
    let track_artist_pid = resolve_named_entity(
        transaction,
        path,
        "track_artist",
        addition.artist.as_deref(),
        &mut next_pid,
        &[
            "pid, name, name_order, sort_name, has_songs, has_music_videos, \
             has_non_compilation_tracks, is_unknown, album_count",
            "VALUES (?1, ?2, ?3, ?4, 1, 0, 1, 0, 0)",
        ],
    )?;
    let composer_pid = resolve_named_entity(
        transaction,
        path,
        "composer",
        addition.composer.as_deref(),
        &mut next_pid,
        &[
            "pid, name, name_order, sort_name, is_unknown, has_music",
            "VALUES (?1, ?2, ?3, ?4, 0, 1)",
        ],
    )?;
    let genre_id = resolve_genre(transaction, path, addition.genre.as_deref())?;

    let orders = compute_orders(transaction, path, addition)?;
    insert_item_row(
        transaction,
        path,
        addition,
        pid,
        album_pid,
        artist_pid,
        track_artist_pid,
        composer_pid,
        genre_id,
        &orders,
    )?;
    insert_avformat_row(transaction, path, addition, pid)?;
    insert_location_row(transaction, path, addition, pid)?;
    insert_container_row(transaction, path, pid)?;
    update_derived_rows(
        transaction,
        path,
        addition,
        album_pid,
        artist_pid,
        track_artist_pid,
        composer_pid,
        genre_id,
    )?;
    Ok(())
}

fn next_pid(transaction: &Transaction<'_>, path: &Path) -> Result<i64> {
    let max: Option<i64> = transaction
        .query_row(
            "SELECT MAX(pid) FROM (SELECT pid FROM album UNION ALL SELECT pid FROM artist \
             UNION ALL SELECT pid FROM track_artist UNION ALL SELECT pid FROM composer)",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("read shared PID counter", path, source))?;
    Ok(max.unwrap_or(0).saturating_add(1))
}

/// Looks up a named entity row (`artist`/`track_artist`/`composer`) by exact name,
/// creating it with the shared counter when absent.
fn resolve_named_entity(
    transaction: &Transaction<'_>,
    path: &Path,
    table: &str,
    name: Option<&str>,
    next_pid: &mut i64,
    insert_sql: &[&str; 2],
) -> Result<i64> {
    let Some(name) = name else {
        return Ok(0);
    };
    if name.is_empty() {
        return Ok(0);
    }
    let existing: Option<i64> = transaction
        .query_row(
            &format!("SELECT pid FROM {table} WHERE name = ?1"),
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| Error::Verification {
            format: "staged SQLite entity lookup",
            reason: format!("{table} lookup failed: {source}"),
        })?;
    if let Some(pid) = existing {
        return Ok(pid);
    }
    let pid = *next_pid;
    *next_pid = next_pid.saturating_add(1);
    let name_order = i64::try_from(insertion_rank(
        transaction,
        path,
        &format!("SELECT name FROM {table}"),
        name,
    )?)
    .unwrap_or(i64::MAX);
    let sort_name = sort_key(name);
    transaction
        .execute(
            &format!("INSERT INTO {table} ({}) {}", insert_sql[0], insert_sql[1]),
            rusqlite::params![pid, name, name_order as i64, sort_name],
        )
        .map_err(|source| Error::Verification {
            format: "staged SQLite entity insert",
            reason: format!("{table} insert failed: {source}"),
        })?;
    Ok(pid)
}

fn resolve_album(
    transaction: &Transaction<'_>,
    path: &Path,
    album_name: Option<&str>,
    artist_pid: i64,
    next_pid: &mut i64,
) -> Result<i64> {
    let album_name = album_name.unwrap_or("");
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT pid FROM album WHERE name = ?1 AND artist_pid = ?2",
            rusqlite::params![album_name, artist_pid],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| sqlite_error("look up album", path, source))?;
    if let Some(pid) = existing {
        return Ok(pid);
    }
    let pid = *next_pid;
    *next_pid = next_pid.saturating_add(1);
    let name_order = i64::try_from(insertion_rank(
        transaction,
        path,
        "SELECT name FROM album",
        album_name,
    )?)
    .unwrap_or(i64::MAX);
    let artist_order = artist_order_rank(transaction, path, artist_pid)?;
    let sort_name = if album_name.is_empty() {
        String::new()
    } else {
        sort_key(album_name)
    };
    transaction
        .execute(
            "INSERT INTO album (pid, kind, artwork_status, artwork_item_pid, artist_pid, \
             user_rating, name, name_order, all_compilations, feed_url, season_number, \
             is_unknown, has_songs, has_music_videos, sort_order, artist_order, \
             has_any_compilations, sort_name, artist_count_calc, has_movies, item_count, \
             min_volume_normalization_energy) \
             VALUES (?1, 2, 0, 0, ?2, 0, ?3, ?4, 0, NULL, 0, ?5, 1, 0, ?4, ?6, 0, ?7, 1, 0, 1, 0)",
            rusqlite::params![
                pid,
                artist_pid,
                album_name,
                name_order,
                i64::from(album_name.is_empty()),
                artist_order,
                sort_name
            ],
        )
        .map_err(|source| sqlite_error("insert album", path, source))?;
    Ok(pid)
}

fn artist_order_rank(transaction: &Transaction<'_>, path: &Path, artist_pid: i64) -> Result<i64> {
    let name: String = transaction
        .query_row(
            "SELECT COALESCE(name, '') FROM artist WHERE pid = ?1",
            [artist_pid],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("look up album artist", path, source))?;
    let rank =
        i64::try_from(insertion_rank(transaction, path, "SELECT name FROM artist", &name)? * 100)
            .unwrap_or(i64::MAX);
    Ok(rank)
}

fn resolve_genre(transaction: &Transaction<'_>, path: &Path, genre: Option<&str>) -> Result<i64> {
    let Some(genre) = genre else {
        return Ok(0);
    };
    if genre.is_empty() {
        return Ok(0);
    }
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT id FROM genre_map WHERE genre = ?1",
            [genre],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| sqlite_error("look up genre", path, source))?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let next: Option<i64> = transaction
        .query_row("SELECT MAX(id) FROM genre_map", [], |row| row.get(0))
        .map_err(|source| sqlite_error("read genre counter", path, source))?;
    let id = next.unwrap_or(0).saturating_add(1);
    let rank = insertion_rank(transaction, path, "SELECT genre FROM genre_map", genre)?;
    transaction
        .execute(
            "INSERT INTO genre_map (id, genre, genre_order, is_unknown, has_music, \
             artist_count_calc, album_count_calc, compilation_count_calc, \
             album_artist_count_calc) \
             VALUES (?1, ?2, ?3, 0, 1, 1, 1, 0, 1)",
            rusqlite::params![id, genre, i64::try_from(rank).unwrap_or(i64::MAX)],
        )
        .map_err(|source| sqlite_error("insert genre", path, source))?;
    Ok(id)
}

fn insertion_rank(
    transaction: &Transaction<'_>,
    path: &Path,
    names_sql: &str,
    new_name: &str,
) -> Result<usize> {
    let mut statement = transaction
        .prepare(names_sql)
        .map_err(|source| sqlite_error("prepare rank query", path, source))?;
    let keys = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| sqlite_error("query rank names", path, source))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| sqlite_error("collect rank names", path, source))?;
    drop(statement);
    let mut distinct: Vec<String> = keys.iter().map(|key| sort_key(key)).collect();
    distinct.sort();
    distinct.dedup();
    let new_key = sort_key(new_name);
    Ok(distinct
        .iter()
        .filter(|key| key.as_str() < new_key.as_str())
        .count()
        + 1)
}

fn compute_orders(
    transaction: &Transaction<'_>,
    path: &Path,
    addition: &ResolvedAddition,
) -> Result<ItemOrders> {
    let title_key = if addition.title.is_empty() {
        String::new()
    } else {
        sort_key(&addition.title)
    };
    let artist_key = addition
        .artist
        .as_ref()
        .map_or_else(String::new, |name| sort_key(name));
    let album_key = addition
        .album
        .as_ref()
        .map_or_else(String::new, |name| sort_key(name));
    let album_artist_key = addition.album_artist.as_ref().map_or_else(
        || {
            addition
                .artist
                .as_ref()
                .map_or_else(String::new, |name| sort_key(name))
        },
        |name| sort_key(name),
    );
    let composer_key = addition
        .composer
        .as_ref()
        .map_or_else(String::new, |name| sort_key(name));
    let genre_key = addition
        .genre
        .as_ref()
        .map_or_else(String::new, |name| sort_key(name));

    let mut statement = transaction
        .prepare(
            "SELECT COALESCE(NULLIF(sort_title, ''), title, ''), \
             COALESCE(NULLIF(sort_artist, ''), artist, ''), \
             COALESCE(NULLIF(sort_album, ''), album, ''), \
             COALESCE(NULLIF(sort_album_artist, ''), NULLIF(album_artist, ''), \
                      NULLIF(sort_artist, ''), artist, ''), \
             COALESCE(NULLIF(sort_composer, ''), composer, ''), \
             COALESCE((SELECT genre FROM genre_map WHERE genre_map.id = item.genre_id), '') \
             FROM item",
        )
        .map_err(|source| sqlite_error("prepare order query", path, source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|source| sqlite_error("query order keys", path, source))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| sqlite_error("collect order keys", path, source))?;
    drop(statement);

    let rank = |existing: Vec<String>, new_key: &str| -> i64 {
        let mut distinct: Vec<String> = existing.iter().map(|key| sort_key(key)).collect();
        distinct.sort();
        distinct.dedup();
        let rank = distinct.iter().filter(|key| key.as_str() < new_key).count() + 1;
        i64::try_from(rank * 100).unwrap_or(i64::MAX)
    };

    Ok(ItemOrders {
        title: rank(rows.iter().map(|row| row.0.clone()).collect(), &title_key),
        artist: rank(rows.iter().map(|row| row.1.clone()).collect(), &artist_key),
        album: rank(rows.iter().map(|row| row.2.clone()).collect(), &album_key),
        album_artist: rank(
            rows.iter().map(|row| row.3.clone()).collect(),
            &album_artist_key,
        ),
        composer: rank(
            rows.iter().map(|row| row.4.clone()).collect(),
            &composer_key,
        ),
        genre: rank(rows.iter().map(|row| row.5.clone()).collect(), &genre_key),
    })
}

struct ItemOrders {
    title: i64,
    artist: i64,
    album: i64,
    album_artist: i64,
    composer: i64,
    genre: i64,
}

#[allow(clippy::too_many_arguments)]
fn insert_item_row(
    transaction: &Transaction<'_>,
    path: &Path,
    addition: &ResolvedAddition,
    pid: i64,
    album_pid: i64,
    artist_pid: i64,
    track_artist_pid: i64,
    composer_pid: i64,
    genre_id: i64,
    orders: &ItemOrders,
) -> Result<()> {
    let sort_title = (!addition.title.is_empty()).then(|| sort_key(&addition.title));
    let sort_artist = non_empty_sort(addition.artist.as_deref());
    let sort_album = non_empty_sort(addition.album.as_deref());
    let sort_album_artist = addition
        .album_artist
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(sort_key)
        .or_else(|| non_empty_sort(addition.artist.as_deref()));
    let sort_composer = non_empty_sort(addition.composer.as_deref());
    let physical_order: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(physical_order), -1) + 1 FROM item",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("read physical order", path, source))?;
    let artwork_status = i64::from(addition.artwork.is_some());
    let artwork_cache_id = addition.artwork.as_ref().map_or(0, |art| art.image_id);

    transaction
        .execute(
            "INSERT INTO item (pid, revision_level, media_kind, is_song, is_audio_book, \
             is_music_video, is_movie, is_tv_show, is_home_video, is_ringtone, is_tone, \
             is_voice_memo, is_book, is_rental, is_itunes_u, is_digital_booklet, is_podcast, \
             date_modified, year, content_rating, content_rating_level, is_compilation, \
             is_user_disabled, remember_bookmark, exclude_from_shuffle, part_of_gapless_album, \
             chosen_by_auto_fill, artwork_status, artwork_cache_id, start_time_ms, stop_time_ms, \
             total_time_ms, total_burn_time_ms, track_number, track_count, disc_number, \
             disc_count, bpm, relative_volume, eq_preset, radio_stream_status, genius_id, \
             genre_id, category_id, album_pid, artist_pid, composer_pid, title, artist, album, \
             album_artist, composer, sort_title, sort_artist, sort_album, sort_album_artist, \
             sort_composer, title_order, artist_order, album_order, genre_order, composer_order, \
             album_artist_order, album_by_artist_order, series_name_order, comment, grouping, \
             description, description_long, collection_description, copyright, track_artist_pid, \
             physical_order, has_lyrics, date_released) \
             VALUES (:pid, NULL, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, :modified, \
             :year, 0, 0, :compilation, 0, 0, 0, 0, 0, :artwork_status, :artwork_cache_id, 0, 0, \
             :length, NULL, :track_number, \
             :total_tracks, :disc_number, :total_discs, 0, NULL, NULL, NULL, 0, :genre_id, 0, \
             :album_pid, :artist_pid, :composer_pid, :title, :artist, :album, :album_artist, \
             :composer, :sort_title, :sort_artist, :sort_album, :sort_album_artist, \
             :sort_composer, :title_order, :artist_order, :album_order, :genre_order, \
             :composer_order, :album_artist_order, :album_by_artist_order, 100, NULL, NULL, \
             NULL, NULL, NULL, NULL, :track_artist_pid, :physical_order, 0, 0)",
            rusqlite::named_params! {
                ":pid": pid,
                ":modified": addition.date_coredata,
                ":year": i64::from(addition.year),
                ":compilation": i64::from(addition.compilation),
                ":artwork_status": artwork_status,
                ":artwork_cache_id": artwork_cache_id,
                ":length": f64::from(addition.length_ms),
                ":track_number": i64::from(addition.track_number),
                ":total_tracks": i64::from(addition.total_tracks),
                ":disc_number": i64::from(addition.disc_number),
                ":total_discs": i64::from(addition.total_discs),
                ":genre_id": genre_id,
                ":album_pid": album_pid,
                ":artist_pid": artist_pid,
                ":composer_pid": composer_pid,
                ":title": opt_text(&addition.title),
                ":artist": addition.artist.as_deref(),
                ":album": addition.album.as_deref(),
                ":album_artist": addition.album_artist.as_deref(),
                ":composer": addition.composer.as_deref(),
                ":sort_title": sort_title,
                ":sort_artist": sort_artist,
                ":sort_album": sort_album,
                ":sort_album_artist": sort_album_artist,
                ":sort_composer": sort_composer,
                ":title_order": orders.title,
                ":artist_order": orders.artist,
                ":album_order": orders.album,
                ":genre_order": orders.genre,
                ":composer_order": orders.composer,
                ":album_artist_order": orders.album_artist,
                ":album_by_artist_order": orders.album_artist,
                ":track_artist_pid": track_artist_pid,
                ":physical_order": physical_order,
            },
        )
        .map_err(|source| sqlite_error("insert item", path, source))?;
    Ok(())
}

fn non_empty_sort(value: Option<&str>) -> Option<String> {
    value.filter(|name| !name.is_empty()).map(sort_key)
}

fn opt_text(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn insert_avformat_row(
    transaction: &Transaction<'_>,
    path: &Path,
    addition: &ResolvedAddition,
    pid: i64,
) -> Result<()> {
    let duration = u64::from(addition.length_ms)
        .saturating_mul(u64::from(addition.sample_rate))
        .saturating_div(1000);
    transaction
        .execute(
            "INSERT INTO avformat_info (item_pid, sub_id, audio_format, bit_rate, channels, \
             sample_rate, duration, gapless_heuristic_info, gapless_encoding_delay, \
             gapless_encoding_drain, gapless_last_frame_resynch, analysis_inhibit_flags, \
             audio_fingerprint, volume_normalization_energy) \
             VALUES (?1, 0, 301, ?2, 0, ?3, ?4, 0, 0, 0, 0, 0, 0, 0)",
            rusqlite::params![
                pid,
                i64::from(addition.bitrate),
                f64::from(addition.sample_rate),
                i64::try_from(duration).unwrap_or(i64::MAX),
            ],
        )
        .map_err(|source| sqlite_error("insert avformat_info", path, source))?;
    Ok(())
}

fn insert_location_row(
    transaction: &Transaction<'_>,
    path: &Path,
    addition: &ResolvedAddition,
    pid: i64,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO locations.location (item_pid, sub_id, base_location_id, location_type, \
             location, extension, kind_id, date_created, file_size, file_creator, file_type, \
             num_dir_levels_file, num_dir_levels_lib) \
             VALUES (?1, 0, 1, 0x46494C45, ?2, 0x4D503320, 1, ?3, ?4, NULL, NULL, NULL, NULL)",
            rusqlite::params![
                pid,
                addition.media_relative,
                addition.date_coredata,
                i64::try_from(addition.file_size).unwrap_or(i64::MAX),
            ],
        )
        .map_err(|source| sqlite_error("insert location", path, source))?;
    Ok(())
}

fn insert_container_row(transaction: &Transaction<'_>, path: &Path, pid: i64) -> Result<()> {
    let master: i64 = transaction
        .query_row(
            "SELECT primary_container_pid FROM db_info LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("read master container", path, source))?;
    transaction
        .execute(
            "INSERT INTO item_to_container (item_pid, container_pid, physical_order, \
             shuffle_order) \
             VALUES (?1, ?2, (SELECT COALESCE(MAX(physical_order), -1) + 1 FROM \
             item_to_container WHERE container_pid = ?2), NULL)",
            rusqlite::params![pid, master],
        )
        .map_err(|source| sqlite_error("insert item_to_container", path, source))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_derived_rows(
    transaction: &Transaction<'_>,
    path: &Path,
    addition: &ResolvedAddition,
    album_pid: i64,
    artist_pid: i64,
    track_artist_pid: i64,
    composer_pid: i64,
    genre_id: i64,
) -> Result<()> {
    if album_pid != 0 {
        transaction
            .execute(
                "UPDATE album SET \
                 item_count = (SELECT COUNT(*) FROM item WHERE item.album_pid = album.pid), \
                 has_songs = 1, \
                 all_compilations = CASE WHEN ?2 = 0 THEN 0 ELSE all_compilations END, \
                 min_volume_normalization_energy = COALESCE((SELECT \
                 MIN(volume_normalization_energy) FROM avformat_info JOIN item \
                 ON item.pid = avformat_info.item_pid WHERE item.album_pid = album.pid), 0) \
                 WHERE pid = ?1",
                rusqlite::params![album_pid, i64::from(addition.compilation)],
            )
            .map_err(|source| sqlite_error("update album totals", path, source))?;
    }
    if artist_pid != 0 {
        transaction
            .execute(
                "UPDATE artist SET has_songs = 1, album_count = (SELECT COUNT(DISTINCT \
                 album_pid) FROM item WHERE item.artist_pid = artist.pid) WHERE pid = ?1",
                [artist_pid],
            )
            .map_err(|source| sqlite_error("update artist totals", path, source))?;
    }
    if track_artist_pid != 0 {
        transaction
            .execute(
                "UPDATE track_artist SET has_songs = 1, has_non_compilation_tracks = \
                 CASE WHEN ?2 != 0 THEN has_non_compilation_tracks ELSE 1 END WHERE pid = ?1",
                rusqlite::params![track_artist_pid, i64::from(addition.compilation)],
            )
            .map_err(|source| sqlite_error("update track_artist totals", path, source))?;
    }
    if composer_pid != 0 {
        transaction
            .execute(
                "UPDATE composer SET has_music = 1 WHERE pid = ?1",
                [composer_pid],
            )
            .map_err(|source| sqlite_error("update composer totals", path, source))?;
    }
    if genre_id != 0 {
        transaction
            .execute(
                "UPDATE genre_map SET has_music = 1, \
                 artist_count_calc = (SELECT COUNT(DISTINCT artist_pid) FROM item WHERE \
                 item.genre_id = genre_map.id), \
                 album_artist_count_calc = (SELECT COUNT(DISTINCT artist_pid) FROM item WHERE \
                 item.genre_id = genre_map.id), \
                 album_count_calc = (SELECT COUNT(DISTINCT album_pid) FROM item WHERE \
                 item.genre_id = genre_map.id), \
                 compilation_count_calc = (SELECT COUNT(DISTINCT album_pid) FROM item WHERE \
                 item.genre_id = genre_map.id AND COALESCE(is_compilation, 0) != 0) \
                 WHERE id = ?1",
                [genre_id],
            )
            .map_err(|source| sqlite_error("update genre totals", path, source))?;
    }
    transaction
        .execute(
            "UPDATE track_size_calc SET size = size + ?1 WHERE kind = 'audio' AND pid = 1",
            [i64::try_from(addition.file_size).unwrap_or(i64::MAX)],
        )
        .map_err(|source| sqlite_error("update track_size_calc", path, source))?;
    Ok(())
}

fn validate_added_invariants(
    transaction: &Transaction<'_>,
    additions: &[ResolvedAddition],
    path: &Path,
) -> Result<()> {
    let pids: Vec<i64> = additions
        .iter()
        .map(|addition| i64::from_ne_bytes(addition.pid.to_bits().to_ne_bytes()))
        .collect();
    let mut missing = 0_i64;
    for pid in &pids {
        missing += transaction
            .query_row(
                "SELECT (SELECT COUNT(*) FROM item WHERE pid = ?1) + \
                 (SELECT COUNT(*) FROM locations.location WHERE item_pid = ?1 AND sub_id = 0) + \
                 (SELECT COUNT(*) FROM avformat_info WHERE item_pid = ?1 AND sub_id = 0) + \
                 (SELECT COUNT(*) FROM item_to_container WHERE item_pid = ?1)",
                [pid],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| sqlite_error("validate added rows", path, source))?;
    }
    if missing != i64::try_from(pids.len() * 4).unwrap_or(i64::MAX) {
        return Err(Error::Verification {
            format: "staged SQLite add",
            reason: "one or more added tracks lack their required companion rows".to_owned(),
        });
    }
    let violations: i64 = transaction
        .query_row(
            "SELECT \
             (SELECT COUNT(*) FROM (SELECT container_pid, COUNT(*) n, MIN(physical_order) lo, \
             MAX(physical_order) hi, COUNT(DISTINCT physical_order) d FROM item_to_container \
             GROUP BY container_pid \
             HAVING lo != 0 OR hi != n - 1 OR d != n)) + \
             (SELECT COUNT(*) FROM album a WHERE a.item_count != (SELECT COUNT(*) FROM item \
             WHERE item.album_pid = a.pid))",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("validate added invariants", path, source))?;
    if violations != 0 {
        return Err(Error::Verification {
            format: "staged SQLite add",
            reason: "ordering or album-count invariants were violated".to_owned(),
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
