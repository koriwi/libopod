//! Disposable synthetic mounts only. Test each namespace/durability boundary,
//! not just interruption after a fully installed file.
use super::*;
use crate::edit::classic_tests::{addition, virtual_classic};
use crate::{recover_interrupted_transaction, InstallMode};
use rename::Step;
use tempfile::{tempdir, TempDir};

const DB: &str = "iPod_Control/iTunes/iTunesDB";

pub(super) fn fixture() -> (TempDir, TempDir, Device, StagedSqliteEdit) {
    let directory = virtual_classic("ModelNumStr: MC293", true);
    let device = Device::open(directory.path()).unwrap();
    let mut edit = device.edit().unwrap();
    edit.add_track(addition(directory.path(), true)).unwrap();
    let first = tempdir().unwrap();
    let seeded = edit
        .stage_sqlite_preview(first.path())
        .unwrap()
        .install_and_open(&device, InstallMode::Full, |_| {})
        .unwrap();
    // Read-only originals can be renamed but must not be opened for append.
    // Keep this suite exercising the replacement fallback on Unix too.
    #[cfg(unix)]
    for entry in fs::read_dir(directory.path().join("iPod_Control/Artwork")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().ends_with(".ithmb") {
            let mut permissions = entry.metadata().unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(entry.path(), permissions).unwrap();
        }
    }
    let mut edit = seeded.edit().unwrap();
    let mut track = addition(directory.path(), true);
    track.title = "Second batch".to_owned();
    edit.add_track(track).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    (directory, bundle, seeded, staged)
}

fn interrupt(
    device: &Device,
    staged: &StagedSqliteEdit,
    index: usize,
    step: Step,
    mode: InstallMode,
) {
    let error = install_staged_with_progress(
        device,
        staged,
        FailureMode::SimulateRenameInterruption(index, step),
        mode,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected transaction interruption"),
        "{error}"
    );
}

fn output_count() -> usize {
    let (_directory, _bundle, _device, staged) = fixture();
    read_staging_manifest(staged.manifest())
        .unwrap()
        .outputs
        .len()
}

#[test]
fn each_forward_rename_boundary_recovers_without_the_host_bundle() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        for step in [
            Step::TemporaryReady,
            Step::OriginalMoved,
            Step::BackupDurable,
            Step::ReplacementMoved,
            Step::ReplacementDurable,
        ] {
            // One new MP3 followed by every existing ithmb and both databases.
            for index in 1..output_count() {
                let (directory, bundle, device, staged) = fixture();
                let manifest = read_staging_manifest(staged.manifest()).unwrap();
                let output = &manifest.outputs[index];
                let target = directory.path().join(&output.target);
                let original = fs::read(&target).unwrap();
                #[cfg(unix)]
                let inode = {
                    use std::os::unix::fs::MetadataExt;
                    fs::metadata(&target).unwrap().ino()
                };
                interrupt(&device, &staged, index, step, mode);
                let transaction = directory.path().join(TRANSACTION_PATH);
                let journal = read_journal(&transaction).unwrap();
                assert_eq!(journal.version, 4);
                assert!(journal.appends.is_empty());
                assert_eq!(journal.installed, index + 1);
                let backup = transaction.join("backup").join(&output.staged);
                if step == Step::TemporaryReady {
                    assert!(!backup.exists());
                    assert_eq!(fs::read(&target).unwrap(), original);
                } else {
                    assert_eq!(fs::read(&backup).unwrap(), original);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        assert_eq!(
                            fs::metadata(&backup).unwrap().ino(),
                            inode,
                            "backup must be the original inode, not a copied file"
                        );
                    }
                }
                assert_eq!(
                    !target.exists(),
                    matches!(step, Step::OriginalMoved | Step::BackupDurable)
                );
                assert!(matches!(
                    Device::open(directory.path()),
                    Err(Error::RecoveryRequired { .. })
                ));
                drop(bundle);
                assert!(recover_interrupted_transaction(directory.path()).unwrap());
                assert_eq!(
                    Device::open(directory.path()).unwrap().generation(),
                    device.generation()
                );
                assert!(!transaction.exists());
                assert!(!directory
                    .path()
                    .join(staged.added_media()[0].as_str())
                    .exists());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
                }
                assert!(!recover_interrupted_transaction(directory.path()).unwrap());
            }
        }
    }
}

