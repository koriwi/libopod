use crate::{Error, Result};

use super::IdentityEvidence;

/// Authoritative database family for a device profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BackendKind {
    Binary,
    SqliteWithBinaryCompanion,
}

/// Database signature algorithm required by a device profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChecksumKind {
    None,
    Hash58,
    Hash72,
    HashAb,
}

/// One artwork frame format required by a device profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArtworkFormatProfile {
    pub format_id: u32,
    pub slot_bytes: u32,
}

/// Write-affecting device capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceCapabilities {
    pub backend: BackendKind,
    pub checksum: ChecksumKind,
    pub compressed_cdb: bool,
    pub cdb_version: u32,
    pub music_directories: u8,
    pub sparse_artwork: bool,
    pub artwork_formats: Vec<ArtworkFormatProfile>,
}

impl DeviceCapabilities {
    /// Whether libopod has writable cover-art formats for this profile (an
    /// `ArtworkDB` plus fixed-slot `.ithmb` files). An empty list means the
    /// writer must leave artwork storage untouched; it does not imply that the
    /// physical device is incapable of displaying artwork.
    #[must_use]
    pub fn supports_artwork(&self) -> bool {
        !self.artwork_formats.is_empty()
    }
}

/// A resolved model profile. Profiles are conservative and hardware-gated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    key: &'static str,
    display_name: &'static str,
    capabilities: DeviceCapabilities,
}

impl DeviceProfile {
    #[must_use]
    pub const fn key(&self) -> &'static str {
        self.key
    }

    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        self.display_name
    }

    #[must_use]
    pub const fn capabilities(&self) -> &DeviceCapabilities {
        &self.capabilities
    }

    /// Whether the writer supports podcast tracks and their special container.
    #[must_use]
    pub fn supports_podcasts(&self) -> bool {
        matches!(
            self.key,
            "nano-7g" | "classic" | "classic-6g" | "classic-6.5g" | "classic-7g"
        )
    }

    /// Reports whether the known profile has the signing evidence needed for a
    /// future write. This does not imply that write support is implemented.
    #[must_use]
    pub fn has_required_signing_identity(&self, evidence: &IdentityEvidence) -> bool {
        match self.capabilities.checksum {
            ChecksumKind::HashAb | ChecksumKind::Hash58 => evidence.firewire_guid().is_some(),
            ChecksumKind::Hash72 => false,
            ChecksumKind::None => true,
        }
    }
}

/// USB product IDs for the iPod Nano generations (fallback identity when
/// `SysInfo` model fields are missing or ambiguous).
/// Normal-mode USB product IDs for the iPod Nano generations, per iOpenPod's
/// `USB_PID_TO_MODEL` (verified on real devices where marked). Nano 1G has no
/// clean normal-mode PID; it is identified via `SysInfo`/`FamilyID`/serial.
const NANO_PRODUCT_IDS: [(u16, u8); 6] = [
    (0x1260, 2),
    (0x1262, 3), // verified: the operator's Nano 3G
    (0x1263, 4),
    (0x1265, 5),
    (0x1266, 6),
    (0x1267, 7), // verified: the Nano 7G backup
];

/// `SysInfoExtended` `FamilyID` values for the Nano generations.
///
/// Verified on real devices: 12 = Nano 3G, 18 = Nano 7G. The remaining
/// entries are provisional until confirmed against hardware.
const NANO_FAMILY_IDS: [(u16, u8); 2] = [(12, 3), (14, 4)];

/// Parses the `SysInfo` generation field into a Nano generation number.
fn nano_generation(evidence: &IdentityEvidence) -> Option<u8> {
    let generation = evidence.generation()?.value.to_ascii_lowercase();
    if generation.contains("7th") || generation.starts_with('7') {
        Some(7)
    } else if generation.contains("6th") || generation.starts_with('6') {
        Some(6)
    } else if generation.contains("5th") || generation.starts_with('5') {
        Some(5)
    } else if generation.contains("4th") || generation.starts_with('4') {
        Some(4)
    } else if generation.contains("3rd") || generation.starts_with('3') {
        Some(3)
    } else if generation.contains("2nd") || generation.starts_with('2') {
        Some(2)
    } else if generation.contains("1st") || generation.starts_with('1') {
        Some(1)
    } else {
        None
    }
}

