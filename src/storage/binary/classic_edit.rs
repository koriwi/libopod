//! Classic (uncompressed) iTunesDB edits: track add/remove and NONE/HASH58
//! signing.
//!
//! The dataset rewrites are shared with the Nano 7G `iTunesCDB` path; only
//! the container differs (no zlib) and the signature uses the profile's
//! checksum scheme instead of HASHAB.

use super::cdb_add::{
    append_album, resolve_album, rewrite_master_playlist_dataset, CdbTrackAddition,
};
use super::cdb_edit::{
    checked_end, chunk_header, read_u32, read_u64, require_magic, split_datasets, usize_value,
    write_u32,
};
use crate::{ChecksumKind, Error, PersistentId, Result};

fn header_and_payload(itunesdb: &[u8]) -> Result<(usize, usize)> {
    if &itunesdb[..4] != b"mhbd" {
        return Err(Error::Malformed {
            format: "classic iTunesDB",
            offset: 0,
            reason: "expected an mhbd header".to_owned(),
        });
    }
    let header_length = usize::try_from(read_u32(itunesdb, 4)?)
        .map_err(|_| malformed(4, "mhbd header length does not fit this host"))?;
    if header_length > itunesdb.len() {
        return Err(malformed(4, "mhbd header exceeds the file length"));
    }
    Ok((header_length, header_length))
}

fn malformed(offset: usize, reason: &str) -> Error {
    Error::Malformed {
        format: "classic iTunesDB",
        offset: u64::try_from(offset).unwrap_or(u64::MAX),
        reason: reason.to_owned(),
    }
}

fn verification(reason: &str) -> Error {
    Error::Verification {
        format: "classic iTunesDB edit",
        reason: reason.to_owned(),
    }
}

/// Removes tracks from an uncompressed classic `iTunesDB` and re-signs it.
pub(crate) fn remove_tracks(
    itunesdb: &[u8],
    checksum: ChecksumKind,
    guid: Option<&[u8; 8]>,
    removals: &[PersistentId],
) -> Result<Vec<u8>> {
    if removals.is_empty() {
        return Err(verification("no iTunesDB removals were requested"));
    }
    let (header_length, payload_start) = header_and_payload(itunesdb)?;
    let payload = &itunesdb[payload_start..];
    let dataset_count = usize::try_from(read_u32(itunesdb, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;

    let mut datasets = split_datasets(payload, dataset_count)?;
    let track_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(1))
        .ok_or_else(|| verification("iTunesDB has no type-1 track dataset"))?;
    let (rewritten_tracks, removed) =
        super::cdb_edit::rewrite_track_dataset(&datasets[track_dataset], removals)?;
    datasets[track_dataset] = rewritten_tracks;

    for dataset in &mut datasets {
        let kind = read_u32(dataset, 12)?;
        if matches!(kind, 2 | 3 | 5) {
            *dataset = super::cdb_edit::rewrite_playlist_dataset(dataset, &removed)?;
        }
    }

    let mut rewritten = Vec::new();
    for dataset in datasets {
        rewritten.extend_from_slice(&dataset);
    }
    finalize(itunesdb, header_length, &rewritten, checksum, guid)
}

/// Adds one track to an uncompressed classic `iTunesDB` and re-signs it.
pub(crate) fn add_track(
    itunesdb: &[u8],
    checksum: ChecksumKind,
    guid: Option<&[u8; 8]>,
    addition: &CdbTrackAddition,
) -> Result<Vec<u8>> {
    let (header_length, payload_start) = header_and_payload(itunesdb)?;
    let payload = &itunesdb[payload_start..];
    let dataset_count = usize::try_from(read_u32(itunesdb, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;
    let db_id_2 = read_u64(itunesdb, 0x24)?;

    let mut datasets = split_datasets(payload, dataset_count)?;
    let track_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(1))
        .ok_or_else(|| verification("iTunesDB has no type-1 track dataset"))?;
    let album_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(4))
        .ok_or_else(|| verification("iTunesDB has no type-4 album dataset"))?;
    let master_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(2))
        .ok_or_else(|| verification("iTunesDB has no type-2 playlist dataset"))?;

    let (album_id, album_is_new) = resolve_album(&datasets[album_dataset], addition)?;
    // Match the firmware's `mhit` header size: read it from the first
    // existing track so a classic device (e.g. 0x248 on Nano 3G) gets a new
    // track with a header its parser already expects.
    let mhit_header_size = existing_mhit_header_size(&datasets[track_dataset])?;
    let (existing_tracks, rewritten_tracks, next_track_id, _artist_id_ref) =
        super::cdb_add::rewrite_track_dataset(
            &datasets[track_dataset],
            addition,
            db_id_2,
            album_id,
            mhit_header_size,
        )?;
    datasets[track_dataset] = rewritten_tracks;
    if album_is_new {
        datasets[album_dataset] = append_album(&datasets[album_dataset], album_id, addition)?;
    }
    datasets[master_dataset] = rewrite_master_playlist_dataset(
        &datasets[master_dataset],
        &existing_tracks,
        addition,
        next_track_id,
    )?;

    let mut rewritten = Vec::new();
    for dataset in datasets {
        rewritten.extend_from_slice(&dataset);
    }
    finalize(itunesdb, header_length, &rewritten, checksum, guid)
}

/// Reads the `mhit` header length used by the device's existing tracks, so
/// newly built tracks use the same header size the firmware expects.
fn existing_mhit_header_size(dataset: &[u8]) -> Result<usize> {
    let header = super::cdb_edit::chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlt")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    if count == 0 {
        return Err(verification("cannot add to an empty classic library"));
    }
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let track = chunk_header(dataset, body, b"mhit")?;
    Ok(track.header_length)
}

/// Reassembles the file, patches the total length, and applies the profile's
/// signature scheme (NONE or HASH58 for classic devices).
fn finalize(
    original: &[u8],
    header_length: usize,
    payload: &[u8],
    checksum: ChecksumKind,
    guid: Option<&[u8; 8]>,
) -> Result<Vec<u8>> {
    let mut output = original[..header_length].to_vec();
    output.extend_from_slice(payload);
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten iTunesDB exceeds 4 GiB"))?;
    write_u32(&mut output, 8, output_len)?;
    match checksum {
        ChecksumKind::None => Ok(output),
        ChecksumKind::Hash58 => {
            let guid = guid.ok_or_else(|| Error::Unsupported {
                feature: "HASH58 signing",
                reason: "the device FireWire GUID is required for HASH58".to_owned(),
            })?;
            crate::crypto::hash58::sign_database(guid, &mut output);
            if !crate::crypto::hash58::verify(guid, &output) {
                return Err(verification(
                    "rewritten iTunesDB HASH58 signature did not verify",
                ));
            }
            Ok(output)
        }
        other => Err(Error::Unsupported {
            feature: "classic iTunesDB edit",
            reason: format!("checksum scheme {other:?} is not a classic scheme"),
        }),
    }
}