#[test]
fn rollback_renames_are_restartable_after_each_restored_original() {
    for step in [Step::RestoredMoved, Step::RestoredDurable] {
        for index in 1..output_count() {
            let (directory, bundle, device, staged) = fixture();
            install_staged_removal(
                &device,
                &staged,
                FailureMode::SimulateInterruptionDuringValidation,
            )
            .unwrap_err();
            drop(bundle);
            let error = recover_with_failure_mode(
                device.mount(),
                FailureMode::SimulateRenameInterruption(index, step),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected transaction interruption"),
                "{error}"
            );
            let transaction = directory.path().join(TRANSACTION_PATH);
            let journal = read_journal(&transaction).unwrap();
            assert_eq!(journal.phase, TransactionPhase::RollingBack);
            let output = &journal.staging.outputs[index];
            assert!(!transaction.join("backup").join(&output.staged).exists());
            if step == Step::RestoredMoved {
                // Retry must execute the missing durability barriers even
                // though the original is already live and its backup is gone.
                let error = recover_with_failure_mode(
                    device.mount(),
                    FailureMode::SimulateRenameInterruption(index, Step::RestoredDurable),
                    &mut |_| {},
                )
                .unwrap_err();
                assert!(error.to_string().contains("RestoredDurable"), "{error}");
            }
            assert!(recover_interrupted_transaction(directory.path()).unwrap());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                device.generation()
            );
        }
    }
}

#[test]
fn cleanup_is_restartable_after_backup_deletion_and_after_journal_deletion() {
    for committed in [false, true] {
        for step in [Step::CleanupEntryRemoved, Step::JournalRemoved] {
            let (directory, bundle, device, staged) = fixture();
            let transaction = directory.path().join(TRANSACTION_PATH);
            let expected = if committed {
                interrupt(&device, &staged, 0, step, InstallMode::Fast);
                Device::open_during_transaction(directory.path())
                    .unwrap()
                    .generation()
                    .clone()
            } else {
                install_staged_removal(
                    &device,
                    &staged,
                    FailureMode::SimulateInterruptionDuringValidation,
                )
                .unwrap_err();
                let error = recover_with_failure_mode(
                    device.mount(),
                    FailureMode::SimulateRenameInterruption(0, step),
                    &mut |_| {},
                )
                .unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("injected transaction interruption"),
                    "{error}"
                );
                device.generation().clone()
            };
            drop(bundle);
            if step == Step::CleanupEntryRemoved {
                let journal = read_journal(&transaction).unwrap();
                assert_eq!(
                    journal.phase,
                    if committed {
                        TransactionPhase::Committed
                    } else {
                        TransactionPhase::RolledBack
                    }
                );
                assert!(!transaction.join("backup").exists());
            } else {
                assert_eq!(fs::read_dir(&transaction).unwrap().count(), 0);
            }
            assert!(recover_interrupted_transaction(directory.path()).unwrap());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                &expected
            );
            assert!(!transaction.exists());
        }
    }
}

#[test]
fn terminal_cleanup_still_requires_the_live_generation_to_verify() {
    for committed in [false, true] {
        let (directory, _bundle, device, staged) = fixture();
        if committed {
            interrupt(
                &device,
                &staged,
                0,
                Step::CleanupEntryRemoved,
                InstallMode::Full,
            );
        } else {
            install_staged_removal(
                &device,
                &staged,
                FailureMode::SimulateInterruptionDuringValidation,
            )
            .unwrap_err();
            recover_with_failure_mode(
                device.mount(),
                FailureMode::SimulateRenameInterruption(0, Step::CleanupEntryRemoved),
                &mut |_| {},
            )
            .unwrap_err();
        }
        let transaction = directory.path().join(TRANSACTION_PATH);
        assert!(!transaction.join("backup").exists());
        fs::write(directory.path().join(DB), b"unexpected terminal bytes").unwrap();
        fs::write(transaction.join("sentinel"), b"keep").unwrap();
        assert!(recover_interrupted_transaction(directory.path()).is_err());
        assert!(transaction.join("sentinel").exists());
        assert!(transaction.join(JOURNAL_NAME).exists());
    }
}

