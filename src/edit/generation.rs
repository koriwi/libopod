use std::{
    fs::File,
    io::{BufReader, Read},
};

use sha2::{Digest, Sha256};

use crate::{
    error::io_error, DeviceProfile, Error, IpodPath, MountRoot, Result, SqliteLibraryFile,
};

/// State of one write-relevant file when a device was opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileFingerprint {
    path: IpodPath,
    bytes: Option<u64>,
    sha256: Option<[u8; 32]>,
}

impl FileFingerprint {
    /// Returns the mount-relative file name.
    #[must_use]
    pub fn path(&self) -> &IpodPath {
        &self.path
    }

    /// Returns the file length, or `None` when the file was absent.
    #[must_use]
    pub const fn bytes(&self) -> Option<u64> {
        self.bytes
    }

    /// Returns the SHA-256 digest, or `None` when the file was absent.
    #[must_use]
    pub const fn sha256(&self) -> Option<&[u8; 32]> {
        self.sha256.as_ref()
    }
}

/// Complete generation identity for database and artwork inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationFingerprint {
    files: Vec<FileFingerprint>,
}

impl GenerationFingerprint {
    pub(crate) fn capture(mount: &MountRoot, profile: Option<&DeviceProfile>) -> Result<Self> {
        let mut paths = required_paths(profile)?;
        paths.sort();
        paths.dedup();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            if mount.contains(&path)? {
                let host = mount.resolve_existing(&path)?;
                let (bytes, sha256) = fingerprint_host_file(&host)?;
                files.push(FileFingerprint {
                    path,
                    bytes: Some(bytes),
                    sha256: Some(sha256),
                });
            } else {
                files.push(FileFingerprint {
                    path,
                    bytes: None,
                    sha256: None,
                });
            }
        }
        Ok(Self { files })
    }

    /// Returns fingerprints in stable path order.
    #[must_use]
    pub fn files(&self) -> &[FileFingerprint] {
        &self.files
    }

    pub(crate) fn verify_unchanged(
        &self,
        mount: &MountRoot,
        profile: Option<&DeviceProfile>,
    ) -> Result<()> {
        if *self != Self::capture(mount, profile)? {
            return Err(Error::Verification {
                format: "device generation",
                reason: "a database or artwork input changed after the device was opened"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

pub(crate) fn fingerprint_host_file(path: &std::path::Path) -> Result<(u64, [u8; 32])> {
    let metadata = std::fs::metadata(path)
        .map_err(|source| io_error("inspect fingerprint input", path, source))?;
    if !metadata.is_file() {
        return Err(Error::Verification {
            format: "file fingerprint",
            reason: format!("{} is not a regular file", path.display()),
        });
    }
    let mut reader = BufReader::new(
        File::open(path).map_err(|source| io_error("open fingerprint input", path, source))?,
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| io_error("hash fingerprint input", path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok((metadata.len(), hasher.finalize().into()))
}

fn required_paths(profile: Option<&DeviceProfile>) -> Result<Vec<IpodPath>> {
    let mut paths = vec![
        IpodPath::new("iPod_Control/Device/SysInfo")?,
        IpodPath::new("iPod_Control/Device/SysInfoExtended")?,
        IpodPath::new("iPod_Control/iTunes/iTunesDB")?,
        IpodPath::new("iPod_Control/iTunes/iTunesCDB")?,
        IpodPath::new("iPod_Control/Artwork/ArtworkDB")?,
        IpodPath::new("iPod_Control/iTunes/iTunes Library.itlp/Locations.itdb.cbk")?,
    ];
    for file in SqliteLibraryFile::ALL {
        paths.push(IpodPath::new(format!(
            "iPod_Control/iTunes/iTunes Library.itlp/{}",
            file.file_name()
        ))?);
    }
    if let Some(profile) = profile {
        for artwork in &profile.capabilities().artwork_formats {
            paths.push(IpodPath::new(format!(
                "iPod_Control/Artwork/F{}_1.ithmb",
                artwork.format_id
            ))?);
        }
    }
    Ok(paths)
}
