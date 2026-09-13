//! Synthetic mounts only: no USB writes or hardware power-loss claims.
#![cfg(unix)]
use super::*;
use crate::edit::classic_tests::{addition, virtual_classic};
use crate::recover_interrupted_transaction;
use append::Step;
use std::os::unix::fs::MetadataExt;
use tempfile::{tempdir, TempDir};

const DB: &str = "iPod_Control/iTunes/iTunesDB";
const LARGE: &str = "iPod_Control/Artwork/F1060_1.ithmb";
const SMALL: &str = "iPod_Control/Artwork/F1061_1.ithmb";

fn fixture() -> (TempDir, TempDir, Device, StagedSqliteEdit) {
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
    let mut edit = seeded.edit().unwrap();
    let mut track = addition(directory.path(), true);
    track.title = "Appended batch".to_owned();
    edit.add_track(track).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    (directory, bundle, seeded, staged)
}

fn indices(device: &Device, staged: &StagedSqliteEdit) -> Vec<usize> {
    let manifest = read_staging_manifest(staged.manifest()).unwrap();
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    assert_eq!(plans.len(), 3);
    assert!(!plans.contains_key(SMALL));
    manifest
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(index, output)| plans.contains_key(&output.target).then_some(index))
        .collect()
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
        FailureMode::SimulateAppendInterruption(index, step),
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

#[test]
fn every_append_boundary_recovers_without_host_files_in_both_modes() {
    for mode in [InstallMode::Full, InstallMode::Fast] {
        for step in [
            Step::SpoolPartial,
            Step::SpoolDurable,
            Step::IntentDurable,
            Step::AppendPartial,
            Step::AppendWritten,
            Step::AppendDurable,
        ] {
            for position in 0..3 {
                let (directory, bundle, device, staged) = fixture();
                let index = indices(&device, &staged)[position];
                let manifest = read_staging_manifest(staged.manifest()).unwrap();
                let output = &manifest.outputs[index];
                let target = directory.path().join(&output.target);
                let before = fs::read(&target).unwrap();
                let inode = fs::metadata(&target).unwrap().ino();
                interrupt(&device, &staged, index, step, mode);
                let transaction = directory.path().join(TRANSACTION_PATH);
                let journal = read_journal(&transaction).unwrap();
                assert_eq!(journal.version, 4);
                assert_eq!(
                    journal.installed,
                    if matches!(step, Step::SpoolPartial | Step::SpoolDurable) {
                        index
                    } else {
                        index + 1
                    }
                );
                assert!(!transaction.join("backup").join(&output.staged).exists());
                assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
                assert!(fs::read(&target).unwrap().starts_with(&before));
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
                assert_eq!(fs::metadata(&target).unwrap().ino(), inode);
                assert!(!directory
                    .path()
                    .join(staged.added_media()[0].as_str())
                    .exists());
                assert!(!transaction.exists());
            }
        }
    }
}

