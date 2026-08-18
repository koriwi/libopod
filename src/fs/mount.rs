use std::{
    fs::{self, File},
    io::{Read, Take},
    path::{Path, PathBuf},
};

use crate::{error::io_error, Error, Result};

use super::IpodPath;

/// A canonical host directory used as an iPod mount root.
#[derive(Clone, Debug)]
pub struct MountRoot {
    canonical: PathBuf,
}

impl MountRoot {
    /// Opens and canonicalizes an existing directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be resolved or is not a directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let supplied = path.as_ref();
        let canonical = fs::canonicalize(supplied)
            .map_err(|source| io_error("canonicalize mount root", supplied, source))?;
        let metadata = fs::metadata(&canonical)
            .map_err(|source| io_error("inspect mount root", &canonical, source))?;
        if !metadata.is_dir() {
            return Err(Error::InvalidMount {
                path: supplied.to_path_buf(),
                reason: "path is not a directory".to_owned(),
            });
        }
        Ok(Self { canonical })
    }

    /// Returns the canonical mount root.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.canonical
    }

    /// Resolves an existing validated path and rejects symlink escapes.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is missing, inaccessible, or resolves
    /// outside this mount root.
    pub fn resolve_existing(&self, relative: &IpodPath) -> Result<PathBuf> {
        let mut joined = self.canonical.clone();
        joined.extend(relative.components());
        let resolved = fs::canonicalize(&joined)
            .map_err(|source| io_error("resolve iPod-relative path", &joined, source))?;
        if !resolved.starts_with(&self.canonical) {
            return Err(Error::InvalidIpodPath {
                path: relative.to_string(),
                reason: "resolved path escapes the mount root through a symlink".to_owned(),
            });
        }
        Ok(resolved)
    }

    /// Resolves a validated path whose final component may be absent, while
    /// still rejecting symlink escapes through existing parent directories.
    ///
    /// # Errors
    ///
    /// Returns an error when a parent directory is missing or resolves
    /// outside this mount root.
    pub fn resolve_possible(&self, relative: &IpodPath) -> Result<PathBuf> {
        let mut joined = self.canonical.clone();
        joined.extend(relative.components());
        let file_name = joined.file_name().ok_or_else(|| Error::InvalidIpodPath {
            path: relative.to_string(),
            reason: "path has no final component".to_owned(),
        })?;
        let parent = joined.parent().ok_or_else(|| Error::InvalidIpodPath {
            path: relative.to_string(),
            reason: "path has no parent".to_owned(),
        })?;
        let resolved_parent = fs::canonicalize(parent)
            .map_err(|source| io_error("resolve iPod-relative parent", parent, source))?;
        if !resolved_parent.starts_with(&self.canonical) {
            return Err(Error::InvalidIpodPath {
                path: relative.to_string(),
                reason: "resolved parent escapes the mount root through a symlink".to_owned(),
            });
        }
        Ok(resolved_parent.join(file_name))
    }

    /// Returns whether a validated path currently exists without following it
    /// outside the mount root.
    ///
    /// # Errors
    ///
    /// Returns an error for inaccessible paths and symlink escapes.
    pub fn contains(&self, relative: &IpodPath) -> Result<bool> {
        let mut joined = self.canonical.clone();
        joined.extend(relative.components());
        match fs::symlink_metadata(&joined) {
            Ok(_) => {
                self.resolve_existing(relative)?;
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(io_error("inspect iPod-relative path", joined, source)),
        }
    }
}

pub(crate) fn read_limited(path: &Path, limit: u64, format: &'static str) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|source| io_error("inspect file", path, source))?;
    if metadata.len() > limit {
        return Err(Error::InputTooLarge {
            format,
            path: path.to_path_buf(),
            actual: metadata.len(),
            limit,
        });
    }

    let file = File::open(path).map_err(|source| io_error("open file", path, source))?;
    let capacity = usize::try_from(metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded: Take<File> = file.take(limit.saturating_add(1));
    bounded
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read file", path, source))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(Error::InputTooLarge {
            format,
            path: path.to_path_buf(),
            actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit,
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use tempfile::tempdir;

    use super::MountRoot;
    use crate::IpodPath;

    #[test]
    fn rejects_a_symlink_escape() {
        let mount_dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"not a device file").unwrap();
        symlink(outside.path(), mount_dir.path().join("escape")).unwrap();

        let mount = MountRoot::open(mount_dir.path()).unwrap();
        let path = IpodPath::new("escape/secret").unwrap();
        assert!(mount.resolve_existing(&path).is_err());
    }
}
