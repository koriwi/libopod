//! `ArtworkDB` structural inspection, record parsing, and removal rewrite.
//!
//! The removal rewrite drops `mhii` artwork records for removed tracks and
//! preserves every other chunk byte-for-byte. `.ithmb` slot payloads are left
//! in place as unreferenced data, mirroring the orphaned-media policy.

mod encode;

pub(crate) use encode::encode_new_frames;
mod write;

pub(crate) use write::{append_artwork_records, build_reused_children, NewArtworkRecord};

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

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
    pub src_img_size: u32,
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
    let src_img_size = le_u32(bytes, offset + 48)?;
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
        src_img_size,
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

/// Slot assignment for one reindex: `(file name, old offset)` -> packed offset.
type SlotMap = BTreeMap<(String, u32), u32>;

/// The rewritten `ArtworkDB` and the rebuilt `.ithmb` file contents after a
/// removal reindex.
type ReindexedArtwork = (Vec<u8>, BTreeMap<String, Vec<u8>>);

/// Rewrites `ArtworkDB` after artwork-bearing removals and rebuilds the
/// `.ithmb` files from scratch, packing the remaining images into contiguous
/// slots.
///
/// `files` maps each canonical `.ithmb` filename (e.g. `F1010_1.ithmb`) to
/// its current on-device content; `slot_bytes` maps the same names to the
/// fixed slot size. Returns the rewritten `ArtworkDB` and the new `.ithmb`
/// file contents. Removed records are dropped and every remaining format
/// reference is repointed at its new packed slot, so no unreferenced data
/// remains.
pub(crate) fn reindex_artwork_removals(
    artworkdb: &[u8],
    removed: &[PersistentId],
    files: &BTreeMap<String, Vec<u8>>,
    slot_bytes: &BTreeMap<String, u32>,
) -> Result<ReindexedArtwork> {
    if removed.is_empty() {
        return Err(verification("no ArtworkDB removals were requested"));
    }
    let records = parse_artwork_records(artworkdb)?;
    let removed_set: BTreeSet<PersistentId> = removed.iter().copied().collect();
    for requested in &removed_set {
        if records
            .iter()
            .filter(|record| record.track_id == *requested)
            .count()
            != 1
        {
            return Err(verification(
                "each requested artwork track must match exactly one ArtworkDB record",
            ));
        }
    }
    // Record file names carry a leading ':' (":F1010_1.ithmb").
    let file_name = |record_name: &str| record_name.trim_start_matches(':').to_owned();

    let mut new_slots_by_track: BTreeMap<PersistentId, Vec<u32>> = BTreeMap::new();
    let mut next_slot: BTreeMap<String, u32> = BTreeMap::new();
    let mut new_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // Shared artwork (reused album art) makes several records reference the
    // same slot; map each (file, old offset) once and reuse the packed slot.
    let mut slot_map: SlotMap = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| !removed_set.contains(&record.track_id))
    {
        let mut new_offsets = Vec::with_capacity(record.formats.len());
        for format in &record.formats {
            let name = file_name(&format.filename);
            let slot_size = u64::from(
                *slot_bytes
                    .get(&name)
                    .ok_or_else(|| verification(&format!("no slot size known for {name}")))?,
            );
            let old_offset = format.ithmb_offset;
            let new_offset = if let Some(&mapped) = slot_map.get(&(name.clone(), old_offset)) {
                mapped
            } else {
                let old = files
                    .get(&name)
                    .ok_or_else(|| verification(&format!("missing on-device ithmb file {name}")))?;
                let start = usize::try_from(u64::from(old_offset))
                    .map_err(|_| verification("ithmb slot offset does not fit this host"))?;
                let end = usize::try_from(u64::from(old_offset) + slot_size)
                    .map_err(|_| verification("ithmb slot end does not fit this host"))?;
                let payload = old.get(start..end).ok_or_else(|| {
                    malformed_error(u64::from(old_offset), "ithmb slot read is out of range")
                })?;
                let target = new_files.entry(name.clone()).or_default();
                let new_offset = u32::try_from(target.len())
                    .map_err(|_| verification("ithmb reindex offset exceeds u32"))?;
                let expected = u64::from(*next_slot.get(&name).unwrap_or(&0)) * slot_size;
                if u64::from(new_offset) != expected {
                    return Err(verification("ithmb reindex lost slot alignment"));
                }
                *next_slot.entry(name.clone()).or_insert(0) += 1;
                target.extend_from_slice(payload);
                slot_map.insert((name.clone(), old_offset), new_offset);
                new_offset
            };
            new_offsets.push(new_offset);
        }
        new_slots_by_track.insert(record.track_id, new_offsets);
    }
    let rewritten = rewrite_mhli_patched(artworkdb, &removed_set, &new_slots_by_track)?;
    Ok((rewritten, new_files))
}

