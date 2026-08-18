//! Linux USB identity probing for mounted iPods.
//!
//! Devices like the Nano 1G/2G keep an empty `SysInfo` and no
//! `SysInfoExtended`, so their identity and `FireWire` GUID are only reachable
//! through the USB layer. When the device is mounted on Linux, the backing
//! USB device exposes its product ID (`idProduct`) and serial (`serial`, the
//! iPod.s `FireWire` GUID) under `/sys`. This module resolves the mount to its
//! block device via `/proc/self/mountinfo` and walks the `sysfs` hierarchy to
//! those values. It is strictly additive: every failure yields `None` and
//! never blocks opening a device.
//!
//! Mirrors iOpenPod's udev/sysfs identity sources (`ID_MODEL_ID`,
//! `ID_SERIAL_SHORT`).

use std::{fs, path::Path};

/// USB identity observed from sysfs.
#[derive(Clone, Debug, Default)]
pub struct UsbIdentity {
    pub product_id: Option<u16>,
    /// The USB serial string (the iPod.s `FireWire` GUID for these devices).
    pub serial: Option<String>,
}

/// Resolves the backing USB identity of a mounted iPod, if discoverable.
pub fn probe(mount: &Path) -> UsbIdentity {
    let Some((major, minor)) = mount_block_device(mount) else {
        return UsbIdentity::default();
    };
    let sysfs = Path::new("/sys/dev/block").join(format!("{major}:{minor}"));
    let Ok(real) = fs::canonicalize(&sysfs) else {
        return UsbIdentity::default();
    };
    let mut identity = UsbIdentity::default();
    for ancestor in real.ancestors() {
        let vendor = fs::read_to_string(ancestor.join("idVendor")).unwrap_or_default();
        if vendor.trim() != "05ac" {
            continue;
        }
        identity.product_id = fs::read_to_string(ancestor.join("idProduct"))
            .ok()
            .and_then(|value| u16::from_str_radix(value.trim(), 16).ok());
        identity.serial = fs::read_to_string(ancestor.join("serial"))
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        break;
    }
    identity
}

/// Finds the `major:minor` block device backing a mount point by parsing
/// `/proc/self/mountinfo`.
fn mount_block_device(mount: &Path) -> Option<(u32, u32)> {
    let canonical = fs::canonicalize(mount).ok()?;
    let content = fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let device = fields[2].to_owned();
        let mount_point = unescape_mountinfo(fields[4]);
        if mount_point == canonical.to_string_lossy() {
            let (major, minor) = device.split_once(':')?;
            return Some((major.parse().ok()?, minor.parse().ok()?));
        }
    }
    None
}

/// Unescapes the octal-escaped mount point field of `/proc/self/mountinfo`
/// (`\040` for space, `\011` for tab, `\012` for newline, `\134` for backslash).
fn unescape_mountinfo(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&raw[index + 1..index + 4], 8) {
                output.push(value);
                index += 4;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::unescape_mountinfo;

    #[test]
    fn unescapes_mountinfo_paths() {
        assert_eq!(
            unescape_mountinfo("/run/media/koriwi/KORIWI_S\\040IP"),
            "/run/media/koriwi/KORIWI_S IP"
        );
        assert_eq!(unescape_mountinfo("/"), "/");
        assert_eq!(unescape_mountinfo("/mnt\\134ipod"), "/mnt\\ipod");
    }
}
