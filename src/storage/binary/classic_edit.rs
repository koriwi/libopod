//! Classic (uncompressed) iTunesDB edits: track add/remove and NONE/HASH58
//! signing.
//!
//! The dataset rewrites are shared with the Nano 7G `iTunesCDB` path; only
//! the container differs (no zlib) and the signature uses the profile's
//! checksum scheme instead of HASHAB.

use std::collections::{BTreeMap, BTreeSet};

use super::cdb_add::{
    append_album, mhod_child, parse_string_mhod, resolve_album, rewrite_master_playlist_dataset,
    CdbTrackAddition,
};
use super::cdb_edit::{
    checked_end, chunk_header, read_u32, read_u64, require_magic, split_datasets, usize_value,
    write_u32, write_u64,
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

/// One requested standard-playlist mutation.
#[derive(Clone, Debug)]
pub(crate) enum ClassicPlaylistMutation {
    Create {
        id: PersistentId,
        name: String,
        track_ids: Vec<PersistentId>,
    },
    Update {
        id: PersistentId,
        name: Option<String>,
        track_ids: Option<Vec<PersistentId>>,
    },
    Delete {
        id: PersistentId,
    },
}

type PlaylistMhod = (u32, Vec<u8>);

impl ClassicPlaylistMutation {
    fn id(&self) -> PersistentId {
        match self {
            Self::Create { id, .. } | Self::Update { id, .. } | Self::Delete { id } => *id,
        }
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
    // Both playlist universes carry a master library on classic databases.
    // iOpenPod rebuilds the type-2 and type-3 masters from the complete track
    // list; leaving type 3 stale made the Nano 2G show only the tracks that
    // predated a libopod sync even though the type-1 list and type-2 master
    // contained every new track.
    let master_datasets: Vec<usize> = datasets
        .iter()
        .enumerate()
        .filter_map(|(index, dataset)| {
            read_u32(dataset, 12)
                .ok()
                .filter(|kind| matches!(*kind, 2 | 3))
                .map(|_| index)
        })
        .collect();
    if master_datasets.is_empty() {
        return Err(verification(
            "iTunesDB has no type-2/type-3 playlist dataset",
        ));
    }

    let (album_id, album_is_new) = resolve_album(&datasets[album_dataset], addition)?;
    // Match the firmware's `mhit` header size: read it from the first
    // existing track so a classic device (e.g. 0x248 on Nano 3G) gets a new
    // track with a header its parser already expects. For an empty library,
    // use iOpenPod's database-version mapping so the first track can be added.
    let db_version = read_u32(itunesdb, 0x10)?;
    let mhit_header_size = existing_mhit_header_size(&datasets[track_dataset], db_version)?;
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
    for master_dataset in master_datasets {
        datasets[master_dataset] = rewrite_master_playlist_dataset(
            &datasets[master_dataset],
            &existing_tracks,
            addition,
            next_track_id,
        )?;
    }

    let mut rewritten = Vec::new();
    for dataset in datasets {
        rewritten.extend_from_slice(&dataset);
    }
    finalize(itunesdb, header_length, &rewritten, checksum, guid)
}

/// Applies standard-playlist CRUD to both classic playlist universes and
/// re-signs the database. Master and smart playlists cannot be mutated.
pub(crate) fn edit_playlists(
    itunesdb: &[u8],
    checksum: ChecksumKind,
    guid: Option<&[u8; 8]>,
    mutations: &[ClassicPlaylistMutation],
) -> Result<Vec<u8>> {
    if mutations.is_empty() {
        return Err(verification("no playlist mutations were requested"));
    }
    let mut ids = BTreeSet::new();
    for mutation in mutations {
        if !ids.insert(mutation.id()) {
            return Err(verification("more than one mutation targets a playlist"));
        }
    }

    let track_ids: BTreeMap<PersistentId, u32> = super::classic::parse_library(itunesdb, None)?
        .tracks
        .into_iter()
        .map(|track| (track.persistent_id, track.track_id))
        .collect();
    for mutation in mutations {
        let members = match mutation {
            ClassicPlaylistMutation::Create { track_ids, .. }
            | ClassicPlaylistMutation::Update {
                track_ids: Some(track_ids),
                ..
            } => Some(track_ids),
            ClassicPlaylistMutation::Update {
                track_ids: None, ..
            }
            | ClassicPlaylistMutation::Delete { .. } => None,
        };
        if members.is_some_and(|members| members.iter().any(|id| !track_ids.contains_key(id))) {
            return Err(Error::TrackNotFound);
        }
    }

    let (header_length, payload_start) = header_and_payload(itunesdb)?;
    let dataset_count = usize::try_from(read_u32(itunesdb, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;
    let mut datasets = split_datasets(&itunesdb[payload_start..], dataset_count)?;
    let mut playlist_datasets = 0_usize;
    for dataset in &mut datasets {
        if matches!(read_u32(dataset, 12)?, 2 | 3) {
            *dataset = rewrite_playlist_crud_dataset(dataset, mutations, &track_ids)?;
            playlist_datasets += 1;
        }
    }
    if playlist_datasets == 0 {
        return Err(verification(
            "iTunesDB has no type-2/type-3 playlist dataset",
        ));
    }

    let rewritten = datasets.into_iter().flatten().collect::<Vec<_>>();
    finalize(itunesdb, header_length, &rewritten, checksum, guid)
}

fn rewrite_playlist_crud_dataset(
    dataset: &[u8],
    mutations: &[ClassicPlaylistMutation],
    track_ids: &BTreeMap<PersistentId, u32>,
) -> Result<Vec<u8>> {
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
    let mut found = BTreeSet::new();
    let mut template = None;
    let mut retained = 0_usize;
    let mut offset = body;
    for _ in 0..count {
        let header = chunk_header(dataset, offset, b"mhyp")?;
        let playlist = &dataset[offset..header.end];
        let id = playlist_id(playlist)?;
        let mutation = mutations.iter().find(|mutation| mutation.id() == id);
        if playlist[0x14] == 0 && !playlist_is_smart(playlist)? {
            template.get_or_insert_with(|| playlist.to_vec());
        }
        match mutation {
            Some(ClassicPlaylistMutation::Delete { .. }) => {
                require_editable_playlist(playlist)?;
                found.insert(id);
            }
            Some(ClassicPlaylistMutation::Update {
                name,
                track_ids: members,
                ..
            }) => {
                require_editable_playlist(playlist)?;
                output.extend_from_slice(&rewrite_standard_playlist(
                    playlist,
                    name.as_deref(),
                    members
                        .as_ref()
                        .map(|ids| resolve_track_ids(ids, track_ids)),
                )?);
                found.insert(id);
                retained += 1;
            }
            Some(ClassicPlaylistMutation::Create { .. }) => {
                return Err(verification("a new playlist ID already exists"));
            }
            None => {
                output.extend_from_slice(playlist);
                retained += 1;
            }
        }
        offset = header.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlp playlists"));
    }

    for mutation in mutations {
        match mutation {
            ClassicPlaylistMutation::Create {
                id,
                name,
                track_ids: members,
            } => {
                output.extend_from_slice(&build_standard_playlist(
                    template.as_deref().ok_or_else(|| {
                        verification(
                            "cannot create a playlist without a standard playlist template",
                        )
                    })?,
                    *id,
                    name,
                    &resolve_track_ids(members, track_ids),
                )?);
                retained += 1;
            }
            ClassicPlaylistMutation::Update { id, .. } | ClassicPlaylistMutation::Delete { id } => {
                if !found.contains(id) {
                    return Err(verification("the requested playlist ID is absent"));
                }
            }
        }
    }

    write_u32(
        &mut output,
        list + 8,
        u32::try_from(retained).map_err(|_| verification("playlist count exceeds u32"))?,
    )?;
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten playlist dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn playlist_id(playlist: &[u8]) -> Result<PersistentId> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    if header.header_length < 0x24 {
        return Err(malformed(
            4,
            "mhyp header is too short for its persistent ID",
        ));
    }
    Ok(PersistentId::from_bits(read_u64(playlist, 0x1c)?))
}

fn playlist_is_smart(playlist: &[u8]) -> Result<bool> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    let mhod_count = usize_value(read_u32(playlist, 12)?, 12)?;
    let mut offset = header.header_length;
    for _ in 0..mhod_count {
        let (kind, end, legacy) = mhod_child(playlist, offset)?;
        if matches!(kind, 50 | 51) {
            return Ok(true);
        }
        offset = parse_string_mhod(playlist, offset, legacy)?.map_or(end, |(_, end)| end);
    }
    Ok(false)
}

fn require_editable_playlist(playlist: &[u8]) -> Result<()> {
    if playlist[0x14] != 0 {
        return Err(Error::Unsupported {
            feature: "playlist mutation",
            reason: "master and hidden playlists cannot be edited".to_owned(),
        });
    }
    if playlist_is_smart(playlist)? {
        return Err(Error::Unsupported {
            feature: "playlist mutation",
            reason: "smart playlists cannot be edited yet".to_owned(),
        });
    }
    Ok(())
}

fn resolve_track_ids(
    members: &[PersistentId],
    track_ids: &BTreeMap<PersistentId, u32>,
) -> Vec<u32> {
    members
        .iter()
        .filter_map(|id| track_ids.get(id).copied())
        .collect()
}

fn playlist_mhods(playlist: &[u8]) -> Result<(Vec<PlaylistMhod>, usize)> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    let mhod_count = usize_value(read_u32(playlist, 12)?, 12)?;
    let mut mhods = Vec::with_capacity(mhod_count);
    let mut offset = header.header_length;
    for _ in 0..mhod_count {
        let (kind, end, legacy) = mhod_child(playlist, offset)?;
        let real_end = parse_string_mhod(playlist, offset, legacy)?.map_or(end, |(_, end)| end);
        mhods.push((kind, playlist[offset..real_end].to_vec()));
        offset = real_end;
    }
    Ok((mhods, offset))
}

fn rewrite_standard_playlist(
    playlist: &[u8],
    name: Option<&str>,
    members: Option<Vec<u32>>,
) -> Result<Vec<u8>> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    let (mhods, mut offset) = playlist_mhods(playlist)?;
    let mhip_count = usize_value(read_u32(playlist, 16)?, 16)?;
    let mut existing_mhips = Vec::with_capacity(mhip_count);
    for _ in 0..mhip_count {
        let mhip = chunk_header(playlist, offset, b"mhip")?;
        existing_mhips.push(playlist[offset..mhip.end].to_vec());
        offset = mhip.end;
    }
    if offset != playlist.len() {
        return Err(malformed(offset, "trailing bytes after mhyp children"));
    }

    let mut output = playlist[..header.header_length].to_vec();
    let mut replaced_name = false;
    for (kind, mhod) in mhods {
        if kind == 1 && name.is_some() {
            output.extend_from_slice(&build_string_mhod(1, name.unwrap_or_default())?);
            replaced_name = true;
        } else {
            output.extend_from_slice(&mhod);
        }
    }
    if name.is_some() && !replaced_name {
        return Err(verification("playlist has no name MHOD"));
    }
    if let Some(members) = members {
        let member_count = u32::try_from(members.len())
            .map_err(|_| verification("playlist member count exceeds u32"))?;
        for (position, track_id) in members.into_iter().enumerate() {
            output.extend_from_slice(&build_playlist_mhip(track_id, position)?);
        }
        write_u32(&mut output, 16, member_count)?;
    } else {
        for mhip in existing_mhips {
            output.extend_from_slice(&mhip);
        }
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("playlist exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn build_standard_playlist(
    template: &[u8],
    id: PersistentId,
    name: &str,
    members: &[u32],
) -> Result<Vec<u8>> {
    require_editable_playlist(template)?;
    let header = chunk_header(template, 0, b"mhyp")?;
    let (mhods, _) = playlist_mhods(template)?;
    let long_mhod = mhods
        .iter()
        .find(|(kind, _)| *kind == 100)
        .map(|(_, bytes)| bytes.as_slice())
        .ok_or_else(|| verification("playlist template has no type-100 MHOD"))?;
    let mut output = template[..header.header_length].to_vec();
    write_u32(&mut output, 12, 2)?;
    write_u32(
        &mut output,
        16,
        u32::try_from(members.len()).map_err(|_| verification("playlist is too large"))?,
    )?;
    output[0x14..0x18].fill(0);
    write_u32(&mut output, 0x18, mac_timestamp())?;
    write_u64(&mut output, 0x1c, id.to_bits())?;
    output.extend_from_slice(&build_string_mhod(1, name)?);
    output.extend_from_slice(long_mhod);
    for (position, track_id) in members.iter().copied().enumerate() {
        output.extend_from_slice(&build_playlist_mhip(track_id, position)?);
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("playlist exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn build_string_mhod(kind: u32, text: &str) -> Result<Vec<u8>> {
    let encoded = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let total = 40_usize
        .checked_add(encoded.len())
        .ok_or_else(|| verification("playlist name is too large"))?;
    let total_u32 = u32::try_from(total).map_err(|_| verification("playlist name is too large"))?;
    let encoded_u32 =
        u32::try_from(encoded.len()).map_err(|_| verification("playlist name is too large"))?;
    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(b"mhod");
    output.extend_from_slice(&24_u32.to_le_bytes());
    output.extend_from_slice(&total_u32.to_le_bytes());
    output.extend_from_slice(&kind.to_le_bytes());
    output.extend_from_slice(&[0; 8]);
    output.extend_from_slice(&1_u32.to_le_bytes());
    output.extend_from_slice(&encoded_u32.to_le_bytes());
    output.extend_from_slice(&1_u32.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&encoded);
    Ok(output)
}

fn build_playlist_mhip(track_id: u32, position: usize) -> Result<Vec<u8>> {
    let position = u32::try_from(position).map_err(|_| verification("playlist is too large"))?;
    let mut output = vec![0_u8; 76];
    output[..4].copy_from_slice(b"mhip");
    write_u32(&mut output, 4, 76)?;
    write_u32(&mut output, 8, 120)?;
    write_u32(&mut output, 12, 1)?;
    write_u32(&mut output, 0x18, track_id)?;
    output.extend_from_slice(b"mhod");
    output.extend_from_slice(&24_u32.to_le_bytes());
    output.extend_from_slice(&44_u32.to_le_bytes());
    output.extend_from_slice(&100_u32.to_le_bytes());
    output.extend_from_slice(&[0; 8]);
    output.extend_from_slice(&position.to_le_bytes());
    output.extend_from_slice(&[0; 16]);
    Ok(output)
}

fn mac_timestamp() -> u32 {
    const MAC_EPOCH_OFFSET: u64 = 2_082_844_800;
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    u32::try_from(unix.saturating_add(MAC_EPOCH_OFFSET)).unwrap_or(u32::MAX)
}

/// Reads the `mhit` header length used by the device's existing tracks, so
/// newly built tracks use the same header size the firmware expects.
fn existing_mhit_header_size(dataset: &[u8], db_version: u32) -> Result<usize> {
    let header = super::cdb_edit::chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlt")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    if count == 0 {
        return Ok(match db_version {
            0..=0x12 => 0x9c,
            0x13..=0x19 => 0x148,
            0x1a..=0x2d => 0x1f8,
            _ => 0x270,
        });
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
