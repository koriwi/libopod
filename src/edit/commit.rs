use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    generation::fingerprint_host_file,
    manifest::{read_staging_manifest, ManifestOutputFile, StagingManifest},
    StagedSqliteEdit,
};
use crate::{error::io_error, Device, Error, IpodPath, MountRoot, Result};

pub(crate) const TRANSACTION_PATH: &str = "iPod_Control/iTunes/.libopod-transaction-v1";
pub(crate) const NOOP_CONFIRMATION: &str = "I HAVE A VERIFIED BACKUP; RUN NANO 7G NO-OP WRITE TEST";
pub(crate) const REMOVAL_CONFIRMATION: &str =
    "I HAVE A VERIFIED BACKUP; REMOVE ONE NO-ARTWORK TRACK AND KEEP ITS MEDIA FILE";
// The addition gate is exercised by virtual tests today; the public hardware
// example will reference it once the removal gate passes on hardware.
#[allow(dead_code)]
pub(crate) const ADDITION_CONFIRMATION: &str =
    "I HAVE A VERIFIED BACKUP; ADD ONE NO-ARTWORK MP3 TRACK";
const JOURNAL_NAME: &str = "journal.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureMode {
    RollBack,
    #[cfg(test)]
    SimulateInterruptionDuringBackupAfter(usize),
    #[cfg(test)]
    SimulateInterruptionAfter(usize),
    #[cfg(test)]
    SimulateInterruptionDuringValidation,
    #[cfg(test)]
    SimulateInterruptionAfterCommitted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TransactionJournal {
    format: String,
    version: u32,
    phase: TransactionPhase,
    installed: usize,
    staging: StagingManifest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TransactionPhase {
    BackingUp,
    Installing,
    Validating,
    Committed,
}

pub(crate) fn install_noop_hardware_test(
    device: &Device,
    staged: &StagedSqliteEdit,
    confirmation: &str,
) -> Result<()> {
    if confirmation != NOOP_CONFIRMATION {
        return Err(Error::Unsupported {
            feature: "Nano 7G no-op hardware test confirmation",
            reason: format!("confirmation must exactly equal: {NOOP_CONFIRMATION}"),
        });
    }
    if device.profile().map(crate::DeviceProfile::key) != Some("nano-7g")
        || staged.removed_tracks() != 0
    {
        return Err(Error::Unsupported {
            feature: "Nano 7G no-op hardware test",
            reason: "only a zero-change bundle for a resolved Nano 7G is accepted".to_owned(),
        });
    }
    install_staged_removal(device, staged, FailureMode::RollBack)
}

pub(crate) fn install_single_removal_hardware_test(
    device: &Device,
    staged: &StagedSqliteEdit,
    confirmation: &str,
) -> Result<()> {
    if confirmation != REMOVAL_CONFIRMATION {
        return Err(Error::Unsupported {
            feature: "Nano 7G removal hardware test confirmation",
            reason: format!("confirmation must exactly equal: {REMOVAL_CONFIRMATION}"),
        });
    }
    if device.profile().map(crate::DeviceProfile::key) != Some("nano-7g")
        || staged.removed_tracks() != 1
        || staged.removed_artwork_tracks() != 0
    {
        return Err(Error::Unsupported {
            feature: "Nano 7G single-track removal hardware test",
            reason: "exactly one no-artwork track on a resolved Nano 7G is required".to_owned(),
        });
    }
    install_staged_removal(device, staged, FailureMode::RollBack)
}

#[allow(dead_code)]
pub(crate) fn install_single_addition_hardware_test(
    device: &Device,
    staged: &StagedSqliteEdit,
    confirmation: &str,
) -> Result<()> {
    if confirmation != ADDITION_CONFIRMATION {
        return Err(Error::Unsupported {
            feature: "Nano 7G addition hardware test confirmation",
            reason: format!("confirmation must exactly equal: {ADDITION_CONFIRMATION}"),
        });
    }
    if device.profile().map(crate::DeviceProfile::key) != Some("nano-7g")
        || staged.removed_tracks() != 0
        || staged.added_tracks() != 1
    {
        return Err(Error::Unsupported {
            feature: "Nano 7G single-track addition hardware test",
            reason: "exactly one added track on a resolved Nano 7G is required".to_owned(),
        });
    }
    install_staged_removal(device, staged, FailureMode::RollBack)
}

pub(crate) fn install_staged_removal(
    device: &Device,
    staged: &StagedSqliteEdit,
    failure_mode: FailureMode,
) -> Result<()> {
    if staged.removed_artwork_tracks() != 0 {
        return Err(Error::Unsupported {
            feature: "installation of artwork-bearing removals",
            reason: "ArtworkDB transaction support is not implemented".to_owned(),
        });
    }
    staged
        .source_generation
        .verify_unchanged(device.mount(), device.profile())?;
    let manifest = read_staging_manifest(staged.manifest())?;
    verify_bundle(device, staged, &manifest)?;
    require_transaction_space(device.mount(), &manifest)?;
    let transaction = transaction_host_path(device.mount())?;
    fs::create_dir(&transaction).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            Error::RecoveryRequired {
                path: transaction.clone(),
            }
        } else {
            io_error("create device transaction directory", &transaction, source)
        }
    })?;
    sync_directory(transaction.parent().unwrap_or(device.mount().as_path()))?;

    let result = install_inner(device, staged, manifest, &transaction, failure_mode);
    if result.is_err() && failure_mode == FailureMode::RollBack {
        let rollback_result = recover_transaction(device.mount());
        if let Err(rollback_error) = rollback_result {
            return Err(Error::Verification {
                format: "device transaction",
                reason: format!("installation failed and rollback also failed: {rollback_error}"),
            });
        }
    }
    result
}