/// Rebuilds the kind-1 `mhli` dataset, dropping removed records and patching
/// the `.ithmb` slot reference of every retained format to its packed slot.
fn rewrite_mhli_patched(
    bytes: &[u8],
    removed: &BTreeSet<PersistentId>,
    new_slots_by_track: &BTreeMap<PersistentId, Vec<u32>>,
) -> Result<Vec<u8>> {
    let header_length = header_length(bytes)?;
    let dataset_count = usize::try_from(le_u32(bytes, 20)?)
        .map_err(|_| malformed_error(20, "dataset count does not fit this host"))?;
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
            output.extend_from_slice(&patch_mhli(bytes, offset, removed, new_slots_by_track)?);
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

fn patch_mhli(
    bytes: &[u8],
    dataset_offset: usize,
    removed: &BTreeSet<PersistentId>,
    new_slots_by_track: &BTreeMap<PersistentId, Vec<u32>>,
) -> Result<Vec<u8>> {
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
        if !removed.contains(&track_id) {
            let record = bytes[record_offset..record_offset + record_total].to_vec();
            let slots = new_slots_by_track
                .get(&track_id)
                .ok_or_else(|| verification("retained artwork record lacks reindexed slots"))?;
            mhli.extend_from_slice(&patch_record_slots(&record, slots)?);
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

/// Replaces the `.ithmb` slot reference of every format container inside one
/// `mhii` record, walking the type-2 `mhod` children exactly like
/// `parse_mhii`/`parse_mhni`.
fn patch_record_slots(record: &[u8], new_offsets: &[u32]) -> Result<Vec<u8>> {
    require_magic(record, 0, b"mhii")?;
    let header_length = usize_value(le_u32(record, 4)?, 4)?;
    let total = usize_value(le_u32(record, 8)?, 8)?;
    let child_count = usize_value(le_u32(record, 12)?, 12)?;
    if total != record.len() {
        return malformed(to_u64(8), "mhii record length mismatch");
    }
    let mut output = record.to_vec();
    let mut format_index = 0usize;
    let mut child_offset = header_length;
    for _ in 0..child_count {
        if child_offset + 16 > total {
            return malformed(to_u64(child_offset), "mhii child header is out of range");
        }
        let mhod_total = usize_value(le_u32(record, child_offset + 8)?, child_offset + 8)?;
        let mhod_type = le_u32(record, child_offset + 12)?;
        if mhod_type == 2 {
            let mhni_offset = child_offset
                .checked_add(usize_value(
                    le_u32(record, child_offset + 4)?,
                    child_offset + 4,
                )?)
                .ok_or_else(|| malformed_error(to_u64(child_offset + 4), "mhod offset overflow"))?;
            let slot = *new_offsets.get(format_index).ok_or_else(|| {
                verification("artwork record has more format refs than reindexed slots")
            })?;
            write_u32(&mut output, mhni_offset + 20, slot)?;
            format_index += 1;
        }
        child_offset = child_offset
            .checked_add(mhod_total)
            .ok_or_else(|| malformed_error(to_u64(child_offset + 8), "mhod length overflow"))?;
    }
    if child_offset != total {
        return malformed(to_u64(child_offset), "trailing bytes after mhii children");
    }
    if format_index != new_offsets.len() {
        return Err(verification(
            "artwork record has fewer format refs than reindexed slots",
        ));
    }
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

pub(super) fn usize_value(value: u32, offset: usize) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| malformed_error(to_u64(offset), "value does not fit this host"))
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
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

pub(super) fn le_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u32"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

pub(super) fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub(super) fn malformed<T>(offset: u64, reason: &str) -> Result<T> {
    Err(malformed_error(offset, reason))
}

pub(super) fn malformed_error(offset: u64, reason: &str) -> Error {
    Error::Malformed {
        format: "ArtworkDB",
        offset,
        reason: reason.to_owned(),
    }
}

pub(super) fn verification(reason: &str) -> Error {
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
    fn parses_records_from_the_private_fixture() {
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
        // Removing a track that has no record must fail (reindex path).
        let unknown = PersistentId::from_bits(0x1234_5678_9abc_def0);
        let slot_bytes: BTreeMap<String, u32> = [
            ("F1010_1.ithmb".to_owned(), 115_200u32),
            ("F1013_1.ithmb".to_owned(), 5_000),
            ("F1015_1.ithmb".to_owned(), 6_728),
            ("F1016_1.ithmb".to_owned(), 6_612),
        ]
        .into_iter()
        .collect();
        assert!(
            reindex_artwork_removals(&bytes, &[unknown], &BTreeMap::new(), &slot_bytes).is_err()
        );
    }

    #[test]
    fn reindexes_slots_after_removal_on_the_private_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        let artwork = fixture.join("iPod_Control/Artwork/ArtworkDB");
        if !artwork.is_file() {
            return;
        }
        let bytes = std::fs::read(&artwork).unwrap();
        let slot_sizes: BTreeMap<String, u32> = [
            ("F1010_1.ithmb", 115_200u32),
            ("F1013_1.ithmb", 5_000),
            ("F1015_1.ithmb", 6_728),
            ("F1016_1.ithmb", 6_612),
        ]
        .into_iter()
        .map(|(name, size)| (name.to_owned(), size))
        .collect();
        let mut files = BTreeMap::new();
        for name in slot_sizes.keys() {
            let path = fixture.join("iPod_Control/Artwork").join(name);
            files.insert(name.clone(), std::fs::read(&path).unwrap());
        }
        let records = parse_artwork_records(&bytes).unwrap();
        let removed = records[0].track_id;
        let (rewritten, new_files) =
            reindex_artwork_removals(&bytes, &[removed], &files, &slot_sizes).unwrap();
        let remaining = parse_artwork_records(&rewritten).unwrap();
        assert_eq!(remaining.len(), 703);
        assert!(remaining.iter().all(|record| record.track_id != removed));
        // The new files must be exactly one slot shorter per format and the
        // repacked payloads must match what the old offsets pointed at.
        for format in ["F1010_1", "F1013_1", "F1015_1", "F1016_1"] {
            let name = format!("{format}.ithmb");
            let slot = u64::from(slot_sizes[&name]);
            let new = &new_files[&name];
            let old = &files[&name];
            assert_eq!(new.len() as u64 % slot, 0, "{name} is not whole slots");
            assert!(
                (new.len() as u64) <= (old.len() as u64),
                "{name} grew during reindex"
            );
            // Every remaining record referencing this format must find its old
            // payload at its new offset.
            for record in &remaining {
                for reference in &record.formats {
                    if reference.filename.trim_start_matches(':') == name {
                        let off = usize::try_from(u64::from(reference.ithmb_offset)).unwrap();
                        let slot = usize::try_from(slot).unwrap();
                        assert_eq!(
                            &new[off..off + slot],
                            &old[off..off + slot],
                            "payload mismatch at new offset {off} in {name}"
                        );
                    }
                }
            }
        }
        // The rewritten ArtworkDB must itself reparse and keep the header.
        assert_eq!(&rewritten[..8], &bytes[..8]);
        let total: u32 = le_u32(&rewritten, 8).unwrap();
        assert_eq!(usize::try_from(total).unwrap(), rewritten.len());
    }
}
