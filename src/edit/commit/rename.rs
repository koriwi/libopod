//! Rename-backed replacements (versions 3 and 4): durable originals move to
//! backup only after replacements are ready. Recovery recognizes the gap and can consume backups only
//! after durable rollback intent. Version 2 keeps its copy-based recovery.
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use super::{
    fingerprint_host_file, hex, io_error, original_state, remove_if_present, sync_directory,
    verify_absent, verify_file, write_installation_temporary, FailureMode, ManifestOutputFile,
    Progress, ProgressEvent, StagingManifest, TransactionJournal, TransactionPhase, JOURNAL_NAME,
};
use crate::{Error, IpodPath, MountRoot, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    TemporaryReady,
    OriginalMoved,
    BackupDurable,
    ReplacementMoved,
    ReplacementDurable,
    RestoredMoved,
    RestoredDurable,
    CleanupEntryRemoved,
    JournalRemoved,
}

#[allow(clippy::unnecessary_wraps)] // Production is a no-op; tests inject I/O-boundary failures.
fn checkpoint(mode: FailureMode, sequence: usize, step: Step) -> Result<()> {
    #[cfg(not(test))]
    let _ = (mode, sequence, step);
    #[cfg(test)]
    if mode == FailureMode::SimulateRenameInterruption(sequence, step) {
        return Err(Error::Verification {
            format: "injected transaction interruption",
            reason: format!("stopped at {step:?} for output {sequence}"),
        });
    }
    Ok(())
}

pub(super) fn validate_paths(manifest: &StagingManifest) -> Result<()> {
    let mut targets = BTreeSet::new();
    let mut staged = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for source in &manifest.source {
        IpodPath::new(source.path.clone())?;
        if !sources.insert(source.path.to_ascii_lowercase()) {
            return invalid("duplicate transaction source");
        }
    }
    for output in &manifest.outputs {
        IpodPath::new(output.target.clone())?;
        IpodPath::new(output.staged.clone())?;
        if !targets.insert(output.target.to_ascii_lowercase())
            || !staged.insert(output.staged.to_ascii_lowercase())
        {
            return invalid("duplicate transaction target or staging path");
        }
        original_state(manifest, output)?;
    }
    Ok(())
}

pub(super) fn replacement_target(
    mount: &MountRoot,
    output: &ManifestOutputFile,
) -> Result<PathBuf> {
    let path = mount.resolve_possible(&IpodPath::new(output.target.clone())?)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            invalid("replacement target must be a regular file, not a symlink")
        }
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(io_error("inspect replacement target", &path, error)),
    }
}

fn backup_relative(output: &ManifestOutputFile) -> Result<IpodPath> {
    IpodPath::new(output.staged.clone())?;
    IpodPath::new(format!("backup/{}", output.staged))
}

pub(super) fn existing_backup(
    transaction: &Path,
    output: &ManifestOutputFile,
) -> Result<Option<PathBuf>> {
    let root = MountRoot::open(transaction)?;
    let relative = backup_relative(output)?;
    if root.contains(&relative)? {
        let path = root.resolve_possible(&relative)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("inspect recovery backup", &path, error))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return invalid("recovery backup must be a regular file, not a symlink");
        }
        Ok(Some(root.resolve_existing(&relative)?))
    } else {
        Ok(None)
    }
}

