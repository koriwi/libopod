//! Recovery observations must not move destructive work ahead of validation.
use std::{collections::BTreeMap, fs};

use tempfile::{tempdir, TempDir};

use super::{
    classic_tests::{addition, virtual_classic},
    commit::{install_staged_removal, install_staged_with_progress, FailureMode, TRANSACTION_PATH},
};
use crate::{
    recover_interrupted_transaction_with_progress, Device, ProgressEvent, StagedSqliteEdit,
};

const DB: &str = "iPod_Control/iTunes/iTunesDB";

#[derive(Default)]
struct Events {
    phases: Vec<&'static str>,
    items: Vec<(&'static str, usize, usize, String)>,
}

impl Events {
    fn record(&mut self, event: ProgressEvent<'_>) {
        match event {
            ProgressEvent::Phase(phase) => self.phases.push(phase),
            ProgressEvent::Item {
                operation,
                current,
                total,
                name,
            } => {
                assert!(current > 0 && current <= total);
                self.items
                    .push((operation, current, total, name.to_owned()));
            }
        }
    }

    fn has_operation(&self, name: &str) -> bool {
        self.items.iter().any(|(operation, ..)| *operation == name)
    }
}

fn fixture() -> (TempDir, TempDir, Device, StagedSqliteEdit) {
    let directory = virtual_classic("ModelNumStr: MC293", true);
    let device = Device::open(directory.path()).unwrap();
    let mut edit = device.edit().unwrap();
    edit.add_track(addition(directory.path(), true)).unwrap();
    let bundle = tempdir().unwrap();
    let staged = edit.stage_sqlite_preview(bundle.path()).unwrap();
    (directory, bundle, device, staged)
}

#[test]
fn recovery_reports_verification_restoration_and_cleanup_as_they_happen() {
    let (directory, bundle, device, staged) = fixture();
    let root = directory.path();
    let original = fs::read(root.join(DB)).unwrap();
    let installed = fs::read(bundle.path().join("iTunesDB")).unwrap();
    install_staged_removal(
        &device,
        &staged,
        FailureMode::SimulateInterruptionDuringValidation,
    )
    .unwrap_err();
    let transaction = root.join(TRANSACTION_PATH);
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(transaction.join("journal.json")).unwrap()).unwrap();
    let outputs = journal["staging"]["outputs"].as_array().unwrap();
    let database_index = outputs
        .iter()
        .position(|output| output["target"] == DB)
        .unwrap();
    let temporary = root.join(format!(
        "iPod_Control/iTunes/.iTunesDB.libopod-{database_index}.tmp"
    ));
    fs::write(&temporary, b"interrupted sibling file").unwrap();
    let media = root.join(staged.added_media()[0].as_str());
    let mut events = Events::default();
    assert!(
        recover_interrupted_transaction_with_progress(root, |event| {
            events.record(event);
            match event {
                ProgressEvent::Item {
                    operation: "Verifying interrupted file and backup",
                    ..
                } => {
                    assert!(media.exists());
                    assert!(
                        temporary.exists(),
                        "cleanup started before validation completed"
                    );
                }
                ProgressEvent::Item {
                    operation: "Restoring recovery backup",
                    name: DB,
                    ..
                } => {
                    assert!(
                        !media.exists(),
                        "new payloads must be removed before restoring databases"
                    );
                    assert!(!temporary.exists());
                    assert_eq!(
                        fs::read(root.join(DB)).unwrap(),
                        installed,
                        "event must precede restoration"
                    );
                }
                ProgressEvent::Item {
                    operation: "Verifying restored file",
                    name: DB,
                    ..
                } => {
                    assert_eq!(fs::read(root.join(DB)).unwrap(), original);
                }
                ProgressEvent::Phase("Removing recovery journal and backups") => {
                    assert!(transaction.exists());
                    assert_eq!(fs::read(root.join(DB)).unwrap(), original);
                }
                _ => {}
            }
        })
        .unwrap()
    );
    assert!(!transaction.exists());
    assert_eq!(
        Device::open(root).unwrap().library().unwrap().track_count(),
        0
    );
    for operation in [
        "Verifying recovery input",
        "Verifying interrupted file and backup",
        "Checking recovery temporary files",
        "Removing new file during recovery",
        "Restoring recovery backup",
        "Verifying restored file",
        "Checking recovered file",
    ] {
        assert!(events.has_operation(operation), "missing {operation}");
    }
    let mut counters = BTreeMap::new();
    for (operation, current, total, _) in events.items {
        let previous = counters.entry(operation).or_insert((0, total));
        assert_eq!((current, total), (previous.0 + 1, previous.1));
        previous.0 = current;
    }
    assert!(counters.values().all(|(current, total)| current == total));
}