#[test]
fn legacy_copy_backup_journals_still_recover_in_all_phases() {
    for phase in [
        TransactionPhase::BackingUp,
        TransactionPhase::Installing,
        TransactionPhase::Validating,
        TransactionPhase::Committed,
    ] {
        let (directory, bundle, device, staged) = fixture();
        let transaction = directory.path().join(TRANSACTION_PATH);
        let expected = if matches!(
            phase,
            TransactionPhase::BackingUp | TransactionPhase::Installing
        ) {
            install_staged_removal(
                &device,
                &staged,
                FailureMode::SimulateInterruptionDuringBackupAfter(5),
            )
            .unwrap_err();
            let manifest = read_staging_manifest(staged.manifest()).unwrap();
            for output in &manifest.outputs {
                if original_state(&manifest, output).unwrap().bytes.is_some() {
                    let backup = rename::prepare_backup_path(&transaction, output).unwrap();
                    fs::copy(directory.path().join(&output.target), backup).unwrap();
                }
            }
            device.generation().clone()
        } else {
            install_staged_removal(
                &device,
                &staged,
                FailureMode::SimulateInterruptionAfterCommitted,
            )
            .unwrap_err();
            if phase == TransactionPhase::Committed {
                // A legacy committed cleanup can already have removed backups.
                fs::remove_dir_all(transaction.join("backup")).unwrap();
                Device::open_during_transaction(directory.path())
                    .unwrap()
                    .generation()
                    .clone()
            } else {
                device.generation().clone()
            }
        };
        let mut journal = read_journal(&transaction).unwrap();
        journal.version = 2;
        journal.phase = phase;
        write_journal(&transaction, &journal).unwrap();
        drop(bundle);
        assert!(recover_interrupted_transaction(directory.path()).unwrap());
        assert_eq!(
            Device::open(directory.path()).unwrap().generation(),
            &expected
        );
    }
}

#[test]
fn unknown_live_or_backup_states_fail_before_any_cleanup() {
    for damage in [
        "missing backup",
        "bad backup",
        "bad live",
        "missing completed live",
        "count",
        "path",
        "alias",
    ] {
        let (directory, _bundle, device, staged) = fixture();
        install_staged_removal(
            &device,
            &staged,
            FailureMode::SimulateInterruptionDuringValidation,
        )
        .unwrap_err();
        let transaction = directory.path().join(TRANSACTION_PATH);
        let mut journal = read_journal(&transaction).unwrap();
        let target = directory.path().join(DB);
        match damage {
            "missing backup" => fs::remove_file(transaction.join("backup/iTunesDB")).unwrap(),
            "bad backup" => fs::write(transaction.join("backup/iTunesDB"), b"bad").unwrap(),
            "bad live" => fs::write(&target, b"bad").unwrap(),
            "missing completed live" => fs::remove_file(&target).unwrap(),
            "count" => {
                journal.installed = 0;
                write_journal(&transaction, &journal).unwrap();
            }
            "path" => {
                journal.staging.outputs[1].staged = "../escaped".to_owned();
                write_journal(&transaction, &journal).unwrap();
            }
            _ => {
                journal.staging.outputs[2].staged =
                    journal.staging.outputs[1].staged.to_uppercase();
                write_journal(&transaction, &journal).unwrap();
            }
        }
        let before = fs::read(&target).ok();
        let sentinel = transaction.join("do-not-delete");
        fs::write(&sentinel, b"validation must precede cleanup").unwrap();
        assert!(
            recover_interrupted_transaction(directory.path()).is_err(),
            "{damage}"
        );
        assert_eq!(fs::read(&target).ok(), before);
        assert!(sentinel.exists());
        assert!(directory
            .path()
            .join(staged.added_media()[0].as_str())
            .exists());
    }
}

#[test]
fn rename_gap_requires_a_verified_backup_and_persisted_install_intent() {
    for damage in ["missing", "corrupt", "unattempted", "validating"] {
        let (directory, _bundle, device, staged) = fixture();
        let index = read_staging_manifest(staged.manifest())
            .unwrap()
            .outputs
            .iter()
            .position(|output| output.target == DB)
            .unwrap();
        interrupt(
            &device,
            &staged,
            index,
            Step::OriginalMoved,
            InstallMode::Full,
        );
        let transaction = directory.path().join(TRANSACTION_PATH);
        let mut journal = read_journal(&transaction).unwrap();
        match damage {
            "missing" => fs::remove_file(transaction.join("backup/iTunesDB")).unwrap(),
            "corrupt" => fs::write(transaction.join("backup/iTunesDB"), b"bad").unwrap(),
            "unattempted" => {
                journal.installed = index;
                write_journal(&transaction, &journal).unwrap();
            }
            _ => {
                journal.phase = TransactionPhase::Validating;
                write_journal(&transaction, &journal).unwrap();
            }
        }
        assert!(
            recover_interrupted_transaction(directory.path()).is_err(),
            "{damage}"
        );
        assert!(!directory.path().join(DB).exists());
        assert!(transaction.exists());
    }
}

