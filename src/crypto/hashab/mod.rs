mod cipher;
mod edonr;
mod expand;
mod hash;
mod reduce;
mod tables;
mod word;

use sha1::{Digest, Sha1};

use crate::{Error, Result};

/// Number of bytes in a HASHAB signature.
pub const SIGNATURE_BYTES: usize = 57;
/// Number of caller-provided random bytes embedded into HASHAB.
pub const RANDOM_BYTES: usize = 23;

/// Random field used by iTunes-compatible CBK writers.
pub const CBK_RANDOM: [u8; RANDOM_BYTES] = *b"ABCDEFGHIJKLMNOPQRSTUVW";

const PERMUTATION_32: [usize; 32] = [
    0x06, 0x18, 0x11, 0x07, 0x13, 0x0d, 0x0e, 0x09, 0x15, 0x1d, 0x02, 0x1f, 0x01, 0x04, 0x1c, 0x1a,
    0x10, 0x14, 0x0b, 0x1e, 0x03, 0x0a, 0x1b, 0x19, 0x05, 0x0f, 0x16, 0x00, 0x12, 0x17, 0x08, 0x0c,
];

const PERMUTATION_56: [usize; 56] = [
    0x15, 0x1c, 0x06, 0x0c, 0x07, 0x1a, 0x05, 0x13, 0x08, 0x19, 0x03, 0x01, 0x2d, 0x1e, 0x10, 0x31,
    0x1d, 0x14, 0x28, 0x27, 0x35, 0x00, 0x2f, 0x1b, 0x26, 0x0b, 0x0e, 0x02, 0x23, 0x17, 0x24, 0x22,
    0x12, 0x1f, 0x20, 0x04, 0x29, 0x25, 0x21, 0x09, 0x18, 0x0d, 0x32, 0x0f, 0x11, 0x2e, 0x33, 0x2b,
    0x30, 0x2a, 0x36, 0x0a, 0x2c, 0x34, 0x16, 0x37,
];

/// Calculates a Nano 6G/7G HASHAB signature.
///
/// `database_sha1` is the SHA-1 digest of the database bytes after the
/// signature field has been cleared. `firewire_guid` is the raw eight-byte
/// signing identity, not the product serial. `random` becomes part of the
/// 57-byte signature and must come from a cryptographically secure source for
/// newly generated CDB signatures. CBK uses [`CBK_RANDOM`] for compatibility.
#[must_use]
pub fn calculate(
    database_sha1: &[u8; 20],
    firewire_guid: &[u8; 8],
    random: &[u8; RANDOM_BYTES],
) -> [u8; SIGNATURE_BYTES] {
    let mut stage1_input = [0_u8; 80];
    for (target, source) in stage1_input[..8].iter_mut().zip(firewire_guid) {
        *target = source.wrapping_mul(0xed);
    }
    for (target, source) in stage1_input[8..28].iter_mut().zip(database_sha1) {
        *target = source.wrapping_mul(0xed);
    }
    for (target, source) in stage1_input[28..51].iter_mut().zip(random) {
        *target = source.wrapping_mul(0xed);
    }
    stage1_input[51..].fill(0xc1);

    let stage1_output = cipher::stage1(&stage1_input);
    let mut middle_ciphertext = [0_u8; 24];
    middle_ciphertext.copy_from_slice(&stage1_output[20..44]);
    let mut leading_ciphertext = [0_u8; 24];
    leading_ciphertext.copy_from_slice(&stage1_output[..24]);
    let expanded = expand::expand(&middle_ciphertext, &leading_ciphertext);
    let compression = reduce::reduce(&expanded);

    let mut stage2_input = [0_u8; 32];
    for (target, source) in stage2_input.iter_mut().zip(PERMUTATION_32) {
        *target = stage1_output[44 + source];
    }
    let stage2_output = cipher::stage2(&stage2_input, &compression);

    let mut signature = [0_u8; SIGNATURE_BYTES];
    signature[0] = 3;
    for (target, source) in signature[2..].iter_mut().zip(PERMUTATION_56) {
        *target = if source < RANDOM_BYTES {
            random[source]
        } else {
            stage2_output[source - RANDOM_BYTES].wrapping_mul(0x2d)
        };
    }
    signature
}

