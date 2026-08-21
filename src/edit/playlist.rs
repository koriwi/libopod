//! Schema-preserving standard-playlist edits for SQLite-era iPods.
//!
//! Nano 6G/7G firmware reads playlist containers and memberships from
//! `Library.itdb`; `Dynamic.itdb` carries one UI-state row per container.
//! Playlist-only edits do not change `Locations.itdb` or `iTunesCDB`.

use std::{collections::BTreeMap, path::Path};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};

use super::PlaylistEdit;
use crate::{Error, PersistentId, Result, SqliteLibraryFile};

pub(super) fn edit_staged_playlists(
    directory: &Path,
    edits: &BTreeMap<PersistentId, PlaylistEdit>,
) -> Result<()> {
    if edits.is_empty() {
        return Ok(());
    }

    let library_path = directory.join(SqliteLibraryFile::Library.file_name());
    let dynamic_path = directory.join(SqliteLibraryFile::Dynamic.file_name());
    let mut connection = Connection::open_with_flags(
        &library_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| {
        sqlite_error(
            "open staged library for playlist edit",
            &library_path,
            source,
        )
    })?;
    let dynamic_text = dynamic_path
        .to_str()
        .ok_or_else(|| Error::InvalidStagingDirectory {
            path: directory.to_path_buf(),
            reason: "SQLite preview currently requires a UTF-8 host path".to_owned(),
        })?;
    connection
        .execute("ATTACH DATABASE ?1 AS dynamic", [dynamic_text])
        .map_err(|source| sqlite_error("attach staged Dynamic.itdb", &dynamic_path, source))?;

    let transaction = connection
        .transaction()
        .map_err(|source| sqlite_error("begin staged playlist edit", &library_path, source))?;
    for (id, edit) in edits {
        apply_edit(&transaction, *id, edit, &library_path)?;
    }
    validate_invariants(&transaction, edits, &library_path)?;
    transaction
        .commit()
        .map_err(|source| sqlite_error("commit staged playlist edit", &library_path, source))?;
    Ok(())
}

fn apply_edit(
    transaction: &Transaction<'_>,
    id: PersistentId,
    edit: &PlaylistEdit,
    path: &Path,
) -> Result<()> {
    let stored_id = stored_pid(id);
    match edit {
        PlaylistEdit::Create { name, track_ids } => {
            let collision: i64 = transaction
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM container WHERE pid=?1) + \
                     (SELECT COUNT(*) FROM item WHERE pid=?1)",
                    [stored_id],
                    |row| row.get(0),
                )
                .map_err(|source| sqlite_error("check playlist persistent ID", path, source))?;
            if stored_id == 0 || collision != 0 {
                return Err(verification(
                    "the generated playlist ID is invalid or already in use",
                ));
            }
            validate_members(transaction, track_ids, path)?;
            insert_container(transaction, stored_id, name, path)?;
            replace_members(transaction, stored_id, track_ids, path)?;
        }
        PlaylistEdit::Update { name, track_ids } => {
            require_standard_playlist(transaction, stored_id, path)?;
            if let Some(name) = name {
                let changed = transaction
                    .execute(
                        "UPDATE container SET name=?2, date_modified=?3 WHERE pid=?1",
                        params![stored_id, name, core_data_now()],
                    )
                    .map_err(|source| sqlite_error("rename staged playlist", path, source))?;
                if changed != 1 {
                    return Err(verification(
                        "the requested playlist disappeared during staging",
                    ));
                }
            }
            if let Some(track_ids) = track_ids {
                validate_members(transaction, track_ids, path)?;
                replace_members(transaction, stored_id, track_ids, path)?;
            }
        }
        PlaylistEdit::Delete => {
            require_standard_playlist(transaction, stored_id, path)?;
            transaction
                .execute(
                    "DELETE FROM item_to_container WHERE container_pid=?1",
                    [stored_id],
                )
                .map_err(|source| {
                    sqlite_error("delete staged playlist memberships", path, source)
                })?;
            transaction
                .execute(
                    "DELETE FROM container_seed WHERE container_pid=?1",
                    [stored_id],
                )
                .map_err(|source| sqlite_error("delete staged playlist seeds", path, source))?;
            transaction
                .execute(
                    "DELETE FROM dynamic.container_ui WHERE container_pid=?1",
                    [stored_id],
                )
                .map_err(|source| sqlite_error("delete staged playlist UI state", path, source))?;
            let changed = transaction
                .execute("DELETE FROM container WHERE pid=?1", [stored_id])
                .map_err(|source| sqlite_error("delete staged playlist", path, source))?;
            if changed != 1 {
                return Err(verification(
                    "the requested playlist disappeared during staging",
                ));
            }
        }
    }
    Ok(())
}

