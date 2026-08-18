use std::{collections::HashMap, io::Write};

use flate2::{write::ZlibEncoder, Compression};

use crate::{crypto::hashab, Error, PersistentId, Result};

use super::cdb::{decode_payload, inspect_cdb};

const MHIT_DB_TRACK_ID: usize = 0x70;
const MHIT_TRACK_ID: usize = 0x10;
const MHIP_TRACK_ID: usize = 0x18;
const MHIP_PERSISTENT_ID: usize = 0x2c;

#[derive(Clone, Copy, Debug)]
struct RemovedTrack {
    persistent_id: PersistentId,
    track_id: u32,
    position: u32,
}

pub(crate) fn remove_tracks_from_cdb(
    bytes: &[u8],
    firewire_guid: [u8; 8],
    removals: &[PersistentId],
) -> Result<Vec<u8>> {
    if removals.is_empty() {
        return Err(verification("no CDB removals were requested"));
    }
    let header_length = usize::try_from(read_u32(bytes, 4)?)
        .map_err(|_| malformed(4, "mhbd header length does not fit this host"))?;
    let payload = decode_payload(bytes, header_length)?;
    let dataset_count = usize::try_from(read_u32(bytes, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;

    let mut datasets = split_datasets(&payload, dataset_count)?;
    let track_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(1))
        .ok_or_else(|| verification("CDB has no type-1 track dataset"))?;
    let (rewritten_tracks, removed) = rewrite_track_dataset(&datasets[track_dataset], removals)?;
    datasets[track_dataset] = rewritten_tracks;

    for dataset in &mut datasets {
        let kind = read_u32(dataset, 12)?;
        if matches!(kind, 2 | 3 | 5) {
            *dataset = rewrite_playlist_dataset(dataset, &removed)?;
        }
    }

    let mut uncompressed = Vec::new();
    for dataset in datasets {
        uncompressed.extend_from_slice(&dataset);
    }
    finalize_cdb(&bytes[..header_length], &uncompressed, firewire_guid)
}

/// Compresses a rewritten dataset payload, patches the `mhbd` header, applies
/// the exact-final-byte HASHAB signature, and verifies the result.
pub(super) fn finalize_cdb(
    header: &[u8],
    uncompressed: &[u8],
    firewire_guid: [u8; 8],
) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(uncompressed)
        .map_err(|source| Error::Malformed {
            format: "iTunesCDB",
            offset: u64::try_from(header.len()).unwrap_or(u64::MAX),
            reason: format!("could not compress rewritten payload: {source}"),
        })?;
    let compressed = encoder.finish().map_err(|source| Error::Malformed {
        format: "iTunesCDB",
        offset: u64::try_from(header.len()).unwrap_or(u64::MAX),
        reason: format!("could not finish rewritten payload: {source}"),
    })?;

    let mut output = header.to_vec();
    output.extend_from_slice(&compressed);
    write_u16(&mut output, 0xa8, 1)?;
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("rewritten CDB exceeds 4 GiB"))?;
    write_u32(&mut output, 8, output_len)?;
    hashab::sign_database(&mut output, &firewire_guid, &hashab::CBK_RANDOM)?;
    let inspected = inspect_cdb(&output, Some(&firewire_guid))?;
    if inspected.hashab_signature_status != Some(hashab::DatabaseSignatureStatus::Valid) {
        return Err(verification(
            "rewritten CDB HASHAB signature did not verify",
        ));
    }
    Ok(output)
}

fn split_datasets(payload: &[u8], expected: usize) -> Result<Vec<Vec<u8>>> {
    let mut datasets = Vec::with_capacity(expected);
    let mut offset = 0;
    while offset < payload.len() {
        require_magic(payload, offset, b"mhsd")?;
        let total = usize_value(read_u32(payload, offset + 8)?, offset + 8)?;
        let end = checked_end(offset, total, payload.len(), offset + 8)?;
        datasets.push(payload[offset..end].to_vec());
        offset = end;
    }
    if datasets.len() != expected {
        return Err(verification("rewritten CDB dataset count is inconsistent"));
    }
    Ok(datasets)
}

