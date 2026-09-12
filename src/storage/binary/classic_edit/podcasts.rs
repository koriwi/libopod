//! Classic podcast playlists: flat in dataset 2, grouped by show in dataset 3.
//! Layout reference: iOpenPod's mhyp/mhip writers and playlist-datasets notes.
use std::collections::{BTreeMap, BTreeSet};

use super::{
    build_playlist_mhip, build_standard_playlist, build_string_mhod, checked_end, chunk_header,
    finalize, header_and_payload, playlist_id, playlist_mhods, read_u32, require_magic,
    rewrite_standard_playlist, split_datasets, usize_value, verification, write_u32,
};
use crate::{ChecksumKind, MediaKind, PersistentId, Result};

/// Synchronizes the special playlist after track edits, preserving its ID,
/// name and metadata. Unrelated playlists/datasets stay byte-identical.
#[allow(clippy::too_many_lines)]
pub(crate) fn sync_podcasts(
    database: &[u8],
    checksum: ChecksumKind,
    guid: Option<&[u8; 8]>,
) -> Result<Vec<u8>> {
    let library = super::super::classic::parse_library(database, None)?;
    let episodes: BTreeMap<_, _> = library
        .tracks
        .iter()
        .filter(|track| track.media_kind == MediaKind::Podcast)
        .map(|track| (track.track_id, track))
        .collect();
    let existing = library.playlists.iter().find(|p| p.is_podcast);
    if episodes.is_empty() && existing.is_none() {
        return Ok(database.to_vec());
    }
    let (header_length, _) = header_and_payload(database)?;
    let mut datasets = split_datasets(
        &database[header_length..],
        usize_value(read_u32(database, 0x14)?, 0x14)?,
    )?;
    // Older libraries may lack dataset 3. Seed it from the normal universe,
    // then replace just the Podcasts playlist with the grouped representation.
    for kind in [2, 3] {
        let count = datasets
            .iter()
            .filter(|d| read_u32(d, 12).ok() == Some(kind))
            .count();
        if count > 1 {
            return Err(verification("duplicate classic playlist dataset"));
        }
        if count == 0 {
            let source = datasets
                .iter()
                .find(|d| read_u32(d, 12).ok() == Some(5 - kind))
                .ok_or_else(|| verification("no classic playlist dataset"))?;
            let mut copy = source.clone();
            write_u32(&mut copy, 12, kind)?;
            datasets.push(copy);
        }
    }
    let mut template: Option<Vec<u8>> = None;
    let mut master = None;
    let mut used_ids = BTreeSet::new();
    let mut next_group_id = 1_u32;
    for kind in [2, 3] {
        let dataset = datasets
            .iter()
            .find(|d| read_u32(d, 12).ok() == Some(kind))
            .expect("ensured above");
        let (_, _, playlists) = playlist_chunks(dataset)?;
        let mut podcast_count = 0;
        for playlist in playlists {
            used_ids.insert(playlist_id(playlist)?);
            if playlist[0x14] == 1 {
                master.get_or_insert_with(|| playlist.to_vec());
            }
            if is_podcast(playlist)? {
                podcast_count += 1;
                if let Some(ref prior) = template {
                    if playlist_id(prior)? != playlist_id(playlist)? {
                        return Err(verification(
                            "podcast playlist IDs disagree between datasets",
                        ));
                    }
                } else {
                    template = Some(playlist.to_vec());
                }
            }
            let (_, mut offset) = playlist_mhods(playlist)?;
            for _ in 0..read_u32(playlist, 16)? {
                let item = chunk_header(playlist, offset, b"mhip")?;
                let id = read_u32(playlist, offset + 0x14)?;
                next_group_id = next_group_id.max(
                    id.checked_add(1)
                        .ok_or_else(|| verification("podcast group ID overflow"))?,
                );
                offset = item.end;
            }
        }
        if podcast_count > 1 {
            return Err(verification("multiple Podcasts playlists in one dataset"));
        }
    }
    let template = if let Some(template) = template {
        template
    } else {
        let id = loop {
            let id = PersistentId::from_bits(crate::random::next_u64());
            if id.to_bits() != 0 && !used_ids.contains(&id) {
                break id;
            }
        };
        let mut playlist = build_standard_playlist(
            master
                .as_deref()
                .ok_or_else(|| verification("no master playlist template"))?,
            id,
            "Podcasts",
            &[],
        )?;
        playlist[0x2a] |= 1;
        playlist
    };
    // Preserve the existing flat episode order; append newly added episodes.
    let mut members = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(existing) = existing {
        for id in &existing.track_ids {
            if episodes.contains_key(id) && seen.insert(*id) {
                members.push(*id);
            }
        }
    }
    for track in &library.tracks {
        if episodes.contains_key(&track.track_id) && seen.insert(track.track_id) {
            members.push(track.track_id);
        }
    }
    let flat = rewrite_standard_playlist(&template, None, Some(members.clone()))?;
    let grouped = grouped_playlist(&template, &members, &episodes, next_group_id)?;
    for dataset in &mut datasets {
        match read_u32(dataset, 12)? {
            2 => *dataset = replace_podcast(dataset, &flat)?,
            3 => *dataset = replace_podcast(dataset, &grouped)?,
            _ => {}
        }
    }
    // Firmware expects the podcast-capable universe before the legacy one.
    let two = datasets
        .iter()
        .position(|d| read_u32(d, 12).ok() == Some(2))
        .expect("ensured above");
    let three = datasets
        .iter()
        .position(|d| read_u32(d, 12).ok() == Some(3))
        .expect("ensured above");
    if three > two {
        let dataset = datasets.remove(three);
        datasets.insert(two, dataset);
    }
    let mut header = database[..header_length].to_vec();
    write_u32(
        &mut header,
        0x14,
        u32::try_from(datasets.len()).map_err(|_| verification("too many datasets"))?,
    )?;
    finalize(&header, header_length, &datasets.concat(), checksum, guid)
}

