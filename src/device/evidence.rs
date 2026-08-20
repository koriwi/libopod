use std::{fmt, io::Cursor};

use plist::Value;

use crate::{fs::read_limited, Error, IpodPath, MountRoot, Result};

const MAX_SYSINFO_BYTES: u64 = 1024 * 1024;
const MAX_EXTENDED_BYTES: u64 = 4 * 1024 * 1024;

/// Origin of one identity or capability value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EvidenceSource {
    SysInfo,
    SysInfoExtended,
    /// Read from the Linux USB hierarchy (`sysfs`) when the files carry no
    /// identity (e.g. Nano 1G/2G with an empty `SysInfo` and no
    /// `SysInfoExtended`).
    Usb,
}

/// A value retained together with its on-device source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: EvidenceSource,
}

/// Filesystem format reported by device evidence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VolumeFormat {
    Fat32,
    HfsPlus,
    Other,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FireWireGuid([u8; 8]);

impl fmt::Debug for FireWireGuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FireWireGuid(<redacted>)")
    }
}

/// Device identity and capability evidence with sensitive values redacted.
#[derive(Clone)]
pub struct IdentityEvidence {
    model_family: Option<Sourced<String>>,
    generation: Option<Sourced<String>>,
    model_number: Option<Sourced<String>>,
    family_id: Option<Sourced<u16>>,
    usb_product_id: Option<Sourced<u16>>,
    sqlite_db: Option<Sourced<bool>>,
    sparse_artwork: Option<Sourced<bool>>,
    volume_format: Option<Sourced<VolumeFormat>>,
    max_tracks: Option<Sourced<u32>>,
    max_file_size_gib: Option<Sourced<u32>>,
    firmware_versions: Vec<Sourced<String>>,
    firewire_guid: Option<FireWireGuid>,
    product_serial_present: bool,
}

impl IdentityEvidence {
    pub(crate) fn read_from(mount: &MountRoot) -> Result<Self> {
        let mut evidence = Self::empty();
        if let Some(bytes) = read_optional(
            mount,
            "iPod_Control/Device/SysInfo",
            MAX_SYSINFO_BYTES,
            "SysInfo",
        )? {
            evidence.parse_sysinfo(&bytes)?;
        }
        if let Some(bytes) = read_optional(
            mount,
            "iPod_Control/Device/SysInfoExtended",
            MAX_EXTENDED_BYTES,
            "SysInfoExtended",
        )? {
            evidence.parse_extended(&bytes)?;
        }
        #[cfg(target_os = "linux")]
        evidence.read_usb_identity(mount);
        Ok(evidence)
    }

    /// Fills identity gaps from the Linux USB hierarchy: the product ID and
    /// the USB serial (the `FireWire` GUID on iPods) of the backing USB device.
    /// Strictly additive; failures leave the existing values untouched.
    #[cfg(target_os = "linux")]
    fn read_usb_identity(&mut self, mount: &MountRoot) {
        let identity = crate::fs::probe_usb_identity(mount.as_path());
        if self.usb_product_id.is_none() {
            if let Some(product_id) = identity.product_id {
                self.usb_product_id = Some(Sourced {
                    value: product_id,
                    source: EvidenceSource::Usb,
                });
            }
        }
        if self.firewire_guid.is_none() {
            if let Some(serial) = identity.serial {
                if let Some(guid) = parse_guid(serial.trim_start_matches("0x").trim()) {
                    self.firewire_guid = Some(guid);
                }
            }
        }
    }

    fn empty() -> Self {
        Self {
            model_family: None,
            generation: None,
            model_number: None,
            family_id: None,
            usb_product_id: None,
            sqlite_db: None,
            sparse_artwork: None,
            volume_format: None,
            max_tracks: None,
            max_file_size_gib: None,
            firmware_versions: Vec::new(),
            firewire_guid: None,
            product_serial_present: false,
        }
    }

