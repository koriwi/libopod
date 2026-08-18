//! Schema-preserving `iTunesCDB` track insertion for Nano 6G/7G databases.
//!
//! The rewrite appends one new `mhit` track to the type-1 track dataset,
//! links or appends its album in the `mhla` dataset, appends a matching
//! `mhip` to the master playlist, and rebuilds every type-52 sorted index
//! and type-53 jump table pair so the new track appears in the correct
//! browse position. The database is recompressed and signed with the
//! exact-final-byte HASHAB scheme.

use std::collections::HashMap;

use crate::edit::sort::{jump_letter, sort_key};
use crate::{PersistentId, Result};

use super::cdb::decode_payload;
use super::cdb_edit::{
    checked_end, chunk_header, finalize_cdb, malformed, read_u32, read_u64, require_magic,
    usize_value, verification, write_u16, write_u32, write_u64,
};

const MHIT_HEADER_SIZE: usize = 0x270;
const MHIP_HEADER_SIZE: usize = 0x4c;
const MHIA_HEADER_SIZE: usize = 0x58;

const MHIT_CHILD_COUNT: usize = 0x0c;
const MHIT_TRACK_ID: usize = 0x10;
const MHIT_FILE_TYPE: usize = 0x18;
const MHIT_SIZE: usize = 0x24;
const MHIT_LENGTH: usize = 0x28;
const MHIT_TRACK_NUMBER: usize = 0x2c;
const MHIT_TOTAL_TRACKS: usize = 0x30;
const MHIT_YEAR: usize = 0x34;
const MHIT_BITRATE: usize = 0x38;
const MHIT_SAMPLE_RATE_1: usize = 0x3c;
const MHIT_DISC_NUMBER: usize = 0x5c;
const MHIT_TOTAL_DISCS: usize = 0x60;
const MHIT_DB_TRACK_ID: usize = 0x70;
const MHIT_MEDIA_TYPE: usize = 0xd0;
const MHIT_SEASON: usize = 0xd4;
const MHIT_EPISODE: usize = 0xd8;

const MHIT_DB_ID2_REF: usize = 0x124;
const MHIT_ARTIST_ID_REF: usize = 0x1e0;

const MHOD_TYPE_TITLE: u32 = 1;
const MHOD_TYPE_LOCATION: u32 = 2;
const MHOD_TYPE_ALBUM: u32 = 3;
const MHOD_TYPE_ARTIST: u32 = 4;
const MHOD_TYPE_GENRE: u32 = 5;
const MHOD_TYPE_COMPOSER: u32 = 12;
const MHOD_TYPE_SHOW_NAME: u32 = 19;
const MHOD_TYPE_ALBUM_ARTIST: u32 = 22;
const MHOD_TYPE_SORT_ARTIST: u32 = 23;
const MHOD_TYPE_SORT_NAME: u32 = 27;
const MHOD_TYPE_SORT_ALBUM: u32 = 28;
const MHOD_TYPE_SORT_ALBUM_ARTIST: u32 = 29;
const MHOD_TYPE_SORT_COMPOSER: u32 = 30;
const MHOD_TYPE_SORT_SHOW: u32 = 31;

const SORT_TITLE: u32 = 0x03;
const SORT_ALBUM: u32 = 0x04;
const SORT_ARTIST: u32 = 0x05;
const SORT_GENRE: u32 = 0x07;
const SORT_COMPOSER: u32 = 0x12;
const SORT_SHOW: u32 = 0x1d;
const SORT_SEASON: u32 = 0x1e;
const SORT_EPISODE: u32 = 0x1f;
const SORT_ALBUM_ARTIST: u32 = 0x23;

/// Reused-artwork link written into a new track's MHIT header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CdbArtworkLink {
    pub image_id: u32,
    pub src_img_size: u32,
}

/// Metadata for one staged track insertion into `iTunesCDB`.
///
/// The CDB-internal `track_id`, `album_id` (MHLA), and `artist_id_ref`
/// values are derived from the existing database during the rewrite.
#[derive(Clone, Debug)]
pub(crate) struct CdbTrackAddition {
    pub persistent_id: PersistentId,
    pub location: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub composer: Option<String>,
    pub file_size: u32,
    pub length_ms: u32,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub track_number: u32,
    pub total_tracks: u32,
    pub disc_number: u32,
    pub total_discs: u32,
    pub year: u32,
    pub compilation: bool,
    pub date_mac: u32,
    pub artwork: Option<CdbArtworkLink>,
}