#[allow(clippy::too_many_lines)]
fn install_inner(
    device: &Device,
    staged: &StagedSqliteEdit,
    manifest: StagingManifest,
    transaction: &Path,
    failure_mode: FailureMode,
) -> Result<()> {
    #[cfg(not(test))]
    let _ = failure_mode;
    let backup = transaction.join("backup");
    fs::create_dir(&backup)
        .map_err(|source| io_error("create device transaction backup", &backup, source))?;
    let mut journal = TransactionJournal {
        format: "libopod-device-transaction".to_owned(),
        version: 1,
        phase: TransactionPhase::BackingUp,
        installed: 0,
        staging: manifest,
    };
    write_journal(transaction, &journal)?;

    for (outputs_backed_up, output) in journal.staging.outputs.iter().enumerate() {
        #[cfg(not(test))]
        let _ = outputs_backed_up;
        let original = original_state(&journal.staging, output)?;
        match (original.bytes, original.sha256.as_deref()) {
            (Some(bytes), Some(digest)) => {
                let target = resolve_target(device.mount(), output)?;
                verify_file(&target, bytes, digest, "live transaction input")?;
                let backup_file = backup.join(&output.staged);
                copy_new_verified(&target, &backup_file, bytes, digest)?;
            }
            (None, None) => {
                let relative = IpodPath::new(output.target.clone())?;
                let target = device.mount().resolve_possible(&relative)?;
                if target.exists() {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: format!(
                            "{} unexpectedly appeared before installation",
                            output.target
                        ),
                    });
                }
            }
            _ => {
                return Err(Error::Verification {
                    format: "device transaction",
                    reason: "source manifest has an incomplete fingerprint".to_owned(),
                });
            }
        }
        #[cfg(test)]
        if failure_mode == FailureMode::SimulateInterruptionDuringBackupAfter(outputs_backed_up) {
            return Err(Error::Verification {
                format: "injected transaction interruption",
                reason: format!("stopped after {outputs_backed_up} backed-up files"),
            });
        }
    }
    sync_directory(&backup)?;
    staged
        .source_generation
        .verify_unchanged(device.mount(), device.profile())?;

    journal.phase = TransactionPhase::Installing;
    write_journal(transaction, &journal)?;
    for (index, output) in journal.staging.outputs.iter().enumerate() {
        #[cfg(test)]
        if failure_mode == FailureMode::SimulateInterruptionAfter(index) {
            return Err(Error::Verification {
                format: "injected transaction interruption",
                reason: format!("stopped before installing file {index}"),
            });
        }
        journal.installed = index + 1;
        write_journal(transaction, &journal)?;
        let staged_file = staged.directory().join(&output.staged);
        verify_file(
            &staged_file,
            output.bytes,
            &output.sha256,
            "staged transaction output",
        )?;
        let original = original_state(&journal.staging, output)?;
        let target = if original.bytes.is_some() {
            resolve_target(device.mount(), output)?
        } else {
            let relative = IpodPath::new(output.target.clone())?;
            device.mount().resolve_possible(&relative)?
        };
        install_file(&staged_file, &target, index)?;
    }

    journal.phase = TransactionPhase::Validating;
    write_journal(transaction, &journal)?;
    for output in &journal.staging.outputs {
        let original = original_state(&journal.staging, output)?;
        if original.bytes.is_some() {
            let target = resolve_target(device.mount(), output)?;
            verify_file(&target, output.bytes, &output.sha256, "installed output")?;
        } else {
            let relative = IpodPath::new(output.target.clone())?;
            let target = device.mount().resolve_possible(&relative)?;
            verify_file(&target, output.bytes, &output.sha256, "installed output")?;
        }
        #[cfg(test)]
        if failure_mode == FailureMode::SimulateInterruptionDuringValidation {
            return Err(Error::Verification {
                format: "injected transaction interruption",
                reason: "stopped during output validation".to_owned(),
            });
        }
    }
    let reopened = Device::open_during_transaction(device.mount().as_path())?;
    if reopened.library().map_or(0, crate::Library::track_count) != staged.remaining_tracks() {
        return Err(Error::Verification {
            format: "device transaction",
            reason: "installed library track count failed read-back validation".to_owned(),
        });
    }

    journal.phase = TransactionPhase::Committed;
    write_journal(transaction, &journal)?;
    #[cfg(test)]
    if failure_mode == FailureMode::SimulateInterruptionAfterCommitted {
        return Err(Error::Verification {
            format: "injected transaction interruption",
            reason: "stopped after the committed journal write".to_owned(),
        });
    }
    remove_transaction_directory(transaction)?;
    Ok(())
}