#[test]
fn truncation_and_its_durability_barriers_are_restartable() {
    for position in 0..3 {
        let (directory, bundle, device, staged) = fixture();
        let index = indices(&device, &staged)[position];
        install_staged_removal(
            &device,
            &staged,
            FailureMode::SimulateInterruptionDuringValidation,
        )
        .unwrap_err();
        drop(bundle);
        let mut intent_inode = None;
        for step in [Step::Truncated, Step::TruncateDurable] {
            let error = recover_with_failure_mode(
                device.mount(),
                FailureMode::SimulateAppendInterruption(index, step),
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
            assert_eq!(
                read_journal(&transaction).unwrap().phase,
                TransactionPhase::RollingBack
            );
            let inode = fs::metadata(transaction.join(JOURNAL_NAME)).unwrap().ino();
            if let Some(previous) = intent_inode {
                assert_eq!(
                    inode, previous,
                    "retry must flush existing intent without another allocation"
                );
            }
            intent_inode = Some(inode);
        }
        assert!(recover_interrupted_transaction(directory.path()).unwrap());
        assert_eq!(
            Device::open(directory.path()).unwrap().generation(),
            device.generation()
        );
    }
}

#[test]
fn committed_append_keeps_the_inode_and_spools_only_new_bytes() {
    let (directory, bundle, device, staged) = fixture();
    let manifest = read_staging_manifest(staged.manifest()).unwrap();
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    let old_inodes: Vec<_> = manifest
        .outputs
        .iter()
        .map(|output| {
            fs::metadata(directory.path().join(&output.target))
                .ok()
                .map(|metadata| metadata.ino())
        })
        .collect();
    let mut events = Vec::new();
    install_staged_with_progress(
        &device,
        &staged,
        FailureMode::SimulateInterruptionAfterCommitted,
        InstallMode::Fast,
        &mut |event| {
            if let ProgressEvent::Item {
                operation, name, ..
            } = event
            {
                events.push((operation.to_owned(), name.to_owned()));
            }
        },
    )
    .unwrap_err();
    let transaction = directory.path().join(TRANSACTION_PATH);
    for (index, output) in manifest.outputs.iter().enumerate() {
        let target = directory.path().join(&output.target);
        if let Some(plan) = plans.get(&output.target) {
            assert_eq!(
                fs::metadata(&target).unwrap().ino(),
                old_inodes[index].unwrap()
            );
            assert_eq!(
                fs::metadata(transaction.join(format!("append-{index}.bin")))
                    .unwrap()
                    .len(),
                output.bytes - plan.original_bytes
            );
            assert!(!transaction.join("backup").join(&output.staged).exists());
            assert!(events.contains(&("Appending artwork".to_owned(), output.target.clone())));
        } else if output.target == SMALL {
            assert_ne!(
                fs::metadata(&target).unwrap().ino(),
                old_inodes[index].unwrap()
            );
            assert!(transaction.join("backup").join(&output.staged).exists());
        }
        verify_file(&target, output.bytes, &output.sha256, "test output").unwrap();
    }
    drop(bundle);
    assert!(recover_interrupted_transaction(directory.path()).unwrap());
    assert_eq!(
        Device::open(directory.path())
            .unwrap()
            .library()
            .unwrap()
            .track_count(),
        2
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Deliberately exercise the same rollback guard for each corruption.
fn byte_partial_suffix_is_accepted_but_unknown_states_never_trigger_cleanup() {
    for damage in [
        "valid partial",
        "old prefix",
        "suffix",
        "short",
        "long",
        "missing spool",
        "bad spool",
        "symlink spool",
        "missing target",
        "intent",
        "offset",
        "strategy",
        "extra backup",
    ] {
        let (directory, bundle, device, staged) = fixture();
        let manifest = read_staging_manifest(staged.manifest()).unwrap();
        let index = manifest
            .outputs
            .iter()
            .position(|output| output.target == LARGE)
            .unwrap();
        interrupt(
            &device,
            &staged,
            index,
            Step::AppendPartial,
            InstallMode::Fast,
        );
        drop(bundle);
        let transaction = directory.path().join(TRANSACTION_PATH);
        let target = directory.path().join(LARGE);
        let mut journal = read_journal(&transaction).unwrap();
        let old = journal.appends[LARGE].original_bytes;
        let spool = transaction.join(format!("append-{index}.bin"));
        match damage {
            "valid partial" => OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap()
                .set_len(old + 123)
                .unwrap(),
            "old prefix" | "suffix" => {
                let mut bytes = fs::read(&target).unwrap();
                let offset = if damage == "old prefix" {
                    0
                } else {
                    usize::try_from(old).unwrap() + 5
                };
                bytes[offset] ^= 1;
                fs::write(&target, bytes).unwrap();
            }
            "short" => OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap()
                .set_len(old - 1)
                .unwrap(),
            "long" => OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap()
                .set_len(manifest.outputs[index].bytes + 1)
                .unwrap(),
            "missing spool" => fs::remove_file(&spool).unwrap(),
            "bad spool" => fs::write(&spool, b"bad").unwrap(),
            "symlink spool" => {
                let data = fs::read(&spool).unwrap();
                fs::remove_file(&spool).unwrap();
                let other = transaction.join("other-spool");
                fs::write(&other, data).unwrap();
                std::os::unix::fs::symlink(&other, &spool).unwrap();
            }
            "missing target" => fs::remove_file(&target).unwrap(),
            "intent" => {
                journal.installed = index;
                write_journal(&transaction, &journal).unwrap();
            }
            "offset" => {
                journal.appends.get_mut(LARGE).unwrap().original_bytes += 4096;
                write_journal(&transaction, &journal).unwrap();
            }
            "strategy" => {
                let plan = journal.appends.remove(LARGE).unwrap();
                journal.appends.insert(DB.to_owned(), plan);
                write_journal(&transaction, &journal).unwrap();
            }
            "extra backup" => {
                let backup =
                    rename::prepare_backup_path(&transaction, &manifest.outputs[index]).unwrap();
                fs::write(backup, b"unexpected").unwrap();
            }
            _ => unreachable!(),
        }
        let sentinel = transaction.join("sentinel");
        fs::write(&sentinel, b"keep").unwrap();
        let db_before = fs::read(directory.path().join(DB)).unwrap();
        let live_before = fs::read(&target).ok();
        if damage == "valid partial" {
            assert!(recover_interrupted_transaction(directory.path()).unwrap());
            assert_eq!(
                Device::open(directory.path()).unwrap().generation(),
                device.generation()
            );
        } else {
            assert!(
                recover_interrupted_transaction(directory.path()).is_err(),
                "{damage}"
            );
            assert!(sentinel.exists(), "{damage}");
            assert!(
                directory
                    .path()
                    .join(staged.added_media()[0].as_str())
                    .exists(),
                "{damage}"
            );
            assert_eq!(
                fs::read(directory.path().join(DB)).unwrap(),
                db_before,
                "{damage}"
            );
            assert_eq!(fs::read(&target).ok(), live_before, "{damage}");
        }
    }
}

#[test]
fn terminal_cleanup_does_not_require_already_deleted_suffix_spools() {
    for committed in [false, true] {
        let (directory, bundle, device, staged) = fixture();
        if committed {
            install_staged_with_progress(
                &device,
                &staged,
                FailureMode::SimulateRenameInterruption(0, rename::Step::CleanupEntryRemoved),
                InstallMode::Full,
                &mut |_| {},
            )
            .unwrap_err();
        } else {
            install_staged_removal(
                &device,
                &staged,
                FailureMode::SimulateInterruptionDuringValidation,
            )
            .unwrap_err();
            recover_with_failure_mode(
                device.mount(),
                FailureMode::SimulateRenameInterruption(0, rename::Step::CleanupEntryRemoved),
                &mut |_| {},
            )
            .unwrap_err();
        }
        let transaction = directory.path().join(TRANSACTION_PATH);
        let journal = read_journal(&transaction).unwrap();
        assert_eq!(
            journal.phase,
            if committed {
                TransactionPhase::Committed
            } else {
                TransactionPhase::RolledBack
            }
        );
        let first = indices(&device, &staged)[0];
        assert!(!transaction.join(format!("append-{first}.bin")).exists());
        drop(bundle);
        // Missing cleanup artifacts are allowed; unknown live bytes are not.
        let target = directory.path().join(LARGE);
        let known = fs::read(&target).unwrap();
        let mut corrupt = known.clone();
        corrupt[0] ^= 1;
        fs::write(&target, corrupt).unwrap();
        assert!(recover_interrupted_transaction(directory.path()).is_err());
        assert!(transaction.join(JOURNAL_NAME).exists());
        fs::write(&target, known).unwrap();
        File::open(&target).unwrap().sync_all().unwrap();
        assert!(recover_interrupted_transaction(directory.path()).unwrap());
        assert_eq!(
            Device::open(directory.path())
                .unwrap()
                .library()
                .unwrap()
                .track_count(),
            if committed { 2 } else { 1 }
        );
    }
}

#[test]
fn later_publish_failure_automatically_undoes_earlier_appends() {
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
    let mut truncated = 0;
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
                fs::remove_file(&temporary).unwrap();
                injected = true;
            }
            if let ProgressEvent::Item {
                operation: "Truncating appended thumbnail",
                ..
            } = event
            {
                truncated += 1;
            }
        })
        .unwrap_err();
    assert!(injected);
    assert_eq!(truncated, 3);
    assert!(
        error.to_string().contains("publish verified replacement"),
        "{error}"
    );
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
}