pub(crate) fn add_track_to_cdb(
    bytes: &[u8],
    firewire_guid: [u8; 8],
    addition: &CdbTrackAddition,
) -> Result<Vec<u8>> {
    let header_length = usize::try_from(read_u32(bytes, 4)?)
        .map_err(|_| malformed(4, "mhbd header length does not fit this host"))?;
    let payload = decode_payload(bytes, header_length)?;
    let dataset_count = usize::try_from(read_u32(bytes, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;
    let db_id_2 = read_u64(bytes, 0x24)?;

    let mut datasets = split_datasets(&payload, dataset_count)?;
    let track_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(1))
        .ok_or_else(|| verification("CDB has no type-1 track dataset"))?;
    let album_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(4))
        .ok_or_else(|| verification("CDB has no type-4 album dataset"))?;
    let master_dataset = datasets
        .iter()
        .position(|dataset| read_u32(dataset, 12).ok() == Some(2))
        .ok_or_else(|| verification("CDB has no type-2 playlist dataset"))?;

    let (album_id, album_is_new) = resolve_album(&datasets[album_dataset], addition)?;

    let (existing_tracks, rewritten_tracks, next_track_id, artist_id_ref) =
        rewrite_track_dataset(&datasets[track_dataset], addition, db_id_2, album_id)?;
    datasets[track_dataset] = rewritten_tracks;

    if album_is_new {
        datasets[album_dataset] = append_album(&datasets[album_dataset], album_id, addition)?;
    }

    let master = &datasets[master_dataset];
    datasets[master_dataset] =
        rewrite_master_playlist_dataset(master, &existing_tracks, addition, next_track_id)?;
    let _ = artist_id_ref;

    let mut uncompressed = Vec::new();
    for dataset in datasets {
        uncompressed.extend_from_slice(&dataset);
    }
    finalize_cdb(&bytes[..header_length], &uncompressed, firewire_guid)
}

/// A parsed existing track with everything needed for index rebuilds.
#[derive(Clone, Debug)]
struct ParsedTrack {
    position: u32,
    persistent_id: PersistentId,
    track_id: u32,
    artist_id_ref: u32,
    track_number: u32,
    disc_number: u32,
    season: u32,
    episode: u32,
    title: String,
    album: String,
    artist: String,
    genre: String,
    composer: String,
    album_artist: String,
    show_name: String,
    sort_name: String,
    sort_album: String,
    sort_artist: String,
    sort_album_artist: String,
    sort_composer: String,
    sort_show: String,
}

/// One comparable component of a sort key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SortField {
    Text(String),
    Number(u32),
}

fn parse_track(chunk: &[u8], position: u32) -> Result<ParsedTrack> {
    let header = chunk_header(chunk, 0, b"mhit")?;
    if header.header_length < MHIT_HEADER_SIZE {
        return Err(malformed(
            4,
            "mhit header is too short for index rebuilding",
        ));
    }
    let mut strings = HashMap::new();
    let mut offset = header.header_length;
    for _ in 0..usize_value(read_u32(chunk, MHIT_CHILD_COUNT)?, MHIT_CHILD_COUNT)? {
        let child = chunk_header(chunk, offset, b"mhod")?;
        let mhod_type = read_u32(chunk, offset + 12)?;
        if let Some(text) = parse_string_mhod(chunk, offset)? {
            strings.insert(mhod_type, text);
        }
        offset = child.end;
    }
    if offset != chunk.len() {
        return Err(malformed(offset, "trailing bytes after mhit children"));
    }
    Ok(ParsedTrack {
        position,
        persistent_id: PersistentId::from_bits(read_u64(chunk, MHIT_DB_TRACK_ID)?),
        track_id: read_u32(chunk, MHIT_TRACK_ID)?,
        artist_id_ref: read_u32(chunk, MHIT_ARTIST_ID_REF)?,
        track_number: read_u32(chunk, MHIT_TRACK_NUMBER)?,
        disc_number: read_u32(chunk, MHIT_DISC_NUMBER)?,
        season: read_u32(chunk, MHIT_SEASON)?,
        episode: read_u32(chunk, MHIT_EPISODE)?,
        title: string_or(&strings, MHOD_TYPE_TITLE),
        album: string_or(&strings, MHOD_TYPE_ALBUM),
        artist: string_or(&strings, MHOD_TYPE_ARTIST),
        genre: string_or(&strings, MHOD_TYPE_GENRE),
        composer: string_or(&strings, MHOD_TYPE_COMPOSER),
        album_artist: string_or(&strings, MHOD_TYPE_ALBUM_ARTIST),
        show_name: string_or(&strings, MHOD_TYPE_SHOW_NAME),
        sort_name: string_or(&strings, MHOD_TYPE_SORT_NAME),
        sort_album: string_or(&strings, MHOD_TYPE_SORT_ALBUM),
        sort_artist: string_or(&strings, MHOD_TYPE_SORT_ARTIST),
        sort_album_artist: string_or(&strings, MHOD_TYPE_SORT_ALBUM_ARTIST),
        sort_composer: string_or(&strings, MHOD_TYPE_SORT_COMPOSER),
        sort_show: string_or(&strings, MHOD_TYPE_SORT_SHOW),
    })
}

fn string_or(strings: &HashMap<u32, String>, mhod_type: u32) -> String {
    strings.get(&mhod_type).cloned().unwrap_or_default()
}

