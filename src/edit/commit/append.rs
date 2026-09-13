//! Version 4: aligned, prefix-preserving thumbnail additions.
//!
//! Keep a durable, verified suffix spool on-device before publishing append
//! intent. It lets recovery verify even a byte-partial append without host
//! files. Rollback checks the entire old prefix and the partial suffix before
//! truncating, then flushes data and namespace changes before terminal cleanup.
//! Never discard spools before a terminal journal. No in-place reindexing.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    hex, io_error, journal_capacity, original_state, remove_if_present, rename, sync_directory,
    verify_file, FailureMode, ManifestOutputFile, Progress, ProgressEvent, StagingManifest,
    TransactionJournal, TransactionPhase,
};
use crate::{Error, MountRoot, Result};

const ALIGNMENT: u64 = 4096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub original_bytes: u64,
    pub suffix_sha256: String,
}

pub(super) type Plans = BTreeMap<String, Plan>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    SpoolPartial,
    SpoolDurable,
    IntentDurable,
    AppendPartial,
    AppendWritten,
    AppendDurable,
    Truncated,
    TruncateDurable,
}

#[allow(clippy::unnecessary_wraps)] // Test-only fault injection.
fn checkpoint(failure: FailureMode, index: usize, step: Step) -> Result<()> {
    #[cfg(not(test))]
    let _ = (failure, index, step);
    #[cfg(test)]
    if failure == FailureMode::SimulateAppendInterruption(index, step) {
        return invalid(&format!(
            "injected transaction interruption at {step:?}, output {index}"
        ));
    }
    Ok(())
}

// Start with the measured, block-aligned formats. Other profiles/formats and
// partial slots retain the existing full replacement protocol.
fn slot_size(profile: &str, target: &str) -> Option<u64> {
    if !matches!(
        profile,
        "classic" | "classic-6g" | "classic-6.5g" | "classic-7g" | "nano-3g" | "nano-4g"
    ) {
        return None;
    }
    match target {
        "iPod_Control/Artwork/F1055_1.ithmb" | "iPod_Control/Artwork/F1068_1.ithmb" => Some(32_768),
        "iPod_Control/Artwork/F1060_1.ithmb" if profile != "nano-4g" => Some(204_800),
        _ => None,
    }
}

fn safe_geometry(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect append target geometry", path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.nlink() == 1
            && metadata.blksize() != 0
            && ALIGNMENT.is_multiple_of(metadata.blksize()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(false) // No supported allocation/write-granularity probe yet.
    }
}

pub(super) fn plan(
    mount: &MountRoot,
    directory: &Path,
    manifest: &StagingManifest,
) -> Result<Plans> {
    let mut plans = Plans::new();
    for output in &manifest.outputs {
        let Some(slot) = slot_size(&manifest.profile, &output.target) else {
            continue;
        };
        let original = original_state(manifest, output)?;
        let (Some(bytes), Some(digest)) = (original.bytes, original.sha256.as_deref()) else {
            continue;
        };
        // Spool plus live suffix must not write more bytes than replacement.
        if bytes == 0
            || bytes % slot != 0
            || output.bytes <= bytes
            || output.bytes - bytes > bytes
            || output.bytes % slot != 0
        {
            continue;
        }
        let live = rename::replacement_target(mount, output)?;
        if !safe_geometry(&live)?
            || fs::metadata(&live)
                .map_err(|error| io_error("inspect append permissions", &live, error))?
                .permissions()
                .readonly()
        {
            continue;
        }
        // Directory permissions can permit replacement even when this user
        // cannot write the old inode (ownership/ACLs). Opening without a write
        // probes append permission without changing bytes or length.
        match OpenOptions::new().append(true).open(&live) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
                ) =>
            {
                continue
            }
            Err(error) => return Err(io_error("probe thumbnail append permission", &live, error)),
        }
        let source = directory.join(&output.staged);
        let mut input = File::open(&source)
            .map_err(|error| io_error("open staged thumbnail", &source, error))?;
        if hash_range(&mut input, bytes, &source)? != digest {
            continue; // Reindexed/changed prefix, even if the new file grew.
        }
        let suffix_sha256 = hash_range(&mut input, output.bytes - bytes, &source)?;
        plans.insert(
            output.target.clone(),
            Plan {
                original_bytes: bytes,
                suffix_sha256,
            },
        );
    }
    Ok(plans)
}

