//! `ArtworkDB` record builders and slot-reuse append for track additions.
//!
//! When a new track joins an album that already has artwork, the new `mhii`
//! record references the album-mate's `.ithmb` slots (iOpenPod's passthrough
//! behaviour). No image decoding or re-encoding is performed, so slot payloads
//! stay byte-identical.

use crate::{PersistentId, Result};

use super::{le_u32, malformed, malformed_error, to_u64, usize_value, verification, write_u32};

const MHII_HEADER_SIZE: usize = 152;
const MHNI_HEADER_SIZE: usize = 76;
const MHOD_HEADER_SIZE: usize = 24;

/// A new artwork record to append: the track's persistent ID, a fresh image
/// ID, the source image size, and the already-serialized type-2 MHOD children
/// (built from an album-mate's format references when reusing slots).
#[derive(Clone, Debug)]
pub(crate) struct NewArtworkRecord {
    pub image_id: u32,
    pub track_id: PersistentId,
    pub src_img_size: u32,
    pub child_count: u32,
    pub mhod_children: Vec<u8>,
}

/// Appends new `mhii` records to the type-1 dataset and bumps the `mhfd`
/// `next_image_id` field. Every other chunk is preserved byte-for-byte.
pub(crate) fn append_artwork_records(
    bytes: &[u8],
    records: &[NewArtworkRecord],
) -> Result<Vec<u8>> {
    if records.is_empty() {
        return Err(verification("no ArtworkDB additions were requested"));
    }
    let header_length = header_length(bytes)?;
    let dataset_count = usize::try_from(le_u32(bytes, 20)?)
        .map_err(|_| malformed_error(to_u64(20), "dataset count does not fit this host"))?;
    let existing_next = le_u32(bytes, 28)?;
    let mut output = bytes[..header_length].to_vec();
    let mut offset = header_length;
    let mut appended = false;
    for _ in 0..dataset_count {
        require_magic(bytes, offset, b"mhsd")?;
        let total = usize_value(le_u32(bytes, offset + 8)?, offset + 8)?;
        let kind = le_u32(bytes, offset + 12)?;
        let end = offset
            .checked_add(total)
            .ok_or_else(|| malformed_error(to_u64(offset + 8), "dataset length overflow"))?;
        if kind == 1 {
            output.extend_from_slice(&rewrite_mhli_with_appends(bytes, offset, records)?);
            appended = true;
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
    if !appended {
        return Err(verification("ArtworkDB has no type-1 image dataset"));
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("ArtworkDB exceeds 4 GiB"))?;
    write_u32(&mut output, 8, output_len)?;
    let next_image_id = records
        .iter()
        .map(|record| record.image_id)
        .max()
        .unwrap_or(existing_next)
        .checked_add(1)
        .ok_or_else(|| verification("image ID overflow"))?;
    write_u32(&mut output, 28, next_image_id.max(existing_next))?;
    Ok(output)
}

fn rewrite_mhli_with_appends(
    bytes: &[u8],
    dataset_offset: usize,
    records: &[NewArtworkRecord],
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
    let mut record_offset = body;
    for _ in 0..count {
        require_magic(bytes, record_offset, b"mhii")?;
        let record_total = usize_value(le_u32(bytes, record_offset + 8)?, record_offset + 8)?;
        mhli.extend_from_slice(&bytes[record_offset..record_offset + record_total]);
        record_offset = record_offset
            .checked_add(record_total)
            .ok_or_else(|| malformed_error(to_u64(record_offset + 8), "mhii length overflow"))?;
    }
    if record_offset != dataset_end {
        return malformed(to_u64(record_offset), "trailing bytes after mhli records");
    }
    for record in records {
        mhli.extend_from_slice(&build_mhii(record));
    }
    write_u32(
        &mut mhli,
        8,
        u32::try_from(count + records.len())
            .map_err(|_| verification("ArtworkDB record count exceeds u32"))?,
    )?;
    let mut output = bytes[dataset_offset..list].to_vec();
    output.extend_from_slice(&mhli);
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("ArtworkDB dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn build_mhii(record: &NewArtworkRecord) -> Vec<u8> {
    let total = MHII_HEADER_SIZE + record.mhod_children.len();
    let mut output = vec![0_u8; MHII_HEADER_SIZE];
    output[..4].copy_from_slice(b"mhii");
    write_u32(
        &mut output,
        4,
        u32::try_from(MHII_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut output, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut output, 12, record.child_count).ok();
    write_u32(&mut output, 16, record.image_id).ok();
    write_u64(&mut output, 20, record.track_id.to_bits()).ok();
    write_u32(&mut output, 48, record.src_img_size).ok();
    output.extend_from_slice(&record.mhod_children);
    output
}

/// Serializes one type-2 container MHOD per format ref, wrapping a fresh MHNI
/// that references an existing `.ithmb` slot, and returns the bytes plus the
/// child count. Used when a new track reuses an album's slots.
pub(crate) fn build_reused_children(format_refs: &[super::ArtworkFormatRef]) -> (Vec<u8>, u32) {
    let mut children = Vec::new();
    let mut count = 0_u32;
    for format in format_refs {
        let mhni = build_mhni(
            format.format_id,
            format.ithmb_offset,
            format.image_size,
            format.width,
            format.height,
            &format.filename,
        );
        let total = MHOD_HEADER_SIZE + mhni.len();
        let mut mhod = vec![0_u8; MHOD_HEADER_SIZE];
        mhod[..4].copy_from_slice(b"mhod");
        write_u32(
            &mut mhod,
            4,
            u32::try_from(MHOD_HEADER_SIZE).unwrap_or(u32::MAX),
        )
        .ok();
        write_u32(&mut mhod, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
        write_u32(&mut mhod, 12, 2).ok();
        mhod.extend_from_slice(&mhni);
        children.extend_from_slice(&mhod);
        count += 1;
    }
    (children, count)
}

fn build_mhni(
    format_id: u32,
    ithmb_offset: u32,
    image_size: u32,
    width: u16,
    height: u16,
    filename: &str,
) -> Vec<u8> {
    let filename_mhod = build_filename_mhod(filename);
    let total = MHNI_HEADER_SIZE + filename_mhod.len();
    let mut output = vec![0_u8; MHNI_HEADER_SIZE];
    output[..4].copy_from_slice(b"mhni");
    write_u32(
        &mut output,
        4,
        u32::try_from(MHNI_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut output, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut output, 12, 1).ok();
    write_u32(&mut output, 16, format_id).ok();
    write_u32(&mut output, 20, ithmb_offset).ok();
    write_u32(&mut output, 24, image_size).ok();
    write_u16(&mut output, 32, height).ok();
    write_u16(&mut output, 34, width).ok();
    write_u32(&mut output, 40, image_size).ok();
    output.extend_from_slice(&filename_mhod);
    output
}

/// Serializes the nested type-3 MHOD holding the `:Fxxxx_1.ithmb` filename.
fn build_filename_mhod(filename: &str) -> Vec<u8> {
    let encoded: Vec<u8> = filename.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let total = MHOD_HEADER_SIZE + 12 + encoded.len();
    let mut output = vec![0_u8; MHOD_HEADER_SIZE + 12 + encoded.len()];
    output[..4].copy_from_slice(b"mhod");
    write_u32(
        &mut output,
        4,
        u32::try_from(MHOD_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut output, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut output, 12, 3).ok();
    write_u32(
        &mut output,
        MHOD_HEADER_SIZE,
        u32::try_from(encoded.len()).unwrap_or(u32::MAX),
    )
    .ok();
    output[MHOD_HEADER_SIZE + 4] = 2; // UTF-16 encoding byte
    output[MHOD_HEADER_SIZE + 12..].copy_from_slice(&encoded);
    output
}

fn header_length(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < 132 || &bytes[..4] != b"mhfd" {
        return malformed(0, "expected an mhfd ArtworkDB");
    }
    let header_length = usize::try_from(le_u32(bytes, 4)?)
        .map_err(|_| malformed_error(to_u64(4), "mhfd header does not fit this host"))?;
    if header_length < 132 || header_length > bytes.len() {
        return malformed(4, "mhfd header length is out of bounds");
    }
    Ok(header_length)
}

fn require_magic(bytes: &[u8], offset: usize, expected: &[u8]) -> Result<()> {
    if bytes.get(offset..offset + 4) != Some(expected) {
        return malformed(to_u64(offset), "unexpected ArtworkDB chunk magic");
    }
    Ok(())
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 2)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u16 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 8)
        .ok_or_else(|| malformed_error(to_u64(offset), "truncated u64 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn appends_a_reused_record_to_the_private_fixture() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g");
        let artwork = fixture.join("iPod_Control/Artwork/ArtworkDB");
        if !artwork.is_file() {
            return;
        }
        let bytes = std::fs::read(&artwork).unwrap();
        let records = super::super::parse_artwork_records(&bytes).unwrap();
        assert_eq!(records.len(), 704);
        let mate = &records[0];
        let (children, count) = build_reused_children(&mate.formats);
        assert_eq!(count, 4);
        let new = NewArtworkRecord {
            image_id: 804,
            track_id: crate::PersistentId::from_bits(0x1122_3344_5566_7788),
            src_img_size: mate.src_img_size,
            child_count: count,
            mhod_children: children,
        };
        let rewritten = append_artwork_records(&bytes, &[new]).unwrap();
        let remaining = super::super::parse_artwork_records(&rewritten).unwrap();
        assert_eq!(remaining.len(), 705);
        let added = remaining
            .iter()
            .find(|record| record.track_id.to_bits() == 0x1122_3344_5566_7788)
            .unwrap();
        assert_eq!(added.image_id, 804);
        assert_eq!(added.src_img_size, mate.src_img_size);
        assert_eq!(added.formats.len(), 4);
        for (old, new) in mate.formats.iter().zip(&added.formats) {
            assert_eq!(old.format_id, new.format_id);
            assert_eq!(old.ithmb_offset, new.ithmb_offset);
            assert_eq!(old.image_size, new.image_size);
            assert_eq!(old.width, new.width);
            assert_eq!(old.height, new.height);
            assert_eq!(old.filename, new.filename);
        }
        // The next-image-id counter is bumped from 804 to 805.
        let next = le_u32(&rewritten, 28).unwrap();
        assert_eq!(next, 805);
        // Header magic and opaque fields other than total length and the
        // bumped next-image-id counter survive.
        assert_eq!(&rewritten[..8], &bytes[..8]);
        assert_eq!(&rewritten[12..28], &bytes[12..28]);
        assert_eq!(&rewritten[32..132], &bytes[32..132]);
        assert_ne!(le_u32(&rewritten, 8).unwrap(), le_u32(&bytes, 8).unwrap());
    }
}
