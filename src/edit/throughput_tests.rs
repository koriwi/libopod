//! Less I/O must retain byte checks and the transaction/recovery boundaries.
use std::fs;

use tempfile::tempdir;

use super::{
    append_artwork_frames,
    classic_tests::{addition, virtual_classic},
    commit::TRANSACTION_PATH,
    ArtworkFrameOut,
};
use crate::{recover_interrupted_transaction, Device, InstallMode, MountRoot, ProgressEvent};

const DB: &str = "iPod_Control/iTunes/iTunesDB";
const FRAME: &str = "F1061_1.ithmb";
const FRAME_PATH: &str = "iPod_Control/Artwork/F1061_1.ithmb";

#[test]
fn returned_device_matches_fresh_open_and_supports_subsequent_artwork_batches() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        let directory = virtual_classic("ModelNumStr: MC293", true);
        let mut device = Device::open(directory.path()).unwrap();
        for batch in 1..=2 {
            let mut edit = device.edit().unwrap();
            let mut track = addition(directory.path(), true);
            track.title = format!("Batch {batch}");
            edit.add_track(track).unwrap();
            let bundle = tempdir().unwrap();
            let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
            device = staged.install_and_open(&device, mode, |_| {}).unwrap();
            let fresh = Device::open(directory.path()).unwrap();
            assert_eq!(device.generation(), fresh.generation());
            assert_eq!(device.mount().as_path(), fresh.mount().as_path());
            assert_eq!(device.library().unwrap().track_count(), batch);
            assert!(!directory.path().join(TRANSACTION_PATH).exists());
        }
    }
}

#[test]
fn damaged_host_backup_fails_before_install_and_cleans_up_safely() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        for missing in [false, true] {
            let directory = virtual_classic("ModelNumStr: MC293", true);
            let device = Device::open(directory.path()).unwrap();
            let original = fs::read(directory.path().join(DB)).unwrap();
            let mut edit = device.edit().unwrap();
            edit.add_track(addition(directory.path(), true)).unwrap();
            let bundle = tempdir().unwrap();
            let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
            let backup = bundle.path().join("original").join(DB);
            if missing {
                fs::remove_file(&backup).unwrap();
            } else {
                let mut corrupt = original.clone();
                corrupt[0] ^= 1;
                fs::write(&backup, corrupt).unwrap();
            }
            let mut installed = false;
            assert!(staged
                .install_and_open(&device, mode, |event| {
                    if let ProgressEvent::Item {
                        operation: "Installing" | "Copying and hashing media (fast)",
                        ..
                    } = event
                    {
                        installed = true;
                    }
                })
                .is_err());
            assert!(!installed);
            assert_eq!(fs::read(directory.path().join(DB)).unwrap(), original);
            assert!(!directory
                .path()
                .join(staged.added_media()[0].as_str())
                .exists());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                device.generation()
            );
            assert!(!directory.path().join(TRANSACTION_PATH).exists());
        }
    }
}

#[test]
fn changed_snapshot_and_manifest_cannot_replace_the_trusted_source_fingerprint() {
    use sha2::{Digest, Sha256};

    for mode in [InstallMode::Full, InstallMode::Fast] {
        for change in ["fingerprint", "duplicate", "missing", "presence"] {
            let directory = virtual_classic("ModelNumStr: MC293", false);
            let device = Device::open(directory.path()).unwrap();
            let mut edit = device.edit().unwrap();
            edit.add_track(addition(directory.path(), false)).unwrap();
            let bundle = tempdir().unwrap();
            let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(staged.manifest()).unwrap()).unwrap();
            let sources = manifest["source"].as_array_mut().unwrap();
            match change {
                "fingerprint" => {
                    let path = bundle.path().join("original").join(DB);
                    let mut bytes = fs::read(&path).unwrap();
                    bytes[0] ^= 1;
                    fs::write(path, &bytes).unwrap();
                    let source = sources
                        .iter_mut()
                        .find(|source| source["path"] == DB)
                        .unwrap();
                    source["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
                }
                "duplicate" => sources[1] = sources[0].clone(),
                "missing" => {
                    sources.pop();
                }
                _ => {
                    let source = sources
                        .iter_mut()
                        .find(|source| source["path"] == DB)
                        .unwrap();
                    source["present"] = false.into();
                }
            }
            fs::write(staged.manifest(), serde_json::to_vec(&manifest).unwrap()).unwrap();
            let error = staged.install_and_open(&device, mode, |_| {}).unwrap_err();
            assert!(error.to_string().contains("source fingerprints"), "{error}");
            assert!(!directory.path().join(TRANSACTION_PATH).exists());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                device.generation()
            );
        }
    }
}

