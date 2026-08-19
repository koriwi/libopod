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

    /// Reports whether the known profile has the signing evidence needed for a
    /// future write. This does not imply that write support is implemented.
    #[must_use]
    pub fn has_required_signing_identity(&self, evidence: &IdentityEvidence) -> bool {
        match self.capabilities.checksum {
            ChecksumKind::HashAb => evidence.firewire_guid().is_some(),
            ChecksumKind::Hash72 => false,
            ChecksumKind::None | ChecksumKind::Hash58 => true,
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

/// Classic Nano 1–4 profile: uncompressed binary `iTunesDB`, no `SQLite`, and
/// artwork preserved; the Nano 3G/4G also write the classic cover formats.
///
/// Covers the signing matrix entry NONE for Nano 1–2G and HASH58 for
/// Nano 3–4G. `cdb_version` matches the device's `mhbd` version field
/// (Nano 1–2G 0x13, Nano 3–4G 0x30).
fn classic_nano(
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

/// The Nano 3G/4G cover formats, measured from a real Nano 3G: 55x55 with a
/// 56-pixel stride (6160-byte slots), two 128x128 (32768), one 320x320
/// (204800).
fn classic_cover_formats() -> Vec<ArtworkFormatProfile> {
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

fn nano_1g() -> DeviceProfile {
    classic_nano(
        "nano-1g",
        "iPod Nano (1st generation)",
        ChecksumKind::None,
        0x13,
        14,
        Vec::new(),
    )
}

fn nano_2g() -> DeviceProfile {
    classic_nano(
        "nano-2g",
        "iPod Nano (2nd generation)",
        ChecksumKind::None,
        0x13,
        14,
        Vec::new(),
    )
}

fn nano_3g() -> DeviceProfile {
    classic_nano(
        "nano-3g",
        "iPod Nano (3rd generation)",
        ChecksumKind::Hash58,
        0x30,
        20,
        classic_cover_formats(),
    )
}

fn nano_4g() -> DeviceProfile {
    classic_nano(
        "nano-4g",
        "iPod Nano (4th generation)",
        ChecksumKind::Hash58,
        0x30,
        20,
        classic_cover_formats(),
    )
}
