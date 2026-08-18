use std::{fmt, str::FromStr};

use crate::{Error, Result};

const MAX_PATH_BYTES: usize = 4_096;
const MAX_COMPONENT_BYTES: usize = 255;

/// A validated, slash-separated path relative to an iPod mount root.
///
/// `IpodPath` rejects absolute paths, platform prefixes, empty components,
/// `.` and `..`, backslashes, NULs, and overlong components. It is for host
/// filesystem paths; colon-separated locations stored inside iTunesDB are a
/// separate format.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IpodPath(String);

impl IpodPath {
    /// Validates a mount-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidIpodPath`] if the path is empty, absolute,
    /// platform-prefixed, escaping, malformed, or over the parser limits.
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        validate(&path)?;
        Ok(Self(path))
    }

    /// Returns the normalized slash-separated representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

fn validate(path: &str) -> Result<()> {
    let invalid = |reason: &str| Error::InvalidIpodPath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };

    if path.is_empty() {
        return Err(invalid("path is empty"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid("path exceeds 4096 UTF-8 bytes"));
    }
    if path.starts_with('/') {
        return Err(invalid("absolute paths are not allowed"));
    }
    if path.contains('\\') {
        return Err(invalid("backslashes and platform prefixes are not allowed"));
    }
    if path.contains(':') {
        return Err(invalid(
            "colon-separated database locations are not host paths",
        ));
    }
    if path.contains('\0') {
        return Err(invalid("NUL bytes are not allowed"));
    }

    for component in path.split('/') {
        if component.is_empty() {
            return Err(invalid("empty path components are not allowed"));
        }
        if matches!(component, "." | "..") {
            return Err(invalid("`.` and `..` components are not allowed"));
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(invalid("a component exceeds 255 UTF-8 bytes"));
        }
    }

    Ok(())
}

impl AsRef<str> for IpodPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for IpodPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("IpodPath").field(&self.0).finish()
    }
}

impl fmt::Display for IpodPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for IpodPath {
    type Err = Error;

    fn from_str(path: &str) -> Result<Self> {
        Self::new(path)
    }
}

#[cfg(test)]
mod tests {
    use super::IpodPath;

    #[test]
    fn accepts_normal_mount_relative_paths() {
        let path = IpodPath::new("iPod_Control/iTunes/iTunesCDB").unwrap();
        assert_eq!(path.as_str(), "iPod_Control/iTunes/iTunesCDB");
    }

    #[test]
    fn rejects_escape_and_platform_paths() {
        for path in [
            "",
            "/etc/passwd",
            "../outside",
            "inside/../outside",
            "inside//file",
            "C:\\device\\file",
            ":iPod_Control:Music:F00:file.mp3",
        ] {
            assert!(IpodPath::new(path).is_err(), "accepted {path:?}");
        }
    }
}
