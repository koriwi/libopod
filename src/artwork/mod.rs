//! `ArtworkDB` structural inspection, record parsing, and removal rewrite.
//!
//! The removal rewrite drops `mhii` artwork records for removed tracks and
//! preserves every other chunk byte-for-byte. `.ithmb` slot payloads are left
//! in place as unreferenced data, mirroring the orphaned-media policy.

use std::{fs, path::Path};

use crate::{error::io_error, Error, PersistentId, Result};

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

/// One `mhii` artwork record: one image for one track, referencing up to four
/// `.ithmb` format slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkRecord {
    pub image_id: u32,
    pub track_id: PersistentId,
    pub formats: Vec<ArtworkFormatRef>,
}

/// One format reference (`mhni` inside a type-2 container MHOD) inside an
/// artwork record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkFormatRef {
    pub format_id: u32,
    pub ithmb_offset: u32,
    pub image_size: u32,
    pub width: u16,
    pub height: u16,
    pub filename: String,
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

/// Parses every `mhii` artwork record from an `ArtworkDB`, returning them in
/// database order. Media payloads are never inspected.
///
/// # Errors
///
/// Returns an error when the database is structurally invalid or truncated.
pub fn parse_artwork_records(bytes: &[u8]) -> Result<Vec<ArtworkRecord>> {
    let header_length = header_length(bytes)?;
    let dataset_count = usize::try_from(le_u32(bytes, 20)?)
        .map_err(|_| malformed_error(20, "dataset count does not fit this host"))?;
    let mut offset = header_length;
    let mut records = Vec::new();
    for _ in 0..dataset_count {
        require_magic(bytes, offset, b"mhsd")?;
        let dataset_total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
        let kind = le_u32(bytes, offset + 12)?;
        if kind == 1 {
            let list = offset
                .checked_add(usize_value(le_u32(bytes, offset + 4)?, offset + 4)?)
                .ok_or_else(|| malformed_error(to_u64(offset + 4), "dataset offset overflow"))?;
            require_magic(bytes, list, b"mhli")?;
            let count = usize_value(le_u32(bytes, list + 8)?, list + 8)?;
            let body = list
                .checked_add(usize_value(le_u32(bytes, list + 4)?, list + 4)?)
                .ok_or_else(|| malformed_error(to_u64(list + 4), "mhli offset overflow"))?;
            let mut record_offset = body;
            for _ in 0..count {
                let record = parse_mhii(bytes, record_offset)?;
                record_offset = record_offset
                    .checked_add(usize_value(
                        le_u32(bytes, record_offset + 8)?,
                        record_offset + 8,
                    )?)
                    .ok_or_else(|| {
                        malformed_error(to_u64(record_offset + 8), "mhii length overflow")
                    })?;
                records.push(record);
            }
            let dataset_end = offset
                .checked_add(dataset_total)
                .ok_or_else(|| malformed_error(to_u64(offset + 8), "dataset length overflow"))?;
            if record_offset != dataset_end {
                return malformed(to_u64(record_offset), "trailing bytes after mhli records");
            }
        }
        offset = offset
            .checked_add(dataset_total)
            .ok_or_else(|| malformed_error(to_u64(offset + 8), "dataset length overflow"))?;
    }
    if offset != bytes.len() {
        return malformed(
            to_u64(offset),
            "ArtworkDB has trailing bytes after its datasets",
        );
    }
    Ok(records)
}

fn parse_mhii(bytes: &[u8], offset: usize) -> Result<ArtworkRecord> {
    require_magic(bytes, offset, b"mhii")?;
    let total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
    let child_count = usize_value(le_u32(bytes, offset + 12)?, offset + 12)?;
    let image_id = le_u32(bytes, offset + 16)?;
    let track_id = PersistentId::from_bits(le_u64(bytes, offset + 20)?);
    let mut formats = Vec::with_capacity(child_count);
    let mut child_offset = offset
        .checked_add(usize_value(le_u32(bytes, offset + 4)?, offset + 4)?)
        .ok_or_else(|| malformed_error(to_u64(offset + 4), "mhii offset overflow"))?;
    let end = offset
        .checked_add(total)
        .ok_or_else(|| malformed_error(to_u64(offset + 8), "mhii length overflow"))?;
    for _ in 0..child_count {
        require_magic(bytes, child_offset, b"mhod")?;
        let mhod_type = le_u32(bytes, child_offset + 12)?;
        let mhod_total = usize_value(le_u32(bytes, child_offset + 8)?, child_offset + 8)?;
        if mhod_type == 2 {
            let mhni_offset = child_offset
                .checked_add(usize_value(
                    le_u32(bytes, child_offset + 4)?,
                    child_offset + 4,
                )?)
                .ok_or_else(|| malformed_error(to_u64(child_offset + 4), "mhod offset overflow"))?;
            formats.push(parse_mhni(bytes, mhni_offset)?);
        }
        child_offset = child_offset
            .checked_add(mhod_total)
            .ok_or_else(|| malformed_error(to_u64(child_offset + 8), "mhod length overflow"))?;
    }
    if child_offset != end {
        return malformed(to_u64(child_offset), "trailing bytes after mhii children");
    }
    Ok(ArtworkRecord {
        image_id,
        track_id,
        formats,
    })
}

