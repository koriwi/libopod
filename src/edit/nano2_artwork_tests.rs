//! Native Nano 2 cover sizes corroborated by libgpod and a read-only device
//! inspection. All writes in these tests target disposable synthetic copies.
use std::{fs, path::Path};

use tempfile::{tempdir, TempDir};

use super::{
    classic_tests::{addition, virtual_classic},
    commit::{install_staged_with_progress, FailureMode},
};
use crate::{Device, InstallMode, MediaDeletionPolicy};

const DB: &str = "iPod_Control/iTunes/iTunesDB";
const ART: &str = "iPod_Control/Artwork/ArtworkDB";
const FORMATS: [(u32, u64); 2] = [(1031, 3_528), (1027, 20_000)];

fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn frame(root: &Path, id: u32) -> std::path::PathBuf {
    root.join(format!("iPod_Control/Artwork/F{id}_1.ithmb"))
}

fn fixture() -> TempDir {
    let directory = virtual_classic("ModelFamily: iPod Nano\nGeneration: 2nd Gen", true);
    let mut database = fs::read(directory.path().join(DB)).unwrap();
    put(&mut database, 16, 0x13); // Native short 0x148 MHIT headers, no HASH58.
    fs::write(directory.path().join(DB), database).unwrap();
    // Include the initialized format catalog, which edits must preserve.
    let mut artwork = fs::read(directory.path().join(ART)).unwrap();
    let mut catalog = vec![0_u8; 96 + 92 + 2 * 124];
    catalog[..4].copy_from_slice(b"mhsd");
    put(&mut catalog, 4, 96);
    let length = u32::try_from(catalog.len()).unwrap();
    put(&mut catalog, 8, length);
    put(&mut catalog, 12, 3);
    catalog[96..100].copy_from_slice(b"mhlf");
    put(&mut catalog, 100, 92);
    put(&mut catalog, 104, 2);
    for (index, (id, bytes)) in FORMATS.iter().enumerate() {
        let start = 96 + 92 + index * 124;
        catalog[start..start + 4].copy_from_slice(b"mhif");
        put(&mut catalog, start + 4, 124);
        put(&mut catalog, start + 8, 124);
        put(&mut catalog, start + 16, *id);
        put(&mut catalog, start + 20, u32::try_from(*bytes).unwrap());
    }
    artwork.extend(catalog);
    let length = u32::try_from(artwork.len()).unwrap();
    put(&mut artwork, 8, length);
    put(&mut artwork, 20, 2);
    fs::write(directory.path().join(ART), artwork).unwrap();
    directory
}

fn add(device: &Device, title: &str, color: [u8; 4]) -> Device {
    let mut track = addition(device.mount().as_path(), true);
    track.title = title.to_owned();
    image::RgbaImage::from_pixel(64, 64, image::Rgba(color))
        .save(track.artwork_source.as_ref().unwrap())
        .unwrap();
    let mut edit = device.edit().unwrap();
    edit.add_track(track).unwrap();
    let bundle = tempdir().unwrap();
    edit.stage_sqlite_preview(bundle.path())
        .unwrap()
        .install_and_open(device, InstallMode::Full, |_| {})
        .unwrap()
}

fn artwork_bytes(root: &Path) -> Vec<Vec<u8>> {
    std::iter::once(fs::read(root.join(ART)).unwrap())
        .chain(
            FORMATS
                .iter()
                .map(|(id, _)| fs::read(frame(root, *id)).unwrap()),
        )
        .collect()
}

#[test]
fn encoder_matches_native_dimensions_sizes_and_rgb565_byte_order() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("cover.png");
    for (color, expected) in [
        ([255, 0, 0, 255], [0x00, 0xf8]),
        ([0, 255, 0, 255], [0xe0, 0x07]),
        ([0, 0, 255, 255], [0x1f, 0x00]),
    ] {
        image::RgbaImage::from_pixel(17, 29, image::Rgba(color))
            .save(&path)
            .unwrap();
        let frames =
            crate::artwork::encode_classic_frames(&fs::read(&path).unwrap(), "nano-2g").unwrap();
        assert_eq!(frames.len(), 2);
        for (frame, (id, size)) in frames.iter().zip(FORMATS) {
            assert_eq!(frame.format_id, id);
            assert_eq!(frame.slot_bytes(), size);
            assert_eq!(frame.rgb565.len() as u64, size);
            assert_eq!(frame.width, if id == 1031 { 42 } else { 100 });
            assert_eq!(frame.width, frame.height);
            assert_eq!(frame.width, frame.stride_pixels);
            assert!(frame.rgb565.chunks_exact(2).all(|pixel| pixel == expected));
        }
    }
}