fn rewrite_track_dataset(
    dataset: &[u8],
    removals: &[PersistentId],
) -> Result<(Vec<u8>, Vec<RemovedTrack>)> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlt")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    if list_header < 12 {
        return Err(malformed(list + 4, "mhlt header is too short"));
    }
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut output = dataset[..body].to_vec();
    let mut removed = Vec::new();
    let mut offset = body;
    for position in 0..count {
        let track = chunk_header(dataset, offset, b"mhit")?;
        if track.header_length < MHIT_DB_TRACK_ID + 8 {
            return Err(malformed(offset + 4, "mhit header lacks a persistent ID"));
        }
        let persistent_id = PersistentId::from_bits(read_u64(dataset, offset + MHIT_DB_TRACK_ID)?);
        if removals.contains(&persistent_id) {
            removed.push(RemovedTrack {
                persistent_id,
                track_id: read_u32(dataset, offset + MHIT_TRACK_ID)?,
                position: u32::try_from(position)
                    .map_err(|_| verification("CDB track position exceeds u32"))?,
            });
        } else {
            output.extend_from_slice(&dataset[offset..track.end]);
        }
        offset = track.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlt tracks"));
    }
    for requested in removals {
        if removed
            .iter()
            .filter(|track| track.persistent_id == *requested)
            .count()
            != 1
        {
            return Err(verification(
                "each requested persistent ID must match exactly one CDB track",
            ));
        }
    }
    write_u32(
        &mut output,
        list + 8,
        u32::try_from(count - removed.len())
            .map_err(|_| verification("remaining CDB track count exceeds u32"))?,
    )?;
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten track dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok((output, removed))
}