#[test]
fn only_empty_scaffolding_can_be_removed_without_a_journal() {
    for kind in [
        "empty",
        "temporary",
        "legacy empty",
        "payload",
        "backup bytes",
    ] {
        let directory = virtual_classic("ModelNumStr: MC293", false);
        let device = Device::open(directory.path()).unwrap();
        let transaction = directory.path().join(TRANSACTION_PATH);
        fs::create_dir(&transaction).unwrap();
        match kind {
            "temporary" => {
                fs::write(transaction.join("journal.tmp"), b"partial initial journal").unwrap();
            }
            "legacy empty" => fs::create_dir(transaction.join("backup")).unwrap(),
            "payload" => fs::write(transaction.join("unknown"), b"keep").unwrap(),
            "backup bytes" => {
                fs::create_dir(transaction.join("backup")).unwrap();
                fs::write(transaction.join("backup/iTunesDB"), b"keep").unwrap();
            }
            _ => {}
        }
        if matches!(kind, "payload" | "backup bytes") {
            assert!(recover_interrupted_transaction(directory.path()).is_err());
            assert!(transaction.exists());
        } else {
            assert!(recover_interrupted_transaction(directory.path()).unwrap());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                device.generation()
            );
        }
    }
}

#[test]
fn publish_failure_automatically_restores_the_missing_live_database() {
    let (directory, _bundle, device, staged) = fixture();
    let manifest = read_staging_manifest(staged.manifest()).unwrap();
    let index = manifest
        .outputs
        .iter()
        .position(|output| output.target == DB)
        .unwrap();
    let temporary = directory
        .path()
        .join(format!("iPod_Control/iTunes/.iTunesDB.libopod-{index}.tmp"));
    let mut at_database = false;
    let mut injected = false;
    let error = staged
        .install_and_open(&device, InstallMode::Fast, |event| {
            if let ProgressEvent::Item {
                operation: "Installing",
                name,
                ..
            } = event
            {
                at_database = name == DB;
            }
            if at_database && event == ProgressEvent::Phase("Publishing verified replacement") {
                assert!(!directory.path().join(DB).exists());
                assert!(directory
                    .path()
                    .join(TRANSACTION_PATH)
                    .join("backup/iTunesDB")
                    .exists());
                fs::remove_file(&temporary).unwrap();
                injected = true;
            }
        })
        .unwrap_err();
    assert!(injected);
    assert!(
        error.to_string().contains("publish verified replacement"),
        "{error}"
    );
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
    assert!(!directory
        .path()
        .join(staged.added_media()[0].as_str())
        .exists());
}

#[test]
fn consumed_backup_is_not_proof_that_rollback_succeeded() {
    let (directory, _bundle, device, staged) = fixture();
    let index = read_staging_manifest(staged.manifest())
        .unwrap()
        .outputs
        .iter()
        .position(|output| output.target == DB)
        .unwrap();
    install_staged_removal(
        &device,
        &staged,
        FailureMode::SimulateInterruptionDuringValidation,
    )
    .unwrap_err();
    recover_with_failure_mode(
        device.mount(),
        FailureMode::SimulateRenameInterruption(index, Step::RestoredMoved),
        &mut |_| {},
    )
    .unwrap_err();
    let transaction = directory.path().join(TRANSACTION_PATH);
    assert!(!transaction.join("backup/iTunesDB").exists());
    fs::write(directory.path().join(DB), b"unknown restored bytes").unwrap();
    assert!(recover_interrupted_transaction(directory.path()).is_err());
    assert!(transaction.exists());
    assert_eq!(
        fs::read(directory.path().join(DB)).unwrap(),
        b"unknown restored bytes"
    );
}

#[test]
#[cfg(unix)]
fn readonly_original_can_be_preserved_and_restored_without_changing_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let (directory, _bundle, device, staged) = fixture();
    let target = directory.path().join(DB);
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
    let index = read_staging_manifest(staged.manifest())
        .unwrap()
        .outputs
        .iter()
        .position(|output| output.target == DB)
        .unwrap();
    interrupt(
        &device,
        &staged,
        index,
        Step::BackupDurable,
        InstallMode::Full,
    );
    assert!(recover_interrupted_transaction(directory.path()).unwrap());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o444
    );
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
}

#[test]
fn free_space_budget_does_not_allocate_another_copy_of_original_files() {
    let (_directory, _bundle, _device, staged) = fixture();
    let mut manifest = read_staging_manifest(staged.manifest()).unwrap();
    let before = required_transaction_bytes(&manifest, &append::Plans::new()).unwrap();
    for source in &mut manifest.source {
        if source.bytes.is_some() {
            source.bytes = Some(1_000_000_000_000);
        }
    }
    let after = required_transaction_bytes(&manifest, &append::Plans::new()).unwrap();
    assert!(
        after.abs_diff(before) < 4096,
        "only serialized journal size should change"
    );
    manifest.outputs[0].bytes = u64::MAX;
    assert!(required_transaction_bytes(&manifest, &append::Plans::new()).is_err());
}