fn parse_string_mhod(chunk: &[u8], offset: usize) -> Result<Option<String>> {
    let header = chunk_header(chunk, offset, b"mhod")?;
    let body = offset + header.header_length;
    if chunk.len().saturating_sub(body) < 16 {
        return Ok(None);
    }
    let encoding = read_u32(chunk, body)?;
    let byte_length = usize_value(read_u32(chunk, body + 4)?, body + 4)?;
    if encoding != 1 {
        return Ok(None);
    }
    let data = chunk
        .get(body + 16..body + 16 + byte_length)
        .ok_or_else(|| malformed(body + 16, "string MHOD length exceeds its chunk"))?;
    let mut units = Vec::with_capacity(byte_length / 2);
    for pair in data.chunks_exact(2) {
        units.push(u16::from_le_bytes([pair[0], pair[1]]));
    }
    Ok(Some(String::from_utf16_lossy(&units)))
}

fn rewrite_track_dataset(
    dataset: &[u8],
    addition: &CdbTrackAddition,
    db_id_2: u64,
    album_id: u32,
) -> Result<(Vec<ParsedTrack>, Vec<u8>, u32, u32)> {
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
    let mut tracks = Vec::with_capacity(count + 1);
    let mut next_track_id = 0_u32;
    let mut offset = body;
    for position in 0..count {
        let track = chunk_header(dataset, offset, b"mhit")?;
        let parsed = parse_track(
            &dataset[offset..track.end],
            u32::try_from(position).map_err(|_| verification("CDB track position exceeds u32"))?,
        )?;
        if parsed.persistent_id == addition.persistent_id {
            return Err(verification(
                "a track with the requested persistent ID already exists",
            ));
        }
        next_track_id = next_track_id.max(parsed.track_id);
        output.extend_from_slice(&dataset[offset..track.end]);
        tracks.push(parsed);
        offset = track.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlt tracks"));
    }
    let next_track_id = next_track_id
        .checked_add(1)
        .ok_or_else(|| verification("CDB track ID overflow"))?;
    let artist_id_ref = resolve_artist_id_ref(&tracks, addition);
    output.extend_from_slice(&build_mhit(
        addition,
        db_id_2,
        next_track_id,
        album_id,
        artist_id_ref,
    ));
    write_u32(
        &mut output,
        list + 8,
        u32::try_from(count + 1).map_err(|_| verification("CDB track count exceeds u32"))?,
    )?;
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten track dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok((tracks, output, next_track_id, artist_id_ref))
}

fn resolve_artist_id_ref(tracks: &[ParsedTrack], addition: &CdbTrackAddition) -> u32 {
    let artist = addition.artist.as_deref().unwrap_or("");
    let mut next = 0_u32;
    let mut matching = None;
    for track in tracks {
        next = next.max(track.artist_id_ref);
        if matching.is_none() && !track.artist.is_empty() && track.artist == artist {
            matching = Some(track.artist_id_ref);
        }
    }
    matching.unwrap_or_else(|| next.saturating_add(1))
}

fn resolve_album(dataset: &[u8], addition: &CdbTrackAddition) -> Result<(u32, bool)> {
    let albums = parse_albums(dataset)?;
    let album_name = addition.album.clone().unwrap_or_default();
    let album_artist = addition
        .album_artist
        .clone()
        .or_else(|| addition.artist.clone())
        .unwrap_or_default();
    if let Some(album) = find_album(&albums, &album_name, &album_artist) {
        Ok((album.id, false))
    } else {
        let next = albums
            .iter()
            .map(|album| album.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| verification("album ID overflow"))?;
        Ok((next, true))
    }
}

fn parse_albums(dataset: &[u8]) -> Result<Vec<ParsedAlbum>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhla")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut albums = Vec::with_capacity(count);
    let mut offset = body;
    for _ in 0..count {
        let album = chunk_header(dataset, offset, b"mhia")?;
        albums.push(parse_album(&dataset[offset..album.end])?);
        offset = album.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhla albums"));
    }
    Ok(albums)
}