#[test]
fn growth_with_changed_prefix_and_unsupported_geometry_falls_back() {
    let (directory, _bundle, device, staged) = fixture();
    let manifest = read_staging_manifest(staged.manifest()).unwrap();
    let index = manifest
        .outputs
        .iter()
        .position(|output| output.target == LARGE)
        .unwrap();
    let staged_path = staged.directory().join(&manifest.outputs[index].staged);
    let mut bytes = fs::read(&staged_path).unwrap();
    bytes[0] ^= 1;
    fs::write(staged_path, bytes).unwrap();
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    assert_eq!(plans.len(), 2);
    assert!(!plans.contains_key(LARGE));
    assert!(!plans.contains_key(SMALL));
    let target = directory.path().join("iPod_Control/Artwork/F1055_1.ithmb");
    fs::hard_link(&target, directory.path().join("alias")).unwrap();
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    assert_eq!(plans.len(), 1);
}

#[test]
fn space_budget_counts_two_suffixes_not_a_complete_artwork_replacement() {
    let (_directory, _bundle, device, staged) = fixture();
    let mut manifest = read_staging_manifest(staged.manifest()).unwrap();
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    let old = plans[LARGE].original_bytes;
    let mut tiny = plans.clone();
    tiny.get_mut(LARGE).unwrap().original_bytes = 4096;
    let with_long_suffix = required_transaction_bytes(&manifest, &tiny).unwrap();
    let with_short_suffix = required_transaction_bytes(&manifest, &plans).unwrap();
    assert!(with_long_suffix > with_short_suffix + old);
    manifest
        .outputs
        .iter_mut()
        .find(|output| output.target == LARGE)
        .unwrap()
        .bytes = u64::MAX;
    assert!(required_transaction_bytes(&manifest, &plans).is_err());
}