#[test]
fn nano2_add_delete_and_reindex_preserve_surviving_covers() {
    for policy in [MediaDeletionPolicy::KeepOrphan, MediaDeletionPolicy::Delete] {
        let directory = fixture();
        let root = directory.path();
        let empty = Device::open(root).unwrap();
        assert_eq!(empty.profile().unwrap().key(), "nano-2g");
        assert!(empty.profile().unwrap().capabilities().supports_artwork());
        let original = fs::read(root.join(ART)).unwrap();
        let catalog = &original[original.len() - (96 + 92 + 2 * 124)..];
        let first = add(&empty, "Red", [255, 0, 0, 255]);
        let second = add(&first, "Blue", [0, 0, 255, 255]);
        let device = add(&second, "Green", [0, 255, 0, 255]);
        assert!(device
            .library()
            .unwrap()
            .tracks()
            .iter()
            .all(|track| track.has_artwork));
        let raw = fs::read(root.join(DB)).unwrap();
        let mhit = raw.windows(4).position(|magic| magic == b"mhit").unwrap();
        assert_eq!(
            u32::from_le_bytes(raw[mhit + 4..mhit + 8].try_into().unwrap()),
            0x148
        );
        assert_eq!(
            u16::from_le_bytes(raw[mhit + 0x7c..mhit + 0x7e].try_into().unwrap()),
            1
        );
        assert_eq!(raw[mhit + 0xa4], 1);
        assert_eq!(&raw[mhit + 0x148..mhit + 0x14c], b"mhod");
        let tracks = device.library().unwrap().tracks();
        let removed = tracks.iter().find(|track| track.title == "Blue").unwrap();
        let media = root.join(removed.location.as_str());
        let old_frames: Vec<_> = FORMATS
            .iter()
            .map(|(id, _)| fs::read(frame(root, *id)).unwrap())
            .collect();
        let mut edit = device.edit().unwrap();
        edit.set_media_policy(policy);
        edit.remove_track(removed.id).unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        assert_eq!(staged.removed_ithmb().len(), 2);
        let installed = staged
            .install_and_open(&device, InstallMode::Full, |_| {})
            .unwrap();
        assert_eq!(installed.library().unwrap().track_count(), 2);
        assert_eq!(media.exists(), policy == MediaDeletionPolicy::KeepOrphan);
        let bytes = fs::read(root.join(ART)).unwrap();
        assert!(bytes.ends_with(catalog));
        let records = crate::artwork::parse_artwork_records(&bytes).unwrap();
        assert_eq!(records.len(), 2);
        for (index, (id, slot)) in FORMATS.iter().enumerate() {
            let slot = usize::try_from(*slot).unwrap();
            let current = fs::read(frame(root, *id)).unwrap();
            assert_eq!(
                current,
                [
                    &old_frames[index][..slot],
                    &old_frames[index][2 * slot..3 * slot]
                ]
                .concat()
            );
            for (position, record) in records.iter().enumerate() {
                assert!(installed
                    .library()
                    .unwrap()
                    .tracks()
                    .iter()
                    .any(|track| track.id == record.track_id));
                let reference = record
                    .formats
                    .iter()
                    .find(|format| format.format_id == *id)
                    .unwrap();
                assert_eq!(reference.ithmb_offset as usize, position * slot);
                assert_eq!(reference.image_size as usize, slot);
            }
        }
    }
}

#[test]
fn nano2_recovery_covers_partial_additions_and_artwork_bearing_deletions() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        for removal in [false, true] {
            for checkpoint in [1, 2, 3] {
                let directory = fixture();
                let empty = Device::open(directory.path()).unwrap();
                let device = add(&empty, "Original", [255, 0, 0, 255]);
                let old_art = artwork_bytes(directory.path());
                let old_audio = device.library().unwrap().tracks()[0].location.clone();
                let mut edit = device.edit().unwrap();
                if removal {
                    edit.remove_track(device.library().unwrap().tracks()[0].id)
                        .unwrap();
                } else {
                    edit.add_track(addition(directory.path(), true)).unwrap();
                }
                let bundle = tempdir().unwrap();
                let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
                install_staged_with_progress(
                    &device,
                    &staged,
                    FailureMode::SimulateInterruptionAfter(checkpoint),
                    mode,
                    &mut |_| {},
                )
                .unwrap_err();
                let journal_path = directory
                    .path()
                    .join(super::commit::TRANSACTION_PATH)
                    .join("journal.json");
                let journal: serde_json::Value =
                    serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
                assert!(
                    journal.get("appends").is_none(),
                    "unaligned Nano 2 formats must use rename backups"
                );
                drop(bundle);
                assert!(crate::recover_interrupted_transaction(directory.path()).unwrap());
                assert_eq!(
                    Device::open(directory.path()).unwrap().generation(),
                    device.generation()
                );
                assert_eq!(artwork_bytes(directory.path()), old_art);
                assert!(directory.path().join(old_audio.as_str()).exists());
                for media in staged.added_media() {
                    assert!(!directory.path().join(media.as_str()).exists());
                }
            }
        }
    }
}