pub(super) fn validate_plans(journal: &TransactionJournal) -> Result<()> {
    if journal.version < 4 && !journal.appends.is_empty() {
        return invalid("legacy journal cannot contain append strategies");
    }
    for (target, plan) in &journal.appends {
        let Some(output) = journal
            .staging
            .outputs
            .iter()
            .find(|output| &output.target == target)
        else {
            return invalid("append strategy has no matching output");
        };
        let Some(slot) = slot_size(&journal.staging.profile, target) else {
            return invalid("append strategy is not a supported aligned thumbnail");
        };
        let original = original_state(&journal.staging, output)?;
        if original.bytes != Some(plan.original_bytes)
            || original.sha256.is_none()
            || plan.original_bytes == 0
            || plan.original_bytes % slot != 0
            || output.bytes <= plan.original_bytes
            || output.bytes % slot != 0
            || plan.suffix_sha256.len() != 64
            || !plan
                .suffix_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return invalid("invalid append strategy fingerprint or length");
        }
    }
    Ok(())
}

/// Reserve real, incompressible bytes, not a sparse extent. A failed in-place
/// append has no disposable replacement temporary to fund rollback journaling.
/// Two capacities cover both rollback and terminal markers, including rounding
/// when a longer phase name crosses an allocation-unit boundary.
pub(super) fn reserve_recovery_space(
    transaction: &Path,
    journal: &TransactionJournal,
    progress: &mut Progress<'_>,
) -> Result<()> {
    if journal.appends.is_empty() {
        return Ok(());
    }
    progress(ProgressEvent::Phase("Reserving recovery journal space"));
    let path = transaction.join("rollback-reserve");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| io_error("create recovery space reserve", &path, error))?;
    let mut remaining = journal_capacity(&journal.staging, &journal.appends)?
        .checked_mul(2)
        .ok_or_else(|| Error::Verification {
            format: "device transaction",
            reason: "recovery reserve size overflow".to_owned(),
        })?;
    let seed = Sha256::digest(format!(
        "{}:{:?}:{}",
        transaction.display(),
        std::time::SystemTime::now(),
        std::process::id()
    ));
    let mut counter = 0_u64;
    let mut buffer = [0_u8; 4096];
    while remaining != 0 {
        for chunk in buffer.chunks_exact_mut(32) {
            let mut hash = Sha256::new();
            hash.update(seed);
            hash.update(counter.to_le_bytes());
            chunk.copy_from_slice(&hash.finalize());
            counter += 1;
        }
        let count =
            usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded reserve write");
        file.write_all(&buffer[..count])
            .map_err(|error| io_error("allocate recovery space reserve", &path, error))?;
        remaining -= count as u64;
    }
    file.sync_all()
        .map_err(|error| io_error("flush recovery space reserve", &path, error))?;
    sync_directory(transaction)
}

/// Global state validation has already succeeded. Discard only expendable
/// allocations here; attempted suffix spools remain the rollback byte proof.
pub(super) fn release_recovery_space(
    transaction: &Path,
    journal: &TransactionJournal,
) -> Result<()> {
    if journal.appends.is_empty() {
        return Ok(());
    }
    remove_if_present(
        &transaction.join("rollback-reserve"),
        "release recovery space reserve",
    )?;
    for (index, output) in journal
        .staging
        .outputs
        .iter()
        .enumerate()
        .skip(journal.installed)
    {
        if journal.appends.contains_key(&output.target) {
            remove_if_present(
                &spool_path(transaction, index),
                "remove unattempted suffix spool",
            )?;
        }
    }
    // Repeat the barrier even if a prior recovery unlinked these already.
    sync_directory(transaction)
}

fn spool_path(transaction: &Path, index: usize) -> PathBuf {
    transaction.join(format!("append-{index}.bin"))
}

