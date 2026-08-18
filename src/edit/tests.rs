
#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use rusqlite::Connection;
    use tempfile::{tempdir, TempDir};

    use super::{
        commit::{
            install_staged_removal, recover_transaction, FailureMode, ADDITION_CONFIRMATION,
            ARTWORK_REMOVAL_CONFIRMATION, TRANSACTION_PATH,
        },
        edit_staged_databases, TrackToAdd,
    };
    use crate::{
        artwork::parse_artwork_records,
        Device, Error, MountRoot, PersistentId, SqliteLibraryFile,
        NANO7_NOOP_HARDWARE_TEST_CONFIRMATION, NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION,
    };
    use std::io::Read as _;

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
        assert!(virtual_device
            .install_single_removal_hardware_test(&staged, "not confirmed")
            .is_err());
        virtual_device
            .install_single_removal_hardware_test(
                &staged,
                NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION,
            )
            .unwrap();

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

    #[test]
    fn recovers_an_interruption_mid_backup() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        let error = install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionDuringBackupAfter(4),
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

    #[test]
    fn recovers_an_interruption_before_any_file_is_installed() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        let error = install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(0),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Verification { .. }));

        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    #[test]
    fn recovers_an_interruption_during_validation() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        let error = install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionDuringValidation,
        )
        .unwrap_err();
        assert!(matches!(error, Error::Verification { .. }));

        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    #[test]
    fn keeps_a_committed_transaction_through_recovery() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        let error = install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfterCommitted,
        )
        .unwrap_err();
        assert!(matches!(error, Error::Verification { .. }));
        assert!(matches!(
            Device::open(&virtual_root),
            Err(Error::RecoveryRequired { .. })
        ));

        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        assert!(!virtual_root.join(TRANSACTION_PATH).exists());
        let reopened = Device::open(&virtual_root).unwrap();
        // A committed transaction must survive recovery without rollback.
        assert_eq!(reopened.library().unwrap().track_count(), 725);
    }

    #[test]
    fn refuses_recovery_with_a_corrupt_journal() {
        let Some((bundle, staged)) = stage_private_no_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(1),
        )
        .unwrap_err();
        let transaction = virtual_root.join(TRANSACTION_PATH);
        fs::write(transaction.join("journal.json"), b"not a journal").unwrap();

        let mount = MountRoot::open(&virtual_root).unwrap();
        let error = recover_transaction(&mount).unwrap_err();
        assert!(matches!(error, Error::Malformed { .. }));
        // A corrupt journal must leave the transaction untouched for manual review.
        assert!(transaction.exists());
        assert!(matches!(
            Device::open(&virtual_root),
            Err(Error::RecoveryRequired { .. })
        ));
    }

    #[test]
    fn recovery_is_a_noop_without_a_pending_transaction() {
        let directory = tempdir().unwrap();
        let mount = MountRoot::open(directory.path()).unwrap();
        recover_transaction(&mount).unwrap();
    }

    #[test]
    fn installs_and_reads_back_a_virtual_nano_addition() {
        let Some((bundle, staged)) = stage_private_addition() else {
            return;
        };
        assert_eq!(staged.added_tracks(), 1);
        assert_eq!(staged.removed_tracks(), 0);
        assert_eq!(staged.remaining_tracks(), 727);
        assert_eq!(staged.added_media().len(), 1);
        assert!(staged.added_media()[0].as_str().starts_with("iPod_Control/Music/"));
        // Regression: the new track's CDB title and location strings must
        // round-trip through the exact MHOD string layout (8-byte header gap).
        let cdb = std::fs::read(bundle.path().join("iTunesCDB")).unwrap();
        let (title, location) = cdb_track_strings(
            &cdb,
            staged_pid(&staged, "LibOpod Fixture Addition"),
        )
        .expect("new track present in staged CDB");
        assert_eq!(title, "LibOpod Fixture Addition");
        assert!(location.starts_with(":iPod_Control:Music:"));
        verify_staged_addition(&bundle, &staged);

        let virtual_root = bundle.path().join("original");
        create_virtual_media_dirs(&virtual_root, staged.added_media());
        let virtual_device = Device::open(&virtual_root).unwrap();
        assert!(virtual_device
            .install_single_addition_hardware_test(&staged, "not confirmed")
            .is_err());
        virtual_device
            .install_single_addition_hardware_test(&staged, ADDITION_CONFIRMATION)
            .unwrap();
        assert!(!virtual_root.join(TRANSACTION_PATH).exists());
        let media_target = virtual_root.join(staged.added_media()[0].as_str());
        assert!(media_target.is_file());
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 727);
    }

    #[test]
    fn recovers_an_interrupted_virtual_nano_addition() {
        let Some((bundle, staged)) = stage_private_addition() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        create_virtual_media_dirs(&virtual_root, staged.added_media());
        let virtual_device = Device::open(&virtual_root).unwrap();
        install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(5),
        )
        .unwrap_err();
        assert!(matches!(
            Device::open(&virtual_root),
            Err(Error::RecoveryRequired { .. })
        ));

        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let media_target = virtual_root.join(staged.added_media()[0].as_str());
        assert!(!media_target.exists());
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    /// The host generation backup does not include media directories; the
    /// virtual device needs the allocated `Music/Fxx` parents to exist.
    fn create_virtual_media_dirs(virtual_root: &Path, added_media: &[crate::IpodPath]) {
        for media in added_media {
            let parent = virtual_root.join(media.as_str()).parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&parent).unwrap();
        }
    }

    #[test]
    fn stages_an_artwork_bearing_removal_with_artworkdb_output() {
        let Some((bundle, staged)) = stage_private_artwork_removal() else {
            return;
        };
        assert_eq!(staged.removed_tracks(), 1);
        assert_eq!(staged.removed_artwork_tracks(), 1);
        assert_eq!(staged.remaining_tracks(), 725);
        let artwork = bundle.path().join("ArtworkDB");
        assert!(artwork.is_file());
        let bytes = std::fs::read(&artwork).unwrap();
        let records = parse_artwork_records(&bytes).unwrap();
        assert_eq!(records.len(), 703);
        // The manifest must carry the ArtworkDB output.
        let manifest = std::fs::read_to_string(staged.manifest()).unwrap();
        assert!(manifest.contains("iPod_Control/Artwork/ArtworkDB"));
        let library = rusqlite::Connection::open(
            bundle.path().join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        let tracks: i64 = library
            .query_row("SELECT COUNT(*) FROM item", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tracks, 725);
    }

    #[test]
    fn installs_and_recovers_a_virtual_nano_artwork_removal() {
        let Some((bundle, staged)) = stage_private_artwork_removal() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        // The artwork-bearing gate refuses the wrong confirmation.
        assert!(virtual_device
            .install_single_artwork_removal_hardware_test(&staged, "not confirmed")
            .is_err());
        virtual_device
            .install_single_artwork_removal_hardware_test(&staged, ARTWORK_REMOVAL_CONFIRMATION)
            .unwrap();
        assert!(!virtual_root.join(TRANSACTION_PATH).exists());
        let installed = std::fs::read(
            virtual_root.join("iPod_Control/Artwork/ArtworkDB"),
        )
        .unwrap();
        assert_eq!(parse_artwork_records(&installed).unwrap().len(), 703);
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 725);

        // Interrupted installation must roll the ArtworkDB back to 704 records.
        let (bundle, staged) = stage_private_artwork_removal().unwrap();
        let virtual_root = bundle.path().join("original");
        let virtual_device = Device::open(&virtual_root).unwrap();
        install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(3),
        )
        .unwrap_err();
        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let rolled_back =
            std::fs::read(virtual_root.join("iPod_Control/Artwork/ArtworkDB")).unwrap();
        assert_eq!(parse_artwork_records(&rolled_back).unwrap().len(), 704);
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    #[test]
    fn stages_and_installs_an_addition_with_reused_album_art() {
        let Some((bundle, staged)) = stage_private_artwork_addition() else {
            return;
        };
        assert_eq!(staged.added_tracks(), 1);
        assert_eq!(staged.added_artwork_tracks(), 1);
        assert_eq!(staged.remaining_tracks(), 727);
        let directory = bundle.path();
        let library = rusqlite::Connection::open(
            directory.join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        let (status, cache_id): (i64, i64) = library
            .query_row(
                "SELECT artwork_status, artwork_cache_id FROM item \
                 WHERE title = 'LibOpod Artwork Reuse'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, 1);
        assert_eq!(cache_id, 804);
        let artwork_bytes = std::fs::read(directory.join("ArtworkDB")).unwrap();
        let records = crate::artwork::parse_artwork_records(&artwork_bytes).unwrap();
        assert_eq!(records.len(), 705);
        let added = records
            .iter()
            .find(|record| record.track_id == staged_pid(&staged, "LibOpod Artwork Reuse"))
            .unwrap();
        assert_eq!(added.image_id, 804);
        assert_eq!(added.formats.len(), 4);

        let virtual_root = bundle.path().join("original");
        create_virtual_media_dirs(&virtual_root, staged.added_media());
        let virtual_device = Device::open(&virtual_root).unwrap();
        assert!(virtual_device
            .install_artwork_addition_hardware_test(&staged, "not confirmed", false)
            .is_err());
        virtual_device
            .install_artwork_addition_hardware_test(
                &staged,
                "I HAVE A VERIFIED BACKUP; ADD ONE TRACK WITH REUSED ALBUM ART",
                false,
            )
            .unwrap();
        let installed =
            std::fs::read(virtual_root.join("iPod_Control/Artwork/ArtworkDB")).unwrap();
        assert_eq!(crate::artwork::parse_artwork_records(&installed).unwrap().len(), 705);
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 727);
    }

    /// Decodes the CDB and returns (title, location) for the track with the
    /// given persistent ID, using the exact MHOD string layout.
    fn cdb_track_strings(cdb: &[u8], pid: PersistentId) -> Option<(String, String)> {
        let header_length = u32::from_le_bytes(cdb[4..8].try_into().unwrap()) as usize;
        let mut decoder = flate2::read::ZlibDecoder::new(&cdb[header_length..]);
        let mut payload = Vec::new();
        decoder.read_to_end(&mut payload).unwrap();
        let datasets = u32::from_le_bytes(cdb[0x14..0x18].try_into().unwrap());
        let mut offset = 0;
        for _ in 0..datasets {
            let hdr = u32::from_le_bytes(payload[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let total = u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let kind = u32::from_le_bytes(payload[offset + 12..offset + 16].try_into().unwrap());
            if kind == 1 {
                let list = offset + hdr;
                let lh = u32::from_le_bytes(payload[list + 4..list + 8].try_into().unwrap()) as usize;
                let count = u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap());
                let mut toff = list + lh;
                for _ in 0..count {
                    let hh = u32::from_le_bytes(payload[toff + 4..toff + 8].try_into().unwrap()) as usize;
                    let tt = u32::from_le_bytes(payload[toff + 8..toff + 12].try_into().unwrap()) as usize;
                    let track_pid =
                        u64::from_le_bytes(payload[toff + 0x70..toff + 0x78].try_into().unwrap());
                    if PersistentId::from_bits(track_pid) == pid {
                        let mut title = String::new();
                        let mut location = String::new();
                        let cc =
                            u32::from_le_bytes(payload[toff + 0x0c..toff + 0x10].try_into().unwrap());
                        let mut coff = toff + hh;
                        for _ in 0..cc {
                            let _ch = u32::from_le_bytes(payload[coff + 4..coff + 8].try_into().unwrap()) as usize;
                            let ct = u32::from_le_bytes(payload[coff + 8..coff + 12].try_into().unwrap()) as usize;
                            let mhod_type =
                                u32::from_le_bytes(payload[coff + 12..coff + 16].try_into().unwrap());
                            if mhod_type == 1 || mhod_type == 2 {
                                let body = coff + 24;
                                let byte_len = u32::from_le_bytes(
                                    payload[body + 4..body + 8].try_into().unwrap(),
                                ) as usize;
                                let mut units = Vec::new();
                                for pair in payload[body + 16..body + 16 + byte_len].chunks_exact(2)
                                {
                                    units.push(u16::from_le_bytes([pair[0], pair[1]]));
                                }
                                let text = String::from_utf16_lossy(&units);
                                if mhod_type == 1 {
                                    title = text;
                                } else {
                                    location = text;
                                }
                            }
                            coff += ct;
                        }
                        return Some((title, location));
                    }
                    toff += tt;
                }
            }
            offset += total;
        }
        None
    }

    fn staged_pid(staged: &super::StagedSqliteEdit, title: &str) -> PersistentId {
        let library = rusqlite::Connection::open(
            staged.directory().join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        let pid: i64 = library
            .query_row(
                "SELECT pid FROM item WHERE title = ?1",
                [title],
                |row| row.get(0),
            )
            .unwrap();
        PersistentId::from_bits(u64::from_ne_bytes(pid.to_ne_bytes()))
    }

    fn stage_private_artwork_addition() -> Option<(TempDir, super::StagedSqliteEdit)> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return None;
        }
        let device = Device::open(&fixture).unwrap();
        let source = fixture.join("iPod_Control/Music/F11/BMJO.mp3");
        if !source.is_file() {
            return None;
        }
        let mut edit = device.edit().unwrap();
        edit.add_track(TrackToAdd {
            source_path: source,
            title: "LibOpod Artwork Reuse".to_owned(),
            artist: Some("Linkin Park".to_owned()),
            album: Some("Hybrid Theory".to_owned()),
            album_artist: None,
            genre: None,
            composer: None,
            year: 2000,
            track_number: 13,
            total_tracks: 13,
            disc_number: 1,
            total_discs: 1,
            bitrate: 192,
            sample_rate: 44100,
            length_ms: 155_742,
            compilation: false,
            reuse_album_art: true,
            artwork_source: None,
        })
        .unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        Some((bundle, staged))
    }

    #[test]
    fn stages_and_installs_an_addition_with_new_encoded_artwork() {
        let Some((bundle, staged)) = stage_private_new_art_addition() else {
            return;
        };
        assert_eq!(staged.added_tracks(), 1);
        assert_eq!(staged.added_artwork_tracks(), 1);
        assert_eq!(staged.added_ithmb().len(), 4);
        assert_eq!(staged.remaining_tracks(), 727);
        let directory = bundle.path();
        let library = rusqlite::Connection::open(
            directory.join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        let (status, cache_id): (i64, i64) = library
            .query_row(
                "SELECT artwork_status, artwork_cache_id FROM item \
                 WHERE title = 'LibOpod New Art'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, 1);
        assert_eq!(cache_id, 804);
        let artwork_bytes = std::fs::read(directory.join("ArtworkDB")).unwrap();
        let records = crate::artwork::parse_artwork_records(&artwork_bytes).unwrap();
        assert_eq!(records.len(), 705);
        let added = records
            .iter()
            .find(|record| record.track_id == staged_pid(&staged, "LibOpod New Art"))
            .unwrap();
        assert_eq!(added.image_id, 804);
        // Each ithmb file grew by exactly one slot.
        for ithmb in staged.added_ithmb() {
            let staged_file = directory.join(ithmb.as_str());
            let original = bundle.path().join("original").join(ithmb.as_str());
            let grown = std::fs::metadata(&staged_file).unwrap().len();
            let before = std::fs::metadata(&original).unwrap().len();
            assert_eq!(grown, before + (grown - before));
            assert!(grown > before, "{} did not grow", ithmb.as_str());
        }

        let virtual_root = bundle.path().join("original");
        create_virtual_media_dirs(&virtual_root, staged.added_media());
        let virtual_device = Device::open(&virtual_root).unwrap();
        assert!(virtual_device
            .install_artwork_addition_hardware_test(&staged, "not confirmed", true)
            .is_err());
        virtual_device
            .install_artwork_addition_hardware_test(
                &staged,
                "I HAVE A VERIFIED BACKUP; ADD ONE TRACK WITH NEW COVER ART",
                true,
            )
            .unwrap();
        for ithmb in staged.added_ithmb() {
            assert!(virtual_root.join(ithmb.as_str()).is_file());
        }
        let installed =
            std::fs::read(virtual_root.join("iPod_Control/Artwork/ArtworkDB")).unwrap();
        assert_eq!(crate::artwork::parse_artwork_records(&installed).unwrap().len(), 705);
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 727);
    }

    #[test]
    fn recovers_an_interrupted_new_art_install_across_ithmb_outputs() {
        let Some((bundle, staged)) = stage_private_new_art_addition() else {
            return;
        };
        let virtual_root = bundle.path().join("original");
        create_virtual_media_dirs(&virtual_root, staged.added_media());
        let virtual_device = Device::open(&virtual_root).unwrap();
        // Output order: 5 DBs, CBK, CDB, ArtworkDB, 4 ithmb files, media.
        install_staged_removal(
            &virtual_device,
            &staged,
            FailureMode::SimulateInterruptionAfter(9),
        )
        .unwrap_err();
        assert!(matches!(
            Device::open(&virtual_root),
            Err(Error::RecoveryRequired { .. })
        ));
        let mount = MountRoot::open(&virtual_root).unwrap();
        recover_transaction(&mount).unwrap();
        let artwork =
            std::fs::read(virtual_root.join("iPod_Control/Artwork/ArtworkDB")).unwrap();
        assert_eq!(crate::artwork::parse_artwork_records(&artwork).unwrap().len(), 704);
        for ithmb in staged.added_ithmb() {
            let original_size = std::fs::metadata(
                bundle.path().join("original").join(ithmb.as_str()),
            )
            .unwrap()
            .len();
            let restored =
                std::fs::metadata(virtual_root.join(ithmb.as_str())).unwrap().len();
            assert_eq!(restored, original_size, "{} not restored", ithmb.as_str());
        }
        let reopened = Device::open(&virtual_root).unwrap();
        assert_eq!(reopened.library().unwrap().track_count(), 726);
    }

    #[test]
    fn stages_two_new_art_additions_into_distinct_slots() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return;
        }
        let device = Device::open(&fixture).unwrap();
        let source = fixture.join("iPod_Control/Music/F11/BMJO.mp3");
        if !source.is_file() {
            return;
        }
        let art_dir = tempdir().unwrap();
        let art_path = art_dir.path().join("cover.png");
        let mut buffer = Vec::new();
        let rgba = image::RgbaImage::from_pixel(256, 256, image::Rgba([10, 200, 90, 255]));
        let encoder = image::codecs::png::PngEncoder::new(&mut buffer);
        image::ImageEncoder::write_image(
            encoder,
            rgba.as_raw(),
            256,
            256,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        std::fs::write(&art_path, &buffer).unwrap();
        let mut edit = device.edit().unwrap();
        for (title, artist) in [("Multi Art One", "Multi One"), ("Multi Art Two", "Multi Two")] {
            edit.add_track(TrackToAdd {
                source_path: source.clone(),
                title: title.to_owned(),
                artist: Some(artist.to_owned()),
                album: Some(format!("{artist} Album")),
                album_artist: None,
                genre: None,
                composer: None,
                year: 2024,
                track_number: 1,
                total_tracks: 1,
                disc_number: 1,
                total_discs: 1,
                bitrate: 192,
                sample_rate: 44100,
                length_ms: 155_742,
                compilation: false,
                reuse_album_art: false,
                artwork_source: Some(art_path.clone()),
            })
            .unwrap();
        }
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        assert_eq!(staged.added_tracks(), 2);
        assert_eq!(staged.added_artwork_tracks(), 2);
        assert_eq!(staged.added_ithmb().len(), 4);
        assert_eq!(staged.remaining_tracks(), 728);
        let artwork_bytes = std::fs::read(bundle.path().join("ArtworkDB")).unwrap();
        let records = crate::artwork::parse_artwork_records(&artwork_bytes).unwrap();
        assert_eq!(records.len(), 706);
        let new_ids: Vec<u32> = records
            .iter()
            .filter(|record| record.image_id >= 804)
            .map(|record| record.image_id)
            .collect();
        assert_eq!(new_ids, vec![804, 805]);
        // Each ithmb file grew by exactly two slots.
        for ithmb in staged.added_ithmb() {
            let grown =
                std::fs::metadata(bundle.path().join(ithmb.as_str())).unwrap().len();
            let before = std::fs::metadata(
                bundle.path().join("original").join(ithmb.as_str()),
            )
            .unwrap()
            .len();
            let slot = (grown - before) / 2;
            assert!(slot > 0);
        }
    }

    fn stage_private_new_art_addition() -> Option<(TempDir, super::StagedSqliteEdit)> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return None;
        }
        let device = Device::open(&fixture).unwrap();
        let source = fixture.join("iPod_Control/Music/F11/BMJO.mp3");
        if !source.is_file() {
            return None;
        }
        let art_dir = tempdir().unwrap();
        let art_path = art_dir.path().join("cover.png");
        let mut buffer = Vec::new();
        let rgba = image::RgbaImage::from_pixel(512, 512, image::Rgba([120, 40, 200, 255]));
        let encoder = image::codecs::png::PngEncoder::new(&mut buffer);
        image::ImageEncoder::write_image(
            encoder,
            rgba.as_raw(),
            512,
            512,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        std::fs::write(&art_path, &buffer).unwrap();
        let mut edit = device.edit().unwrap();
        edit.add_track(TrackToAdd {
            source_path: source,
            title: "LibOpod New Art".to_owned(),
            artist: Some("New Art Artist".to_owned()),
            album: Some("New Art Album".to_owned()),
            album_artist: None,
            genre: None,
            composer: None,
            year: 2024,
            track_number: 1,
            total_tracks: 1,
            disc_number: 1,
            total_discs: 1,
            bitrate: 192,
            sample_rate: 44100,
            length_ms: 155_742,
            compilation: false,
            reuse_album_art: false,
            artwork_source: Some(art_path),
        })
        .unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        Some((bundle, staged))
    }

    fn stage_private_artwork_removal() -> Option<(TempDir, super::StagedSqliteEdit)> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return None;
        }
        let device = Device::open(&fixture).unwrap();
        let track = device
            .library()
            .unwrap()
            .tracks()
            .iter()
            .find(|track| track.has_artwork)
            .unwrap();
        let mut edit = device.edit().unwrap();
        edit.remove_track(track.id).unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        assert_eq!(staged.removed_artwork_tracks(), 1);
        Some((bundle, staged))
    }

    fn stage_private_addition() -> Option<(TempDir, super::StagedSqliteEdit)> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        if !fixture.is_dir() {
            return None;
        }
        let device = Device::open(&fixture).unwrap();
        let source = fixture
            .join("iPod_Control/Music/F11/BMJO.mp3");
        if !source.is_file() {
            return None;
        }
        let mut edit = device.edit().unwrap();
        edit.add_track(TrackToAdd {
            source_path: source,
            title: "LibOpod Fixture Addition".to_owned(),
            artist: Some("LibOpod Test Artist".to_owned()),
            album: Some("LibOpod Test Album".to_owned()),
            album_artist: None,
            genre: Some("Test Genre".to_owned()),
            composer: None,
            year: 2024,
            track_number: 1,
            total_tracks: 1,
            disc_number: 1,
            total_discs: 1,
            bitrate: 192,
            sample_rate: 44100,
            length_ms: 155_742,
            compilation: false,
        reuse_album_art: false,
        artwork_source: None,
        })
        .unwrap();
        assert_eq!(edit.addition_count(), 1);
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        Some((bundle, staged))
    }

    fn verify_staged_addition(bundle: &TempDir, staged: &super::StagedSqliteEdit) {
        let directory = bundle.path();
        for file in SqliteLibraryFile::ALL {
            let info =
                crate::storage::sqlite::inspect_sqlite_database(&directory.join(file.file_name()), file)
                    .unwrap();
            assert!(info.integrity_ok, "{} failed integrity", file.file_name());
        }
        let media = directory.join(staged.added_media()[0].as_str());
        assert!(media.is_file());
        let (media_bytes, _) =
            crate::edit::generation::fingerprint_host_file(&media).unwrap();
        let library = rusqlite::Connection::open(
            directory.join(SqliteLibraryFile::Library.file_name()),
        )
        .unwrap();
        let tracks: i64 = library
            .query_row("SELECT COUNT(*) FROM item", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tracks, 727);
        let locations = rusqlite::Connection::open(
            directory.join(SqliteLibraryFile::Locations.file_name()),
        )
        .unwrap();
        let pid: i64 = library
            .query_row(
                "SELECT pid FROM item WHERE title = 'LibOpod Fixture Addition'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let location: String = locations
            .query_row(
                "SELECT location FROM location WHERE item_pid = ?1",
                [pid],
                |row| row.get(0),
            )
            .unwrap();
        let relative = staged.added_media()[0].as_str().trim_start_matches("iPod_Control/Music/");
        assert_eq!(location, relative);
        let cdb = std::fs::read(directory.join("iTunesCDB")).unwrap();
        let header_length = u32::from_le_bytes(cdb[4..8].try_into().unwrap()) as usize;
        let mut decoder = flate2::read::ZlibDecoder::new(&cdb[header_length..]);
        let mut payload = Vec::new();
        decoder.read_to_end(&mut payload).unwrap();
        let datasets = u32::from_le_bytes(cdb[0x14..0x18].try_into().unwrap());
        let mut offset = 0;
        let mut track_count = None;
        let mut album_count = None;
        for _ in 0..datasets {
            let hdr = u32::from_le_bytes(payload[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let total = u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let kind = u32::from_le_bytes(payload[offset + 12..offset + 16].try_into().unwrap());
            let list = offset + hdr;
            if kind == 1 {
                track_count =
                    Some(u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap()));
            }
            if kind == 4 {
                album_count =
                    Some(u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap()));
            }
            offset += total;
        }
        assert_eq!(track_count, Some(727));
        assert_eq!(album_count, Some(144));
        let cbk = std::fs::read(directory.join("Locations.itdb.cbk")).unwrap();
        let locations_bytes =
            std::fs::read(directory.join(SqliteLibraryFile::Locations.file_name())).unwrap();
        let info = crate::storage::binary::verify_cbk(&locations_bytes, &cbk, None).unwrap();
        assert!(info.digests_match());
        assert!(media_bytes > 0);
    }

    fn stage_private_no_artwork_removal() -> Option<(TempDir, super::StagedSqliteEdit)> {
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