fn insert_container(
    transaction: &Transaction<'_>,
    pid: i64,
    name: &str,
    path: &Path,
) -> Result<()> {
    let name_order: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(name_order), 0) FROM container",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("read playlist display order", path, source))?;
    let name_order = name_order.saturating_add(100);
    let now = core_data_now();
    transaction
        .execute(
            "INSERT INTO container (pid, distinguished_kind, date_created, date_modified, \
             name, name_order, parent_pid, media_kinds, workout_template_id, is_hidden, \
             smart_is_folder, smart_is_dynamic, smart_is_filtered, smart_is_genius, \
             smart_enabled_only, smart_is_limited, smart_limit_kind, smart_limit_order, \
             smart_evaluation_order, smart_limit_value, smart_reverse_limit_order, \
             smart_criteria, description) VALUES \
             (?1, 0, ?2, ?2, ?3, ?4, 0, 1, 0, 0, 0, NULL, NULL, 0, 0, NULL, NULL, \
              NULL, NULL, NULL, NULL, NULL, NULL)",
            params![pid, now, name, name_order],
        )
        .map_err(|source| sqlite_error("insert staged playlist", path, source))?;
    transaction
        .execute(
            "INSERT INTO dynamic.container_ui (container_pid, play_order, is_reversed, \
             album_field_order, repeat_mode, shuffle_items, has_been_shuffled) \
             VALUES (?1, 0, 0, 1, 0, 0, 0)",
            [pid],
        )
        .map_err(|source| sqlite_error("insert staged playlist UI state", path, source))?;
    Ok(())
}

fn replace_members(
    transaction: &Transaction<'_>,
    container_pid: i64,
    track_ids: &[PersistentId],
    path: &Path,
) -> Result<()> {
    transaction
        .execute(
            "DELETE FROM item_to_container WHERE container_pid=?1",
            [container_pid],
        )
        .map_err(|source| sqlite_error("replace staged playlist memberships", path, source))?;
    let mut statement = transaction
        .prepare(
            "INSERT INTO item_to_container \
             (item_pid, container_pid, physical_order, shuffle_order) \
             VALUES (?1, ?2, ?3, NULL)",
        )
        .map_err(|source| sqlite_error("prepare staged playlist memberships", path, source))?;
    for (position, id) in track_ids.iter().enumerate() {
        let position = i64::try_from(position)
            .map_err(|_| verification("playlist membership exceeds SQLite integer range"))?;
        statement
            .execute(params![stored_pid(*id), container_pid, position])
            .map_err(|source| sqlite_error("insert staged playlist membership", path, source))?;
    }
    Ok(())
}

fn validate_members(
    transaction: &Transaction<'_>,
    track_ids: &[PersistentId],
    path: &Path,
) -> Result<()> {
    let mut statement = transaction
        .prepare("SELECT 1 FROM item WHERE pid=?1 LIMIT 1")
        .map_err(|source| sqlite_error("prepare playlist member validation", path, source))?;
    for id in track_ids {
        let exists = statement
            .query_row([stored_pid(*id)], |_| Ok(()))
            .optional()
            .map_err(|source| sqlite_error("validate staged playlist member", path, source))?
            .is_some();
        if !exists {
            return Err(Error::TrackNotFound);
        }
    }
    Ok(())
}