fn append_album(dataset: &[u8], album_id: u32, addition: &CdbTrackAddition) -> Result<Vec<u8>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhla")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut output = dataset[..body].to_vec();
    let mut offset = body;
    for _ in 0..count {
        let album = chunk_header(dataset, offset, b"mhia")?;
        output.extend_from_slice(&dataset[offset..album.end]);
        offset = album.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhla albums"));
    }
    let album_name = addition.album.clone().unwrap_or_default();
    let album_artist = addition
        .album_artist
        .clone()
        .or_else(|| addition.artist.clone())
        .unwrap_or_default();
    output.extend_from_slice(&build_mhia(
        album_id,
        &album_name,
        &album_artist,
        addition.persistent_id,
    ));
    write_u32(
        &mut output,
        list + 8,
        u32::try_from(count + 1).map_err(|_| verification("CDB album count exceeds u32"))?,
    )?;
    let output_len = u32::try_from(output.len())
        .map_err(|_| verification("rewritten album dataset exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

#[derive(Clone, Debug)]
struct ParsedAlbum {
    id: u32,
    name: String,
    artist: String,
}

fn parse_album(chunk: &[u8]) -> Result<ParsedAlbum> {
    let header = chunk_header(chunk, 0, b"mhia")?;
    let mut name = String::new();
    let mut artist = String::new();
    let mut offset = header.header_length;
    let child_count = usize_value(read_u32(chunk, 12)?, 12)?;
    for _ in 0..child_count {
        let child = chunk_header(chunk, offset, b"mhod")?;
        let mhod_type = read_u32(chunk, offset + 12)?;
        if let Some(text) = parse_string_mhod(chunk, offset)? {
            if mhod_type == 200 {
                name = text;
            } else if mhod_type == 201 {
                artist = text;
            }
        }
        offset = child.end;
    }
    if offset != chunk.len() {
        return Err(malformed(offset, "trailing bytes after mhia children"));
    }
    Ok(ParsedAlbum {
        id: read_u32(chunk, 0x10)?,
        name,
        artist,
    })
}

fn find_album<'a>(albums: &'a [ParsedAlbum], name: &str, artist: &str) -> Option<&'a ParsedAlbum> {
    if artist.is_empty() {
        albums
            .iter()
            .find(|album| album.name == name && album.artist.is_empty())
            .or_else(|| albums.iter().find(|album| album.name == name))
    } else {
        albums
            .iter()
            .find(|album| album.name == name && album.artist == artist)
            .or_else(|| {
                albums
                    .iter()
                    .find(|album| album.name == name && album.artist.is_empty())
            })
    }
}

