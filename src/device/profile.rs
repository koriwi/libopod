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

pub(crate) fn resolve(evidence: &IdentityEvidence) -> Result<Option<DeviceProfile>> {
    let family = evidence
        .model_family()
        .map(|value| value.value.to_ascii_lowercase());
    let generation = evidence
        .generation()
        .map(|value| value.value.to_ascii_lowercase());
    let product_id = evidence.usb_product_id().map(|value| value.value);

    let model_says_nano7 = family
        .as_deref()
        .is_some_and(|value| value.contains("nano"))
        && generation
            .as_deref()
            .is_some_and(|value| value.contains("7th") || value.starts_with('7'));
    let pid_says_nano7 = product_id == Some(0x1267);

    if pid_says_nano7
        && family
            .as_deref()
            .is_some_and(|value| !value.contains("nano"))
    {
        return Err(Error::ConflictingEvidence {
            reason:
                "USB product ID 0x1267 indicates Nano 7G but ModelFamily does not indicate a Nano"
                    .to_owned(),
        });
    }
    if model_says_nano7 && product_id.is_some_and(|value| value != 0x1267) {
        return Err(Error::ConflictingEvidence {
            reason: "model fields indicate Nano 7G but SysInfo has an unexpected USB product ID"
                .to_owned(),
        });
    }

    if model_says_nano7 || pid_says_nano7 {
        return Ok(Some(nano_7g()));
    }
    Ok(None)
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
