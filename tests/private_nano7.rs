use std::{
    io::Read,
    path::{Path, PathBuf},
};

use flate2::read::ZlibDecoder;

use libopod::{crypto::hashab::DatabaseSignatureStatus, Device, SqliteLibraryFile};
use rusqlite::{Connection, OpenFlags};
use tempfile::tempdir;

fn fixture_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LIBOPOD_PRIVATE_FIXTURE") {
        return Some(PathBuf::from(path));
    }
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
    local.is_dir().then_some(local)
}

#[test]
#[allow(clippy::too_many_lines)]
fn inspects_private_nano7_without_reading_media_payloads() {
    let Some(root) = fixture_root() else {
        return;
    };
    let device = Device::open(root).expect("private fixture must remain structurally valid");

    let profile = device.profile().expect("Nano 7G profile must resolve");
    assert_eq!(profile.key(), "nano-7g");
    assert_eq!(profile.capabilities().music_directories, 20);
    assert!(profile.has_required_signing_identity(device.evidence()));
    assert!(device.evidence().has_product_serial());
    assert!(device.evidence().has_firewire_guid());
    assert_eq!(device.generation().files().len(), 15);

    let library = device.library().expect("Nano 7G SQLite read adapter");
    assert_eq!(library.track_count(), 726);
    assert_eq!(library.playlists().len(), 6);
    assert_eq!(
        library
            .playlists()
            .iter()
            .map(|playlist| playlist.track_ids().len())
            .sum::<usize>(),
        726
    );
    assert_eq!(
        library
            .tracks()
            .iter()
            .filter(|track| track.has_artwork)
            .count(),
        704
    );
    assert!(library
        .tracks()
        .iter()
        .all(|track| track.location.as_str().starts_with("iPod_Control/Music/F")));
    for track in library.tracks() {
        let media_path = device
            .track_path(track)
            .expect("every database location must resolve inside the mount");
        let media_size = std::fs::metadata(media_path)
            .expect("every database location must identify a media file")
            .len();
        assert_eq!(media_size, track.size);
    }

    let inspected = device.inspection();
    let cdb = inspected.cdb.as_ref().expect("current CDB");
    assert_eq!(cdb.physical_bytes, 140_169);
    assert_eq!(cdb.header_length, 244);
    assert_eq!(cdb.version, 110);
    assert_eq!(cdb.declared_children, 5);
    assert_eq!(cdb.checksum_scheme, 3);
    assert_eq!(cdb.checksum_indicator, 4);
    assert_eq!(cdb.compression_flag, 1);
    assert_eq!(cdb.hashab_version_prefix, [3, 0]);
    assert_eq!(
        cdb.hashab_signature_status,
        Some(DatabaseSignatureStatus::LegacyScheme4Then3)
    );
    assert_eq!(cdb.uncompressed_payload_bytes, 1_093_398);
    assert_eq!(
        cdb.datasets
            .iter()
            .map(|dataset| dataset.kind)
            .collect::<Vec<_>>(),
        [4, 1, 3, 2, 5]
    );

    assert_eq!(inspected.sqlite_databases.len(), 5);
    let expected = [
        (SqliteLibraryFile::Library, 647_168, 158),
        (SqliteLibraryFile::Locations, 69_632, 17),
        (SqliteLibraryFile::Dynamic, 45_056, 11),
        (SqliteLibraryFile::Extras, 12_288, 3),
        (SqliteLibraryFile::Genius, 20_480, 5),
    ];
    for (file, bytes, pages) in expected {
        let database = inspected
            .sqlite_databases
            .iter()
            .find(|database| database.file == file)
            .expect("required SQLite database");
        assert_eq!(database.bytes, bytes);
        assert_eq!(database.page_size, 4_096);
        assert_eq!(database.page_count, pages);
        assert!(database.integrity_ok);
    }

    let cbk = inspected.cbk.as_ref().expect("Locations CBK");
    assert_eq!(cbk.bytes, 1_437);
    assert_eq!(cbk.block_size, 1_024);
    assert_eq!(cbk.block_count, 68);
    assert_eq!(cbk.checksum_scheme, 3);
    assert_eq!(cbk.hashab_signature_matches, Some(true));
    assert!(cbk.digests_match());

    let artwork = inspected
        .artwork_database
        .as_ref()
        .expect("current ArtworkDB");
    assert_eq!(artwork.bytes, 570_024);
    assert_eq!(artwork.header_length, 132);
    assert_eq!(artwork.declared_children, 3);
    assert_eq!(artwork.next_image_id, 804);
    assert_eq!(
        artwork
            .datasets
            .iter()
            .map(|dataset| (dataset.kind, dataset.list_magic, dataset.item_count))
            .collect::<Vec<_>>(),
        [(1, *b"mhli", 704), (2, *b"mhla", 0), (3, *b"mhlf", 4),]
    );

    assert_eq!(inspected.artwork_frames.len(), 4);
    for frames in &inspected.artwork_frames {
        assert_eq!(frames.complete_slots, 140);
        assert_eq!(frames.remainder_bytes, 0);
    }
}