pub(crate) fn resolve(evidence: &IdentityEvidence) -> Result<Option<DeviceProfile>> {
    if let Some(profile) = resolve_classic(evidence)? {
        return Ok(Some(profile));
    }
    let family = evidence
        .model_family()
        .map(|value| value.value.to_ascii_lowercase());
    let product_id = evidence.usb_product_id().map(|value| value.value);

    let is_nano = family
        .as_deref()
        .is_some_and(|value| value.contains("nano"));
    let generation = nano_generation(evidence);
    let family_id_generation = evidence.family_id_value().and_then(|value| {
        NANO_FAMILY_IDS
            .iter()
            .find_map(|(candidate, generation)| (*candidate == value.value).then_some(*generation))
    });
    let pid_generation = product_id.and_then(|pid| {
        NANO_PRODUCT_IDS
            .iter()
            .find_map(|(candidate, generation)| (*candidate == pid).then_some(*generation))
    });

    if pid_generation.is_some()
        && family
            .as_deref()
            .is_some_and(|value| !value.contains("nano"))
    {
        return Err(Error::ConflictingEvidence {
            reason: format!(
                "USB product ID 0x{:04x} indicates a Nano but ModelFamily does not",
                product_id.unwrap_or(0)
            ),
        });
    }
    if family
        .as_deref()
        .is_some_and(|value| value.contains("nano"))
        && pid_generation.is_some_and(|pid_generation| {
            generation.is_some_and(|generation| generation != pid_generation)
        })
    {
        return Err(Error::ConflictingEvidence {
            reason: "SysInfo generation and USB product ID disagree on the Nano generation"
                .to_owned(),
        });
    }

    let generation = generation.or(family_id_generation).or(pid_generation);
    if !is_nano && pid_generation.is_none() && family_id_generation.is_none() {
        return Ok(None);
    }
    let profile = match generation {
        Some(7) => nano_7g(),
        // HASH72 Nano 5G/6G profiles are not implemented yet.
        Some(4) => nano_4g(),
        Some(3) => nano_3g(),
        Some(2) => nano_2g(),
        Some(1) => nano_1g(),
        Some(_) | None => return Ok(None),
    };
    Ok(Some(profile))
}