fn rewrite_master_playlist_dataset(
    dataset: &[u8],
    tracks: &[ParsedTrack],
    addition: &CdbTrackAddition,
    track_id: u32,
) -> Result<Vec<u8>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlp")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    if count == 0 {
        return Err(verification("CDB has no playlists in its type-2 dataset"));
    }
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut output = dataset[..body].to_vec();
    let mut offset = body;
    for position in 0..count {
        let playlist = chunk_header(dataset, offset, b"mhyp")?;
        let rewritten = if position == 0 {
            rewrite_master_playlist(&dataset[offset..playlist.end], tracks, addition, track_id)?
        } else {
            dataset[offset..playlist.end].to_vec()
        };
        output.extend_from_slice(&rewritten);
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

fn rewrite_master_playlist(
    playlist: &[u8],
    tracks: &[ParsedTrack],
    addition: &CdbTrackAddition,
    track_id: u32,
) -> Result<Vec<u8>> {
    let header = chunk_header(playlist, 0, b"mhyp")?;
    if header.header_length < 20 {
        return Err(malformed(4, "mhyp header is too short"));
    }
    let mhod_count = usize_value(read_u32(playlist, 12)?, 12)?;
    let mhip_count = usize_value(read_u32(playlist, 16)?, 16)?;
    let mut output = playlist[..header.header_length].to_vec();
    let mut offset = header.header_length;
    let mut sort_types = Vec::new();

    for _ in 0..mhod_count {
        let mhod = chunk_header(playlist, offset, b"mhod")?;
        let kind = read_u32(playlist, offset + 12)?;
        let chunk = &playlist[offset..mhod.end];
        let rewritten = if kind == 52 {
            let sort_type = read_u32(chunk, mhod.header_length)?;
            sort_types.push(sort_type);
            rebuild_mhod52(chunk, tracks, addition, sort_type)?
        } else if kind == 53 {
            let sort_type = read_u32(chunk, mhod.header_length)?;
            if !sort_types.contains(&sort_type) {
                return Err(verification(
                    "type-53 jump table has no preceding type-52 index",
                ));
            }
            rebuild_mhod53(chunk, tracks, addition, sort_type)?
        } else {
            chunk.to_vec()
        };
        output.extend_from_slice(&rewritten);
        offset = mhod.end;
    }

    let mut position = usize_value(read_u32(playlist, 16)?, 16)?;
    for _ in 0..mhip_count {
        let mhip = chunk_header(playlist, offset, b"mhip")?;
        output.extend_from_slice(&playlist[offset..mhip.end]);
        offset = mhip.end;
    }
    if offset != playlist.len() {
        return Err(malformed(offset, "trailing bytes after mhyp children"));
    }
    let position_u32 = u32::try_from(position)
        .map_err(|_| verification("master playlist position exceeds u32"))?;
    output.extend_from_slice(&build_mhip(track_id, position_u32));
    position += 1;
    write_u32(
        &mut output,
        16,
        u32::try_from(position).map_err(|_| verification("master playlist count exceeds u32"))?,
    )?;
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("rewritten playlist exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

/// Builds the sorted index entries for one sort category, including the new
/// track, and returns the sorted positions plus the grouped jump entries.
fn sorted_index(
    tracks: &[ParsedTrack],
    addition: &CdbTrackAddition,
    sort_type: u32,
) -> Result<Vec<(SortFields, u32)>> {
    let mut indexed: Vec<(SortFields, u32)> = tracks
        .iter()
        .map(|track| {
            let view = CdbSortView::from_track(track);
            (sort_fields(&view, sort_type), track.position)
        })
        .collect();
    let new_track = CdbSortView::from_addition(addition);
    let new_position =
        u32::try_from(tracks.len()).map_err(|_| verification("new track position exceeds u32"))?;
    indexed.push((sort_fields(&new_track, sort_type), new_position));
    indexed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(indexed)
}

struct CdbSortView {
    title: String,
    album: String,
    artist: String,
    genre: String,
    composer: String,
    album_artist: String,
    show_name: String,
    sort_name: String,
    sort_album: String,
    sort_artist: String,
    sort_album_artist: String,
    sort_composer: String,
    sort_show: String,
    track_number: u32,
    disc_number: u32,
    season: u32,
    episode: u32,
}

impl CdbSortView {
    fn from_track(track: &ParsedTrack) -> Self {
        Self {
            title: track.title.clone(),
            album: track.album.clone(),
            artist: track.artist.clone(),
            genre: track.genre.clone(),
            composer: track.composer.clone(),
            album_artist: track.album_artist.clone(),
            show_name: track.show_name.clone(),
            sort_name: track.sort_name.clone(),
            sort_album: track.sort_album.clone(),
            sort_artist: track.sort_artist.clone(),
            sort_album_artist: track.sort_album_artist.clone(),
            sort_composer: track.sort_composer.clone(),
            sort_show: track.sort_show.clone(),
            track_number: track.track_number,
            disc_number: track.disc_number,
            season: track.season,
            episode: track.episode,
        }
    }

    fn from_addition(addition: &CdbTrackAddition) -> Self {
        Self {
            title: addition.title.clone(),
            album: addition.album.clone().unwrap_or_default(),
            artist: addition.artist.clone().unwrap_or_default(),
            genre: addition.genre.clone().unwrap_or_default(),
            composer: addition.composer.clone().unwrap_or_default(),
            album_artist: addition
                .album_artist
                .clone()
                .or_else(|| addition.artist.clone())
                .unwrap_or_default(),
            show_name: String::new(),
            sort_name: String::new(),
            sort_album: String::new(),
            sort_artist: String::new(),
            sort_album_artist: String::new(),
            sort_composer: String::new(),
            sort_show: String::new(),
            track_number: addition.track_number,
            disc_number: addition.disc_number,
            season: 0,
            episode: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SortFields(Vec<SortField>);

#[allow(clippy::match_same_arms)]
fn sort_fields(track: &CdbSortView, sort_type: u32) -> SortFields {
    let title = prefer(&track.sort_name, &track.title);
    let album = prefer(&track.sort_album, &track.album);
    let artist = prefer(&track.sort_artist, &track.artist);
    let composer = prefer(&track.sort_composer, &track.composer);
    let album_artist = first_nonempty(&[
        &track.sort_album_artist,
        &track.album_artist,
        &track.sort_artist,
        &track.artist,
    ]);
    let show = prefer(&track.sort_show, &track.show_name);
    let text = |value: &str| SortField::Text(sort_key(value));
    let number = SortField::Number;
    match sort_type {
        SORT_ALBUM => SortFields(vec![
            text(&album),
            number(track.disc_number),
            number(track.track_number),
            text(&title),
        ]),
        SORT_ARTIST => SortFields(vec![
            text(&artist),
            text(&album),
            number(track.disc_number),
            number(track.track_number),
            text(&title),
        ]),
        SORT_GENRE => SortFields(vec![
            text(&track.genre),
            text(&artist),
            text(&album),
            number(track.disc_number),
            number(track.track_number),
            text(&title),
        ]),
        SORT_COMPOSER => SortFields(vec![
            text(&composer),
            text(&album),
            number(track.disc_number),
            number(track.track_number),
            text(&title),
        ]),
        SORT_SHOW => SortFields(vec![
            text(&show),
            number(track.season),
            number(track.episode),
            text(&title),
        ]),
        SORT_SEASON => SortFields(vec![
            number(track.season),
            number(track.episode),
            text(&show),
            text(&title),
        ]),
        SORT_EPISODE => SortFields(vec![
            number(track.episode),
            number(track.season),
            text(&show),
            text(&title),
        ]),
        SORT_ALBUM_ARTIST => SortFields(vec![
            text(&album_artist),
            text(&album),
            number(track.disc_number),
            number(track.track_number),
            text(&title),
        ]),
        SORT_TITLE => SortFields(vec![text(&title)]),
        _ => SortFields(vec![text(&title)]),
    }
}

fn prefer(sort: &str, display: &str) -> String {
    if sort.is_empty() {
        display.to_owned()
    } else {
        sort.to_owned()
    }
}

fn first_nonempty(values: &[&str]) -> String {
    values
        .iter()
        .find(|value| !value.is_empty())
        .map_or_else(String::new, |value| (*value).to_owned())
}

fn rebuild_mhod52(
    chunk: &[u8],
    tracks: &[ParsedTrack],
    addition: &CdbTrackAddition,
    sort_type: u32,
) -> Result<Vec<u8>> {
    let header = chunk_header(chunk, 0, b"mhod")?;
    let body = header.header_length;
    if chunk.len().saturating_sub(body) < 48 {
        return Err(malformed(body, "type-52 mhod body is too short"));
    }
    let indexed = sorted_index(tracks, addition, sort_type)?;
    let mut output = chunk[..body + 48].to_vec();
    write_u32(
        &mut output,
        body + 4,
        u32::try_from(indexed.len())
            .map_err(|_| verification("type-52 index count exceeds u32"))?,
    )?;
    for (_, position) in indexed {
        output.extend_from_slice(&position.to_le_bytes());
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("type-52 mhod exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn rebuild_mhod53(
    chunk: &[u8],
    tracks: &[ParsedTrack],
    addition: &CdbTrackAddition,
    sort_type: u32,
) -> Result<Vec<u8>> {
    let header = chunk_header(chunk, 0, b"mhod")?;
    let body = header.header_length;
    if chunk.len().saturating_sub(body) < 16 {
        return Err(malformed(body, "type-53 mhod body is too short"));
    }
    let indexed = sorted_index(tracks, addition, sort_type)?;
    let views: Vec<CdbSortView> = indexed
        .iter()
        .map(|(_, position)| {
            if *position < u32::try_from(tracks.len()).unwrap_or(u32::MAX) {
                let index = usize::try_from(*position).unwrap_or(usize::MAX);
                CdbSortView::from_track(&tracks[index])
            } else {
                CdbSortView::from_addition(addition)
            }
        })
        .collect();
    let mut entries: Vec<(u16, u32, u32)> = Vec::new();
    let mut last_letter = None;
    for (index, view) in views.iter().enumerate() {
        let letter = jump_letter(&index_letter_source(view, sort_type));
        match last_letter {
            Some(previous) if previous == letter => {
                if let Some((_, _, count)) = entries.last_mut() {
                    *count += 1;
                }
            }
            _ => {
                entries.push((
                    letter,
                    u32::try_from(index).map_err(|_| verification("type-53 index exceeds u32"))?,
                    1,
                ));
                last_letter = Some(letter);
            }
        }
    }
    let mut output = chunk[..body + 16].to_vec();
    write_u32(
        &mut output,
        body + 4,
        u32::try_from(entries.len())
            .map_err(|_| verification("type-53 entry count exceeds u32"))?,
    )?;
    for (letter, start, count) in entries {
        output.extend_from_slice(&letter.to_le_bytes());
        output.extend_from_slice(&0_u16.to_le_bytes());
        output.extend_from_slice(&start.to_le_bytes());
        output.extend_from_slice(&count.to_le_bytes());
    }
    let output_len =
        u32::try_from(output.len()).map_err(|_| verification("type-53 mhod exceeds u32"))?;
    write_u32(&mut output, 8, output_len)?;
    Ok(output)
}

fn index_letter_source(view: &CdbSortView, sort_type: u32) -> String {
    match sort_type {
        SORT_ALBUM => prefer(&view.sort_album, &view.album),
        SORT_ARTIST => prefer(&view.sort_artist, &view.artist),
        SORT_GENRE => view.genre.clone(),
        SORT_COMPOSER => prefer(&view.sort_composer, &view.composer),
        SORT_SHOW => prefer(&view.sort_show, &view.show_name),
        SORT_SEASON => view.season.to_string(),
        SORT_EPISODE => view.episode.to_string(),
        SORT_ALBUM_ARTIST => first_nonempty(&[
            &view.sort_album_artist,
            &view.album_artist,
            &view.sort_artist,
            &view.artist,
        ]),
        _ => prefer(&view.sort_name, &view.title),
    }
}

fn build_mhod_string(mhod_type: u32, text: &str) -> Vec<u8> {
    let encoded = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    // Header (24): magic, header_length, total_length, type, then 8 zero
    // bytes; body (16): encoding=1, string_length, unk, unk; then UTF-16 data.
    let total = 24 + 16 + encoded.len();
    let mut output = Vec::with_capacity(total);
    output.extend_from_slice(b"mhod");
    output.extend_from_slice(&24_u32.to_le_bytes());
    output.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_le_bytes());
    output.extend_from_slice(&mhod_type.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&1_u32.to_le_bytes());
    output.extend_from_slice(
        &u32::try_from(encoded.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    output.extend_from_slice(&1_u32.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&encoded);
    debug_assert_eq!(output.len(), total);
    output
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn build_mhit(
    addition: &CdbTrackAddition,
    db_id_2: u64,
    track_id: u32,
    album_id: u32,
    artist_id_ref: u32,
) -> Vec<u8> {
    let artwork = addition.artwork;
    let mhod_fields = [
        (MHOD_TYPE_TITLE, Some(addition.title.clone())),
        (MHOD_TYPE_LOCATION, Some(addition.location.clone())),
        (MHOD_TYPE_ARTIST, addition.artist.clone()),
        (MHOD_TYPE_ALBUM, addition.album.clone()),
        (MHOD_TYPE_GENRE, addition.genre.clone()),
        (MHOD_TYPE_ALBUM_ARTIST, addition.album_artist.clone()),
        (MHOD_TYPE_COMPOSER, addition.composer.clone()),
    ];
    let mut mhods = Vec::new();
    let mut child_count = 0_u32;
    for (mhod_type, text) in mhod_fields {
        if let Some(text) = text {
            if !text.is_empty() {
                mhods.extend_from_slice(&build_mhod_string(mhod_type, &text));
                child_count += 1;
            }
        }
    }
    if child_count < 2 {
        return Vec::new();
    }
    let total = MHIT_HEADER_SIZE + mhods.len();
    let mut header = vec![0_u8; MHIT_HEADER_SIZE];
    header[..4].copy_from_slice(b"mhit");
    write_u32(
        &mut header,
        4,
        u32::try_from(MHIT_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut header, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut header, MHIT_CHILD_COUNT, child_count).ok();
    write_u32(&mut header, MHIT_TRACK_ID, track_id).ok();
    write_u32(&mut header, 0x14, 1).ok(); // visible
    write_u32(&mut header, MHIT_FILE_TYPE, 0x4d50_3320).ok(); // "MP3 "
    header[0x1c] = 0; // vbr_flag
    header[0x1d] = 1; // mp3_flag
    header[0x1e] = u8::from(addition.compilation);
    header[0x1f] = 0; // rating
    write_u32(&mut header, 0x20, addition.date_mac).ok(); // last_modified
    write_u32(&mut header, MHIT_SIZE, addition.file_size).ok();
    write_u32(&mut header, MHIT_LENGTH, addition.length_ms).ok();
    write_u32(&mut header, MHIT_TRACK_NUMBER, addition.track_number).ok();
    write_u32(&mut header, MHIT_TOTAL_TRACKS, addition.total_tracks).ok();
    write_u32(&mut header, MHIT_YEAR, addition.year).ok();
    write_u32(&mut header, MHIT_BITRATE, addition.bitrate).ok();
    write_u32(&mut header, MHIT_SAMPLE_RATE_1, addition.sample_rate << 16).ok();
    write_u32(&mut header, 0x40, 0).ok(); // volume
    write_u32(&mut header, 0x44, 0).ok(); // start_time
    write_u32(&mut header, 0x48, 0).ok(); // stop_time
    write_u32(&mut header, 0x4c, 0).ok(); // sound_check
    write_u32(&mut header, 0x50, 0).ok(); // play_count_1
    write_u32(&mut header, 0x54, 0).ok(); // play_count_2
    write_u32(&mut header, 0x58, 0).ok(); // last_played
    write_u32(&mut header, MHIT_DISC_NUMBER, addition.disc_number).ok();
    write_u32(&mut header, MHIT_TOTAL_DISCS, addition.total_discs).ok();
    write_u32(&mut header, 0x64, 0).ok(); // user_id
    write_u32(&mut header, 0x68, addition.date_mac).ok(); // date_added
    write_u32(&mut header, 0x6c, 0).ok(); // bookmark_time
    write_u64(
        &mut header,
        MHIT_DB_TRACK_ID,
        addition.persistent_id.to_bits(),
    )
    .ok();
    write_u16(&mut header, 0x7c, artwork.map_or(0, |_| 1)).ok(); // artwork_count
    write_u16(&mut header, 0x7e, 0xffff).ok(); // audio_format_flag
    write_u32(&mut header, 0x80, artwork.map_or(0, |art| art.src_img_size)).ok(); // artwork_size
    header[0x84..0x88].copy_from_slice(&0_u32.to_le_bytes());
    write_u32(
        &mut header,
        0x88,
        u32::from_le_bytes((addition.sample_rate as f32).to_le_bytes()),
    )
    .ok(); // sample_rate_2 f32
    write_u32(&mut header, 0x8c, 0).ok(); // date_released
    write_u16(&mut header, 0x90, 0).ok(); // mpeg_audio_type
    write_u32(&mut header, 0x94, 0).ok(); // unk
    write_u32(&mut header, 0x98, 0).ok(); // genius_category_id
    write_u32(&mut header, 0x9c, 0).ok(); // skip_count
    write_u32(&mut header, 0xa0, 0).ok(); // last_skipped
    header[0xa4] = if artwork.is_some() { 1 } else { 2 }; // has_artwork
    write_u64(&mut header, 0xa8, addition.persistent_id.to_bits()).ok(); // db_track_id_2
    write_u32(&mut header, 0xb8, 0).ok(); // pregap
    write_u64(&mut header, 0xbc, 0).ok(); // sample_count
    write_u32(&mut header, 0xc8, 0).ok(); // postgap
    write_u32(&mut header, 0xcc, 0).ok(); // encoder
    write_u32(&mut header, MHIT_MEDIA_TYPE, 1).ok(); // audio
    write_u32(&mut header, MHIT_SEASON, 0).ok();
    write_u32(&mut header, MHIT_EPISODE, 0).ok();
    write_u32(&mut header, 0x100, 0).ok(); // gapless_track_flag
    write_u32(&mut header, 0x104, 0).ok();
    write_u32(&mut header, 0x120, album_id).ok(); // album_id
    write_u64(&mut header, MHIT_DB_ID2_REF, db_id_2).ok();
    write_u32(&mut header, 0x12c, addition.file_size).ok(); // size_2
    write_u32(&mut header, 0x160, artwork.map_or(0, |art| art.image_id)).ok(); // artwork_id_ref
    header[0x134..0x13a].copy_from_slice(&[0x80; 6]); // sort indicators, no sort MHODs
    write_u32(&mut header, 0x168, 1).ok(); // opaque marker
    write_u32(&mut header, MHIT_ARTIST_ID_REF, artist_id_ref).ok();
    write_u32(&mut header, 0x1f4, 0).ok(); // composer_id
    let mut output = header;
    output.extend_from_slice(&mhods);
    output
}

fn build_mhod_position(position: u32) -> Vec<u8> {
    let mut output = Vec::with_capacity(44);
    output.extend_from_slice(b"mhod");
    output.extend_from_slice(&24_u32.to_le_bytes());
    output.extend_from_slice(&44_u32.to_le_bytes());
    output.extend_from_slice(&100_u32.to_le_bytes());
    output.extend_from_slice(&position.to_le_bytes());
    output.extend_from_slice(&[0_u8; 16]);
    output
}

fn build_mhip(track_id: u32, position: u32) -> Vec<u8> {
    let mhod = build_mhod_position(position);
    let total = MHIP_HEADER_SIZE + mhod.len();
    let mut output = vec![0_u8; MHIP_HEADER_SIZE];
    output[..4].copy_from_slice(b"mhip");
    write_u32(
        &mut output,
        4,
        u32::try_from(MHIP_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut output, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut output, 0x0c, 1).ok(); // child_count
    write_u32(&mut output, 0x18, track_id).ok();
    output.extend_from_slice(&mhod);
    output
}

fn build_mhia(album_id: u32, name: &str, artist: &str, representative: PersistentId) -> Vec<u8> {
    let mut mhods = Vec::new();
    let mut child_count = 0_u32;
    for (mhod_type, text) in [(200_u32, name), (201_u32, artist)] {
        if !text.is_empty() {
            mhods.extend_from_slice(&build_mhod_string(mhod_type, text));
            child_count += 1;
        }
    }
    let total = MHIA_HEADER_SIZE + mhods.len();
    let mut output = vec![0_u8; MHIA_HEADER_SIZE];
    output[..4].copy_from_slice(b"mhia");
    write_u32(
        &mut output,
        4,
        u32::try_from(MHIA_HEADER_SIZE).unwrap_or(u32::MAX),
    )
    .ok();
    write_u32(&mut output, 8, u32::try_from(total).unwrap_or(u32::MAX)).ok();
    write_u32(&mut output, 0x0c, child_count).ok();
    write_u32(&mut output, 0x10, album_id).ok();
    write_u64(&mut output, 0x14, random_u64()).ok(); // sql_id
    write_u16(&mut output, 0x1c, 2).ok(); // platform_flag
    write_u64(&mut output, 0x20, representative.to_bits()).ok();
    output.extend_from_slice(&mhods);
    output
}

fn random_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut state = u64::try_from(nanos).unwrap_or(u64::MAX) ^ 0x9e37_79b9_7f4a_7c15;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    state
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_new_track_after_existing_same_key() {
        let addition = CdbTrackAddition {
            persistent_id: PersistentId::from_bits(1),
            location: "F00/ZZZZ.mp3".to_owned(),
            title: "Same Title".to_owned(),
            artist: None,
            album: None,
            album_artist: None,
            genre: None,
            composer: None,
            file_size: 1,
            length_ms: 1,
            bitrate: 1,
            sample_rate: 44100,
            track_number: 1,
            total_tracks: 1,
            disc_number: 1,
            total_discs: 1,
            year: 2024,
            compilation: false,
            date_mac: 0,
            artwork: None,
        };
        let fields = sort_fields(&CdbSortView::from_addition(&addition), SORT_TITLE);
        assert_eq!(fields.0[0], SortField::Text("same title".to_owned()));
    }
}