fn rewrite_playlist_dataset(dataset: &[u8], removed: &[RemovedTrack]) -> Result<Vec<u8>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlp")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    if list_header < 12 {
        return Err(malformed(list + 4, "mhlp header is too short"));
    }
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut output = dataset[..body].to_vec();
    let mut offset = body;
    for _ in 0..count {
        let playlist = chunk_header(dataset, offset, b"mhyp")?;
        output.extend_from_slice(&rewrite_playlist(&dataset[offset..playlist.end], removed)?);
        offset = playlist.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlp playlists"));
    }
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten playlist dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn rewrite_playlist(playlist: &[u8], removed: &[RemovedTrack]) -> Result<Vec<u8>> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    if header.header_length < 20 {
        return Err(malformed(4, "mhyp header is too short"));
    }
    let mhod_count = usize_value(read_u32(playlist, 12)?, 12)?;
    let mhip_count = usize_value(read_u32(playlist, 16)?, 16)?;
    let removed_positions: Vec<u32> = removed.iter().map(|track| track.position).collect();
    let mut index_removals = HashMap::<u32, Vec<u32>>::new();
    let mut output = playlist[..header.header_length].to_vec();
    let mut offset = header.header_length;

    for _ in 0..mhod_count {
        let mhod = chunk_header(playlist, offset, b"mhod")?;
        let chunk = &playlist[offset..mhod.end];
        let kind = read_u32(chunk, 12)?;
        let rewritten = if kind == 52 {
            let (bytes, removed_slots) = rewrite_mhod52(chunk, &removed_positions)?;
            index_removals.insert(read_u32(chunk, mhod.header_length)?, removed_slots);
            bytes
        } else if kind == 53 {
            let sort_type = read_u32(chunk, mhod.header_length)?;
            rewrite_mhod53(
                chunk,
                index_removals.get(&sort_type).map_or(&[], Vec::as_slice),
            )?
        } else {
            chunk.to_vec()
        };
        output.extend_from_slice(&rewritten);
        offset = mhod.end;
    }

    let removed_track_ids: Vec<u32> = removed.iter().map(|track| track.track_id).collect();
    let removed_persistent_ids: Vec<PersistentId> =
        removed.iter().map(|track| track.persistent_id).collect();
    let mut retained_mhips = 0_usize;
    let mut mhips = Vec::with_capacity(mhip_count);
    let mut removed_order_values = Vec::new();
    for _ in 0..mhip_count {
        let mhip = chunk_header(playlist, offset, b"mhip")?;
        let chunk = &playlist[offset..mhip.end];
        let track_id = read_u32(chunk, MHIP_TRACK_ID)?;
        let persistent_id = (mhip.header_length >= MHIP_PERSISTENT_ID + 8)
            .then(|| read_u64(chunk, MHIP_PERSISTENT_ID))
            .transpose()?
            .map(PersistentId::from_bits);
        let remove = removed_track_ids.contains(&track_id)
            || persistent_id.is_some_and(|id| removed_persistent_ids.contains(&id));
        if remove {
            if let Some(value) = mhip_position(chunk)? {
                removed_order_values.push(value);
            }
        } else {
            mhips.push(chunk.to_vec());
            retained_mhips += 1;
        }
        offset = mhip.end;
    }
    if offset != playlist.len() {
        return Err(malformed(offset, "trailing bytes after mhyp children"));
    }
    for mhip in mhips {
        output.extend_from_slice(&rewrite_mhip_position(mhip, &removed_order_values)?);
    }
    write_u32(
        &mut output,
        16,
        u32::try_from(retained_mhips)
            .map_err(|_| verification("remaining playlist count exceeds u32"))?,
    )?;
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("rewritten playlist exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn rewrite_mhod52(chunk: &[u8], removed_positions: &[u32]) -> Result<(Vec<u8>, Vec<u32>)> {
    let header = chunk_header(chunk, 0, b"mhod")?;
    let body = header.header_length;
    if chunk.len().saturating_sub(body) < 48 {
        return Err(malformed(body, "type-52 mhod body is too short"));
    }
    let count = usize_value(read_u32(chunk, body + 4)?, body + 4)?;
    let entries = checked_end(body + 48, count.saturating_mul(4), chunk.len(), body + 4)?;
    if entries != chunk.len() {
        return Err(malformed(entries, "type-52 mhod has an unexpected size"));
    }
    let mut output = chunk[..body + 48].to_vec();
    let mut removed_slots = Vec::new();
    for index in 0..count {
        let value = read_u32(chunk, body + 48 + index * 4)?;
        if removed_positions.contains(&value) {
            removed_slots
                .push(u32::try_from(index).map_err(|_| verification("type-52 index exceeds u32"))?);
        } else {
            let shift = removed_positions
                .iter()
                .filter(|position| **position < value)
                .count();
            output.extend_from_slice(
                &value
                    .saturating_sub(u32::try_from(shift).unwrap_or(u32::MAX))
                    .to_le_bytes(),
            );
        }
    }
    write_u32(
        &mut output,
        body + 4,
        u32::try_from(count - removed_slots.len())
            .map_err(|_| verification("type-52 count exceeds u32"))?,
    )?;
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("type-52 mhod exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok((output, removed_slots))
}

fn rewrite_mhod53(chunk: &[u8], removed_slots: &[u32]) -> Result<Vec<u8>> {
    let header = chunk_header(chunk, 0, b"mhod")?;
    let body = header.header_length;
    if chunk.len().saturating_sub(body) < 16 {
        return Err(malformed(body, "type-53 mhod body is too short"));
    }
    let count = usize_value(read_u32(chunk, body + 4)?, body + 4)?;
    let entries = checked_end(body + 16, count.saturating_mul(12), chunk.len(), body + 4)?;
    if entries != chunk.len() {
        return Err(malformed(entries, "type-53 mhod has an unexpected size"));
    }
    let mut output = chunk[..body + 16].to_vec();
    let mut retained = 0_usize;
    for index in 0..count {
        let start = read_u32(chunk, body + 16 + index * 12 + 4)?;
        let item_count = read_u32(chunk, body + 16 + index * 12 + 8)?;
        let end = start.saturating_add(item_count);
        let removed_before = removed_slots.iter().filter(|slot| **slot < start).count();
        let removed_inside = removed_slots
            .iter()
            .filter(|slot| **slot >= start && **slot < end)
            .count();
        let new_count =
            item_count.saturating_sub(u32::try_from(removed_inside).unwrap_or(u32::MAX));
        if new_count == 0 {
            continue;
        }
        let entry_offset = body + 16 + index * 12;
        let mut entry = chunk[entry_offset..entry_offset + 12].to_vec();
        write_u32(
            &mut entry,
            4,
            start.saturating_sub(u32::try_from(removed_before).unwrap_or(u32::MAX)),
        )?;
        write_u32(&mut entry, 8, new_count)?;
        output.extend_from_slice(&entry);
        retained += 1;
    }
    write_u32(
        &mut output,
        body + 4,
        u32::try_from(retained).map_err(|_| verification("type-53 count exceeds u32"))?,
    )?;
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("type-53 mhod exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

/// Returns the end of an `mhod` child at `offset`, clamped to the parent
/// length, or `None` when the bytes there are not a plausible `mhod`.
///
/// Nano 7G playlists embed each track's playlist order as a single `mhod`
/// child inside its `mhip`, and the firmware may truncate the trailing bytes
/// of the last `mhip` while leaving the child's claimed total unchanged.
/// Clamping lets the walk survive that quirk instead of failing the whole
/// rewrite.
/// Returns the claimed end of an `mhod` child at `offset`, or `None` when
/// the bytes there are not a plausible `mhod`.
///
/// The claimed end may run past the parent: Nano 7G playlists embed each
/// track's playlist order as a single `mhod` child inside its `mhip`, and the
/// firmware may truncate the trailing bytes of the last `mhip` while leaving
/// the child's claimed total unchanged.
fn mhod_child_claimed_end(bytes: &[u8], offset: usize) -> Option<usize> {
    if bytes.get(offset..offset + 4) != Some(b"mhod") {
        return None;
    }
    let header_length = read_u32(bytes, offset + 4).ok()? as usize;
    let total_length = read_u32(bytes, offset + 8).ok()? as usize;
    if header_length < 12 || total_length < header_length {
        return None;
    }
    offset.checked_add(total_length)
}

/// Offset of the position value inside an mhip's embedded type-100 mhod.
///
/// The standard layout (Apple-written mhips and the fixture) stores the
/// position at `offset + 24`, after the mhod's 8-byte gap. The firmware's
/// truncated last mhip instead stores it at `offset + 16`; fall back to that
/// variant only when the child's claimed total runs past the parent.
fn mhip_value_offset(truncated: bool) -> usize {
    if truncated {
        16
    } else {
        24
    }
}

fn mhip_position(chunk: &[u8]) -> Result<Option<u32>> {
    let header = chunk_header(chunk, 0, b"mhip")?;
    if header.header_length < 16 {
        return Err(malformed(4, "mhip header is too short"));
    }
    let child_count = usize_value(read_u32(chunk, 12)?, 12)?;
    let mut offset = header.header_length;
    for _ in 0..child_count {
        let Some(claimed_end) = mhod_child_claimed_end(chunk, offset) else {
            break;
        };
        let truncated = claimed_end > chunk.len();
        let end = claimed_end.min(chunk.len());
        let value_offset = offset + mhip_value_offset(truncated);
        if read_u32(chunk, offset + 12)? == 100
            && value_offset + 4 <= chunk.len()
            && value_offset + 4 <= end
        {
            return read_u32(chunk, value_offset).map(Some);
        }
        offset = end;
    }
    Ok(None)
}

fn rewrite_mhip_position(mut chunk: Vec<u8>, removed_values: &[u32]) -> Result<Vec<u8>> {
    let header = chunk_header(&chunk, 0, b"mhip")?;
    let child_count = usize_value(read_u32(&chunk, 12)?, 12)?;
    let mut offset = header.header_length;
    for _ in 0..child_count {
        let Some(claimed_end) = mhod_child_claimed_end(&chunk, offset) else {
            break;
        };
        let truncated = claimed_end > chunk.len();
        let end = claimed_end.min(chunk.len());
        let value_offset = offset + mhip_value_offset(truncated);
        if read_u32(&chunk, offset + 12)? == 100 && value_offset + 4 <= chunk.len() {
            let value = read_u32(&chunk, value_offset)?;
            let shift = removed_values
                .iter()
                .filter(|removed| **removed < value)
                .count();
            write_u32(
                &mut chunk,
                value_offset,
                value.saturating_sub(u32::try_from(shift).unwrap_or(u32::MAX)),
            )?;
        }
        offset = end;
        if end == chunk.len() {
            // A firmware-truncated trailing child fills the mhip: keep the
            // chunk byte-for-byte otherwise (the firmware runs this exact
            // structure on the device), just stop the child walk here.
            break;
        }
    }
    Ok(chunk)
}

#[derive(Clone, Copy)]
pub(super) struct ChunkHeader {
    pub header_length: usize,
    pub end: usize,
}

pub(super) fn chunk_header(bytes: &[u8], offset: usize, magic: &[u8]) -> Result<ChunkHeader> {
    require_magic(bytes, offset, magic)?;
    let header_length = usize_value(read_u32(bytes, offset + 4)?, offset + 4)?;
    let total_length = usize_value(read_u32(bytes, offset + 8)?, offset + 8)?;
    if header_length < 12 || total_length < header_length {
        return Err(malformed(offset + 4, "invalid chunk lengths"));
    }
    let end = checked_end(offset, total_length, bytes.len(), offset + 8)?;
    Ok(ChunkHeader { header_length, end })
}

pub(super) fn require_magic(bytes: &[u8], offset: usize, expected: &[u8]) -> Result<()> {
    if expected.len() != 4 || bytes.get(offset..offset + 4) != Some(expected) {
        return Err(malformed(offset, "unexpected chunk magic"));
    }
    Ok(())
}

pub(super) fn checked_end(
    start: usize,
    length: usize,
    bound: usize,
    offset: usize,
) -> Result<usize> {
    let end = start
        .checked_add(length)
        .ok_or_else(|| malformed(offset, "chunk length overflow"))?;
    if end > bound {
        return Err(malformed(offset, "chunk extends beyond its parent"));
    }
    Ok(end)
}

pub(super) fn usize_value(value: u32, offset: usize) -> Result<usize> {
    usize::try_from(value).map_err(|_| malformed(offset, "value does not fit this host"))
}

pub(super) fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| malformed(offset, "truncated u32"))?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| malformed(offset, "truncated u32"))?,
    ))
}

