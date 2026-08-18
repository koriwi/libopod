use sha1::{Digest, Sha1};

use crate::{crypto::hashab, Error, Result};

const BLOCK_BYTES: usize = 1_024;
const SIGNATURE_BYTES: usize = 57;
const DIGEST_BYTES: usize = 20;
const PREFIX_BYTES: usize = SIGNATURE_BYTES + DIGEST_BYTES;

/// Structural and verification results for `Locations.itdb.cbk`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CbkInfo {
    pub bytes: u64,
    pub block_size: u32,
    pub block_count: u32,
    pub checksum_scheme: u16,
    pub hashab_signature_matches: Option<bool>,
    pub master_digest_matches: bool,
    pub block_digests_match: bool,
}

impl CbkInfo {
    /// Returns true only if both levels of stored SHA-1 digests match.
    #[must_use]
    pub fn digests_match(&self) -> bool {
        self.master_digest_matches && self.block_digests_match
    }
}

pub(crate) fn build_hashab_cbk(locations: &[u8], firewire_guid: [u8; 8]) -> Vec<u8> {
    let (block_digests, master) = location_digests(locations);
    let signature = hashab::calculate(&master, &firewire_guid, &hashab::CBK_RANDOM);
    let mut cbk = Vec::with_capacity(PREFIX_BYTES + block_digests.len());
    cbk.extend_from_slice(&signature);
    cbk.extend_from_slice(&master);
    cbk.extend_from_slice(&block_digests);
    cbk
}

pub(crate) fn verify_cbk(
    locations: &[u8],
    cbk: &[u8],
    firewire_guid: Option<&[u8; 8]>,
) -> Result<CbkInfo> {
    let block_count = locations.len().div_ceil(BLOCK_BYTES);
    let expected = PREFIX_BYTES
        .checked_add(
            block_count
                .checked_mul(DIGEST_BYTES)
                .ok_or_else(|| malformed_error("block digest byte count overflowed"))?,
        )
        .ok_or_else(|| malformed_error("CBK length overflowed"))?;
    if cbk.len() != expected {
        return Err(Error::Malformed {
            format: "Locations.itdb.cbk",
            offset: 0,
            reason: format!(
                "expected {expected} bytes for {block_count} blocks, found {}",
                cbk.len()
            ),
        });
    }

    let (concatenated, master) = location_digests(locations);

    let master_digest_matches = cbk[SIGNATURE_BYTES..PREFIX_BYTES] == master;
    let block_digests_match = cbk[PREFIX_BYTES..] == concatenated;
    let hashab_signature_matches = firewire_guid.map(|guid| {
        build_hashab_cbk(locations, *guid)[..SIGNATURE_BYTES] == cbk[..SIGNATURE_BYTES]
    });

    Ok(CbkInfo {
        bytes: u64::try_from(cbk.len()).unwrap_or(u64::MAX),
        block_size: u32::try_from(BLOCK_BYTES).unwrap_or(u32::MAX),
        block_count: u32::try_from(block_count).unwrap_or(u32::MAX),
        checksum_scheme: u16::from_le_bytes([cbk[0], cbk[1]]),
        hashab_signature_matches,
        master_digest_matches,
        block_digests_match,
    })
}

fn location_digests(locations: &[u8]) -> (Vec<u8>, [u8; DIGEST_BYTES]) {
    let block_count = locations.len().div_ceil(BLOCK_BYTES);
    let mut concatenated = Vec::with_capacity(block_count * DIGEST_BYTES);
    for block in locations.chunks(BLOCK_BYTES) {
        concatenated.extend_from_slice(&Sha1::digest(block));
    }
    let master = Sha1::digest(&concatenated).into();
    (concatenated, master)
}

fn malformed_error(reason: &str) -> Error {
    Error::Malformed {
        format: "Locations.itdb.cbk",
        offset: 0,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use sha1::{Digest, Sha1};

    use super::{
        build_hashab_cbk, verify_cbk, BLOCK_BYTES, DIGEST_BYTES, PREFIX_BYTES, SIGNATURE_BYTES,
    };

    #[test]
    fn verifies_a_partial_final_block() {
        let locations = vec![0x5a; BLOCK_BYTES + 17];
        let first = Sha1::digest(&locations[..BLOCK_BYTES]);
        let second = Sha1::digest(&locations[BLOCK_BYTES..]);
        let mut all = Vec::new();
        all.extend_from_slice(&first);
        all.extend_from_slice(&second);
        let master = Sha1::digest(&all);

        let mut cbk = vec![0_u8; PREFIX_BYTES + 2 * DIGEST_BYTES];
        cbk[0] = 3;
        cbk[SIGNATURE_BYTES..PREFIX_BYTES].copy_from_slice(&master);
        cbk[PREFIX_BYTES..].copy_from_slice(&all);

        let info = verify_cbk(&locations, &cbk, None).unwrap();
        assert_eq!(info.block_count, 2);
        assert!(info.digests_match());
    }

    #[test]
    fn builds_a_verifiable_hashab_cbk() {
        let locations = vec![0xa5; BLOCK_BYTES * 2 + 7];
        let guid = [0x42; 8];
        let cbk = build_hashab_cbk(&locations, guid);
        let info = verify_cbk(&locations, &cbk, Some(&guid)).unwrap();
        assert!(info.digests_match());
        assert_eq!(info.hashab_signature_matches, Some(true));
    }
}