fn regular(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io_error("inspect append file", path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return invalid("append file must be regular, not a symlink");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    transaction: &Path,
    source: &Path,
    output: &ManifestOutputFile,
    plan: &Plan,
    index: usize,
    failure: FailureMode,
    progress: &mut Progress<'_>,
) -> Result<()> {
    progress(ProgressEvent::Phase("Preparing verified thumbnail suffix"));
    let spool = spool_path(transaction, index);
    let mut input = File::open(source)
        .map_err(|error| io_error("open thumbnail suffix source", source, error))?;
    input
        .seek(SeekFrom::Start(plan.original_bytes))
        .map_err(|error| io_error("seek thumbnail suffix", source, error))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&spool)
        .map_err(|error| io_error("create thumbnail suffix spool", &spool, error))?;
    copy_exact(
        &mut input,
        &mut file,
        output.bytes - plan.original_bytes,
        &spool,
        failure,
        index,
        Step::SpoolPartial,
    )?;
    file.sync_all()
        .map_err(|error| io_error("flush thumbnail suffix spool", &spool, error))?;
    verify_file(
        &spool,
        output.bytes - plan.original_bytes,
        &plan.suffix_sha256,
        "thumbnail suffix spool",
    )?;
    sync_directory(transaction)?;
    checkpoint(failure, index, Step::SpoolDurable)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn install(
    transaction: &Path,
    target: &Path,
    output: &ManifestOutputFile,
    original_digest: &str,
    plan: &Plan,
    index: usize,
    failure: FailureMode,
    progress: &mut Progress<'_>,
) -> Result<()> {
    checkpoint(failure, index, Step::IntentDurable)?;
    progress(ProgressEvent::Phase("Appending verified thumbnail suffix"));
    if !safe_geometry(target)? {
        return invalid("append target geometry changed");
    }
    verify_file(
        target,
        plan.original_bytes,
        original_digest,
        "live append original",
    )?;
    rename::flush_original(target, "flush original before append")?;
    let spool = spool_path(transaction, index);
    regular(&spool)?;
    verify_file(
        &spool,
        output.bytes - plan.original_bytes,
        &plan.suffix_sha256,
        "thumbnail suffix spool",
    )?;
    let mut input = File::open(&spool)
        .map_err(|error| io_error("open thumbnail suffix spool", &spool, error))?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(target)
        .map_err(|error| io_error("open thumbnail for append", target, error))?;
    copy_exact(
        &mut input,
        &mut file,
        output.bytes - plan.original_bytes,
        target,
        failure,
        index,
        Step::AppendPartial,
    )?;
    checkpoint(failure, index, Step::AppendWritten)?;
    file.sync_all()
        .map_err(|error| io_error("flush appended thumbnail", target, error))?;
    sync_directory(target.parent().expect("validated thumbnail parent"))?;
    verify_file(target, output.bytes, &output.sha256, "appended thumbnail")?;
    checkpoint(failure, index, Step::AppendDurable)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate(
    mount: &MountRoot,
    transaction: &Path,
    journal: &TransactionJournal,
    index: usize,
    output: &ManifestOutputFile,
    original_digest: &str,
    plan: &Plan,
) -> Result<()> {
    let target = rename::replacement_target(mount, output)?;
    match journal.phase {
        TransactionPhase::Committed => {
            return verify_file(&target, output.bytes, &output.sha256, "committed append")
        }
        TransactionPhase::RolledBack => {
            return verify_file(
                &target,
                plan.original_bytes,
                original_digest,
                "rolled-back append",
            )
        }
        _ => {}
    }
    if rename::existing_backup(transaction, output)?.is_some() {
        return invalid("append strategy unexpectedly has a rename backup");
    }
    if index >= journal.installed {
        // A spool copy can be incomplete, but no append intent was published.
        return verify_file(
            &target,
            plan.original_bytes,
            original_digest,
            "unattempted append original",
        );
    }
    if !safe_geometry(&target)? {
        return invalid("unsupported recovery append geometry");
    }
    let size = fs::metadata(&target)
        .map_err(|error| io_error("inspect interrupted append", &target, error))?
        .len();
    let partial_allowed = journal.phase == TransactionPhase::RollingBack
        || (journal.phase == TransactionPhase::Installing && index + 1 == journal.installed);
    if size < plan.original_bytes
        || size > output.bytes
        || (!partial_allowed && size != output.bytes)
    {
        return invalid("unexpected interrupted append length");
    }
    let spool = spool_path(transaction, index);
    regular(&spool)?;
    verify_file(
        &spool,
        output.bytes - plan.original_bytes,
        &plan.suffix_sha256,
        "recovery suffix spool",
    )?;
    let mut live = File::open(&target)
        .map_err(|error| io_error("open interrupted thumbnail", &target, error))?;
    let mut whole = Sha256::new();
    let mut prefix = (&mut live).take(plan.original_bytes);
    let mut old = Sha256::new();
    let mut buffer = [0_u8; 4096];
    let mut read = 0_u64;
    loop {
        let count = prefix
            .read(&mut buffer)
            .map_err(|error| io_error("read preserved thumbnail prefix", &target, error))?;
        if count == 0 {
            break;
        }
        read += count as u64;
        old.update(&buffer[..count]);
        whole.update(&buffer[..count]);
    }
    if read != plan.original_bytes || hex(&old.finalize()) != original_digest {
        return invalid("preserved thumbnail prefix does not verify");
    }
    let mut expected =
        File::open(&spool).map_err(|error| io_error("open recovery suffix", &spool, error))?;
    let mut remaining = size - plan.original_bytes;
    let mut actual = [0_u8; 4096];
    loop {
        let count = expected
            .read(&mut buffer)
            .map_err(|error| io_error("read recovery suffix", &spool, error))?;
        if count == 0 {
            break;
        }
        whole.update(&buffer[..count]);
        let compare = usize::try_from(remaining.min(count as u64)).expect("bounded read");
        live.read_exact(&mut actual[..compare])
            .map_err(|error| io_error("read partial append", &target, error))?;
        if actual[..compare] != buffer[..compare] {
            return invalid("partial thumbnail suffix does not verify");
        }
        remaining -= compare as u64;
    }
    if remaining != 0 || hex(&whole.finalize()) != output.sha256 {
        return invalid("suffix spool is not bound to the expected full output");
    }
    Ok(())
}

pub(super) fn rollback(
    target: &Path,
    plan: &Plan,
    index: usize,
    failure: FailureMode,
) -> Result<()> {
    // Global validation precedes all destructive recovery operations. Spools
    // stay intact, so a retry can validate either a partial append or old EOF.
    let size = fs::metadata(target)
        .map_err(|error| io_error("inspect rollback thumbnail", target, error))?
        .len();
    if size != plan.original_bytes {
        let file = OpenOptions::new()
            .write(true)
            .open(target)
            .map_err(|error| io_error("open thumbnail for rollback", target, error))?;
        file.set_len(plan.original_bytes)
            .map_err(|error| io_error("truncate appended thumbnail", target, error))?;
        checkpoint(failure, index, Step::Truncated)?;
    }
    // Also repeat the barrier when a preceding recovery truncated but failed
    // before fsync. Correct visible bytes do not imply durable restoration.
    rename::flush_original(target, "flush truncated thumbnail")?;
    sync_directory(target.parent().expect("validated thumbnail parent"))?;
    checkpoint(failure, index, Step::TruncateDurable)
}

#[allow(clippy::too_many_arguments)]
fn copy_exact(
    input: &mut File,
    output: &mut File,
    bytes: u64,
    path: &Path,
    failure: FailureMode,
    index: usize,
    step: Step,
) -> Result<()> {
    let mut remaining = bytes;
    let mut buffer = [0_u8; 4096];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).expect("bounded copy");
        input
            .read_exact(&mut buffer[..count])
            .map_err(|error| io_error("read thumbnail suffix", path, error))?;
        output
            .write_all(&buffer[..count])
            .map_err(|error| io_error("write thumbnail suffix", path, error))?;
        if remaining == bytes {
            checkpoint(failure, index, step)?;
        }
        remaining -= count as u64;
    }
    Ok(())
}

fn hash_range(input: &mut File, bytes: u64, path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    let copied = std::io::copy(&mut input.take(bytes), &mut hash)
        .map_err(|error| io_error("hash thumbnail range", path, error))?;
    if copied != bytes {
        return invalid("thumbnail range is shorter than its fingerprint");
    }
    Ok(hex(&hash.finalize()))
}

fn invalid<T>(reason: &str) -> Result<T> {
    Err(Error::Verification {
        format: "artwork append transaction",
        reason: reason.to_owned(),
    })
}