/// Model numbers and the shared normal-mode PID follow iOpenPod's device
/// table. Apple calls the 2009 160 GB Classic "revision B"; the community
/// calls it 7G. A1238 and USB PID 0x1261 alone do NOT identify the revision.
fn resolve_classic(evidence: &IdentityEvidence) -> Result<Option<DeviceProfile>> {
    let family = evidence
        .model_family()
        .map(|v| v.value.to_ascii_lowercase());
    let named_classic = family.as_deref().is_some_and(|v| v.contains("classic"));
    let pid = evidence.usb_product_id().map(|v| v.value);
    let model_revision = evidence.model_number().and_then(|v| {
        let model = v.value.trim().to_ascii_uppercase();
        let model = model.strip_prefix('M').unwrap_or(&model);
        match model.get(..4)? {
            "B029" | "B147" | "B145" | "B150" => Some("classic-6g"),
            "B562" | "B565" => Some("classic-6.5g"),
            "C293" | "C297" => Some("classic-7g"),
            _ => None,
        }
    });
    if !named_classic && pid != Some(0x1261) && model_revision.is_none() {
        return Ok(None);
    }
    if family
        .as_deref()
        .is_some_and(|v| !v.contains("classic") && v != "ipod")
        || pid.is_some_and(|pid| NANO_PRODUCT_IDS.iter().any(|(id, _)| *id == pid))
        || evidence
            .family_id_value()
            .is_some_and(|v| NANO_FAMILY_IDS.iter().any(|(id, _)| *id == v.value))
        || evidence.sqlite_db().is_some_and(|v| v.value)
    {
        return Err(Error::ConflictingEvidence {
            reason: "Classic identity conflicts with device family or database evidence".to_owned(),
        });
    }
    let named_revision = if named_classic {
        evidence.generation().and_then(|v| {
            let generation = v.value.to_ascii_lowercase();
            if generation.contains("rev b")
                || generation.contains("revision b")
                || generation.starts_with('7')
            {
                Some("classic-7g")
            } else if generation.starts_with("6.5") {
                Some("classic-6.5g")
            } else if generation.starts_with('6') {
                Some("classic-6g")
            } else {
                None
            }
        })
    } else {
        None
    };
    if model_revision
        .zip(named_revision)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(Error::ConflictingEvidence {
            reason: "Classic model number and generation disagree".to_owned(),
        });
    }
    let key = model_revision.or(named_revision).unwrap_or("classic");
    let display_name = match key {
        "classic-6g" => "iPod Classic (6th generation)",
        "classic-6.5g" => "iPod Classic (120 GB, revision A)",
        "classic-7g" => "iPod Classic (160 GB, revision B / 7th generation)",
        _ => "iPod Classic (revision unknown)",
    };
    Ok(Some(binary_profile(
        key,
        display_name,
        ChecksumKind::Hash58,
        0x30,
        50,
        classic_cover_formats(),
    )))
}

/// Classic F1061 is 56x56, unlike the measured Nano 3G 55x55 format.
/// The remaining cover formats have the same dimensions on both devices.
fn classic_cover_formats() -> Vec<ArtworkFormatProfile> {
    let mut formats = nano_3g_cover_formats();
    formats[0].slot_bytes = 6_272;
    formats
}

fn nano_7g() -> DeviceProfile {
    DeviceProfile {
        key: "nano-7g",
        display_name: "iPod Nano (7th generation)",
        capabilities: DeviceCapabilities {
            backend: BackendKind::SqliteWithBinaryCompanion,
            checksum: ChecksumKind::HashAb,
            compressed_cdb: true,
            cdb_version: 110,
            music_directories: 20,
            sparse_artwork: true,
            artwork_formats: vec![
                ArtworkFormatProfile {
                    format_id: 1010,
                    slot_bytes: 115_200,
                },
                ArtworkFormatProfile {
                    format_id: 1013,
                    slot_bytes: 5_000,
                },
                ArtworkFormatProfile {
                    format_id: 1015,
                    slot_bytes: 6_728,
                },
                ArtworkFormatProfile {
                    format_id: 1016,
                    slot_bytes: 6_612,
                },
            ],
        },
    }
}

/// Uncompressed binary `iTunesDB` profile, without `SQLite`.
/// Nano 1G/2G artwork writing remains unimplemented.
///
/// Covers the signing matrix entry NONE for Nano 1–2G and HASH58 for
/// Nano 3–4G. `cdb_version` matches the device's `mhbd` version field
/// (Nano 1–2G 0x13, Nano 3–4G 0x30).
fn binary_profile(
    key: &'static str,
    display_name: &'static str,
    checksum: ChecksumKind,
    cdb_version: u32,
    music_directories: u8,
    artwork_formats: Vec<ArtworkFormatProfile>,
) -> DeviceProfile {
    DeviceProfile {
        key,
        display_name,
        capabilities: DeviceCapabilities {
            backend: BackendKind::Binary,
            checksum,
            compressed_cdb: false,
            cdb_version,
            music_directories,
            sparse_artwork: false,
            artwork_formats,
        },
    }
}