fn parse_mhni(bytes: &[u8], offset: usize) -> Result<ArtworkFormatRef> {
    require_magic(bytes, offset, b"mhni")?;
    let total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
    let nested = offset
        .checked_add(usize_value(le_u32(bytes, offset + 4)?, offset + 4)?)
        .ok_or_else(|| malformed_error(to_u64(offset + 4), "mhni offset overflow"))?;
    let filename = if nested + 12 <= offset + total && &bytes[nested..nested + 4] == b"mhod" {
        parse_filename_mhod(bytes, nested)?
    } else {
        String::new()
    };
    Ok(ArtworkFormatRef {
        format_id: le_u32(bytes, offset + 16)?,
        ithmb_offset: le_u32(bytes, offset + 20)?,
        image_size: le_u32(bytes, offset + 24)?,
        height: le_u16(bytes, offset + 32)?,
        width: le_u16(bytes, offset + 34)?,
        filename,
    })
}

fn parse_filename_mhod(bytes: &[u8], offset: usize) -> Result<String> {
    require_magic(bytes, offset, b"mhod")?;
    let mhod_total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
    let body = offset
        .checked_add(usize_value(le_u32(bytes, offset + 4)?, offset + 4)?)
        .ok_or_else(|| malformed_error(to_u64(offset + 4), "mhod offset overflow"))?;
    if body + 12 > offset + mhod_total {
        return Ok(String::new());
    }
    let string_length = usize_value(le_u32(bytes, body)?, body)?;
    let encoding = bytes[body + 4];
    let data = bytes
        .get(body + 12..body + 12 + string_length)
        .ok_or_else(|| malformed_error(to_u64(body + 12), "filename length exceeds its chunk"))?;
    if encoding == 2 {
        let mut units = Vec::with_capacity(string_length / 2);
        for pair in data.chunks_exact(2) {
            units.push(u16::from_le_bytes([pair[0], pair[1]]));
        }
        Ok(String::from_utf16_lossy(&units))
    } else {
        Ok(String::from_utf8_lossy(data).into_owned())
    }
}