pub(crate) fn recover_transaction(mount: &MountRoot) -> Result<()> {
    let transaction = transaction_host_path(mount)?;
    if !transaction.exists() {
        return Ok(());
    }
    let journal = read_journal(&transaction)?;
    validate_recovery_state(mount, &transaction, &journal)?;
    if journal.phase != TransactionPhase::Committed {
        for (index, output) in journal.staging.outputs.iter().enumerate().rev() {
            if index >= journal.installed {
                continue;
            }
            let source = original_state(&journal.staging, output)?;
            match (source.bytes, source.sha256.as_deref()) {
                (Some(bytes), Some(digest)) => {
                    let target = resolve_target(mount, output)?;
                    let backup = transaction.join("backup").join(&output.staged);
                    install_file(&backup, &target, index)?;
                    verify_file(&target, bytes, digest, "rolled-back output")?;
                }
                (None, None) => {
                    let relative = IpodPath::new(output.target.clone())?;
                    let target = mount.resolve_possible(&relative)?;
                    if target.exists() {
                        verify_file(
                            &target,
                            output.bytes,
                            &output.sha256,
                            "installed new output",
                        )?;
                        fs::remove_file(&target).map_err(|source| {
                            io_error("remove rolled-back new output", &target, source)
                        })?;
                        sync_directory(target.parent().unwrap_or(mount.as_path()))?;
                    }
                    verify_absent(&target, "rolled-back new output")?;
                }
                _ => {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: "source manifest has an incomplete fingerprint".to_owned(),
                    });
                }
            }
        }
        for output in &journal.staging.outputs {
            let source = original_state(&journal.staging, output)?;
            match (source.bytes, source.sha256.as_deref()) {
                (Some(bytes), Some(digest)) => {
                    let target = resolve_target(mount, output)?;
                    verify_file(&target, bytes, digest, "rolled-back output")?;
                }
                (None, None) => {
                    let relative = IpodPath::new(output.target.clone())?;
                    let target = mount.resolve_possible(&relative)?;
                    verify_absent(&target, "rolled-back new output")?;
                }
                _ => {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: "source manifest has an incomplete fingerprint".to_owned(),
                    });
                }
            }
        }
    }
    remove_transaction_directory(&transaction)
}