/// Nano 3G cover formats measured from a real device: 55x55 with a 56-pixel
/// stride (6160-byte slots), two 128x128 (32768), and one 320x320 (204800).
fn nano_3g_cover_formats() -> Vec<ArtworkFormatProfile> {
    vec![
        ArtworkFormatProfile {
            format_id: 1061,
            slot_bytes: 6_160,
        },
        ArtworkFormatProfile {
            format_id: 1055,
            slot_bytes: 32_768,
        },
        ArtworkFormatProfile {
            format_id: 1068,
            slot_bytes: 32_768,
        },
        ArtworkFormatProfile {
            format_id: 1060,
            slot_bytes: 204_800,
        },
    ]
}

/// Nano 4G cover formats from Apple's on-device profile (also documented by
/// libgpod): two 128x128, two 240x240, one 50x50, and one 80x80 RGB565 image.
fn nano_4g_cover_formats() -> Vec<ArtworkFormatProfile> {
    vec![
        ArtworkFormatProfile {
            format_id: 1055,
            slot_bytes: 32_768,
        },
        ArtworkFormatProfile {
            format_id: 1068,
            slot_bytes: 32_768,
        },
        ArtworkFormatProfile {
            format_id: 1071,
            slot_bytes: 115_200,
        },
        ArtworkFormatProfile {
            format_id: 1074,
            slot_bytes: 5_000,
        },
        ArtworkFormatProfile {
            format_id: 1078,
            slot_bytes: 12_800,
        },
        ArtworkFormatProfile {
            format_id: 1084,
            slot_bytes: 115_200,
        },
    ]
}

fn nano_1g() -> DeviceProfile {
    binary_profile(
        "nano-1g",
        "iPod Nano (1st generation)",
        ChecksumKind::None,
        0x13,
        14,
        Vec::new(),
    )
}

fn nano_2g() -> DeviceProfile {
    binary_profile(
        "nano-2g",
        "iPod Nano (2nd generation)",
        ChecksumKind::None,
        0x13,
        14,
        Vec::new(),
    )
}

fn nano_3g() -> DeviceProfile {
    binary_profile(
        "nano-3g",
        "iPod Nano (3rd generation)",
        ChecksumKind::Hash58,
        0x30,
        20,
        nano_3g_cover_formats(),
    )
}