/// Every directory entry leading to a backup must be durable BEFORE removing
/// a live name. Sync newly created parents rather than just the backup root.
pub(super) fn prepare_backup_path(
    transaction: &Path,
    output: &ManifestOutputFile,
) -> Result<PathBuf> {
    let root = MountRoot::open(transaction)?;
    let relative = backup_relative(output)?;
    let components: Vec<_> = relative.components().collect();
    let mut prefix = String::new();
    for component in &components[..components.len() - 1] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let directory = IpodPath::new(prefix.clone())?;
        if !root.contains(&directory)? {
            let path = root.resolve_possible(&directory)?;
            fs::create_dir(&path)
                .map_err(|error| io_error("create recovery directory", &path, error))?;
            sync_directory(path.parent().expect("root-relative directory"))?;
        }
    }
    let path = root.resolve_possible(&relative)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => return invalid("recovery backup unexpectedly exists before installation"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("inspect unused backup path", &path, error)),
    }
    Ok(path)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn install_replacement(
    source: &Path,
    target: &Path,
    backup: &Path,
    output: &ManifestOutputFile,
    original: (u64, &str),
    sequence: usize,
    failure: FailureMode,
    progress: &mut Progress<'_>,
) -> Result<()> {
    let parent = target.parent().expect("validated replacement parent");
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Verification {
            format: "device transaction",
            reason: "replacement name is not UTF-8".to_owned(),
        })?;
    let temporary = parent.join(format!(".{name}.libopod-{sequence}.tmp"));
    remove_if_present(&temporary, "remove stale replacement temporary")?;
    // Intent (installed = sequence + 1) is already durable. A partial or
    // complete temporary can be discarded by recovery without the host bundle.
    write_installation_temporary(source, &temporary, None)?;
    verify_file(
        &temporary,
        output.bytes,
        &output.sha256,
        "replacement temporary",
    )?;
    sync_directory(parent)?;
    checkpoint(failure, sequence, Step::TemporaryReady)?;

    progress(ProgressEvent::Phase("Preserving original file by rename"));
    // Recheck the file type after the potentially long replacement copy.
    // Never preserve a newly substituted symlink in place of the original.
    let metadata = fs::symlink_metadata(target)
        .map_err(|error| io_error("inspect original before preservation", target, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return invalid("original is no longer a regular replacement file");
    }
    verify_file(target, original.0, original.1, "live replacement input")?;
    flush_original(target, "flush original before preservation")?;
    verify_absent(backup, "unused recovery backup path")?;
    fs::rename(target, backup)
        .map_err(|error| io_error("move original to recovery backup", target, error))?;
    checkpoint(failure, sequence, Step::OriginalMoved)?;
    // Persist the destination name before flushing removal of the source name.
    sync_directory(backup.parent().expect("validated backup parent"))?;
    sync_directory(parent)?;
    verify_file(backup, original.0, original.1, "renamed recovery backup")?;
    checkpoint(failure, sequence, Step::BackupDurable)?;

    progress(ProgressEvent::Phase("Publishing verified replacement"));
    fs::rename(&temporary, target)
        .map_err(|error| io_error("publish verified replacement", target, error))?;
    checkpoint(failure, sequence, Step::ReplacementMoved)?;
    sync_directory(parent)?;
    checkpoint(failure, sequence, Step::ReplacementDurable)
}

pub(super) fn restore_backup(
    backup: &Path,
    target: &Path,
    sequence: usize,
    failure: FailureMode,
) -> Result<()> {
    // RollingBack is durable before this operation. Consuming an original is
    // safe: a retry accepts a missing backup ONLY if the live original verifies.
    flush_original(backup, "flush recovery original")?;
    fs::rename(backup, target)
        .map_err(|error| io_error("restore original by rename", target, error))?;
    checkpoint(failure, sequence, Step::RestoredMoved)?;
    sync_directory(target.parent().expect("validated replacement parent"))?;
    sync_directory(backup.parent().expect("validated backup parent"))?;
    checkpoint(failure, sequence, Step::RestoredDurable)
}

/// A previous recovery may have stopped after rename but before its directory
/// syncs. Seeing the correct live bytes does NOT prove those names are durable.
/// Repeat the barriers even when there is no backup left to rename this time.
pub(super) fn finish_restored_sync(
    transaction: &Path,
    output: &ManifestOutputFile,
    target: &Path,
    sequence: usize,
    failure: FailureMode,
) -> Result<()> {
    flush_original(target, "flush restored original")?;
    sync_directory(target.parent().expect("validated replacement parent"))?;
    let root = MountRoot::open(transaction)?;
    let relative = backup_relative(output)?;
    let mut parent = Path::new(relative.as_str()).parent();
    while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
        let directory = IpodPath::new(path.to_string_lossy().into_owned())?;
        if root.contains(&directory)? {
            sync_directory(&root.resolve_existing(&directory)?)?;
        }
        parent = path.parent();
    }
    sync_directory(root.as_path())?;
    checkpoint(failure, sequence, Step::RestoredDurable)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_replacement(
    mount: &MountRoot,
    transaction: &Path,
    journal: &TransactionJournal,
    index: usize,
    output: &ManifestOutputFile,
    bytes: u64,
    digest: &str,
) -> Result<()> {
    let target = replacement_target(mount, output)?;
    let fingerprint = if target.exists() {
        Some(fingerprint_host_file(&target)?)
    } else {
        None
    };
    let original_matches = fingerprint
        .as_ref()
        .is_some_and(|(actual_bytes, actual_digest)| {
            *actual_bytes == bytes && hex(actual_digest) == digest
        });
    let output_matches = fingerprint
        .as_ref()
        .is_some_and(|(actual_bytes, actual_digest)| {
            *actual_bytes == output.bytes && hex(actual_digest) == output.sha256
        });
    // Terminal journals prove which generation to keep. Backups may already
    // have disappeared during cleanup; verify live bytes, never require them.
    match journal.phase {
        TransactionPhase::Committed if output_matches => return Ok(()),
        TransactionPhase::RolledBack if original_matches => return Ok(()),
        TransactionPhase::Committed | TransactionPhase::RolledBack => {
            return invalid("terminal transaction output does not verify");
        }
        _ => {}
    }
    let backup = existing_backup(transaction, output)?;
    if let Some(backup) = &backup {
        verify_file(backup, bytes, digest, "recovery backup")?;
    }
    let attempted = index < journal.installed;
    let rollback = journal.phase == TransactionPhase::RollingBack;
    // Only the in-flight replacement can have lost its original name during
    // installation. During rollback, a valid backup can fill a rename gap too.
    let gap_allowed = attempted
        && (rollback
            || (journal.phase == TransactionPhase::Installing && index + 1 == journal.installed));
    let valid = if !attempted {
        backup.is_none() && original_matches
    } else if backup.is_some() {
        original_matches || output_matches || (fingerprint.is_none() && gap_allowed)
    } else {
        original_matches
            && (rollback
                || (journal.phase == TransactionPhase::Installing
                    && index + 1 == journal.installed))
    };
    if !valid {
        return invalid(&format!(
            "{} has an unexpected rename/backup state",
            output.target
        ));
    }
    Ok(())
}