pub(crate) fn pending_transaction(mount: &MountRoot) -> Result<Option<PathBuf>> {
    let relative = IpodPath::new(TRANSACTION_PATH)?;
    if mount.contains(&relative)? {
        mount.resolve_existing(&relative).map(Some)
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_lines)]
fn validate_recovery_state(
    mount: &MountRoot,
    transaction: &Path,
    journal: &TransactionJournal,
) -> Result<()> {
    if journal.installed > journal.staging.outputs.len() {
        return Err(Error::Verification {
            format: "device transaction",
            reason: "journal installation count is invalid".to_owned(),
        });
    }
    let expected_targets = [
        "iPod_Control/iTunes/iTunes Library.itlp/Library.itdb",
        "iPod_Control/iTunes/iTunes Library.itlp/Locations.itdb",
        "iPod_Control/iTunes/iTunes Library.itlp/Dynamic.itdb",
        "iPod_Control/iTunes/iTunes Library.itlp/Extras.itdb",
        "iPod_Control/iTunes/iTunes Library.itlp/Genius.itdb",
        "iPod_Control/iTunes/iTunes Library.itlp/Locations.itdb.cbk",
        "iPod_Control/iTunes/iTunesCDB",
    ];
    for expected in expected_targets {
        if journal
            .staging
            .outputs
            .iter()
            .filter(|output| output.target == expected)
            .count()
            != 1
        {
            return Err(Error::Verification {
                format: "device transaction",
                reason: "journal target set is invalid".to_owned(),
            });
        }
    }
    for output in &journal.staging.outputs {
        let relative = IpodPath::new(output.target.clone())?;
        if !expected_targets.contains(&output.target.as_str())
            && !output.target.starts_with("iPod_Control/Music/")
        {
            return Err(Error::Verification {
                format: "device transaction",
                reason: format!("unexpected transaction output target {}", output.target),
            });
        }
        let _ = relative;
    }

    for source in &journal.staging.source {
        if journal
            .staging
            .outputs
            .iter()
            .any(|output| output.target == source.path)
        {
            continue;
        }
        let relative = IpodPath::new(source.path.clone())?;
        match (source.bytes, source.sha256.as_deref()) {
            (Some(bytes), Some(digest)) => {
                let live = mount.resolve_existing(&relative)?;
                verify_file(&live, bytes, digest, "recovery generation input")?;
            }
            (None, None) if mount.contains(&relative)? => {
                return Err(Error::Verification {
                    format: "device transaction",
                    reason: format!("{} appeared during the transaction", source.path),
                });
            }
            (None, None) => {}
            _ => {
                return Err(Error::Verification {
                    format: "device transaction",
                    reason: "source manifest has an incomplete fingerprint".to_owned(),
                });
            }
        }
    }

    for (index, output) in journal.staging.outputs.iter().enumerate() {
        let original = original_state(&journal.staging, output)?;
        match (original.bytes, original.sha256.as_deref()) {
            (Some(original_bytes), Some(original_digest)) => {
                let target = resolve_target(mount, output)?;
                if journal.phase != TransactionPhase::BackingUp {
                    let backup = transaction.join("backup").join(&output.staged);
                    verify_file(&backup, original_bytes, original_digest, "recovery backup")?;
                }
                let original_matches =
                    fingerprint_matches(&target, original_bytes, original_digest)?;
                let output_matches = fingerprint_matches(&target, output.bytes, &output.sha256)?;
                let may_be_output = index < journal.installed;
                if (!(original_matches || may_be_output && output_matches))
                    || (journal.phase == TransactionPhase::Committed && !output_matches)
                {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: format!("{} has an unexpected interrupted state", output.target),
                    });
                }
            }
            (None, None) => {
                let relative = IpodPath::new(output.target.clone())?;
                let target = mount.resolve_possible(&relative)?;
                let may_be_output = index < journal.installed;
                let present = target.exists();
                let output_matches =
                    present && fingerprint_matches(&target, output.bytes, &output.sha256)?;
                if !(!present || may_be_output && output_matches) {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: format!(
                            "{} has an unexpected interrupted new-file state",
                            output.target
                        ),
                    });
                }
                if journal.phase == TransactionPhase::Committed && !output_matches {
                    return Err(Error::Verification {
                        format: "device transaction",
                        reason: format!("{} is missing in the committed state", output.target),
                    });
                }
            }
            _ => {
                return Err(Error::Verification {
                    format: "device transaction",
                    reason: "source manifest has an incomplete fingerprint".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn fingerprint_matches(path: &Path, bytes: u64, digest: &str) -> Result<bool> {
    let (actual_bytes, actual_digest) = fingerprint_host_file(path)?;
    Ok(actual_bytes == bytes && hex(&actual_digest) == digest)
}

fn verify_bundle(
    device: &Device,
    staged: &StagedSqliteEdit,
    manifest: &StagingManifest,
) -> Result<()> {
    let profile = device.profile().ok_or_else(|| Error::Unsupported {
        feature: "device transaction",
        reason: "the device profile is unknown".to_owned(),
    })?;
    if manifest.profile != profile.key()
        || manifest.removed_tracks != staged.removed_tracks()
        || manifest.added_tracks != staged.added_tracks()
        || manifest.outputs.len() != 7 + staged.added_tracks()
    {
        return Err(Error::Verification {
            format: "staging manifest",
            reason: "bundle profile, operation, or output count is inconsistent".to_owned(),
        });
    }
    if manifest.outputs.len() != staged.added_media().len() + 7 {
        return Err(Error::Verification {
            format: "staging manifest",
            reason: "media output count is inconsistent".to_owned(),
        });
    }
    for output in &manifest.outputs {
        verify_file(
            &staged.directory().join(&output.staged),
            output.bytes,
            &output.sha256,
            "staging bundle output",
        )?;
    }
    Ok(())
}

fn original_state<'a>(
    manifest: &'a StagingManifest,
    output: &ManifestOutputFile,
) -> Result<&'a super::manifest::ManifestSourceFile> {
    manifest
        .source
        .iter()
        .find(|source| source.path == output.target)
        .ok_or_else(|| Error::Verification {
            format: "device transaction",
            reason: format!("manifest lacks original state for {}", output.target),
        })
}

fn require_transaction_space(mount: &MountRoot, manifest: &StagingManifest) -> Result<()> {
    let backup_bytes = manifest.outputs.iter().try_fold(0_u64, |total, output| {
        let original = original_state(manifest, output)?;
        total
            .checked_add(original.bytes.unwrap_or(0))
            .ok_or_else(|| Error::Verification {
                format: "device transaction",
                reason: "required backup space overflowed u64".to_owned(),
            })
    })?;
    let temporary_bytes = manifest
        .outputs
        .iter()
        .map(|output| output.bytes)
        .max()
        .unwrap_or(0);
    let required = backup_bytes
        .checked_add(temporary_bytes)
        .and_then(|bytes| bytes.checked_add(4 * 1024 * 1024))
        .ok_or_else(|| Error::Verification {
            format: "device transaction",
            reason: "required transaction space overflowed u64".to_owned(),
        })?;
    let available = fs2::available_space(mount.as_path())
        .map_err(|source| io_error("check transaction free space", mount.as_path(), source))?;
    if available < required {
        return Err(Error::Verification {
            format: "device transaction",
            reason: format!(
                "at least {required} bytes are required, but only {available} bytes are available"
            ),
        });
    }
    Ok(())
}

fn transaction_host_path(mount: &MountRoot) -> Result<PathBuf> {
    let relative = IpodPath::new(TRANSACTION_PATH)?;
    let mut path = mount.as_path().to_path_buf();
    path.extend(relative.components());
    Ok(path)
}

fn resolve_target(mount: &MountRoot, output: &ManifestOutputFile) -> Result<PathBuf> {
    mount.resolve_existing(&IpodPath::new(output.target.clone())?)
}

fn copy_new_verified(source: &Path, destination: &Path, bytes: u64, digest: &str) -> Result<()> {
    let mut input = File::open(source)
        .map_err(|error| io_error("open transaction backup source", source, error))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| io_error("create transaction backup", destination, error))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| io_error("copy transaction backup", destination, error))?;
    output
        .sync_all()
        .map_err(|error| io_error("flush transaction backup", destination, error))?;
    verify_file(destination, bytes, digest, "transaction backup")
}