fn is_podcast(playlist: &[u8]) -> Result<bool> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    Ok(header.header_length >= 0x2c && playlist[0x2a] & 1 != 0)
}

fn playlist_chunks(dataset: &[u8]) -> Result<(usize, usize, Vec<&[u8]>)> {
    let list = chunk_header(dataset, 0, b"mhsd")?.header_length;
    require_magic(dataset, list, b"mhlp")?;
    let body = checked_end(
        list,
        usize_value(read_u32(dataset, list + 4)?, list + 4)?,
        dataset.len(),
        list + 4,
    )?;
    let mut offset = body;
    let mut playlists = Vec::new();
    for _ in 0..read_u32(dataset, list + 8)? {
        let header = chunk_header(dataset, offset, b"mhyp")?;
        playlists.push(&dataset[offset..header.end]);
        offset = header.end;
    }
    if offset != dataset.len() {
        return Err(verification("trailing playlist dataset bytes"));
    }
    Ok((list, body, playlists))
}

fn replace_podcast(dataset: &[u8], replacement: &[u8]) -> Result<Vec<u8>> {
    let (list, body, playlists) = playlist_chunks(dataset)?;
    let mut output = dataset[..body].to_vec();
    let mut replaced = false;
    let mut count = playlists.len();
    for playlist in playlists {
        if is_podcast(playlist)? {
            output.extend_from_slice(replacement);
            replaced = true;
        } else {
            output.extend_from_slice(playlist);
        }
    }
    if !replaced {
        output.extend_from_slice(replacement);
        count += 1;
    }
    write_u32(
        &mut output,
        list + 8,
        u32::try_from(count).map_err(|_| verification("too many playlists"))?,
    )?;
    let len = output.len();
    write_u32(
        &mut output,
        8,
        u32::try_from(len).map_err(|_| verification("playlist dataset too large"))?,
    )?;
    Ok(output)
}

fn grouped_playlist(
    template: &[u8],
    members: &[u32],
    episodes: &BTreeMap<u32, &super::super::classic::ClassicTrack>,
    mut next_id: u32,
) -> Result<Vec<u8>> {
    let (_, body) = playlist_mhods(template)?;
    let mut output = template[..body].to_vec();
    let mut groups: Vec<(&str, Vec<u32>)> = Vec::new();
    for id in members {
        let album = episodes[id].album.as_str();
        if let Some((_, tracks)) = groups.iter_mut().find(|(name, _)| *name == album) {
            tracks.push(*id);
        } else {
            groups.push((album, vec![*id]));
        }
    }
    let count = members.len() + groups.len();
    for (album, tracks) in groups {
        let group_id = next_id;
        next_id = next_id
            .checked_add(1)
            .ok_or_else(|| verification("podcast group ID overflow"))?;
        let title = build_string_mhod(1, if album.is_empty() { "Unknown" } else { album })?;
        let mut group = vec![0_u8; 76];
        group[..4].copy_from_slice(b"mhip");
        write_u32(&mut group, 4, 76)?;
        write_u32(
            &mut group,
            8,
            u32::try_from(76 + title.len()).map_err(|_| verification("podcast title too large"))?,
        )?;
        write_u32(&mut group, 12, 1)?;
        write_u32(&mut group, 0x10, 0x100)?;
        write_u32(&mut group, 0x14, group_id)?;
        group.extend_from_slice(&title);
        output.extend(group);
        for id in tracks {
            let mut item = build_playlist_mhip(id, usize_value(next_id, 0)?)?;
            write_u32(&mut item, 0x14, next_id)?;
            write_u32(&mut item, 0x20, group_id)?;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| verification("podcast group ID overflow"))?;
            output.extend(item);
        }
    }
    write_u32(
        &mut output,
        16,
        u32::try_from(count).map_err(|_| verification("too many podcast entries"))?,
    )?;
    let len = output.len();
    write_u32(
        &mut output,
        8,
        u32::try_from(len).map_err(|_| verification("podcast playlist too large"))?,
    )?;
    Ok(output)
}