#[test]
fn backup_only_and_committed_recovery_do_not_claim_file_restoration() {
    for failure in [
        FailureMode::SimulateInterruptionDuringBackupAfter(0),
        FailureMode::SimulateInterruptionAfterCommitted,
    ] {
        let (directory, _bundle, device, staged) = fixture();
        install_staged_removal(&device, &staged, failure).unwrap_err();
        let before = fs::read(directory.path().join(DB)).unwrap();
        let mut events = Events::default();
        assert!(
            recover_interrupted_transaction_with_progress(directory.path(), |event| events
                .record(event))
            .unwrap()
        );
        assert!(!events.has_operation("Restoring recovery backup"));
        assert!(!events.has_operation("Removing new file during recovery"));
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), before);
        assert_eq!(
            events.phases.last(),
            Some(&"Removing recovery journal and backups")
        );
        let committed = failure == FailureMode::SimulateInterruptionAfterCommitted;
        assert_eq!(
            events
                .phases
                .contains(&"Transaction already committed; keeping installed files"),
            committed
        );
        assert_eq!(
            Device::open(directory.path())
                .unwrap()
                .library()
                .unwrap()
                .track_count(),
            usize::from(committed)
        );
    }
}

#[test]
fn failed_recovery_validation_never_reports_or_performs_cleanup() {
    for corrupt_journal in [false, true] {
        let (directory, _bundle, device, staged) = fixture();
        install_staged_removal(
            &device,
            &staged,
            FailureMode::SimulateInterruptionDuringValidation,
        )
        .unwrap_err();
        let transaction = directory.path().join(TRANSACTION_PATH);
        if corrupt_journal {
            fs::write(transaction.join("journal.json"), b"invalid journal").unwrap();
        } else {
            fs::write(transaction.join("backup/iTunesDB"), b"invalid backup").unwrap();
        }
        let before = fs::read(directory.path().join(DB)).unwrap();
        let mut events = Events::default();
        assert!(
            recover_interrupted_transaction_with_progress(directory.path(), |event| events
                .record(event))
            .is_err()
        );
        assert!(!events
            .phases
            .contains(&"Cleaning interrupted temporary files"));
        assert!(!events
            .phases
            .contains(&"Removing recovery journal and backups"));
        assert!(!events.has_operation("Restoring recovery backup"));
        assert!(!events.has_operation("Removing new file during recovery"));
        assert_eq!(fs::read(directory.path().join(DB)).unwrap(), before);
        assert!(directory
            .path()
            .join(staged.added_media()[0].as_str())
            .exists());
        assert!(transaction.exists());
    }
}

