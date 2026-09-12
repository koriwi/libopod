//! Fast mode must reduce media reads, not weaken database transactions.
use std::{collections::BTreeSet, fs, path::Path};

use tempfile::{tempdir, TempDir};

use super::{
    classic_tests::{addition, virtual_classic},
    commit::{install_staged_with_progress, FailureMode},
};
use crate::{Device, InstallMode, ProgressEvent, StagedSqliteEdit};

const DB: &str = "iPod_Control/iTunes/iTunesDB";

fn fixture(artwork: bool) -> (TempDir, TempDir, Device, StagedSqliteEdit) {
    let directory = virtual_classic("ModelNumStr: MC293", artwork);
    let device = Device::open(directory.path()).unwrap();
    let mut edit = device.edit().unwrap();
    let track = addition(directory.path(), artwork);
    // More than one streaming buffer; these tests do not decode audio.
    fs::write(&track.source_path, vec![0x55; 150_000]).unwrap();
    edit.add_track(track).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    (directory, bundle, device, staged)
}

fn alter_same_size(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();
}

#[test]
fn fast_verifies_media_during_copy_and_keeps_database_and_artwork_readback() {
    assert_eq!(InstallMode::default(), InstallMode::Full);
    let (directory, bundle, device, staged) = fixture(true);
    let media = staged.added_media()[0].as_str();
    let mut operations = BTreeSet::new();
    let mut database_checked = false;
    let mut artwork_checked = false;
    staged
        .install_with_mode(&device, InstallMode::Fast, |event| {
            if let ProgressEvent::Item {
                operation, name, ..
            } = event
            {
                if name == media {
                    operations.insert(operation);
                }
                if operation == "Verifying installed file" {
                    database_checked |= name == DB;
                    artwork_checked |= name == "iPod_Control/Artwork/F1061_1.ithmb";
                }
            }
        })
        .unwrap();
    assert!(operations.contains("Checking staged media size (fast)"));
    assert!(operations.contains("Copying and hashing media (fast)"));
    assert!(operations.contains("Checking installed media size (fast)"));
    assert!(!operations.contains("Verifying staged file"));
    assert!(!operations.contains("Verifying installed file"));
    assert!(database_checked && artwork_checked);
    assert_eq!(
        fs::read(directory.path().join(media)).unwrap(),
        fs::read(bundle.path().join(media)).unwrap()
    );
    assert_eq!(
        Device::open(directory.path())
            .unwrap()
            .library()
            .unwrap()
            .track_count(),
        1
    );
}

#[test]
fn fast_rejects_staged_media_corruption_and_cleans_up_the_failed_copy() {
    for change in ["same size", "truncated", "grown"] {
        let (directory, bundle, device, staged) = fixture(false);
        let original = fs::read(directory.path().join(DB)).unwrap();
        let media = staged.added_media()[0].as_str();
        let source = bundle.path().join(media);
        let error = staged
            .install_with_mode(&device, InstallMode::Fast, |event| {
                // Tamper after the initial size check but before streaming. Both
                // a changed source and incorrect lengths must fail before rename.
                if let ProgressEvent::Item {
                    operation: "Copying and hashing media (fast)",
                    ..
                } = event
                {
                    match change {
                        "same size" => alter_same_size(&source),
                        "truncated" => fs::write(&source, b"short").unwrap(),
                        _ => fs::write(&source, vec![0x55; 150_001]).unwrap(),
                    }
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("streamed media"), "{error}");
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), original);
        let target = directory.path().join(media);
        assert!(!target.exists());
        assert_eq!(
            fs::read_dir(target.parent().unwrap()).unwrap().count(),
            0,
            "temporary copy leaked"
        );
        assert!(
            Device::open(directory.path()).is_ok(),
            "journal not recovered"
        );
    }
}