#[test]
fn recovery_releases_real_reserved_space_and_unattempted_spools_before_journaling() {
    let (directory, bundle, device, staged) = fixture();
    install_staged_removal(
        &device,
        &staged,
        FailureMode::SimulateInterruptionDuringBackupAfter(0),
    )
    .unwrap_err();
    let transaction = directory.path().join(TRANSACTION_PATH);
    let journal = read_journal(&transaction).unwrap();
    let reserve = transaction.join("rollback-reserve");
    let metadata = fs::metadata(&reserve).unwrap();
    assert_eq!(
        metadata.len(),
        journal_capacity(&journal.staging, &journal.appends).unwrap() * 2
    );
    assert!(
        metadata.blocks() * 512 >= metadata.len(),
        "reserve must not be sparse"
    );
    let index = indices(&device, &staged)[0];
    let spool = transaction.join(format!("append-{index}.bin"));
    fs::write(&spool, b"partial unattempted spool").unwrap();
    drop(bundle);
    let mut checked = false;
    recover_with_failure_mode(device.mount(), FailureMode::RollBack, &mut |event| {
        if event == ProgressEvent::Phase("Flushing recovery temporary-file cleanup") {
            assert!(!reserve.exists());
            assert!(!spool.exists());
            assert_eq!(
                read_journal(&transaction).unwrap().phase,
                TransactionPhase::BackingUp
            );
            checked = true;
        }
    })
    .unwrap();
    assert!(checked);
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
}

#[test]
fn large_addition_falls_back_when_two_suffix_writes_would_cost_more() {
    let (_directory, _bundle, device, staged) = fixture();
    let mut manifest = read_staging_manifest(staged.manifest()).unwrap();
    let output = manifest
        .outputs
        .iter_mut()
        .find(|output| output.target == LARGE)
        .unwrap();
    let path = staged.directory().join(&output.staged);
    let mut data = fs::read(&path).unwrap();
    data.extend_from_within(..204_800);
    fs::write(&path, data).unwrap();
    let (bytes, digest) = fingerprint_host_file(&path).unwrap();
    output.bytes = bytes;
    output.sha256 = hex(&digest);
    let plans = append::plan(device.mount(), staged.directory(), &manifest).unwrap();
    assert_eq!(plans.len(), 2);
    assert!(!plans.contains_key(LARGE));
}

#[test]
fn version_three_rename_journals_remain_recoverable() {
    let (directory, bundle, device, staged) = super::rename_tests::fixture();
    install_staged_removal(
        &device,
        &staged,
        FailureMode::SimulateInterruptionDuringValidation,
    )
    .unwrap_err();
    let transaction = directory.path().join(TRANSACTION_PATH);
    let mut journal = read_journal(&transaction).unwrap();
    assert!(journal.appends.is_empty());
    journal.version = 3;
    write_journal(&transaction, &journal).unwrap();
    drop(bundle);
    assert!(recover_interrupted_transaction(directory.path()).unwrap());
    assert_eq!(
        Device::open(directory.path()).unwrap().generation(),
        device.generation()
    );
}