fn require_standard_playlist(transaction: &Transaction<'_>, pid: i64, path: &Path) -> Result<()> {
    let flags = transaction
        .query_row(
            "SELECT COALESCE(is_hidden,0), \
             smart_criteria IS NOT NULL OR COALESCE(smart_is_dynamic,0)!=0 \
             OR COALESCE(smart_is_filtered,0)!=0 \
             FROM container WHERE pid=?1",
            [pid],
            |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(|source| sqlite_error("read staged playlist flags", path, source))?
        .ok_or(Error::PlaylistNotFound)?;
    if flags.0 {
        return Err(Error::Unsupported {
            feature: "playlist mutation",
            reason: "master and hidden playlists cannot be edited".to_owned(),
        });
    }
    if flags.1 {
        return Err(Error::Unsupported {
            feature: "playlist mutation",
            reason: "smart playlists cannot be edited yet".to_owned(),
        });
    }
    Ok(())
}

fn validate_invariants(
    transaction: &Transaction<'_>,
    edits: &BTreeMap<PersistentId, PlaylistEdit>,
    path: &Path,
) -> Result<()> {
    let dangling: i64 = transaction
        .query_row(
            "SELECT \
             (SELECT COUNT(*) FROM item_to_container m \
              LEFT JOIN container c ON c.pid=m.container_pid WHERE c.pid IS NULL) + \
             (SELECT COUNT(*) FROM item_to_container m \
              LEFT JOIN item i ON i.pid=m.item_pid WHERE i.pid IS NULL) + \
             (SELECT COUNT(*) FROM dynamic.container_ui u \
              LEFT JOIN container c ON c.pid=u.container_pid WHERE c.pid IS NULL) + \
             (SELECT COUNT(*) FROM (SELECT container_pid, COUNT(*) n, \
              MIN(physical_order) lo, MAX(physical_order) hi, \
              COUNT(DISTINCT physical_order) d FROM item_to_container \
              GROUP BY container_pid HAVING lo!=0 OR hi!=n-1 OR d!=n))",
            [],
            |row| row.get(0),
        )
        .map_err(|source| sqlite_error("validate staged playlist relationships", path, source))?;
    if dangling != 0 {
        return Err(verification(
            "playlist edits left dangling references or non-contiguous membership order",
        ));
    }

    for (id, edit) in edits {
        let pid = stored_pid(*id);
        let containers: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM container WHERE pid=?1",
                [pid],
                |row| row.get(0),
            )
            .map_err(|source| sqlite_error("validate staged playlist row", path, source))?;
        let ui_rows: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM dynamic.container_ui WHERE container_pid=?1",
                [pid],
                |row| row.get(0),
            )
            .map_err(|source| sqlite_error("validate staged playlist UI row", path, source))?;
        let expected = i64::from(!matches!(edit, PlaylistEdit::Delete));
        if containers != expected || ui_rows != expected {
            return Err(verification(
                "playlist and Dynamic.itdb UI rows do not match the requested edit",
            ));
        }
    }
    Ok(())
}

fn stored_pid(id: PersistentId) -> i64 {
    i64::from_ne_bytes(id.to_bits().to_ne_bytes())
}

fn core_data_now() -> i64 {
    const CORE_DATA_EPOCH: u64 = 978_307_200;
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(unix.saturating_sub(CORE_DATA_EPOCH)).unwrap_or(i64::MAX)
}

fn verification(reason: &str) -> Error {
    Error::Verification {
        format: "staged SQLite playlist edit",
        reason: reason.to_owned(),
    }
}

fn sqlite_error(operation: &'static str, path: &Path, source: rusqlite::Error) -> Error {
    Error::Sqlite {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