/// Writes a HASHAB signature into an uncompressed or compressed database.
///
/// The caller must finish serialization and CDB compression first. This sets
/// MHBD scheme 3 before hashing, clears all checksum regions for the digest,
/// and writes the resulting 57-byte signature at offset `0xAB`.
///
/// # Errors
///
/// Returns [`Error::Malformed`] if `database` is too short or does not begin
/// with an `mhbd` header.
pub fn sign_database(
    database: &mut [u8],
    firewire_guid: &[u8; 8],
    random: &[u8; RANDOM_BYTES],
) -> Result<()> {
    if database.len() < 0xe4 {
        return Err(Error::Malformed {
            format: "HASHAB database",
            offset: u64::try_from(database.len()).unwrap_or(u64::MAX),
            reason: "database is shorter than the HASHAB header region".to_owned(),
        });
    }
    if database.get(..4) != Some(b"mhbd") {
        return Err(Error::Malformed {
            format: "HASHAB database",
            offset: 0,
            reason: "expected mhbd magic".to_owned(),
        });
    }
    database[0x30..0x32].copy_from_slice(&3_u16.to_le_bytes());
    let digest = database_digest(database, None);
    let signature = calculate(&digest, firewire_guid, random);
    database[0xab..0xe4].copy_from_slice(&signature);
    Ok(())
}

pub(crate) fn verify_digest_signature(
    digest: &[u8; 20],
    firewire_guid: [u8; 8],
    signature: &[u8; SIGNATURE_BYTES],
) -> bool {
    let mut random = [0_u8; RANDOM_BYTES];
    let mut found = [false; RANDOM_BYTES];
    for (value, source) in signature[2..].iter().zip(PERMUTATION_56) {
        if source < RANDOM_BYTES {
            random[source] = *value;
            found[source] = true;
        }
    }
    found.iter().all(|present| *present) && calculate(digest, &firewire_guid, &random) == *signature
}

/// Result of checking an on-disk HASHAB database signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DatabaseSignatureStatus {
    /// The signature matches the exact on-disk bytes after clearing hash fields.
    Valid,
    /// A historical writer signed with scheme 4, then changed the field to 3.
    ///
    /// This records a reproducible writer bug and must not be emitted by new
    /// libopod writes.
    LegacyScheme4Then3,
    /// Neither the exact nor recognized historical form verifies.
    Invalid,
}

pub(crate) fn verify_database_signature(
    data: &[u8],
    firewire_guid: [u8; 8],
) -> Option<DatabaseSignatureStatus> {
    if data.len() < 0xe4 || data.get(..4) != Some(b"mhbd") {
        return None;
    }
    let signature: &[u8; SIGNATURE_BYTES] = data[0xab..0xe4].try_into().ok()?;
    let digest = database_digest(data, None);
    if verify_digest_signature(&digest, firewire_guid, signature) {
        return Some(DatabaseSignatureStatus::Valid);
    }

    if data[0x30..0x32] == 3_u16.to_le_bytes() {
        let legacy_digest = database_digest(data, Some(4));
        if verify_digest_signature(&legacy_digest, firewire_guid, signature) {
            return Some(DatabaseSignatureStatus::LegacyScheme4Then3);
        }
    }
    Some(DatabaseSignatureStatus::Invalid)
}

fn database_digest(data: &[u8], scheme_override: Option<u16>) -> [u8; 20] {
    let zeros = [0_u8; SIGNATURE_BYTES];
    let mut hasher = Sha1::new();
    hasher.update(&data[..0x18]);
    hasher.update(&zeros[..8]);
    hasher.update(&data[0x20..0x30]);
    if let Some(scheme) = scheme_override {
        hasher.update(scheme.to_le_bytes());
    } else {
        hasher.update(&data[0x30..0x32]);
    }
    hasher.update(&zeros[..20]);
    hasher.update(&data[0x46..0x58]);
    hasher.update(&zeros[..20]);
    hasher.update(&data[0x6c..0x72]);
    hasher.update(&zeros[..46]);
    hasher.update(&data[0xa0..0xab]);
    hasher.update(&zeros[..SIGNATURE_BYTES]);
    hasher.update(&data[0xe4..]);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::{sign_database, verify_database_signature, DatabaseSignatureStatus, CBK_RANDOM};

    #[test]
    fn signs_the_final_database_bytes_with_scheme_three() {
        let mut database = vec![0x5a; 512];
        database[..4].copy_from_slice(b"mhbd");
        let guid = [0x42; 8];
        sign_database(&mut database, &guid, &CBK_RANDOM).unwrap();

        assert_eq!(&database[0x30..0x32], &3_u16.to_le_bytes());
        assert_eq!(
            verify_database_signature(&database, guid),
            Some(DatabaseSignatureStatus::Valid)
        );
        database[300] ^= 1;
        assert_eq!(
            verify_database_signature(&database, guid),
            Some(DatabaseSignatureStatus::Invalid)
        );
    }
}
