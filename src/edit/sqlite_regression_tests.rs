//! Synthetic regressions for PR #1. No private dump or physical iPod is needed.
//!
//! Compare these tests with and without PR #1. The album-row tests enforce
//! libopod's chosen empty-last policy, not independently verified behavior
//! across all firmware versions.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension};
use tempfile::{tempdir, TempDir};

use super::{
    artist_order_rank, key_orders, reindex_browse_orders, resolve_album, resolve_genre,
    resolve_named_entity,
};
use crate::{PersistentId, SqliteLibraryFile};

fn fixture() -> (TempDir, Connection) {
    let directory = tempdir().unwrap();
    crate::edit::tests::create_database_set(directory.path());
    let connection = Connection::open(
        directory
            .path()
            .join(SqliteLibraryFile::Library.file_name()),
    )
    .unwrap();
    (directory, connection)
}

fn remove_tracks(directory: &TempDir, ids: &[u64]) {
    let removals: BTreeSet<_> = ids.iter().copied().map(PersistentId::from_bits).collect();
    crate::edit::edit_staged_databases(directory.path(), &removals).unwrap();
}

fn null_name_does_not_block_named_insertion(table: &str) {
    let (directory, mut connection) = fixture();
    connection
        .execute(
            &format!("INSERT INTO {table} (pid, name) VALUES (900, NULL)"),
            [],
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let mut next_pid = 1_000;
    let pid = resolve_named_entity(
        &transaction,
        directory.path(),
        table,
        Some("New Named Entity"),
        &mut next_pid,
        &[
            "pid, name, name_order, sort_name",
            "VALUES (?1, ?2, ?3, ?4)",
        ],
    )
    .expect("an unrelated NULL-name placeholder must not prevent a named insertion");
    let name: String = transaction
        .query_row(
            &format!("SELECT name FROM {table} WHERE pid = ?1"),
            [pid],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "New Named Entity");
    let placeholder: Option<String> = transaction
        .query_row(
            &format!("SELECT name FROM {table} WHERE pid = 900"),
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(placeholder, None, "do not modify the firmware placeholder");
}

#[test]
fn null_artist_name_does_not_block_insertion() {
    null_name_does_not_block_named_insertion("artist");
}

#[test]
fn null_track_artist_name_does_not_block_insertion() {
    null_name_does_not_block_named_insertion("track_artist");
}

#[test]
fn null_composer_name_does_not_block_insertion() {
    null_name_does_not_block_named_insertion("composer");
}

#[test]
fn null_genre_does_not_block_insertion() {
    let (directory, mut connection) = fixture();
    // The shared fixture already contains a NULL-genre placeholder (id 50).
    // Its removal-only schema omits this insertion column.
    connection
        .execute_batch("ALTER TABLE genre_map ADD COLUMN is_unknown INTEGER;")
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let id = resolve_genre(&transaction, directory.path(), Some("New Genre"))
        .expect("a NULL genre must not prevent insertion of a named genre");
    let genre: String = transaction
        .query_row("SELECT genre FROM genre_map WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(genre, "New Genre");
}

#[test]
fn null_album_name_does_not_block_insertion() {
    let (directory, mut connection) = fixture();
    // Supply the additional insertion columns; the removal fixture omits them.
    connection
        .execute_batch(
            "ALTER TABLE album ADD COLUMN kind INTEGER;\
             ALTER TABLE album ADD COLUMN user_rating INTEGER;\
             ALTER TABLE album ADD COLUMN feed_url TEXT;\
             ALTER TABLE album ADD COLUMN season_number INTEGER;\
             ALTER TABLE album ADD COLUMN is_unknown INTEGER;\
             ALTER TABLE album ADD COLUMN artist_count_calc INTEGER;\
             INSERT INTO album (pid, artist_pid, name) VALUES (900, 20, NULL);",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let mut next_pid = 1_000;
    let pid = resolve_album(
        &transaction,
        directory.path(),
        Some("New Album"),
        20,
        &mut next_pid,
    )
    .expect("a NULL album name must not prevent insertion of a named album");
    let name: String = transaction
        .query_row("SELECT name FROM album WHERE pid = ?1", [pid], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(name, "New Album");
}

#[test]
fn null_artist_name_does_not_block_album_artist_order() {
    let (directory, mut connection) = fixture();
    connection
        .execute_batch("INSERT INTO artist (pid,name) VALUES (900,NULL);")
        .unwrap();
    let transaction = connection.transaction().unwrap();
    let order = artist_order_rank(&transaction, directory.path(), 20)
        .expect("an unrelated NULL artist must not prevent an album artist rank lookup");
    assert_eq!(order, 100);
}

fn multi_artist_album() -> (TempDir, Connection) {
    let (directory, connection) = fixture();
    // Album 10 belongs to artist 20. Only track 1 directly names artist 20;
    // track 2 names artist 21 but keeps the same album alive after removal.
    connection
        .execute_batch(
            "INSERT INTO artist VALUES (21,'Guest Artist',NULL,200,1,0,1,1,10,1);\
             UPDATE item SET artist_pid=21, artist='Guest Artist', sort_artist=NULL WHERE pid=2;",
        )
        .unwrap();
    (directory, connection)
}

#[test]
fn removal_preserves_an_artist_still_referenced_by_an_album() {
    let (directory, connection) = multi_artist_album();
    remove_tracks(&directory, &[1]);
    let missing_artists: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM album a LEFT JOIN artist ar ON ar.pid=a.artist_pid \
             WHERE a.artist_pid!=0 AND ar.pid IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(missing_artists, 0, "the surviving album needs artist 20");
    let artist_pid: i64 = connection
        .query_row("SELECT artist_pid FROM album WHERE pid=10", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(artist_pid, 20, "preserve the album's original artist link");
}

#[test]
fn retained_artist_aggregates_match_the_existing_direct_track_queries() {
    let (directory, connection) = multi_artist_album();
    remove_tracks(&directory, &[1]);
    // This checks the semantics of the existing UPDATE expressions, not a
    // claim about the firmware UI. An album reference must not freeze old
    // direct-track flags/counts when the PR starts retaining these rows.
    let matches: Option<bool> = connection
        .query_row(
            "SELECT \
               has_songs=EXISTS(SELECT 1 FROM item WHERE artist_pid=artist.pid AND is_song!=0) \
               AND has_music_videos=EXISTS(SELECT 1 FROM item WHERE artist_pid=artist.pid AND is_music_video!=0) \
               AND has_non_compilation_tracks=EXISTS(SELECT 1 FROM item WHERE artist_pid=artist.pid AND COALESCE(is_compilation,0)=0) \
               AND album_count=(SELECT COUNT(DISTINCT album_pid) FROM item WHERE artist_pid=artist.pid) \
               AND artwork_album_pid=10 AND artwork_status=1 \
             FROM artist WHERE pid=20",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        matches,
        Some(true),
        "keep the artist, refresh direct-track aggregates, and retain its surviving album artwork"
    );
}

#[test]
fn a_later_removal_deletes_the_now_unreferenced_album_artist() {
    let (directory, connection) = multi_artist_album();
    remove_tracks(&directory, &[1]);
    remove_tracks(&directory, &[2]);
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artist WHERE pid IN (20,21)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 0,
        "after both edits, neither a track nor an album needs either artist"
    );
}

#[test]
fn one_removal_deletes_artists_when_no_track_or_album_needs_them() {
    let (directory, connection) = multi_artist_album();
    remove_tracks(&directory, &[1, 2]);
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artist WHERE pid IN (20,21)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn removal_cleans_up_an_album_artist_with_no_direct_tracks() {
    let (directory, connection) = multi_artist_album();
    connection
        .execute_batch("UPDATE item SET artist_pid=21,artist='Guest Artist',sort_artist=NULL;")
        .unwrap();
    remove_tracks(&directory, &[1, 2]);
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artist WHERE pid IN (20,21)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "cleanup must include the album's own artist");
}

#[test]
fn album_artist_survives_until_its_last_other_album_disappears() {
    let (directory, connection) = multi_artist_album();
    connection
        .execute_batch(
            "INSERT INTO item \
               (pid,physical_order,album_pid,artist_pid,track_artist_pid,composer_pid,genre_id,\
                category_id,genius_id,media_kind,is_song,is_music_video,is_movie,is_compilation,\
                artwork_status,title,artist,album) \
             VALUES (3,2,11,21,30,40,50,0,0,1,1,0,0,0,1,'Other Track','Guest Artist','Other Album');\
             INSERT INTO album \
               (pid,item_count,has_songs,has_music_videos,has_movies,has_any_compilations,\
                all_compilations,artwork_item_pid,artwork_status,min_volume_normalization_energy,\
                artist_pid,name_order,name) \
             VALUES (11,1,1,0,0,0,0,3,1,9,20,200,'Other Album');\
             INSERT INTO avformat_info VALUES (3,9);",
        )
        .unwrap();
    remove_tracks(&directory, &[1]);
    remove_tracks(&directory, &[2]);
    let artist: Option<(i64, i64, i64)> = connection
        .query_row(
            "SELECT has_songs,album_count,artwork_album_pid FROM artist WHERE pid=20",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(
        artist,
        Some((0, 0, 11)),
        "keep artist 20 for album 11 and move its artwork link away from deleted album 10"
    );
    remove_tracks(&directory, &[3]);
    let remaining: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artist WHERE pid IN (20,21)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        remaining, 0,
        "no artist reference remains after the last album"
    );
}

#[test]
fn album_artist_cleanup_does_not_touch_unrelated_firmware_placeholders() {
    let (directory, connection) = multi_artist_album();
    connection
        .execute_batch(
            "INSERT INTO artist \
               (pid,name,has_songs,album_count,artwork_album_pid,artwork_status) \
             VALUES (900,NULL,1,7,0,0);",
        )
        .unwrap();
    remove_tracks(&directory, &[1, 2]);
    let placeholder: Option<(Option<String>, i64, i64)> = connection
        .query_row(
            "SELECT name,has_songs,album_count FROM artist WHERE pid=900",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(
        placeholder,
        Some((None, 1, 7)),
        "only refresh and remove artists affected by this edit"
    );
}

#[test]
fn key_orders_put_empty_keys_last_and_keep_dense_named_ranks() {
    let keys = ["Zulu", "", "Alpha", "ALPHA", ""].map(str::to_owned);
    for scale in [1, 100] {
        let orders = key_orders(keys.iter(), scale);
        assert_eq!(
            orders.len(),
            3,
            "deduplicate normalized names and empty keys"
        );
        assert_eq!(orders["alpha"], scale);
        assert_eq!(orders["zulu"], 2 * scale);
        assert_eq!(orders[""], 3 * scale);
    }
}

#[test]
fn named_key_order_does_not_change_when_no_empty_keys_exist() {
    let keys = ["Zulu", "Alpha", "ALPHA"].map(str::to_owned);
    let orders = key_orders(keys.iter(), 100);
    assert_eq!(orders.len(), 2);
    assert_eq!(orders["alpha"], 100);
    assert_eq!(orders["zulu"], 200);
}

fn reindexed_fixture() -> (TempDir, Connection) {
    let (directory, mut connection) = fixture();
    for table in ["artist", "track_artist", "composer"] {
        connection
            .execute_batch(&format!(
                "DELETE FROM {table};\
                 INSERT INTO {table} (pid,name) VALUES \
                 (100,NULL),(101,''),(102,'Zulu'),(103,'Alpha'),(104,'ALPHA');"
            ))
            .unwrap();
    }
    connection
        .execute_batch(
            "DELETE FROM genre_map;\
             INSERT INTO genre_map (id,genre) VALUES (50,NULL),(51,''),(52,'Zulu'),(53,'Alpha');\
             UPDATE item SET artist='Zulu',album='Zulu',composer='Zulu',album_artist='Zulu',\
               sort_artist=NULL,sort_album=NULL,sort_composer=NULL,sort_album_artist=NULL,genre_id=52 WHERE pid=1;\
             UPDATE item SET artist='Alpha',album='Alpha',composer='Alpha',album_artist='Alpha',\
               sort_artist=NULL,sort_album=NULL,sort_composer=NULL,sort_album_artist=NULL,genre_id=53 WHERE pid=2;\
             INSERT INTO item (pid) VALUES (3);\
             INSERT INTO item (pid,artist,album,composer,album_artist,genre_id) VALUES (4,'','','','',51);\
             DELETE FROM album;\
             INSERT INTO album (pid,name,artist_pid) VALUES \
               (10,'Alpha',103),(11,'Zulu',102),(12,NULL,100),(13,'',101);",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    reindex_browse_orders(&transaction, directory.path()).unwrap();
    transaction.commit().unwrap();
    (directory, connection)
}

fn integer_pairs(connection: &Connection, sql: &str) -> Vec<(i64, i64)> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn reindex_puts_null_and_empty_named_entities_last() {
    let (_directory, connection) = reindexed_fixture();
    for table in ["artist", "track_artist", "composer"] {
        assert_eq!(
            integer_pairs(
                &connection,
                &format!("SELECT pid,name_order FROM {table} ORDER BY pid")
            ),
            [(100, 300), (101, 300), (102, 200), (103, 100), (104, 100)],
            "empty-last policy for {table}"
        );
    }
}

#[test]
fn reindex_puts_null_and_empty_genres_last_without_scaling_the_catalog() {
    let (_directory, connection) = reindexed_fixture();
    assert_eq!(
        integer_pairs(
            &connection,
            "SELECT id,genre_order FROM genre_map ORDER BY id"
        ),
        [(50, 3), (51, 3), (52, 2), (53, 1)]
    );
}

#[test]
fn reindex_puts_empty_item_keys_last_for_each_changed_order_column() {
    let (_directory, connection) = reindexed_fixture();
    for column in [
        "artist_order",
        "album_order",
        "genre_order",
        "composer_order",
        "album_artist_order",
        "album_by_artist_order",
    ] {
        assert_eq!(
            integer_pairs(
                &connection,
                &format!("SELECT pid,{column} FROM item ORDER BY pid")
            ),
            [(1, 200), (2, 100), (3, 300), (4, 300)],
            "empty-last policy for item.{column}"
        );
    }
}

#[test]
fn reindex_album_artist_orders_match_the_named_artist_catalog() {
    let (_directory, connection) = reindexed_fixture();
    assert_eq!(
        integer_pairs(
            &connection,
            "SELECT pid,artist_order FROM album ORDER BY pid"
        ),
        [(10, 100), (11, 200), (12, 300), (13, 300)]
    );
}

#[test]
fn album_rows_put_null_and_empty_names_last() {
    let (_directory, connection) = reindexed_fixture();
    // Album rows use their own comparator and retain one rank per row.
    for column in ["name_order", "sort_order"] {
        assert_eq!(
            integer_pairs(
                &connection,
                &format!("SELECT pid,{column} FROM album ORDER BY pid")
            ),
            [(10, 100), (11, 200), (12, 300), (13, 400)],
            "empty-last policy for album.{column}; keep one rank per row"
        );
    }
}

#[test]
fn album_rows_keep_distinct_slots_and_pid_order_for_equal_keys() {
    let (directory, mut connection) = fixture();
    connection
        .execute_batch(
            "DELETE FROM album;\
             INSERT INTO album (pid,name,artist_pid) VALUES \
               (40,'Zulu',20),(30,'ALPHA',20),(20,'Alpha',20),(60,NULL,20),(50,'',20);",
        )
        .unwrap();
    let transaction = connection.transaction().unwrap();
    reindex_browse_orders(&transaction, directory.path()).unwrap();
    transaction.commit().unwrap();
    for column in ["name_order", "sort_order"] {
        assert_eq!(
            integer_pairs(
                &connection,
                &format!("SELECT pid,{column} FROM album ORDER BY pid")
            ),
            [(20, 100), (30, 200), (40, 300), (50, 400), (60, 500)],
            "equal normalized names and empty keys need separate slots in PID order"
        );
    }
}