pub(super) fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let value = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| malformed(offset, "truncated u64"))?;
    Ok(u64::from_le_bytes(
        value
            .try_into()
            .map_err(|_| malformed(offset, "truncated u64"))?,
    ))
}

pub(super) fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 2)
        .ok_or_else(|| malformed(offset, "truncated u16 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub(super) fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 4)
        .ok_or_else(|| malformed(offset, "truncated u32 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub(super) fn write_u64(bytes: &mut [u8], offset: usize, value: u64) -> Result<()> {
    let target = bytes
        .get_mut(offset..offset + 8)
        .ok_or_else(|| malformed(offset, "truncated u64 target"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub(super) fn malformed(offset: usize, reason: &str) -> Error {
    Error::Malformed {
        format: "iTunesCDB",
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
        reason: reason.to_owned(),
    }
}

pub(super) fn verification(reason: &str) -> Error {
    Error::Verification {
        format: "iTunesCDB",
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod device_cdb_tests {
    use std::path::Path;

    use crate::PersistentId;

    /// Walks the master playlist (kind-2 dataset) of a CDB and returns the
    /// embedded position value of every mhip, in playlist order. The standard
    /// layout stores the position at +24 inside the type-100 mhod child that
    /// starts at mhip+76; the firmware's truncated trailing mhip stores it at
    /// +16 instead.
    fn master_mhip_positions(cdb: &[u8]) -> Vec<u32> {
        let header_length = u32::from_le_bytes(cdb[4..8].try_into().unwrap()) as usize;
        let payload = super::super::cdb::decode_payload(cdb, header_length).unwrap();
        let datasets = u32::from_le_bytes(cdb[0x14..0x18].try_into().unwrap());
        let mut offset = 0usize;
        let mut positions = Vec::new();
        for _ in 0..datasets {
            let hdr =
                u32::from_le_bytes(payload[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let total =
                u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let kind = u32::from_le_bytes(payload[offset + 12..offset + 16].try_into().unwrap());
            if kind == 2 {
                let list = offset + hdr;
                let lh =
                    u32::from_le_bytes(payload[list + 4..list + 8].try_into().unwrap()) as usize;
                let count = u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap());
                let poff = list + lh;
                for _ in 0..count {
                    let ph = u32::from_le_bytes(payload[poff + 4..poff + 8].try_into().unwrap())
                        as usize;
                    let ic = u32::from_le_bytes(payload[poff + 16..poff + 20].try_into().unwrap());
                    let mc = u32::from_le_bytes(payload[poff + 12..poff + 16].try_into().unwrap());
                    let mut coff = poff + ph;
                    for _ in 0..mc {
                        coff += u32::from_le_bytes(payload[coff + 8..coff + 12].try_into().unwrap())
                            as usize;
                    }
                    for _ in 0..ic {
                        let mtot =
                            u32::from_le_bytes(payload[coff + 8..coff + 12].try_into().unwrap())
                                as usize;
                        let truncated = coff + 76 + 44 > coff + mtot;
                        let value_off = coff + 76 + if truncated { 16 } else { 24 };
                        positions.push(u32::from_le_bytes(
                            payload[value_off..value_off + 4].try_into().unwrap(),
                        ));
                        coff += mtot;
                    }
                }
            }
            offset += total;
        }
        positions
    }

    #[test]
    fn fixture_master_mhip_positions_are_consecutive() {
        // Apple's own master playlist embeds each track's playlist order in a
        // type-100 mhod child at +76 inside every mhip, with the position at
        // +24 (after the mhod's 8-byte gap). The values must be the running
        // playlist index; this guards the offset used by
        // mhip_position/rewrite_mhip_position.
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backup_7g/iPod_Control/iTunes/iTunesCDB");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let positions = master_mhip_positions(&bytes);
        let expected: Vec<u32> = (0..726).collect();
        assert_eq!(positions, expected);
    }

    #[test]
    fn removal_round_trips_the_copied_device_cdb() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("iTunesCDB");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let header_length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let payload = super::super::cdb::decode_payload(&bytes, header_length).unwrap();
        let datasets = u32::from_le_bytes(bytes[0x14..0x18].try_into().unwrap());
        let mut offset = 0;
        let mut pids = Vec::new();
        for _ in 0..datasets {
            let hdr =
                u32::from_le_bytes(payload[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let total =
                u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let kind = u32::from_le_bytes(payload[offset + 12..offset + 16].try_into().unwrap());
            if kind == 1 {
                let list = offset + hdr;
                let lh =
                    u32::from_le_bytes(payload[list + 4..list + 8].try_into().unwrap()) as usize;
                let count = u32::from_le_bytes(payload[list + 8..list + 12].try_into().unwrap());
                let mut toff = list + lh;
                for _ in 0..count {
                    let track_pid =
                        u64::from_le_bytes(payload[toff + 0x70..toff + 0x78].try_into().unwrap());
                    pids.push(PersistentId::from_bits(track_pid));
                    toff += u32::from_le_bytes(payload[toff + 8..toff + 12].try_into().unwrap())
                        as usize;
                }
            }
            offset += total;
        }
        assert!(pids.len() > 2, "device CDB has enough tracks");
        let first = super::remove_tracks_from_cdb(&bytes, [0u8; 8], &[pids[1]]);
        let rewritten = first
            .unwrap_or_else(|error| panic!("removal of the copied device CDB failed: {error:?}"));
        // The rewritten CDB must itself re-parse and re-sign: remove a second
        // track from our own output to prove the round trip stays sound.
        let second = super::remove_tracks_from_cdb(&rewritten, [0u8; 8], &[pids[2]]);
        let final_cdb = second.unwrap_or_else(|error| {
            panic!("second removal from rewritten device CDB failed: {error:?}")
        });
        // 726 tracks minus two removals: the master playlist must still carry
        // consecutive 0..723 positions, proving the shift logic now reads and
        // writes the real embedded values.
        let positions = master_mhip_positions(&final_cdb);
        let expected: Vec<u32> = (0..724).collect();
        assert_eq!(positions, expected);
    }
}