/// Removes the artwork records for every requested persistent ID, preserving
/// all other chunks byte-for-byte. `.ithmb` slot payloads are intentionally
/// left in place as unreferenced data, mirroring the orphaned-media policy.
pub(crate) fn remove_tracks_from_artworkdb(
    bytes: &[u8],
    removed: &[PersistentId],
) -> Result<Vec<u8>> {
    if removed.is_empty() {
        return Err(verification("no ArtworkDB removals were requested"));
    }
    let header_length = header_length(bytes)?;
    let dataset_count = usize::try_from(le_u32(bytes, 20)?)
        .map_err(|_| malformed_error(20, "dataset count does not fit this host"))?;
    for requested in removed {
        let found = parse_artwork_records(bytes)?
            .iter()
            .filter(|record| record.track_id == *requested)
            .count();
        if found != 1 {
            return Err(verification(
                "each requested artwork track must match exactly one ArtworkDB record",
            ));
        }
    }
    let mut output = bytes[..header_length].to_vec();
    let mut offset = header_length;
    for _ in 0..dataset_count {
        require_magic(bytes, offset, b"mhsd")?;
        let total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
        let kind = le_u32(bytes, offset + 12)?;
        let end = offset
            .checked_add(total)
            .ok_or_else(|| malformed_error(to_u64(offset + 8), "dataset length overflow"))?;
        if kind == 1 {
            output.extend_from_slice(&rewrite_mhli(bytes, offset, removed)?);
        } else {
            output.extend_from_slice(&bytes[offset..end]);
        }
        offset = end;
    }
    if offset != bytes.len() {
        return malformed(
            to_u64(offset),
            "ArtworkDB has trailing bytes after its datasets",
        );
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("ArtworkDB exceeds 4 GiB"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn rewrite_mhli(bytes: &[u8], dataset_offset: usize, removed: &[PersistentId]) -> Result<Vec<u8>> {
    let dataset_total = usize_value(le_u32(bytes, dataset_offset + 8)?, dataset_offset + 8)?;
    let list = dataset_offset
        .checked_add(usize_value(
            le_u32(bytes, dataset_offset + 4)?,
            dataset_offset + 4,
        )?)
        .ok_or_else(|| malformed_error(to_u64(dataset_offset + 4), "dataset offset overflow"))?;
    require_magic(bytes, list, b"mhli")?;
    let list_header = usize_value(le_u32(bytes, list + 4)?, list + 4)?;
    let count = usize_value(le_u32(bytes, list + 8)?, list + 8)?;
    let body = list
        .checked_add(list_header)
        .ok_or_else(|| malformed_error(to_u64(list + 4), "mhli offset overflow"))?;
    let dataset_end = dataset_offset
        .checked_add(dataset_total)
        .ok_or_else(|| malformed_error(to_u64(dataset_offset + 8), "dataset length overflow"))?;

    let mut mhli = bytes[list..body].to_vec();
    let mut retained = 0_usize;
    let mut record_offset = body;
    for _ in 0..count {
        require_magic(bytes, record_offset, b"mhii")?;
        let record_total = usize_value(le_u32(bytes, record_offset + 8)?, record_offset + 8)?;
        let track_id = PersistentId::from_bits(le_u64(bytes, record_offset + 20)?);
        if removed.contains(&track_id) {
            if removed.iter().filter(|id| **id == track_id).count() != 1 {
                return Err(verification(
                    "each requested persistent ID must match exactly one ArtworkDB record",
                ));
            }
        } else {
            mhli.extend_from_slice(&bytes[record_offset..record_offset + record_total]);
            retained += 1;
        }
        record_offset = record_offset
            .checked_add(record_total)
            .ok_or_else(|| malformed_error(to_u64(record_offset + 8), "mhii length overflow"))?;
    }
    if record_offset != dataset_end {
        return malformed(to_u64(record_offset), "trailing bytes after mhli records");
    }
    write_u32(
        &mut mhli,
        8,
        u32::try_from(retained).map_err(|_| verification("ArtworkDB record count exceeds u32"))?,
    )?;
    let mut output = bytes[dataset_offset..list].to_vec();
    output.extend_from_slice(&mhli);
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("ArtworkDB dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn header_length(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < MIN_MHFD_HEADER {
        return malformed(0, "file is shorter than the minimum mhfd header");
    }
    if &bytes[..4] != b"mhfd" {
        return malformed(0, "expected mhfd magic");
    }
    let header_length = usize::try_from(le_u32(bytes, 4)?)
        .map_err(|_| malformed_error(4, "mhfd header does not fit this host"))?;
    if header_length < MIN_MHFD_HEADER || header_length > bytes.len() {
        return malformed(4, "mhfd header length is out of bounds");
    }
    Ok(header_length)
}

fn require_magic(bytes: &[u8], offset: usize, expected: &[u8]) -> Result<()> {
    if expected.len() != 4 || bytes.get(offset..offset + 4) != Some(expected) {
        return malformed(to_u64(offset), "unexpected chunk magic");
    }
    Ok(())
}

fn usize_value(value: u32, offset: usize) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| malformed_error(to_u64(offset), "value does not fit this host"))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u32 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u16"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u64"))?;
    Ok(u64::from_le_bytes(value.try_into().map_err(|_| {
        malformed_error(to_u64(offset), "truncated u64")
    })?))
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

fn verification(reason: &str) -> Error {
    Error::Verification {
        format: "ArtworkDB",
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn parses_and_removes_records_from_the_private_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        let artwork = fixture.join("iPod_Control/Artwork/ArtworkDB");
        if !artwork.is_file() {
            return;
        }
        let bytes = std::fs::read(&artwork).unwrap();
        let records = parse_artwork_records(&bytes).unwrap();
        assert_eq!(records.len(), 704);
        let first = &records[0];
        assert_eq!(first.image_id, 100);
        assert_eq!(first.formats.len(), 4);
        for format in &first.formats {
            assert!(format.filename.starts_with(':'));
        }
        let removed = records[0].track_id;
        let rewritten = remove_tracks_from_artworkdb(&bytes, &[removed]).unwrap();
        let remaining = parse_artwork_records(&rewritten).unwrap();
        assert_eq!(remaining.len(), 703);
        assert!(remaining.iter().all(|record| record.track_id != removed));
        // Opaque mhfd header fields (other than the total length) are preserved.
        assert_eq!(&rewritten[..8], &bytes[..8]);
        assert_eq!(&rewritten[12..132], &bytes[12..132]);
        let total: u32 = le_u32(&rewritten, 8).unwrap();
        assert_eq!(usize::try_from(total).unwrap(), rewritten.len());
        assert_eq!(rewritten.len(), bytes.len() - 808);
        // Removing a track that has no record must fail.
        let unknown = PersistentId::from_bits(0x1234_5678_9abc_def0);
        assert!(remove_tracks_from_artworkdb(&bytes, &[unknown]).is_err());
    }
}
