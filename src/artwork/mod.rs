use std::{fs, path::Path};

use crate::{error::io_error, Error, Result};

const MIN_MHFD_HEADER: usize = 132;
const MAX_DATASETS: u32 = 32;

/// Structural information about one `ArtworkDB` `mhsd` dataset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkDatasetInfo {
    pub kind: u32,
    pub header_length: u32,
    pub total_length: u32,
    pub list_magic: [u8; 4],
    pub item_count: u32,
}

/// Redacted structural information from `ArtworkDB`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkDatabaseInfo {
    pub bytes: u64,
    pub header_length: u32,
    pub declared_children: u32,
    pub next_image_id: u32,
    pub datasets: Vec<ArtworkDatasetInfo>,
}

/// Slot arithmetic for one profile-specific `.ithmb` file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkFrameInfo {
    pub format_id: u32,
    pub bytes: u64,
    pub slot_bytes: u32,
    pub complete_slots: u64,
    pub remainder_bytes: u32,
}

pub(crate) fn inspect_artwork_db(bytes: &[u8]) -> Result<ArtworkDatabaseInfo> {
    if bytes.len() < MIN_MHFD_HEADER {
        return malformed(0, "file is shorter than the minimum mhfd header");
    }
    if &bytes[..4] != b"mhfd" {
        return malformed(0, "expected mhfd magic");
    }
    let header_length = le_u32(bytes, 4)?;
    let header = usize::try_from(header_length)
        .map_err(|_| malformed_error(4, "mhfd header does not fit this host"))?;
    if header < MIN_MHFD_HEADER || header > bytes.len() {
        return malformed(4, "mhfd header length is out of bounds");
    }
    if u64::from(le_u32(bytes, 8)?) != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return malformed(8, "mhfd total length does not match the file size");
    }
    let declared_children = le_u32(bytes, 20)?;
    if declared_children > MAX_DATASETS {
        return malformed(20, "dataset count exceeds the parser limit");
    }

    let mut datasets = Vec::with_capacity(usize::try_from(declared_children).unwrap_or(0));
    let mut offset = header;
    for _ in 0..declared_children {
        if bytes.get(offset..offset + 16).is_none() {
            return malformed(to_u64(offset), "truncated ArtworkDB mhsd");
        }
        if &bytes[offset..offset + 4] != b"mhsd" {
            return malformed(to_u64(offset), "expected ArtworkDB mhsd magic");
        }
        let dataset_header = le_u32(bytes, offset + 4)?;
        let total = le_u32(bytes, offset + 8)?;
        let kind = le_u32(bytes, offset + 12)?;
        if dataset_header < 16 || total < dataset_header {
            return malformed(to_u64(offset + 4), "invalid ArtworkDB mhsd lengths");
        }
        let inner =
            offset
                .checked_add(usize::try_from(dataset_header).map_err(|_| {
                    malformed_error(to_u64(offset + 4), "dataset header is too large")
                })?)
                .ok_or_else(|| malformed_error(to_u64(offset + 4), "dataset offset overflow"))?;
        let end = offset
            .checked_add(
                usize::try_from(total)
                    .map_err(|_| malformed_error(to_u64(offset + 8), "dataset is too large"))?,
            )
            .ok_or_else(|| malformed_error(to_u64(offset + 8), "dataset length overflow"))?;
        if end > bytes.len() || inner + 12 > end {
            return malformed(to_u64(offset + 8), "dataset extends beyond ArtworkDB");
        }
        let list_magic: [u8; 4] = bytes[inner..inner + 4]
            .try_into()
            .map_err(|_| malformed_error(to_u64(inner), "truncated list magic"))?;
        let item_count = le_u32(bytes, inner + 8)?;
        datasets.push(ArtworkDatasetInfo {
            kind,
            header_length: dataset_header,
            total_length: total,
            list_magic,
            item_count,
        });
        offset = end;
    }
    if offset != bytes.len() {
        return malformed(
            to_u64(offset),
            "ArtworkDB has trailing bytes after its datasets",
        );
    }

    Ok(ArtworkDatabaseInfo {
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        header_length,
        declared_children,
        next_image_id: le_u32(bytes, 28)?,
        datasets,
    })
}

pub(crate) fn inspect_frame_file(
    path: &Path,
    format_id: u32,
    slot_bytes: u32,
) -> Result<ArtworkFrameInfo> {
    if slot_bytes == 0 {
        return malformed(0, "artwork slot size is zero");
    }
    let bytes = fs::metadata(path)
        .map_err(|source| io_error("inspect artwork frame file", path, source))?
        .len();
    Ok(ArtworkFrameInfo {
        format_id,
        bytes,
        slot_bytes,
        complete_slots: bytes / u64::from(slot_bytes),
        remainder_bytes: u32::try_from(bytes % u64::from(slot_bytes)).unwrap_or(u32::MAX),
    })
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u32"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn malformed<T>(offset: u64, reason: &str) -> Result<T> {
    Err(malformed_error(offset, reason))
}

fn malformed_error(offset: u64, reason: &str) -> Error {
    Error::Malformed {
        format: "ArtworkDB",
        offset,
        reason: reason.to_owned(),
    }
}