#[test]
fn reused_nano2_covers_preserve_padding_and_opaque_children() {
    let directory = fixture();
    let empty = Device::open(directory.path()).unwrap();
    add(&empty, "Original", [255, 0, 0, 255]);
    let mut bytes = fs::read(directory.path().join(ART)).unwrap();
    let positions: Vec<_> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(position, magic)| (magic == b"mhni").then_some(position))
        .collect();
    for position in positions {
        let id = u32::from_le_bytes(bytes[position + 16..position + 20].try_into().unwrap());
        let (padding, height) = if id == 1031 {
            (5_u16, 37_u16)
        } else {
            (12, 87)
        };
        bytes[position + 28..position + 30].copy_from_slice(&padding.to_le_bytes());
        bytes[position + 32..position + 34].copy_from_slice(&height.to_le_bytes());
        put(&mut bytes, position + 36, 0x1234_5678);
    }
    fs::write(directory.path().join(ART), &bytes).unwrap();
    let device = Device::open(directory.path()).unwrap();
    let old_pid = device.library().unwrap().tracks()[0].id;
    let children = crate::artwork::reuse_record_children(&bytes, old_pid).unwrap();
    let old_frames: Vec<_> = FORMATS
        .iter()
        .map(|(id, _)| fs::read(frame(directory.path(), *id)).unwrap())
        .collect();
    let mut track = addition(directory.path(), false);
    track.title = "Reused".to_owned();
    track.reuse_album_art = true;
    let mut edit = device.edit().unwrap();
    edit.add_track(track).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    assert!(staged.added_ithmb().is_empty());
    let installed = staged
        .install_and_open(&device, InstallMode::Full, |_| {})
        .unwrap();
    let new_pid = installed
        .library()
        .unwrap()
        .tracks()
        .iter()
        .find(|track| track.title == "Reused")
        .unwrap()
        .id;
    let current = fs::read(directory.path().join(ART)).unwrap();
    assert_eq!(
        crate::artwork::reuse_record_children(&current, new_pid).unwrap(),
        children
    );
    for (index, (id, _)) in FORMATS.iter().enumerate() {
        assert_eq!(
            fs::read(frame(directory.path(), *id)).unwrap(),
            old_frames[index]
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Keep the optional native-copy qualification sequence together.
fn native_nano2_snapshot_roundtrip_when_supplied() {
    // Optional qualification input, never a write target. No private bytes or
    // metadata enter the repository. Without a supplied snapshot, skip it.
    let Some(source) = std::env::var_os("LIBOPOD_TEST_NANO2_SNAPSHOT") else {
        return;
    };
    let source = Path::new(&source);
    let directory = fixture();
    for relative in [
        "iTunes/iTunesDB",
        "Artwork/ArtworkDB",
        "Artwork/F1031_1.ithmb",
        "Artwork/F1027_1.ithmb",
    ] {
        fs::copy(
            source.join(relative),
            directory.path().join("iPod_Control").join(relative),
        )
        .unwrap();
    }
    let device = Device::open(directory.path()).unwrap();
    // Reserve old audio names on the copy without reading/copying any songs.
    for track in device.library().unwrap().tracks() {
        let path = directory.path().join(track.location.as_str());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"unchanged audio placeholder").unwrap();
    }
    let original_count = device.library().unwrap().track_count();
    let before = artwork_bytes(directory.path());
    let records = crate::artwork::parse_artwork_records(&before[0]).unwrap();
    let removed = records
        .iter()
        .find(|record| {
            device
                .library()
                .unwrap()
                .tracks()
                .iter()
                .any(|track| track.id == record.track_id && track.has_artwork)
        })
        .unwrap()
        .track_id;
    let mut edit = device.edit().unwrap();
    edit.remove_track(removed).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    assert_eq!(staged.removed_ithmb().len(), 2);
    install_staged_with_progress(
        &device,
        &staged,
        FailureMode::SimulateInterruptionDuringValidation,
        InstallMode::Full,
        &mut |_| {},
    )
    .unwrap_err();
    drop(bundle);
    assert!(crate::recover_interrupted_transaction(directory.path()).unwrap());
    assert_eq!(artwork_bytes(directory.path()), before);
    let device = Device::open(directory.path()).unwrap();
    let installed = add(&device, "Synthetic snapshot cover", [255, 0, 0, 255]);
    assert_eq!(
        installed.library().unwrap().track_count(),
        original_count + 1
    );
    let new = installed
        .library()
        .unwrap()
        .tracks()
        .iter()
        .find(|track| track.title == "Synthetic snapshot cover")
        .unwrap();
    assert!(new.has_artwork);
    let after = artwork_bytes(directory.path());
    let record = crate::artwork::parse_artwork_records(&after[0])
        .unwrap()
        .into_iter()
        .find(|record| record.track_id == new.id)
        .unwrap();
    assert_eq!(record.formats.len(), 2);
    for (index, (id, slot)) in FORMATS.iter().enumerate() {
        assert!(after[index + 1].starts_with(&before[index + 1]));
        assert_eq!(
            after[index + 1].len() as u64,
            before[index + 1].len() as u64 + slot
        );
        assert_eq!(
            u64::from(
                record
                    .formats
                    .iter()
                    .find(|format| format.format_id == *id)
                    .unwrap()
                    .image_size
            ),
            *slot
        );
    }
    let mut edit = installed.edit().unwrap();
    edit.remove_track(removed).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    let final_device = staged
        .install_and_open(&installed, InstallMode::Full, |_| {})
        .unwrap();
    assert_eq!(
        final_device.library().unwrap().track_count(),
        original_count
    );
    let records =
        crate::artwork::parse_artwork_records(&fs::read(directory.path().join(ART)).unwrap())
            .unwrap();
    assert!(!records.iter().any(|record| record.track_id == removed));
    assert!(records.iter().any(|record| record.track_id == new.id));
}

#[test]
fn an_existing_music_only_track_can_gain_nano2_artwork_in_one_edit() {
    let directory = fixture();
    let empty = Device::open(directory.path()).unwrap();
    let mut edit = empty.edit().unwrap();
    edit.add_track(addition(directory.path(), false)).unwrap();
    let first = tempdir().unwrap();
    let device = edit
        .stage_sqlite_preview(first.path())
        .unwrap()
        .install_and_open(&empty, InstallMode::Full, |_| {})
        .unwrap();
    let old = &device.library().unwrap().tracks()[0];
    assert!(!old.has_artwork);
    let mut edit = device.edit().unwrap();
    edit.remove_track(old.id).unwrap();
    edit.add_track(addition(directory.path(), true)).unwrap();
    let bundle = tempdir().unwrap();
    let installed = edit
        .stage_sqlite_preview(bundle.path())
        .unwrap()
        .install_and_open(&device, InstallMode::Full, |_| {})
        .unwrap();
    assert_eq!(installed.library().unwrap().track_count(), 1);
    let track = &installed.library().unwrap().tracks()[0];
    assert!(track.has_artwork);
    let records =
        crate::artwork::parse_artwork_records(&fs::read(directory.path().join(ART)).unwrap())
            .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].track_id, track.id);
    assert_eq!(records[0].formats.len(), 2);
}