#[test]
fn live_changes_during_backup_preparation_still_block_installation() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        let directory = virtual_classic("ModelNumStr: MC293", false);
        let device = Device::open(directory.path()).unwrap();
        let original = fs::read(directory.path().join(DB)).unwrap();
        let mut corrupt = original.clone();
        corrupt[0] ^= 1;
        let mut edit = device.edit().unwrap();
        edit.add_track(addition(directory.path(), false)).unwrap();
        let bundle = tempdir().unwrap();
        let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
        let mut changed = false;
        let mut installed = false;
        assert!(staged
            .install_and_open(&device, mode, |event| {
                match event {
                    ProgressEvent::Item {
                        operation: "Preparing device backup",
                        name: DB,
                        ..
                    } => {
                        fs::write(directory.path().join(DB), &corrupt).unwrap();
                        changed = true;
                    }
                    ProgressEvent::Item {
                        operation: "Installing" | "Copying and hashing media (fast)",
                        ..
                    } => {
                        installed = true;
                    }
                    _ => {}
                }
            })
            .is_err());
        assert!(changed && !installed);
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), corrupt);
        assert!(!directory
            .path()
            .join(staged.added_media()[0].as_str())
            .exists());
        // Strict recovery refuses unknown live bytes; do not overwrite them
        // just because a valid host snapshot is available.
        assert!(directory.path().join(TRANSACTION_PATH).exists());
        fs::write(directory.path().join(DB), &original).unwrap();
        assert!(recover_interrupted_transaction(directory.path()).unwrap());
        assert_eq!(
            Device::open(directory.path()).unwrap().generation(),
            device.generation()
        );
    }
}

#[test]
fn snapshot_verification_rejects_changed_missing_and_new_generation_inputs() {
    for change in ["changed", "missing", "appeared"] {
        let directory = virtual_classic("ModelNumStr: MC293", false);
        let device = Device::open(directory.path()).unwrap();
        let mut edit = device.edit().unwrap();
        edit.add_track(addition(directory.path(), false)).unwrap();
        match change {
            "changed" => {
                let path = directory.path().join(DB);
                let mut bytes = fs::read(&path).unwrap();
                bytes[0] ^= 1;
                fs::write(path, bytes).unwrap();
            }
            "missing" => fs::remove_file(directory.path().join(DB)).unwrap(),
            _ => fs::write(
                directory.path().join("iPod_Control/Device/SysInfoExtended"),
                b"new input",
            )
            .unwrap(),
        }
        let bundle = tempdir().unwrap();
        let error = edit.stage_sqlite_preview(bundle.path()).unwrap_err();
        assert!(!error.to_string().contains("empty edit"), "{error}");
        assert!(!bundle.path().join("libopod-preview-manifest.json").exists());
        assert!(!directory.path().join(TRANSACTION_PATH).exists());
    }
}

#[test]
fn staging_still_detects_changes_after_the_verified_host_snapshot() {
    for relative in [
        "iPod_Control/Device/SysInfo",
        "iPod_Control/Device/SysInfoExtended",
    ] {
        let directory = virtual_classic("ModelNumStr: MC293", false);
        let device = Device::open(directory.path()).unwrap();
        let bundle = tempdir().unwrap();
        let mut changed = false;
        let mut edit = device.edit().unwrap();
        edit.add_track(addition(directory.path(), false)).unwrap();
        let error = edit
            .stage_sqlite_preview_with_progress(bundle.path(), |event| {
                if event == ProgressEvent::Phase("Verifying source and preparing staging manifest")
                {
                    fs::write(directory.path().join(relative), b"changed after snapshot").unwrap();
                    changed = true;
                }
            })
            .unwrap_err();
        assert!(changed);
        assert!(error.to_string().contains("device generation"), "{error}");
        assert!(!bundle.path().join("libopod-preview-manifest.json").exists());
    }
}