fn install_file(source: &Path, target: &Path, sequence: usize) -> Result<()> {
    let parent = target.parent().ok_or_else(|| Error::Verification {
        format: "device transaction",
        reason: "installation target has no parent".to_owned(),
    })?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Verification {
            format: "device transaction",
            reason: "installation target name is not UTF-8".to_owned(),
        })?;
    let temporary = parent.join(format!(".{name}.libopod-{sequence}.tmp"));
    let _ = fs::remove_file(&temporary);
    let mut input = File::open(source)
        .map_err(|error| io_error("open staged installation source", source, error))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| io_error("create sibling installation file", &temporary, error))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| io_error("copy sibling installation file", &temporary, error))?;
    output
        .sync_all()
        .map_err(|error| io_error("flush sibling installation file", &temporary, error))?;
    fs::rename(&temporary, target)
        .map_err(|error| io_error("replace live database file", target, error))?;
    sync_directory(parent)
}

fn verify_file(path: &Path, bytes: u64, digest: &str, format: &'static str) -> Result<()> {
    let (actual_bytes, actual_digest) = fingerprint_host_file(path)?;
    if actual_bytes != bytes || hex(&actual_digest) != digest {
        return Err(Error::Verification {
            format,
            reason: format!("{} does not match its manifest fingerprint", path.display()),
        });
    }
    Ok(())
}