#[test]
fn stages_a_schema_preserving_removal_outside_the_private_fixture() {
    let Some(root) = fixture_root() else {
        return;
    };
    let itlp = root.join("iPod_Control/iTunes/iTunes Library.itlp");
    let protected_files = [
        "Library.itdb",
        "Locations.itdb",
        "Dynamic.itdb",
        "Extras.itdb",
        "Genius.itdb",
        "Locations.itdb.cbk",
    ];
    let original_cdb =
        std::fs::read(root.join("iPod_Control/iTunes/iTunesCDB")).expect("read protected CDB");
    let original_bytes: Vec<_> = protected_files
        .iter()
        .map(|name| std::fs::read(itlp.join(name)).expect("read protected metadata file"))
        .collect();

    let device = Device::open(&root).expect("open immutable fixture");
    let removed = device.library().unwrap().tracks()[0].clone();
    let mut edit = device.edit().expect("start in-memory edit");
    assert!(edit.remove_track(removed.id).unwrap());
    assert!(!edit.remove_track(removed.id).unwrap());
    assert_eq!(edit.removal_count(), 1);

    let staging = tempdir().expect("host staging directory");
    let staged = edit
        .stage_sqlite_preview(staging.path())
        .expect("stage SQLite-only removal preview");
    assert_eq!(staged.removed_tracks(), 1);
    assert_eq!(staged.remaining_tracks(), 725);
    assert_eq!(staged.removed_media(), &[removed.location]);
    assert_eq!(staged.source_generation(), device.generation());
    let staged_cdb = staging.path().join("iTunesCDB");
    assert_eq!(cdb_track_count(&staged_cdb), 725);

    for file in SqliteLibraryFile::ALL {
        let source = itlp.join(file.file_name());
        let output = staging.path().join(file.file_name());
        assert_eq!(schema(&source), schema(&output));
        let connection = open_read_only(&output);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }
    let library = open_read_only(&staging.path().join("Library.itdb"));
    let stored_pid = i64::from_ne_bytes(removed.id.to_bits().to_ne_bytes());
    assert_eq!(
        library
            .query_row(
                "SELECT COUNT(*) FROM item WHERE pid=?1",
                [stored_pid],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let locations = open_read_only(&staging.path().join("Locations.itdb"));
    assert_eq!(
        locations
            .query_row(
                "SELECT COUNT(*) FROM location WHERE item_pid=?1",
                [stored_pid],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );

    assert_eq!(
        std::fs::read(root.join("iPod_Control/iTunes/iTunesCDB")).unwrap(),
        original_cdb,
        "private fixture CDB changed"
    );
    for (name, expected) in protected_files.iter().zip(original_bytes) {
        assert_eq!(
            std::fs::read(itlp.join(name)).expect("reread protected metadata file"),
            expected,
            "private fixture changed: {name}"
        );
    }
}

fn cdb_track_count(path: &Path) -> u32 {
    let bytes = std::fs::read(path).unwrap();
    let header = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let mut payload = Vec::new();
    ZlibDecoder::new(&bytes[header..])
        .read_to_end(&mut payload)
        .unwrap();
    let mut offset = 0_usize;
    while offset < payload.len() {
        let dataset_header =
            u32::from_le_bytes(payload[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let dataset_total =
            u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
        let kind = u32::from_le_bytes(payload[offset + 12..offset + 16].try_into().unwrap());
        if kind == 1 {
            let list = offset + dataset_header;
            assert_eq!(&payload[list..list + 4], b"mhlt");
            return u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap());
        }
        offset += dataset_total;
    }
    panic!("type-1 dataset missing")
}

fn open_read_only(path: &Path) -> Connection {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap()
}

fn schema(path: &Path) -> Vec<(String, String, String, Option<String>)> {
    let connection = open_read_only(path);
    let mut statement = connection
        .prepare(
            "SELECT type,name,tbl_name,sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