/// The terminal journal is removed last. An interruption during recursive
/// backup cleanup must never erase the only proof that no rollback is needed.
pub(super) fn remove_transaction_directory(path: &Path, failure: FailureMode) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| io_error("read completed transaction directory", path, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| io_error("read completed transaction entry", path, error))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for (index, entry) in entries
        .iter()
        .filter(|entry| entry.file_name() != JOURNAL_NAME)
        .enumerate()
    {
        let file = entry.path();
        if entry
            .file_type()
            .map_err(|error| io_error("inspect cleanup entry", &file, error))?
            .is_dir()
        {
            fs::remove_dir_all(&file)
                .map_err(|error| io_error("remove recovery backups", &file, error))?;
        } else {
            fs::remove_file(&file)
                .map_err(|error| io_error("remove recovery artifact", &file, error))?;
        }
        sync_directory(path)?;
        checkpoint(failure, index, Step::CleanupEntryRemoved)?;
    }
    fs::remove_file(path.join(JOURNAL_NAME))
        .map_err(|error| io_error("remove terminal recovery journal", path, error))?;
    checkpoint(failure, 0, Step::JournalRemoved)?;
    fs::remove_dir(path)
        .map_err(|error| io_error("remove empty transaction directory", path, error))?;
    sync_directory(path.parent().expect("transaction parent"))
}

/// Before initial journal publication (or after terminal journal deletion),
/// only empty scaffolding can remain. Never guess when payload/backup bytes
/// exist without a journal. This also handles legacy empty backup directories.
pub(super) fn remove_empty_scaffold(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path.join(JOURNAL_NAME)) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("inspect recovery journal", path, error)),
    }
    let entries = fs::read_dir(path)
        .map_err(|error| io_error("inspect transaction scaffold", path, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| io_error("read transaction scaffold", path, error))?;
    for entry in &entries {
        let kind = entry
            .file_type()
            .map_err(|error| io_error("inspect scaffold entry", entry.path(), error))?;
        let safe = (entry.file_name() == "journal.tmp" && kind.is_file())
            || (entry.file_name() == "backup"
                && kind.is_dir()
                && fs::read_dir(entry.path())
                    .map_err(|error| {
                        io_error("inspect empty backup directory", entry.path(), error)
                    })?
                    .next()
                    .is_none());
        if !safe {
            return Ok(false);
        }
    }
    for entry in entries {
        if entry.file_name() == "backup" {
            fs::remove_dir(entry.path())
                .map_err(|error| io_error("remove empty backup scaffold", entry.path(), error))?;
        } else {
            fs::remove_file(entry.path())
                .map_err(|error| io_error("remove unpublished journal", entry.path(), error))?;
        }
    }
    fs::remove_dir(path)
        .map_err(|error| io_error("remove empty transaction scaffold", path, error))?;
    sync_directory(path.parent().expect("transaction parent"))?;
    Ok(true)
}

pub(super) fn flush_original(path: &Path, operation: &'static str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    // FlushFileBuffers needs a writable handle on Windows; Unix fsync works
    // with a read-only descriptor and should not require writable file modes.
    #[cfg(windows)]
    options.write(true);
    options
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_error(operation, path, error))
}

fn invalid<T>(reason: &str) -> Result<T> {
    Err(Error::Verification {
        format: "device transaction",
        reason: reason.to_owned(),
    })
}
