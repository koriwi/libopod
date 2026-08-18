
#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use rusqlite::Connection;
    use tempfile::{tempdir, TempDir};

    use super::{
        commit::{
            install_staged_removal, recover_transaction, FailureMode, TRANSACTION_PATH,
        },
        edit_staged_databases, StagedSqliteEdit,
    };
    use crate::{
        Device, Error, MountRoot, PersistentId, SqliteLibraryFile,
        NANO7_NOOP_HARDWARE_TEST_CONFIRMATION,
    };

    #[test]
    fn removes_direct_references_and_repairs_derived_rows() {
        let directory = tempdir().unwrap();
        create_database_set(directory.path());
        let removals = BTreeSet::from([PersistentId::from_bits(1)]);
        edit_staged_databases(directory.path(), &removals).unwrap();

        let library = Connection::open(directory.path().join("Library.itdb")).unwrap();
        assert_eq!(scalar(&library, "SELECT COUNT(*) FROM item"), 1);
        assert_eq!(scalar(&library, "SELECT physical_order FROM item"), 0);
        assert_eq!(scalar(&library, "SELECT item_count FROM album"), 1);
        assert_eq!(scalar(&library, "SELECT artwork_item_pid FROM album"), 2);
        assert_eq!(scalar(&library, "SELECT size FROM track_size_calc"), 200);
        assert_eq!(scalar(&library, "SELECT COUNT(*) FROM avformat_info WHERE item_pid=1"), 0);
        assert_eq!(scalar(&library, "SELECT physical_order FROM item_to_container"), 0);
        assert_eq!(scalar(&library, "SELECT value FROM unknown_extension"), 99);

        let locations = Connection::open(directory.path().join("Locations.itdb")).unwrap();
        assert_eq!(scalar(&locations, "SELECT COUNT(*) FROM location"), 1);
        let dynamic = Connection::open(directory.path().join("Dynamic.itdb")).unwrap();
        assert_eq!(scalar(&dynamic, "SELECT COUNT(*) FROM item_stats"), 1);
        let genius = Connection::open(directory.path().join("Genius.itdb")).unwrap();
        assert_eq!(scalar(&genius, "SELECT COUNT(*) FROM genius_metadata"), 0);
    }

    #[test]
    fn installs_and_reads_back_a_virtual_nano_noop() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return;
        }
        let device = Device::open(fixture).unwrap();
        let bundle = tempdir().unwrap();
        let staged = device.stage_noop_preview(bundle.path()).unwrap();
        assert_eq!(staged.removed_tracks(), 0);
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        assert!(virtual_device
            .install_noop_hardware_test(&staged, "not confirmed")
            .is_err());
        assert!(!virtual_root.join(TRANSACTION_PATH).exists());
        virtual_device
            .install_noop_hardware_test(&staged, NANO7_NOOP_HARDWARE_TEST_CONFIRMATION)
            .unwrap();
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    #[test]
    fn installs_and_reads_back_a_virtual_nano_removal() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        install_staged_removal(&virtual_device, &staged, FailureMode::RollBack).unwrap();

        assert!(!virtual_root.join(TRANSACTION_PATH).exists());
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 725);
    }

    #[test]
    fn recovers_an_interrupted_virtual_nano_removal() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        let error = install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(3),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Verification { .. }));
        assert!(matches!(
            Device::open(&virtual_root),
            Err(Error::RecoveryRequired { .. })
        ));

        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    fn stage_private_no_artwork_removal() -> Option<(TempDir, StagedSqliteEdit)> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return None;
        }
        let device = Device::open(fixture).unwrap();
        let track = device
            .library()
            .unwrap()
            .tracks()
            .iter()
            .find(|track| !track.has_artwork)
            .unwrap();
        let mut edit = device.edit().unwrap();
        edit.remove_track(track.id).unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        assert_eq!(staged.removed_artwork_tracks(), 0);
        Some((bundle, staged))
    }

    fn scalar(connection: &Connection, sql: &str) -> i64 {
        connection.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn create_database_set(directory: &std::path::Path) {
        let library = Connection::open(
            directory.join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        library
            .execute_batch(
                "CREATE TABLE item(pid INTEGER PRIMARY KEY,physical_order INTEGER,album_pid INTEGER,artist_pid INTEGER,track_artist_pid INTEGER,composer_pid INTEGER,genre_id INTEGER,category_id INTEGER,genius_id INTEGER,media_kind INTEGER,is_song INTEGER,is_music_video INTEGER,is_movie INTEGER,is_compilation INTEGER,artwork_status INTEGER);\
                 CREATE TABLE album(pid INTEGER PRIMARY KEY,item_count INTEGER,has_songs INTEGER,has_music_videos INTEGER,has_movies INTEGER,has_any_compilations INTEGER,all_compilations INTEGER,artwork_item_pid INTEGER,artwork_status INTEGER,min_volume_normalization_energy INTEGER,artist_pid INTEGER,name_order INTEGER);\
                 CREATE TABLE artist(pid INTEGER PRIMARY KEY,has_songs INTEGER,has_music_videos INTEGER,has_non_compilation_tracks INTEGER,album_count INTEGER,artwork_album_pid INTEGER,artwork_status INTEGER);\
                 CREATE TABLE track_artist(pid INTEGER PRIMARY KEY,has_songs INTEGER,has_music_videos INTEGER,has_non_compilation_tracks INTEGER);\
                 CREATE TABLE composer(pid INTEGER PRIMARY KEY,has_music INTEGER);\
                 CREATE TABLE genre_map(id INTEGER PRIMARY KEY,has_music INTEGER,artist_count_calc INTEGER,album_artist_count_calc INTEGER,album_count_calc INTEGER,compilation_count_calc INTEGER);\
                 CREATE TABLE category_map(id INTEGER PRIMARY KEY);\
                 CREATE TABLE avformat_info(item_pid INTEGER PRIMARY KEY,volume_normalization_energy INTEGER);\
                 CREATE TABLE item_to_container(item_pid INTEGER,container_pid INTEGER,physical_order INTEGER,shuffle_order INTEGER);\
                 CREATE TABLE container_seed(container_pid INTEGER,item_pid INTEGER,seed_order INTEGER);\
                 CREATE TABLE video_info(item_pid INTEGER);\
                 CREATE TABLE video_characteristics(item_pid INTEGER);\
                 CREATE TABLE podcast_info(item_pid INTEGER);\
                 CREATE TABLE store_info(item_pid INTEGER);\
                 CREATE TABLE track_size_calc(pid INTEGER PRIMARY KEY,kind TEXT,size INTEGER);\
                 CREATE TABLE unknown_extension(value INTEGER);\
                 INSERT INTO item VALUES(1,0,10,20,30,40,50,0,60,1,1,0,0,0,1);\
                 INSERT INTO item VALUES(2,1,10,20,30,40,50,0,0,1,1,0,0,0,1);\
                 INSERT INTO album VALUES(10,2,1,0,0,0,0,1,1,5,20,100);\
                 INSERT INTO artist VALUES(20,1,0,1,1,10,1);\
                 INSERT INTO track_artist VALUES(30,1,0,1);\
                 INSERT INTO composer VALUES(40,1);\
                 INSERT INTO genre_map VALUES(50,1,1,1,1,0);\
                 INSERT INTO avformat_info VALUES(1,5);\
                 INSERT INTO avformat_info VALUES(2,7);\
                 INSERT INTO item_to_container VALUES(1,100,0,NULL);\
                 INSERT INTO item_to_container VALUES(2,100,1,NULL);\
                 INSERT INTO container_seed VALUES(100,1,0);\
                 INSERT INTO track_size_calc VALUES(1,'audio',300);\
                 INSERT INTO unknown_extension VALUES(99);",
            )
            .unwrap();

        let locations = Connection::open(
            directory.join(SqliteLibraryFile::Locations.file_name()),
        )
        .unwrap();
        locations
            .execute_batch(
                "CREATE TABLE location(item_pid INTEGER,sub_id INTEGER,file_size INTEGER);\
                 INSERT INTO location VALUES(1,0,100);\
                 INSERT INTO location VALUES(2,0,200);",
            )
            .unwrap();

        let dynamic = Connection::open(
            directory.join(SqliteLibraryFile::Dynamic.file_name()),
        )
        .unwrap();
        dynamic
            .execute_batch(
                "CREATE TABLE item_stats(item_pid INTEGER);\
                 CREATE TABLE rental_info(item_pid INTEGER);\
                 INSERT INTO item_stats VALUES(1);\
                 INSERT INTO item_stats VALUES(2);",
            )
            .unwrap();

        let extras = Connection::open(
            directory.join(SqliteLibraryFile::Extras.file_name()),
        )
        .unwrap();
        extras
            .execute_batch(
                "CREATE TABLE chapter(item_pid INTEGER);\
                 CREATE TABLE lyrics(item_pid INTEGER);\
                 INSERT INTO chapter VALUES(1);\
                 INSERT INTO lyrics VALUES(1);",
            )
            .unwrap();

        let genius = Connection::open(
            directory.join(SqliteLibraryFile::Genius.file_name()),
        )
        .unwrap();
        genius
            .execute_batch(
                "CREATE TABLE genius_metadata(genius_id INTEGER);\
                 CREATE TABLE genius_similarities(genius_id INTEGER);\
                 INSERT INTO genius_metadata VALUES(60);\
                 INSERT INTO genius_similarities VALUES(60);",
            )
            .unwrap();
    }
}
