use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::generation::fingerprint_host_file;
use crate::{
    error::io_error, Device, Error, GenerationFingerprint, IpodPath, Result, SqliteLibraryFile,
};

pub(crate) const MANIFEST_NAME: &str = "libopod-preview-manifest.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct StagingManifest {
    pub format: String,
    pub version: u32,
    pub profile: String,
    pub operation: String,
    pub removed_tracks: usize,
    pub added_tracks: usize,
    pub source: Vec<ManifestSourceFile>,
    pub outputs: Vec<ManifestOutputFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ManifestSourceFile {
    pub path: String,
    pub present: bool,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    pub backup: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ManifestOutputFile {
    pub staged: String,
    pub target: String,
    pub bytes: u64,
    pub sha256: String,
}

pub(crate) fn back_up_generation(
    device: &Device,
    destination: &Path,
    generation: &GenerationFingerprint,
) -> Result<()> {
    let backup_root = destination.join("original");
    fs::create_dir(&backup_root)
        .map_err(|source| io_error("create host backup directory", &backup_root, source))?;
    for fingerprint in generation.files() {
        let (Some(expected_bytes), Some(expected_digest)) =
            (fingerprint.bytes(), fingerprint.sha256())
        else {
            continue;
        };
        let source = device.mount().resolve_existing(fingerprint.path())?;
        let backup = backup_root.join(fingerprint.path().as_str());
        let parent = backup.parent().ok_or_else(|| Error::Verification {
            format: "host backup",
            reason: "backup path has no parent".to_owned(),
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create host backup parent", parent, error))?;
        let mut source_file = File::open(&source)
            .map_err(|error| io_error("open generation file for backup", &source, error))?;
        let mut backup_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
            .map_err(|error| io_error("create generation backup", &backup, error))?;
        std::io::copy(&mut source_file, &mut backup_file)
            .map_err(|error| io_error("copy generation backup", &backup, error))?;
        backup_file
            .sync_all()
            .map_err(|error| io_error("flush generation backup", &backup, error))?;
        let (actual_bytes, actual_digest) = fingerprint_host_file(&backup)?;
        if actual_bytes != expected_bytes || &actual_digest != expected_digest {
            return Err(Error::Verification {
                format: "host backup",
                reason: format!("backup of {} did not verify", fingerprint.path()),
            });
        }
    }
    sync_directory(&backup_root)?;
    Ok(())
}

pub(crate) fn write_staging_manifest(
    device: &Device,
    destination: &Path,
    generation: &GenerationFingerprint,
    removed_tracks: usize,
    added_targets: &[IpodPath],
) -> Result<PathBuf> {
    let profile = device.profile().ok_or_else(|| Error::Unsupported {
        feature: "staging manifest",
        reason: "the device profile is unknown".to_owned(),
    })?;
    let mut source: Vec<ManifestSourceFile> = generation
        .files()
        .iter()
        .map(|file| ManifestSourceFile {
            path: file.path().to_string(),
            present: file.bytes().is_some(),
            bytes: file.bytes(),
            sha256: file.sha256().map(|digest| hex(digest)),
            backup: file
                .bytes()
                .is_some()
                .then(|| format!("original/{}", file.path())),
        })
        .collect();
    for target in added_targets {
        source.push(ManifestSourceFile {
            path: target.to_string(),
            present: false,
            bytes: None,
            sha256: None,
            backup: None,
        });
    }
    let mut outputs = Vec::new();
    for (staged, target) in output_targets(destination)? {
        let path = destination.join(&staged);
        let (bytes, digest) = fingerprint_host_file(&path)?;
        outputs.push(ManifestOutputFile {
            staged,
            target: target.to_string(),
            bytes,
            sha256: hex(&digest),
        });
    }
    for target in added_targets {
        let path = destination.join(target.as_str());
        let (bytes, digest) = fingerprint_host_file(&path)?;
        outputs.push(ManifestOutputFile {
            staged: target.to_string(),
            target: target.to_string(),
            bytes,
            sha256: hex(&digest),
        });
    }
    let operation = if removed_tracks == 0 && added_targets.is_empty() {
        "no-op"
    } else if removed_tracks == 0 {
        "add-track"
    } else if added_targets.is_empty() {
        "remove-tracks"
    } else {
        "edit"
    }
    .to_owned();
    let manifest = StagingManifest {
        format: "libopod-staging-manifest".to_owned(),
        version: 1,
        profile: profile.key().to_owned(),
        operation,
        removed_tracks,
        added_tracks: added_targets.len(),
        source,
        outputs,
    };
    let encoded = serde_json::to_vec_pretty(&manifest).map_err(|source| Error::Malformed {
        format: "libopod staging manifest",
        offset: 0,
        reason: format!("could not serialize manifest: {source}"),
    })?;
    let path = destination.join(MANIFEST_NAME);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| io_error("create staging manifest", &path, source))?;
    file.write_all(&encoded)
        .map_err(|source| io_error("write staging manifest", &path, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush staging manifest", &path, source))?;
    sync_directory(destination)?;
    if read_staging_manifest(&path)? != manifest {
        return Err(Error::Verification {
            format: "libopod staging manifest",
            reason: "manifest changed during durable write".to_owned(),
        });
    }
    Ok(path)
}

pub(crate) fn read_staging_manifest(path: &Path) -> Result<StagingManifest> {
    let bytes = fs::read(path).map_err(|source| io_error("read staging manifest", path, source))?;
    let manifest: StagingManifest =
        serde_json::from_slice(&bytes).map_err(|source| Error::Malformed {
            format: "libopod staging manifest",
            offset: u64::try_from(source.column()).unwrap_or(u64::MAX),
            reason: source.to_string(),
        })?;
    if manifest.format != "libopod-staging-manifest" || manifest.version != 1 {
        return Err(Error::Unsupported {
            feature: "staging manifest version",
            reason: "expected libopod-staging-manifest version 1".to_owned(),
        });
    }
    Ok(manifest)
}

fn output_targets(destination: &Path) -> Result<Vec<(String, IpodPath)>> {
    let mut targets = Vec::with_capacity(8);
    for file in SqliteLibraryFile::ALL {
        targets.push((
            file.file_name().to_owned(),
            IpodPath::new(format!(
                "iPod_Control/iTunes/iTunes Library.itlp/{}",
                file.file_name()
            ))?,
        ));
    }
    targets.push((
        "Locations.itdb.cbk".to_owned(),
        IpodPath::new("iPod_Control/iTunes/iTunes Library.itlp/Locations.itdb.cbk")?,
    ));
    targets.push((
        "iTunesCDB".to_owned(),
        IpodPath::new("iPod_Control/iTunes/iTunesCDB")?,
    ));
    if destination.join("ArtworkDB").exists() {
        targets.push((
            "ArtworkDB".to_owned(),
            IpodPath::new("iPod_Control/Artwork/ArtworkDB")?,
        ));
    }
    Ok(targets)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("flush directory", path, source))
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