#[test]
fn full_mode_detects_same_size_destination_corruption_but_fast_only_checks_size() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        let (directory, bundle, device, staged) = fixture(false);
        let media = staged.added_media()[0].as_str();
        let result = staged.install_with_mode(&device, mode, |event| {
            if let ProgressEvent::Item {
                operation, name, ..
            } = event
            {
                if name == media
                    && matches!(
                        operation,
                        "Verifying installed file" | "Checking installed media size (fast)"
                    )
                {
                    alter_same_size(&directory.path().join(media));
                }
            }
        });
        // This is the explicitly documented speed/detection trade-off.
        assert_eq!(result.is_ok(), mode == InstallMode::Fast);
        if mode == InstallMode::Full {
            // Recovery deliberately refuses an externally modified live
            // output. Restore the known staged bytes before retrying it.
            assert!(Device::open(directory.path()).is_err());
            fs::copy(bundle.path().join(media), directory.path().join(media)).unwrap();
            crate::recover_interrupted_transaction(directory.path()).unwrap();
        }
        let device = Device::open(directory.path()).unwrap();
        assert_eq!(
            device.library().unwrap().track_count(),
            usize::from(mode == InstallMode::Fast)
        );
    }
}

#[test]
fn fast_still_rejects_destination_truncation_and_database_or_artwork_corruption() {
    for output in ["media", DB, "iPod_Control/Artwork/F1061_1.ithmb"] {
        let (directory, bundle, device, staged) = fixture(true);
        let original = fs::read(directory.path().join(DB)).unwrap();
        let media = staged.added_media()[0].as_str();
        let mut changed = false;
        let result = staged.install_with_mode(&device, InstallMode::Fast, |event| {
            if let ProgressEvent::Item {
                operation, name, ..
            } = event
            {
                if output == "media"
                    && name == media
                    && operation == "Checking installed media size (fast)"
                {
                    fs::write(directory.path().join(media), b"short").unwrap();
                    changed = true;
                } else if name == output && operation == "Verifying installed file" {
                    alter_same_size(&directory.path().join(name));
                    changed = true;
                }
            }
        });
        assert!(changed);
        assert!(result.is_err());
        assert!(
            Device::open(directory.path()).is_err(),
            "corruption must leave recovery required"
        );
        let target = if output == "media" { media } else { output };
        let source = if target == DB { "iTunesDB" } else { target };
        fs::copy(bundle.path().join(source), directory.path().join(target)).unwrap();
        crate::recover_interrupted_transaction(directory.path()).unwrap();
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), original);
        assert_eq!(
            Device::open(directory.path())
                .unwrap()
                .library()
                .unwrap()
                .track_count(),
            0
        );
    }
}

#[test]
fn fast_rejects_corrupt_staged_databases_before_copying_any_media() {
    let (directory, bundle, device, staged) = fixture(false);
    alter_same_size(&bundle.path().join("iTunesDB"));
    let mut copying = false;
    assert!(staged
        .install_with_mode(&device, InstallMode::Fast, |event| {
            if let ProgressEvent::Item {
                operation: "Copying and hashing media (fast)",
                ..
            } = event
            {
                copying = true;
            }
        })
        .is_err());
    assert!(!copying);
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
fn fast_interruption_uses_the_existing_strict_recovery_path() {
    let (directory, _bundle, device, staged) = fixture(false);
    let original = fs::read(directory.path().join(DB)).unwrap();
    assert!(install_staged_with_progress(
        &device,
        &staged,
        FailureMode::SimulateInterruptionAfter(1),
        InstallMode::Fast,
        &mut |_| {},
    )
    .is_err());
    assert!(directory
        .path()
        .join(staged.added_media()[0].as_str())
        .exists());
    assert!(Device::open(directory.path()).is_err());
    crate::recover_interrupted_transaction(directory.path()).unwrap();
    assert_eq!(fs::read(directory.path().join(DB)).unwrap(), original);
    assert!(!directory
        .path()
        .join(staged.added_media()[0].as_str())
        .exists());
    staged
        .install_with_mode(
            &Device::open(directory.path()).unwrap(),
            InstallMode::Fast,
            |_| {},
        )
        .unwrap();
    assert_eq!(
        Device::open(directory.path())
            .unwrap()
            .library()
            .unwrap()
            .track_count(),
        1
    );
}