#[test]
fn automatic_install_rollback_forwards_recovery_events() {
    let (directory, bundle, device, staged) = fixture();
    let media = staged.added_media()[0].as_str();
    let original = fs::read(directory.path().join(DB)).unwrap();
    let mut events = Events::default();
    assert!(staged
        .install_with_progress(&device, |event| {
            events.record(event);
            if let ProgressEvent::Item {
                operation: "Installing",
                name,
                ..
            } = event
            {
                if name == media {
                    // Invalidate staged audio after preflight. Installation must
                    // fail, roll back, and report that recovery through this callback.
                    fs::write(bundle.path().join(media), b"changed audio").unwrap();
                }
            }
        })
        .is_err());
    assert!(events.phases.contains(&"Reading recovery journal"));
    assert!(events.has_operation("Verifying interrupted file and backup"));
    assert!(events
        .phases
        .contains(&"Removing recovery journal and backups"));
    assert_eq!(fs::read(directory.path().join(DB)).unwrap(), original);
    assert!(!directory.path().join(TRANSACTION_PATH).exists());
}

#[test]
fn later_batch_recovery_preserves_committed_tracks_without_revisiting_their_audio() {
    for mode in [crate::InstallMode::Full, crate::InstallMode::Fast] {
        let (directory, _first_bundle, device, first_batch) = fixture();
        let root = directory.path();
        first_batch
            .install_with_mode(&device, mode, |_| {})
            .unwrap();
        let committed_device = Device::open(root).unwrap();
        let committed_track = &committed_device.library().unwrap().tracks()[0];
        let first_id = committed_track.id;
        let first_media = committed_track.location.as_str().to_owned();
        let first_audio = fs::read(root.join(&first_media)).unwrap();
        let first_modified = fs::metadata(root.join(&first_media))
            .unwrap()
            .modified()
            .unwrap();
        let first_database = fs::read(root.join(DB)).unwrap();

        // The next batch starts from the committed generation, not the
        // original library from before the complete mirror run.
        let mut edit = committed_device.edit().unwrap();
        for title in ["Batch two, track one", "Batch two, track two"] {
            let mut track = addition(root, false);
            track.title = title.to_owned();
            edit.add_track(track).unwrap();
        }
        let second_bundle = tempdir().unwrap();
        let second_batch = edit.stage_sqlite_preview(second_bundle.path()).unwrap();
        install_staged_with_progress(
            &committed_device,
            &second_batch,
            FailureMode::SimulateInterruptionDuringValidation,
            mode,
            &mut |_| {},
        )
        .unwrap_err();
        let mut events = Events::default();
        assert!(
            recover_interrupted_transaction_with_progress(root, |event| events.record(event))
                .unwrap()
        );
        let removed: Vec<_> = events
            .items
            .iter()
            .filter(|(operation, ..)| *operation == "Removing new file during recovery")
            .collect();
        assert_eq!(
            removed.len(),
            2,
            "only the current batch's audio needs rollback"
        );
        assert!(events
            .items
            .iter()
            .all(|(_, _, _, name)| name != &first_media));
        assert_eq!(fs::read(root.join(DB)).unwrap(), first_database);
        assert_eq!(fs::read(root.join(&first_media)).unwrap(), first_audio);
        assert_eq!(
            fs::metadata(root.join(&first_media))
                .unwrap()
                .modified()
                .unwrap(),
            first_modified
        );
        let recovered = Device::open(root).unwrap();
        let tracks = recovered.library().unwrap().tracks();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, first_id);
        assert!(tracks[0].has_artwork);

        second_batch
            .install_with_mode(&recovered, mode, |_| {})
            .unwrap();
        let finished = Device::open(root).unwrap();
        assert_eq!(finished.library().unwrap().track_count(), 3);
        assert!(finished
            .library()
            .unwrap()
            .tracks()
            .iter()
            .any(|track| track.id == first_id));
    }
}

#[test]
fn recovery_without_a_pending_transaction_only_reports_the_initial_check() {
    let directory = virtual_classic("ModelNumStr: MC293", false);
    let mut events = Events::default();
    assert!(
        !recover_interrupted_transaction_with_progress(directory.path(), |event| events
            .record(event))
        .unwrap()
    );
    assert_eq!(events.phases, ["Opening device for recovery"]);
    assert!(events.items.is_empty());
}
