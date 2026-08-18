use std::io::{Read, Take};

use flate2::read::ZlibDecoder;

use crate::{crypto::hashab, Error, Result};

const MIN_MHBD_HEADER: usize = 0xad;
const MAX_DATASETS: u32 = 64;
const MAX_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

/// Structural information about one top-level `mhsd` CDB dataset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CdbDatasetInfo {
    pub kind: u32,
    pub header_length: u32,
    pub total_length: u32,
}

/// Redacted structural information from an `iTunesCDB` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CdbInfo {
    pub physical_bytes: u64,
    pub header_length: u32,
    pub version: u32,
    pub declared_children: u32,
    pub checksum_scheme: u16,
    pub checksum_indicator: u16,
    pub compression_flag: u16,
    pub hashab_version_prefix: [u8; 2],
    pub hashab_signature_status: Option<hashab::DatabaseSignatureStatus>,
    pub uncompressed_payload_bytes: u64,
    pub datasets: Vec<CdbDatasetInfo>,
}

pub(crate) fn inspect_cdb(bytes: &[u8], firewire_guid: Option<&[u8; 8]>) -> Result<CdbInfo> {
    if bytes.len() < MIN_MHBD_HEADER {
        return malformed(0, "file is shorter than the minimum mhbd header");
    }
    if &bytes[..4] != b"mhbd" {
        return malformed(0, "expected mhbd magic");
    }

    let header_length = le_u32(bytes, 4)?;
    let header_len = usize::try_from(header_length)
        .map_err(|_| malformed_error(4, "header length does not fit this host"))?;
    if header_len < MIN_MHBD_HEADER || header_len > bytes.len() {
        return malformed(4, "mhbd header length is out of bounds");
    }

    let declared_total = u64::from(le_u32(bytes, 8)?);
    let physical_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if declared_total != physical_bytes {
        return malformed(8, "mhbd total length does not match the file size");
    }

    let declared_children = le_u32(bytes, 0x14)?;
    if declared_children > MAX_DATASETS {
        return malformed(0x14, "dataset count exceeds the parser limit");
    }
    let compression_flag = le_u16(bytes, 0xa8)?;
    if compression_flag != 1 {
        return Err(Error::Unsupported {
            feature: "iTunesCDB compression",
            reason: format!("expected compression flag 1, found {compression_flag}"),
        });
    }

    let payload = decode_payload(bytes, header_len)?;

    let mut datasets = Vec::with_capacity(usize::try_from(declared_children).unwrap_or(0));
    let mut offset = 0_usize;
    while offset < payload.len() {
        if datasets.len() >= usize::try_from(declared_children).unwrap_or(usize::MAX) {
            return malformed_payload(offset, "payload has more datasets than declared");
        }
        let remaining = payload.len() - offset;
        if remaining < 16 {
            return malformed_payload(offset, "truncated mhsd header");
        }
        if &payload[offset..offset + 4] != b"mhsd" {
            return malformed_payload(offset, "expected top-level mhsd magic");
        }
        let header = le_u32(&payload, offset + 4)?;
        let total = le_u32(&payload, offset + 8)?;
        let kind = le_u32(&payload, offset + 12)?;
        if header < 16 || total < header {
            return malformed_payload(offset + 4, "invalid mhsd lengths");
        }
        let total_usize = usize::try_from(total)
            .map_err(|_| malformed_error(payload_offset(offset + 8), "mhsd is too large"))?;
        let end = offset
            .checked_add(total_usize)
            .ok_or_else(|| malformed_error(payload_offset(offset + 8), "mhsd length overflow"))?;
        if end > payload.len() {
            return malformed_payload(offset + 8, "mhsd extends beyond the payload");
        }
        datasets.push(CdbDatasetInfo {
            kind,
            header_length: header,
            total_length: total,
        });
        offset = end;
    }

    if datasets.len() != usize::try_from(declared_children).unwrap_or(usize::MAX) {
        return malformed(
            0x14,
            "declared dataset count does not match the decompressed payload",
        );
    }

    Ok(CdbInfo {
        physical_bytes,
        header_length,
        version: le_u32(bytes, 0x10)?,
        declared_children,
        checksum_scheme: le_u16(bytes, 0x30)?,
        checksum_indicator: le_u16(bytes, 0x70)?,
        compression_flag,
        hashab_version_prefix: [bytes[0xab], bytes[0xac]],
        hashab_signature_status: firewire_guid
            .and_then(|guid| hashab::verify_database_signature(bytes, *guid)),
        uncompressed_payload_bytes: u64::try_from(payload.len()).unwrap_or(u64::MAX),
        datasets,
    })
}

pub(super) fn decode_payload(bytes: &[u8], header_len: usize) -> Result<Vec<u8>> {
    let compressed = bytes.get(header_len..).ok_or_else(|| {
        malformed_error(
            u64::try_from(header_len).unwrap_or(u64::MAX),
            "payload starts beyond the file",
        )
    })?;
    let decoder = ZlibDecoder::new(compressed);
    let mut limited: Take<_> = decoder.take(MAX_UNCOMPRESSED_BYTES + 1);
    let mut payload = Vec::new();
    limited
        .read_to_end(&mut payload)
        .map_err(|source| Error::Malformed {
            format: "iTunesCDB",
            offset: u64::try_from(header_len).unwrap_or(u64::MAX),
            reason: format!("invalid zlib payload: {source}"),
        })?;
    if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_UNCOMPRESSED_BYTES {
        return malformed(
            u64::try_from(header_len).unwrap_or(u64::MAX),
            "uncompressed payload exceeds 512 MiB",
        );
    }
    Ok(payload)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes.get(offset..offset + 2).ok_or_else(|| {
        malformed_error(u64::try_from(offset).unwrap_or(u64::MAX), "truncated u16")
    })?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        malformed_error(u64::try_from(offset).unwrap_or(u64::MAX), "truncated u32")
    })?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn payload_offset(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

fn malformed<T>(offset: u64, reason: &str) -> Result<T> {
    Err(malformed_error(offset, reason))
}

fn malformed_payload<T>(offset: usize, reason: &str) -> Result<T> {
    malformed(payload_offset(offset), reason)
}

fn malformed_error(offset: u64, reason: &str) -> Error {
    Error::Malformed {
        format: "iTunesCDB",
        offset,
        reason: reason.to_owned(),
    }
}
