//! Synthetic Classic fixtures: these tests never need private device backups.
use std::{fs, path::Path};

use tempfile::{tempdir, TempDir};

use super::commit::{install_staged_removal, recover_transaction, FailureMode};
use crate::{Device, Error, MediaDeletionPolicy, MediaKind, MountRoot, TrackToAdd};

const GUID: [u8; 8] = [1, 35, 69, 103, 137, 171, 205, 239];
const DB: &str = "iPod_Control/iTunes/iTunesDB";

fn put(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 4].copy_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
}

fn chunk(magic: [u8; 4], header: usize) -> Vec<u8> {
    let mut bytes = vec![0; header];
    bytes[..4].copy_from_slice(&magic);
    put(&mut bytes, 4, header);
    put(&mut bytes, 8, header);
    bytes
}

fn dataset(kind: usize, magic: [u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = chunk(*b"mhsd", 96);
    put(&mut bytes, 12, kind);
    let mut list = chunk(magic, 92);
    put(&mut list, 8, children.len());
    bytes.extend(list);
    for child in children {
        bytes.extend(child);
    }
    let len = bytes.len();
    put(&mut bytes, 8, len);
    bytes
}

fn empty_database() -> Vec<u8> {
    let mut master = chunk(*b"mhyp", 108);
    master[0x14] = 1;
    put(&mut master, 0x1c, 0x1234);
    put(&mut master, 0x2c, 5);
    let mut preferences = chunk(*b"mhod", 24);
    put(&mut preferences, 12, 100);
    master.extend(preferences);
    put(&mut master, 12, 1);
    let len = master.len();
    put(&mut master, 8, len);
    let mut bytes = chunk(*b"mhbd", 244);
    put(&mut bytes, 0x10, 0x30);
    put(&mut bytes, 0x14, 4);
    put(&mut bytes, 0x18, 0x5678);
    bytes.extend(dataset(1, *b"mhlt", &[]));
    bytes.extend(dataset(2, *b"mhlp", &[master.clone()]));
    bytes.extend(dataset(3, *b"mhlp", &[master]));
    bytes.extend(dataset(4, *b"mhla", &[]));
    let len = bytes.len();
    put(&mut bytes, 8, len);
    crate::crypto::hash58::sign_database(&GUID, &mut bytes);
    bytes
}

fn virtual_classic(identity: &str, artwork: bool) -> TempDir {
    let directory = tempdir().unwrap();
    let root = directory.path();
    for folder in ["Device", "iTunes", "Artwork"] {
        fs::create_dir_all(root.join("iPod_Control").join(folder)).unwrap();
    }
    fs::write(
        root.join("iPod_Control/Device/SysInfo"),
        format!("{identity}\nFirewireGuid: 0123456789abcdef\n"),
    )
    .unwrap();
    fs::write(root.join(DB), empty_database()).unwrap();
    if artwork {
        let mut bytes = chunk(*b"mhfd", 132);
        put(&mut bytes, 20, 1);
        put(&mut bytes, 28, 100);
        bytes.extend(dataset(1, *b"mhli", &[]));
        let len = bytes.len();
        put(&mut bytes, 8, len);
        fs::write(root.join("iPod_Control/Artwork/ArtworkDB"), bytes).unwrap();
    }
    for index in 0..50 {
        fs::create_dir_all(root.join(format!("iPod_Control/Music/F{index:02}"))).unwrap();
    }
    directory
}

fn addition(root: &Path, artwork: bool) -> TrackToAdd {
    let source = root.join("source.mp3");
    fs::write(&source, b"synthetic media; libopod does not decode audio").unwrap();
    let artwork_source = artwork.then(|| {
        let path = root.join("cover.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 40, 120, 255]))
            .save(&path)
            .unwrap();
        path
    });
    TrackToAdd {
        source_path: source,
        title: "Classic test".to_owned(),
        artist: Some("Artist".to_owned()),
        album: Some("Album".to_owned()),
        album_artist: None,
        genre: None,
        composer: None,
        year: 2009,
        track_number: 1,
        total_tracks: 1,
        disc_number: 1,
        total_discs: 1,
        bitrate: 128,
        sample_rate: 44_100,
        length_ms: 60_000,
        compilation: false,
        media_kind: MediaKind::Song,
        reuse_album_art: false,
        artwork_source,
    }
}

fn assert_signed(root: &Path) {
    assert!(crate::crypto::hash58::verify(
        &GUID,
        &fs::read(root.join(DB)).unwrap()
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn classic_add_playlist_remove_and_recovery_roundtrip() {
    for identity in [
        "ModelNumStr: MC293LL/A",
        "USBProductID: 0x1261",
        "ModelNumStr: MB029",
        "ModelNumStr: MB562",
    ] {
        // Exercise both artwork and no-ArtworkDB paths, including the first
        // addition to an initialized but empty iTunesDB.
        for artwork in [false, true] {
            let directory = virtual_classic(identity, artwork);
            let root = directory.path();
            let device = Device::open(root).unwrap();
            let original = fs::read(root.join(DB)).unwrap();
            let mut edit = device.edit().unwrap();
            edit.add_track(addition(root, artwork)).unwrap();
            let bundle = tempdir().unwrap();
            let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
            assert_eq!(staged.added_tracks(), 1);
            assert_eq!(staged.added_artwork_tracks(), usize::from(artwork));
            assert_eq!(
                fs::read(root.join(DB)).unwrap(),
                original,
                "staging mutated source"
            );

            install_staged_removal(&device, &staged, FailureMode::SimulateInterruptionAfter(1))
                .unwrap_err();
            recover_transaction(&MountRoot::open(root).unwrap()).unwrap();
            assert_eq!(fs::read(root.join(DB)).unwrap(), original);
            assert!(!root.join(staged.added_media()[0].as_str()).exists());
            let device = Device::open(root).unwrap();
            staged.install(&device).unwrap();
            assert_signed(root);
            let device = Device::open(root).unwrap();
            let tracks = device.library().unwrap().tracks();
            assert_eq!(tracks.len(), 1);
            assert_eq!(tracks[0].title, "Classic test");
            assert_eq!(tracks[0].has_artwork, artwork);
            let id = tracks[0].id;
            let media = root.join(tracks[0].location.as_str());
            assert!(media.is_file());
            if artwork {
                let frames = &device.inspection().artwork_frames;
                assert_eq!(frames.len(), 4);
                assert_eq!(
                    fs::metadata(root.join("iPod_Control/Artwork/F1061_1.ithmb"))
                        .unwrap()
                        .len(),
                    6_272
                );
            }

            let mut edit = device.edit().unwrap();
            let playlist_id = edit.create_playlist("Classic mix", &[id, id]).unwrap();
            let bundle = tempdir().unwrap();
            edit.stage_sqlite_preview(bundle.path())
                .unwrap()
                .install(&device)
                .unwrap();
            let device = Device::open(root).unwrap();
            let playlist = device
                .library()
                .unwrap()
                .playlists()
                .iter()
                .find(|p| p.id == playlist_id)
                .unwrap();
            assert_eq!(playlist.track_ids(), &[id, id]);
            let mut edit = device.edit().unwrap();
            edit.rename_playlist(playlist_id, "Renamed").unwrap();
            edit.set_playlist_tracks(playlist_id, &[id]).unwrap();
            let bundle = tempdir().unwrap();
            edit.stage_sqlite_preview(bundle.path())
                .unwrap()
                .install(&device)
                .unwrap();
            let device = Device::open(root).unwrap();
            let playlist = device
                .library()
                .unwrap()
                .playlists()
                .iter()
                .find(|p| p.id == playlist_id)
                .unwrap();
            assert_eq!(playlist.name, "Renamed");
            assert_eq!(playlist.track_ids(), &[id]);
            assert_signed(root);

            let mut edit = device.edit().unwrap();
            edit.delete_playlist(playlist_id).unwrap();
            edit.remove_track(id).unwrap();
            edit.set_media_policy(MediaDeletionPolicy::Delete);
            let bundle = tempdir().unwrap();
            edit.stage_sqlite_preview(bundle.path())
                .unwrap()
                .install(&device)
                .unwrap();
            let device = Device::open(root).unwrap();
            assert_eq!(device.library().unwrap().track_count(), 0);
            assert!(device
                .library()
                .unwrap()
                .playlists()
                .iter()
                .all(|p| p.id != playlist_id));
            assert!(!media.exists());
            assert_signed(root);
        }
    }
}

fn number(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

fn children(bytes: &[u8], offset: usize, count: usize) -> Vec<&[u8]> {
    let mut offset = offset;
    (0..count)
        .map(|_| {
            let end = offset + number(bytes, offset + 8);
            let child = &bytes[offset..end];
            offset = end;
            child
        })
        .collect()
}

fn podcast_items(database: &[u8], kind: usize) -> Vec<&[u8]> {
    let datasets = children(database, number(database, 4), number(database, 0x14));
    assert!(
        datasets.iter().position(|d| number(d, 12) == 3).unwrap()
            < datasets.iter().position(|d| number(d, 12) == 2).unwrap()
    );
    let dataset = datasets.iter().find(|d| number(d, 12) == kind).unwrap();
    let list = number(dataset, 4);
    let playlists = children(
        dataset,
        list + number(dataset, list + 4),
        number(dataset, list + 8),
    );
    let podcasts: Vec<_> = playlists.iter().filter(|p| p[0x2a] & 1 != 0).collect();
    assert_eq!(podcasts.len(), 1);
    let playlist = podcasts[0];
    let metadata = children(playlist, number(playlist, 4), number(playlist, 12));
    let body = number(playlist, 4) + metadata.iter().map(|m| m.len()).sum::<usize>();
    children(playlist, body, number(playlist, 16))
}

fn replace_datasets(root: &Path, datasets: &[Vec<u8>]) {
    let original = fs::read(root.join(DB)).unwrap();
    let mut database = original[..number(&original, 4)].to_vec();
    put(&mut database, 0x14, datasets.len());
    database.extend(datasets.concat());
    let len = database.len();
    put(&mut database, 8, len);
    crate::crypto::hash58::sign_database(&GUID, &mut database);
    fs::write(root.join(DB), database).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn classic_podcasts_have_a_protected_container_and_show_groups() {
    for identity in [
        "ModelNumStr: MC293",
        "USBProductID: 0x1261",
        "ModelNumStr: MB029",
        "ModelNumStr: MB562",
    ] {
        let directory = virtual_classic(identity, false);
        let root = directory.path();
        if identity == "USBProductID: 0x1261" {
            // A library without the podcast-capable dataset must gain one.
            let database = fs::read(root.join(DB)).unwrap();
            let datasets: Vec<_> =
                children(&database, number(&database, 4), number(&database, 0x14))
                    .into_iter()
                    .filter(|d| number(d, 12) != 3)
                    .map(<[u8]>::to_vec)
                    .collect();
            replace_datasets(root, &datasets);
        }
        let device = Device::open(root).unwrap();
        assert!(device.profile().unwrap().supports_podcasts());
        let mut edit = device.edit().unwrap();
        edit.add_track(addition(root, false)).unwrap(); // a normal song
        for (title, album) in [("A1", "Show A"), ("B1", "Show B"), ("A2", "Show A")] {
            let mut track = addition(root, false);
            track.title = title.to_owned();
            track.album = Some(album.to_owned());
            track.media_kind = MediaKind::Podcast;
            edit.add_track(track).unwrap();
        }
        let original = fs::read(root.join(DB)).unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        assert_eq!(fs::read(root.join(DB)).unwrap(), original);
        install_staged_removal(&device, &staged, FailureMode::SimulateInterruptionAfter(1))
            .unwrap_err();
        recover_transaction(&MountRoot::open(root).unwrap()).unwrap();
        assert_eq!(fs::read(root.join(DB)).unwrap(), original);
        staged.install(&Device::open(root).unwrap()).unwrap();
        let device = Device::open(root).unwrap();
        let library = device.library().unwrap();
        let podcast = library
            .playlists()
            .iter()
            .find(|p| p.distinguished_kind == 11)
            .unwrap();
        let podcast_id = podcast.id;
        assert_eq!(podcast.name, "Podcasts");
        assert!(podcast.is_hidden);
        assert_eq!(podcast.track_ids().len(), 3);
        assert!(podcast.track_ids().iter().all(|id| library
            .tracks()
            .iter()
            .any(|t| t.id == *id && t.media_kind == MediaKind::Podcast)));
        let mut edit = device.edit().unwrap();
        assert!(edit.rename_playlist(podcast_id, "Not editable").is_err());
        assert!(edit.delete_playlist(podcast_id).is_err());
        let database = fs::read(root.join(DB)).unwrap();
        let flat = podcast_items(&database, 2);
        assert_eq!(flat.len(), 3);
        let grouped = podcast_items(&database, 3);
        assert_eq!(grouped.len(), 5);
        let mut group_id = 0;
        let mut ids = std::collections::BTreeSet::new();
        for item in grouped {
            assert!(
                ids.insert(number(item, 0x14)),
                "group and episode IDs must be unique"
            );
            if number(item, 0x10) == 0x100 {
                group_id = number(item, 0x14);
                assert_eq!(number(item, 0x18), 0);
                assert_eq!(number(item, number(item, 4) + 12), 1);
            } else {
                assert_ne!(number(item, 0x18), 0);
                assert_eq!(number(item, 0x20), group_id);
                assert_eq!(number(item, number(item, 4) + 24), number(item, 0x14));
            }
        }
        let datasets = children(&database, number(&database, 4), number(&database, 0x14));
        let tracks = datasets.iter().find(|d| number(d, 12) == 1).unwrap();
        let list = number(tracks, 4);
        for track in children(
            tracks,
            list + number(tracks, list + 4),
            number(tracks, list + 8),
        ) {
            let podcast = number(track, 0xd0) == 4;
            assert_eq!(
                &track[0xa5..0xa8],
                if podcast { &[1, 1, 1] } else { &[0, 0, 0] }
            );
            assert_eq!(track[0xb2], if podcast { 2 } else { 0 });
        }
        assert_signed(root);

        if identity == "ModelNumStr: MC293" {
            // iOpenPod may keep Podcasts exclusively in dataset 3. Ensure
            // the reader exposes it and a later addition reuses its identity.
            let mut datasets: Vec<_> =
                children(&database, number(&database, 4), number(&database, 0x14))
                    .into_iter()
                    .map(<[u8]>::to_vec)
                    .collect();
            let two = datasets.iter_mut().find(|d| number(d, 12) == 2).unwrap();
            let list = number(two, 4);
            let body = list + number(two, list + 4);
            let playlists: Vec<_> = children(two, body, number(two, list + 8))
                .into_iter()
                .filter(|p| p[0x2a] & 1 == 0)
                .map(<[u8]>::to_vec)
                .collect();
            two.truncate(body);
            two.extend(playlists.concat());
            put(two, list + 8, playlists.len());
            let len = two.len();
            put(two, 8, len);
            replace_datasets(root, &datasets);
        }
        // Add another episode in a second sync: reuse the container ID.
        let device = Device::open(root).unwrap();
        assert_eq!(
            device
                .library()
                .unwrap()
                .playlists()
                .iter()
                .find(|p| p.distinguished_kind == 11)
                .unwrap()
                .id,
            podcast_id
        );
        let mut edit = device.edit().unwrap();
        let mut track = addition(root, false);
        track.title = "B2".to_owned();
        track.album = Some("Show B".to_owned());
        track.media_kind = MediaKind::Podcast;
        edit.add_track(track).unwrap();
        let bundle = tempdir().unwrap();
        edit.stage_sqlite_preview(bundle.path())
            .unwrap()
            .install(&device)
            .unwrap();
        let device = Device::open(root).unwrap();
        assert_eq!(
            device
                .library()
                .unwrap()
                .playlists()
                .iter()
                .find(|p| p.distinguished_kind == 11)
                .unwrap()
                .id,
            podcast_id
        );
        // Removing a complete show drops its group header; removing the final
        // episodes retains an empty, protected Podcasts container.
        for album in ["Show B", "Show A"] {
            let device = Device::open(root).unwrap();
            let mut edit = device.edit().unwrap();
            for track in device
                .library()
                .unwrap()
                .tracks()
                .iter()
                .filter(|t| t.album == album)
            {
                edit.remove_track(track.id).unwrap();
            }
            let bundle = tempdir().unwrap();
            edit.stage_sqlite_preview(bundle.path())
                .unwrap()
                .install(&device)
                .unwrap();
            let database = fs::read(root.join(DB)).unwrap();
            let expected = if album == "Show B" { 3 } else { 0 };
            assert_eq!(podcast_items(&database, 3).len(), expected);
            assert_signed(root);
        }
        assert_eq!(
            Device::open(root).unwrap().library().unwrap().track_count(),
            1
        );
    }
}

#[test]
fn progress_events_bracket_real_staging_install_and_deletion_work() {
    use crate::ProgressEvent;

    let directory = virtual_classic("ModelNumStr: MC293", false);
    let root = directory.path();
    let device = Device::open(root).unwrap();
    let mut edit = device.edit().unwrap();
    for title in ["First", "Second"] {
        let mut track = addition(root, false);
        track.title = title.to_owned();
        edit.add_track(track).unwrap();
    }
    let bundle = tempdir().unwrap();
    let mut track_events = Vec::new();
    let staged = edit
        .stage_sqlite_preview_with_progress(bundle.path(), |event| {
            if let ProgressEvent::Item {
                operation,
                current,
                total,
                name,
            } = event
            {
                assert!(current > 0 && current <= total);
                if operation == "Staging audio and artwork" {
                    let music = bundle.path().join("iPod_Control/Music");
                    let copied = fs::read_dir(music).map_or(0, |dirs| {
                        dirs.map(|dir| fs::read_dir(dir.unwrap().path()).unwrap().count())
                            .sum::<usize>()
                    });
                    assert_eq!(
                        copied,
                        current - 1,
                        "event must precede copying the current track"
                    );
                    track_events.push((current, total, name.to_owned()));
                }
            }
        })
        .unwrap();
    assert_eq!(
        track_events,
        [(1, 2, "First".to_owned()), (2, 2, "Second".to_owned())]
    );
    let mut installed_media = 0;
    staged
        .install_with_progress(&device, |event| {
            if let ProgressEvent::Item {
                operation,
                current,
                total,
                name,
            } = event
            {
                assert!(current > 0 && current <= total);
                if name.starts_with("iPod_Control/Music/") {
                    if operation == "Installing" {
                        assert!(!root.join(name).exists());
                        installed_media += 1;
                    } else if operation == "Verifying installed file" {
                        assert!(root.join(name).is_file());
                    }
                }
            }
        })
        .unwrap();
    assert_eq!(installed_media, 2);
    assert_signed(root);

    let device = Device::open(root).unwrap();
    let track = &device.library().unwrap().tracks()[0];
    let media = track.location.as_str().to_owned();
    let mut edit = device.edit().unwrap();
    edit.set_media_policy(MediaDeletionPolicy::Delete);
    edit.remove_track(track.id).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    let mut deleting = false;
    staged
        .install_with_progress(&device, |event| {
            if let ProgressEvent::Item {
                operation: "Deleting media",
                current,
                total,
                name,
            } = event
            {
                assert_eq!((current, total, name), (1, 1, media.as_str()));
                assert!(root.join(name).exists());
                deleting = true;
            }
        })
        .unwrap();
    assert!(deleting);
    assert!(!root.join(media).exists());
    assert_signed(root);
}

#[test]
fn progress_does_not_claim_installation_when_bundle_verification_fails() {
    use crate::ProgressEvent;
    let directory = virtual_classic("ModelNumStr: MC293", false);
    let device = Device::open(directory.path()).unwrap();
    let mut edit = device.edit().unwrap();
    edit.add_track(addition(directory.path(), false)).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    fs::write(bundle.path().join("iTunesDB"), b"corrupt staged database").unwrap();
    let mut installing = false;
    assert!(staged
        .install_with_progress(&device, |event| {
            if let ProgressEvent::Item {
                operation: "Installing",
                ..
            } = event
            {
                installing = true;
            }
        })
        .is_err());
    assert!(!installing);
    assert_eq!(
        Device::open(directory.path())
            .unwrap()
            .library()
            .unwrap()
            .track_count(),
        0
    );
}

#[test]
fn classic_without_guid_is_readable_but_not_writable() {
    let directory = virtual_classic("ModelNumStr: MC297", false);
    fs::write(
        directory.path().join("iPod_Control/Device/SysInfo"),
        "ModelNumStr: MC297",
    )
    .unwrap();
    let device = Device::open(directory.path()).unwrap();
    assert!(device.library().is_some());
    assert!(matches!(device.edit(), Err(Error::Unsupported { .. })));
}