fn nano_4g() -> DeviceProfile {
    binary_profile(
        "nano-4g",
        "iPod Nano (4th generation)",
        ChecksumKind::Hash58,
        0x30,
        20,
        nano_4g_cover_formats(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_with_identity(sysinfo: &str, extended: Option<&str>) -> Result<crate::Device> {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("iPod_Control/Device");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("SysInfo"), sysinfo).unwrap();
        if let Some(xml) = extended {
            std::fs::write(root.join("SysInfoExtended"), xml).unwrap();
        }
        crate::Device::open(directory.path())
    }

    #[test]
    fn detects_classic_revision_b_model_numbers_and_names() {
        for identity in [
            "ModelNumStr: MC293LL/A",
            "ModelNumStr: C297",
            "ModelNumStr: mc297zd/a",
            "ModelFamily: iPod Classic\nGeneration: 7th Gen",
            "ModelFamily: Classic\nGeneration: 6th generation revision B",
        ] {
            let device = device_with_identity(identity, None).unwrap();
            let profile = device.profile().unwrap();
            assert_eq!(profile.key(), "classic-7g", "{identity}");
            let caps = profile.capabilities();
            assert_eq!(caps.backend, BackendKind::Binary);
            assert_eq!(caps.checksum, ChecksumKind::Hash58);
            assert_eq!(caps.music_directories, 50);
            assert_eq!(caps.cdb_version, 0x30);
            assert!(!caps.compressed_cdb);
            assert!(caps.supports_artwork());
            assert_eq!(caps.artwork_formats[0].slot_bytes, 6_272);
            assert!(!profile.has_required_signing_identity(device.evidence()));
        }
        let device = device_with_identity("", Some(
            "<plist version=\"1.0\"><dict><key>ModelNumStr</key><string>MC293LL/A</string><key>FireWireGUID</key><string>0123456789abcdef</string></dict></plist>"
        )).unwrap();
        let profile = device.profile().unwrap();
        assert_eq!(profile.key(), "classic-7g");
        assert!(profile.has_required_signing_identity(device.evidence()));
    }

    #[test]
    fn shared_classic_usb_id_does_not_claim_a_revision() {
        for identity in [
            "USBProductID: 0x1261",
            "ModelNumStr: A1238\nUSBProductID: 4705",
            "ModelFamily: iPod Classic",
        ] {
            let device = device_with_identity(identity, None).unwrap();
            assert_eq!(device.profile().unwrap().key(), "classic");
        }
        assert!(device_with_identity("ModelNumStr: A1238", None)
            .unwrap()
            .profile()
            .is_none());
        for (model, key) in [
            ("MB029", "classic-6g"),
            ("MB150", "classic-6g"),
            ("MB562", "classic-6.5g"),
            ("B565", "classic-6.5g"),
        ] {
            let device =
                device_with_identity(&format!("ModelNumStr: {model}\nUSBProductID: 0x1261"), None)
                    .unwrap();
            assert_eq!(device.profile().unwrap().key(), key);
        }
    }

    #[test]
    fn rejects_conflicting_classic_identity() {
        for identity in [
            "ModelFamily: iPod Nano\nUSBProductID: 0x1261",
            "ModelFamily: iPod Classic\nUSBProductID: 0x1267",
            "ModelNumStr: MC293\nUSBProductID: 0x1262",
            "ModelNumStr: MC297\nModelFamily: iPod Classic\nGeneration: 6th Gen",
        ] {
            assert!(
                matches!(
                    device_with_identity(identity, None),
                    Err(Error::ConflictingEvidence { .. })
                ),
                "{identity}"
            );
        }
        assert!(matches!(
            device_with_identity(
                "ModelNumStr: MC293",
                Some("<plist version=\"1.0\"><dict><key>SQLiteDB</key><true/></dict></plist>")
            ),
            Err(Error::ConflictingEvidence { .. })
        ));
        let nano = device_with_identity(
            "ModelFamily: iPod Nano\nGeneration: 7th Gen\nUSBProductID: 0x1267",
            None,
        )
        .unwrap();
        assert_eq!(nano.profile().unwrap().key(), "nano-7g");
    }

    #[test]
    fn podcast_support_excludes_unimplemented_nano_profiles() {
        assert!(nano_7g().supports_podcasts());
        for profile in [nano_1g(), nano_2g(), nano_3g(), nano_4g()] {
            assert!(!profile.supports_podcasts());
        }
    }

    #[test]
    fn artwork_support_matches_the_device_family() {
        // Nano 1G/2G can display artwork, but their writer profiles are not
        // implemented yet. Staging must therefore leave their ArtworkDB and
        // ithmb files untouched. Nano 3G/4G and 7G are writable.
        for profile in [nano_1g(), nano_2g()] {
            assert!(!profile.capabilities().supports_artwork());
        }
        for profile in [nano_3g(), nano_4g(), nano_7g()] {
            assert!(profile.capabilities().supports_artwork());
        }
    }

    #[test]
    fn nano4_uses_its_six_native_cover_formats() {
        let profile = nano_4g();
        let formats = &profile.capabilities().artwork_formats;
        let actual: Vec<_> = formats
            .iter()
            .map(|format| (format.format_id, format.slot_bytes))
            .collect();
        assert_eq!(
            actual,
            vec![
                (1055, 32_768),
                (1068, 32_768),
                (1071, 115_200),
                (1074, 5_000),
                (1078, 12_800),
                (1084, 115_200),
            ]
        );
        assert!(!formats.iter().any(|format| format.format_id == 1061));
    }
}