    fn parse_sysinfo(&mut self, bytes: &[u8]) -> Result<()> {
        let text = std::str::from_utf8(bytes).map_err(|source| Error::Malformed {
            format: "SysInfo",
            offset: u64::try_from(source.valid_up_to()).unwrap_or(u64::MAX),
            reason: "file is not valid UTF-8".to_owned(),
        })?;
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "ModelFamily" => set_string(&mut self.model_family, value, EvidenceSource::SysInfo),
                "Generation" => set_string(&mut self.generation, value, EvidenceSource::SysInfo),
                "ModelNumStr" => set_string(&mut self.model_number, value, EvidenceSource::SysInfo),
                "USBProductID" => {
                    if let Some(parsed) = parse_u16(value) {
                        self.usb_product_id = Some(Sourced {
                            value: parsed,
                            source: EvidenceSource::SysInfo,
                        });
                    }
                }
                "visibleBuildID" => self.firmware_versions.push(Sourced {
                    value: value.to_owned(),
                    source: EvidenceSource::SysInfo,
                }),
                "FirewireGuid" => self.firewire_guid = parse_guid(value),
                "pszSerialNumber" => self.product_serial_present = !value.is_empty(),
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_extended(&mut self, bytes: &[u8]) -> Result<()> {
        let value =
            Value::from_reader_xml(Cursor::new(bytes)).map_err(|source| Error::Malformed {
                format: "SysInfoExtended",
                offset: 0,
                reason: format!("invalid XML property list: {source}"),
            })?;
        let dictionary = value.as_dictionary().ok_or_else(|| Error::Malformed {
            format: "SysInfoExtended",
            offset: 0,
            reason: "property-list root is not a dictionary".to_owned(),
        })?;

        if let Some(text) = dictionary.get("ModelFamily").and_then(Value::as_string) {
            set_string(
                &mut self.model_family,
                text,
                EvidenceSource::SysInfoExtended,
            );
        }
        if let Some(text) = dictionary.get("Generation").and_then(Value::as_string) {
            set_string(&mut self.generation, text, EvidenceSource::SysInfoExtended);
        }
        if let Some(text) = dictionary.get("ModelNumStr").and_then(Value::as_string) {
            set_string(
                &mut self.model_number,
                text,
                EvidenceSource::SysInfoExtended,
            );
        }
        self.family_id = unsigned(dictionary.get("FamilyID")).and_then(|value| {
            u16::try_from(value).ok().map(|value| Sourced {
                value,
                source: EvidenceSource::SysInfoExtended,
            })
        });

        self.sqlite_db = dictionary
            .get("SQLiteDB")
            .and_then(Value::as_boolean)
            .map(|value| Sourced {
                value,
                source: EvidenceSource::SysInfoExtended,
            });
        self.sparse_artwork = dictionary
            .get("SupportsSparseArtwork")
            .and_then(Value::as_boolean)
            .map(|value| Sourced {
                value,
                source: EvidenceSource::SysInfoExtended,
            });
        self.max_tracks = unsigned(dictionary.get("MaxTracks")).and_then(|value| {
            u32::try_from(value).ok().map(|value| Sourced {
                value,
                source: EvidenceSource::SysInfoExtended,
            })
        });
        self.max_file_size_gib = unsigned(dictionary.get("MaxFileSizeInGB")).and_then(|value| {
            u32::try_from(value).ok().map(|value| Sourced {
                value,
                source: EvidenceSource::SysInfoExtended,
            })
        });
        self.volume_format = dictionary
            .get("VolumeFormat")
            .and_then(Value::as_string)
            .map(|value| Sourced {
                value: match value.to_ascii_uppercase().as_str() {
                    "FAT32" => VolumeFormat::Fat32,
                    "HFSPLUS" | "HFS+" => VolumeFormat::HfsPlus,
                    _ => VolumeFormat::Other,
                },
                source: EvidenceSource::SysInfoExtended,
            });
        for key in ["BuildVersion", "VisibleBuildID"] {
            if let Some(value) = dictionary.get(key).and_then(Value::as_string) {
                self.firmware_versions.push(Sourced {
                    value: value.to_owned(),
                    source: EvidenceSource::SysInfoExtended,
                });
            }
        }
        if let Some(value) = dictionary.get("FireWireGUID") {
            if let Some(text) = value.as_string() {
                self.firewire_guid = parse_guid(text).or(self.firewire_guid);
            } else if let Some(number) = value.as_unsigned_integer() {
                self.firewire_guid = Some(FireWireGuid(number.to_be_bytes()));
            }
        }
        if dictionary.contains_key("SerialNumber") {
            self.product_serial_present = true;
        }
        Ok(())
    }

    #[must_use]
    pub fn model_family(&self) -> Option<&Sourced<String>> {
        self.model_family.as_ref()
    }

    #[must_use]
    pub fn generation(&self) -> Option<&Sourced<String>> {
        self.generation.as_ref()
    }

    #[must_use]
    pub fn family_id_value(&self) -> Option<&Sourced<u16>> {
        self.family_id.as_ref()
    }

    #[must_use]
    pub fn model_number(&self) -> Option<&Sourced<String>> {
        self.model_number.as_ref()
    }

    #[must_use]
    pub fn usb_product_id(&self) -> Option<&Sourced<u16>> {
        self.usb_product_id.as_ref()
    }

    #[must_use]
    pub fn sqlite_db(&self) -> Option<&Sourced<bool>> {
        self.sqlite_db.as_ref()
    }

    #[must_use]
    pub fn sparse_artwork(&self) -> Option<&Sourced<bool>> {
        self.sparse_artwork.as_ref()
    }

    #[must_use]
    pub fn volume_format(&self) -> Option<&Sourced<VolumeFormat>> {
        self.volume_format.as_ref()
    }

    #[must_use]
    pub fn max_tracks(&self) -> Option<&Sourced<u32>> {
        self.max_tracks.as_ref()
    }

    #[must_use]
    pub fn max_file_size_gib(&self) -> Option<&Sourced<u32>> {
        self.max_file_size_gib.as_ref()
    }

    #[must_use]
    pub fn firmware_versions(&self) -> &[Sourced<String>] {
        &self.firmware_versions
    }

    #[must_use]
    pub fn has_firewire_guid(&self) -> bool {
        self.firewire_guid.is_some()
    }

    #[must_use]
    pub fn has_product_serial(&self) -> bool {
        self.product_serial_present
    }

    pub(crate) fn firewire_guid(&self) -> Option<[u8; 8]> {
        self.firewire_guid.map(|guid| guid.0)
    }
}

impl fmt::Debug for IdentityEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityEvidence")
            .field("model_family", &self.model_family)
            .field("generation", &self.generation)
            .field("model_number", &self.model_number)
            .field("family_id", &self.family_id)
            .field("usb_product_id", &self.usb_product_id)
            .field("sqlite_db", &self.sqlite_db)
            .field("sparse_artwork", &self.sparse_artwork)
            .field("volume_format", &self.volume_format)
            .field("max_tracks", &self.max_tracks)
            .field("max_file_size_gib", &self.max_file_size_gib)
            .field("firmware_versions", &self.firmware_versions)
            .field("firewire_guid", &self.firewire_guid)
            .field("product_serial_present", &self.product_serial_present)
            .finish()
    }
}

fn read_optional(
    mount: &MountRoot,
    relative: &str,
    limit: u64,
    format: &'static str,
) -> Result<Option<Vec<u8>>> {
    let relative = IpodPath::new(relative)?;
    if !mount.contains(&relative)? {
        return Ok(None);
    }
    let path = mount.resolve_existing(&relative)?;
    read_limited(&path, limit, format).map(Some)
}

fn set_string(target: &mut Option<Sourced<String>>, value: &str, source: EvidenceSource) {
    if !value.is_empty() {
        *target = Some(Sourced {
            value: value.to_owned(),
            source,
        });
    }
}

fn parse_u16(value: &str) -> Option<u16> {
    let value = value.trim();
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or_else(
            || value.parse().ok(),
            |hex| u16::from_str_radix(hex, 16).ok(),
        )
}

fn parse_guid(value: &str) -> Option<FireWireGuid> {
    let hex = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or(value.trim());
    if hex.len() != 16 {
        return None;
    }
    let mut bytes = [0_u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(FireWireGuid(bytes))
}

fn unsigned(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_unsigned_integer)
}