#[test]
fn artwork_appends_preserve_host_prefix_and_chain_on_already_staged_bytes() {
    let original = tempdir().unwrap();
    fs::create_dir_all(original.path().join("iPod_Control/Artwork")).unwrap();
    let prefix = vec![0x55; 32_768];
    fs::write(original.path().join(FRAME_PATH), &prefix).unwrap();
    let original_mount = MountRoot::open(original.path()).unwrap();
    let bundle = tempdir().unwrap();
    let frames: Vec<_> = (0..100u8)
        .map(|index| ArtworkFrameOut {
            filename: FRAME.to_owned(),
            ithmb_offset: 32_768 + u32::from(index) * 256,
            frame: vec![index; 256],
        })
        .collect();
    let refs: Vec<_> = frames.iter().collect();
    append_artwork_frames(bundle.path(), &original_mount, FRAME, &refs).unwrap();
    let mut expected = prefix.clone();
    for frame in &frames {
        expected.extend_from_slice(&frame.frame);
    }
    assert_eq!(fs::read(bundle.path().join(FRAME_PATH)).unwrap(), expected);
    assert_eq!(fs::read(original.path().join(FRAME_PATH)).unwrap(), prefix);

    // A removal/reindex may already have staged a different prefix. It must
    // take precedence over the host original and must never be truncated.
    fs::write(bundle.path().join(FRAME_PATH), b"reindexed").unwrap();
    let frame = ArtworkFrameOut {
        filename: FRAME.to_owned(),
        ithmb_offset: 9,
        frame: b"new slot".to_vec(),
    };
    append_artwork_frames(bundle.path(), &original_mount, FRAME, &[&frame]).unwrap();
    assert_eq!(
        fs::read(bundle.path().join(FRAME_PATH)).unwrap(),
        b"reindexednew slot"
    );
    assert_eq!(fs::read(original.path().join(FRAME_PATH)).unwrap(), prefix);
}

#[test]
fn artwork_appends_create_new_files_and_reject_invalid_offsets() {
    let original = tempdir().unwrap();
    let original_mount = MountRoot::open(original.path()).unwrap();
    let bundle = tempdir().unwrap();
    let mut frame = ArtworkFrameOut {
        filename: FRAME.to_owned(),
        ithmb_offset: 0,
        frame: b"first slot".to_vec(),
    };
    append_artwork_frames(bundle.path(), &original_mount, FRAME, &[&frame]).unwrap();
    for bad_offset in [0, 9, 11] {
        frame.ithmb_offset = bad_offset;
        assert!(append_artwork_frames(bundle.path(), &original_mount, FRAME, &[&frame]).is_err());
        assert_eq!(
            fs::read(bundle.path().join(FRAME_PATH)).unwrap(),
            b"first slot"
        );
    }
}

#[test]
#[cfg(unix)]
fn host_snapshot_and_artwork_output_reject_symlink_escapes() {
    let directory = virtual_classic("ModelNumStr: MC293", false);
    let device = Device::open(directory.path()).unwrap();
    let bundle = tempdir().unwrap();
    let mut edit = device.edit().unwrap();
    edit.add_track(addition(directory.path(), false)).unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    let backup = bundle.path().join("original").join(DB);
    let outside = tempdir().unwrap();
    let original = fs::read(&backup).unwrap();
    fs::write(outside.path().join("database"), &original).unwrap();
    fs::remove_file(&backup).unwrap();
    std::os::unix::fs::symlink(outside.path().join("database"), &backup).unwrap();
    assert!(staged
        .install_and_open(&device, InstallMode::Full, |_| {})
        .is_err());
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
    assert_eq!(fs::read(outside.path().join("database")).unwrap(), original);

    let staging = tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), staging.path().join("iPod_Control")).unwrap();
    let frame = ArtworkFrameOut {
        filename: FRAME.to_owned(),
        ithmb_offset: 0,
        frame: vec![1],
    };
    assert!(append_artwork_frames(staging.path(), device.mount(), FRAME, &[&frame]).is_err());
    assert!(!outside.path().join("Artwork").exists());
}