fn verify_absent(path: &Path, format: &'static str) -> Result<()> {
    if path.exists() {
        return Err(Error::Verification {
            format,
            reason: format!("{} still exists after rollback", path.display()),
        });
    }
    Ok(())
}

fn write_journal(directory: &Path, journal: &TransactionJournal) -> Result<()> {
    let encoded = serde_json::to_vec_pretty(journal).map_err(|source| Error::Malformed {
        format: "libopod transaction journal",
        offset: 0,
        reason: source.to_string(),
    })?;
    let temporary = directory.join("journal.tmp");
    let target = directory.join(JOURNAL_NAME);
    let mut file = File::create(&temporary)
        .map_err(|source| io_error("create transaction journal temporary", &temporary, source))?;
    file.write_all(&encoded)
        .map_err(|source| io_error("write transaction journal temporary", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush transaction journal temporary", &temporary, source))?;
    fs::rename(&temporary, &target)
        .map_err(|source| io_error("install transaction journal", &target, source))?;
    sync_directory(directory)
}

fn read_journal(directory: &Path) -> Result<TransactionJournal> {
    let path = directory.join(JOURNAL_NAME);
    let bytes =
        fs::read(&path).map_err(|source| io_error("read transaction journal", &path, source))?;
    let journal: TransactionJournal =
        serde_json::from_slice(&bytes).map_err(|source| Error::Malformed {
            format: "libopod transaction journal",
            offset: u64::try_from(source.column()).unwrap_or(u64::MAX),
            reason: source.to_string(),
        })?;
    if journal.format != "libopod-device-transaction" || journal.version != 1 {
        return Err(Error::Unsupported {
            feature: "transaction journal version",
            reason: "expected libopod-device-transaction version 1".to_owned(),
        });
    }
    Ok(journal)
}

fn remove_transaction_directory(path: &Path) -> Result<()> {
    fs::remove_dir_all(path)
        .map_err(|source| io_error("remove completed transaction directory", path, source))?;
    sync_directory(path.parent().ok_or_else(|| Error::Verification {
        format: "device transaction",
        reason: "transaction directory has no parent".to_owned(),
    })?)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("flush transaction directory", path, source))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