#[test]
fn unknown_nano2_formats_and_incomplete_slots_still_fail_closed() {
    for unknown_format in [false, true] {
        let directory = fixture();
        let empty = Device::open(directory.path()).unwrap();
        add(&empty, "Original", [255, 0, 0, 255]);
        if unknown_format {
            let mut bytes = fs::read(directory.path().join(ART)).unwrap();
            let position = bytes.windows(4).position(|magic| magic == b"mhni").unwrap();
            put(&mut bytes, position + 16, 9999);
            fs::write(directory.path().join(ART), bytes).unwrap();
        } else {
            fs::write(frame(directory.path(), 1031), b"incomplete slot").unwrap();
        }
        let device = Device::open(directory.path()).unwrap();
        let before = artwork_bytes(directory.path());
        let database = fs::read(directory.path().join(DB)).unwrap();
        let mut edit = device.edit().unwrap();
        edit.add_track(addition(directory.path(), true)).unwrap();
        let bundle = tempdir().unwrap();
        assert!(edit.stage_sqlite_preview(bundle.path()).is_err());
        assert_eq!(artwork_bytes(directory.path()), before);
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), database);
        assert!(!directory
            .path()
            .join(super::commit::TRANSACTION_PATH)
            .exists());
    }
}

#[test]
fn unsupported_nano1_deletion_leaves_existing_artwork_untouched() {
    let directory = fixture();
    let empty = Device::open(directory.path()).unwrap();
    add(&empty, "Existing cover", [255, 0, 0, 255]);
    fs::write(
        directory.path().join("iPod_Control/Device/SysInfo"),
        "ModelFamily: iPod Nano\nGeneration: 1st Gen\n",
    )
    .unwrap();
    let device = Device::open(directory.path()).unwrap();
    assert_eq!(device.profile().unwrap().key(), "nano-1g");
    let old = artwork_bytes(directory.path());
    let mut edit = device.edit().unwrap();
    edit.remove_track(device.library().unwrap().tracks()[0].id)
        .unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    assert!(staged.removed_ithmb().is_empty());
    assert!(!bundle.path().join("ArtworkDB").exists());
    staged.install(&device).unwrap();
    assert_eq!(artwork_bytes(directory.path()), old);
}
