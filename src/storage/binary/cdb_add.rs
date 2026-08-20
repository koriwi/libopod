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
const MHOD_TYPE_FILETYPE: u32 = 6;
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

    let (existing_tracks, rewritten_tracks, next_track_id, artist_id_ref) = rewrite_track_dataset(
        &datasets[track_dataset],
        addition,
        db_id_2,
        album_id,
        MHIT_HEADER_SIZE,
    )?;
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
pub(super) struct ParsedTrack {
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
    // Header size varies by database version. Nano 1G/2G uses 0x148 and
    // therefore has no later artist-id field; only the persistent ID is
    // required. Extended sort fields default to zero when absent.
    if header.header_length < MHIT_DB_TRACK_ID + 8 {
        return Err(malformed(
            4,
            "mhit header is too short for index rebuilding",
        ));
    }
    let mut strings = HashMap::new();
    let mut offset = header.header_length;
    for _ in 0..usize_value(read_u32(chunk, MHIT_CHILD_COUNT)?, MHIT_CHILD_COUNT)? {
        let (mhod_type, claimed_end, legacy) = mhod_child(chunk, offset)?;
        match parse_string_mhod(chunk, offset, legacy)? {
            Some((text, real_end)) => {
                strings.insert(mhod_type, text);
                offset = real_end;
            }
            None => offset = claimed_end,
        }
    }
    if offset != chunk.len() {
        return Err(malformed(offset, "trailing bytes after mhit children"));
    }
    Ok(ParsedTrack {
        position,
        persistent_id: PersistentId::from_bits(read_u64(chunk, MHIT_DB_TRACK_ID)?),
        track_id: read_u32(chunk, MHIT_TRACK_ID)?,
        artist_id_ref: read_optional_u32(chunk, header.header_length, MHIT_ARTIST_ID_REF)?,
        track_number: read_u32(chunk, MHIT_TRACK_NUMBER)?,
        disc_number: read_u32(chunk, MHIT_DISC_NUMBER)?,
        season: read_optional_u32(chunk, header.header_length, MHIT_SEASON)?,
        episode: read_optional_u32(chunk, header.header_length, MHIT_EPISODE)?,
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

fn read_optional_u32(chunk: &[u8], header_length: usize, offset: usize) -> Result<u32> {
    if header_length >= offset.saturating_add(4) {
        read_u32(chunk, offset)
    } else {
        Ok(0)
    }
}

/// Parses a string `mhod` at `offset`, returning the string and the mhod's
/// real end.
///
/// The real end may be shorter than the claimed total: string mhods written
/// by the pre-gap Nano 7G writer (before the 8-byte gap fix) claim
/// `40 + len` bytes but store only `32 + len`, so walking by the claimed
/// total misaligns the next child. Callers must advance by the returned end.
/// Reads an `mhod` child at `offset` leniently, returning its type, its
/// claimed end (which may exceed the parent for legacy string mhods), and
/// whether it uses the legacy pre-gap string layout.
pub(super) fn mhod_child(chunk: &[u8], offset: usize) -> Result<(u32, usize, bool)> {
    require_magic(chunk, offset, b"mhod")?;
    let header_length = usize_value(read_u32(chunk, offset + 4)?, offset + 4)?;
    let total_length = usize_value(read_u32(chunk, offset + 8)?, offset + 8)?;
    if header_length < 12 || total_length < header_length {
        return Err(malformed(offset + 4, "invalid chunk lengths"));
    }
    let mhod_type = read_u32(chunk, offset + 12)?;
    // Legacy layout (pre-gap writer): encoding at +16 (and legacy unk at
    // +24). Current layout: 8-byte gap at +16..+24, encoding at +24.
    let legacy = read_u32(chunk, offset + 16).ok() == Some(1)
        && read_u32(chunk, offset + 24).ok() == Some(1);
    Ok((mhod_type, offset + total_length, legacy))
}

/// Parses a string `mhod` at `offset`, returning the string and the mhod's
/// real end.
///
/// The real end may be shorter than the claimed total: string mhods written
/// by the pre-gap Nano 7G writer claim `40 + len` bytes but store only
/// `32 + len`, so walking by the claimed total misaligns the next child.
/// Callers must advance by the returned end.
pub(super) fn parse_string_mhod(
    chunk: &[u8],
    offset: usize,
    legacy: bool,
) -> Result<Option<(String, usize)>> {
    let mhod_type = read_u32(chunk, offset + 12)?;
    if !matches!(
        mhod_type,
        1..=14 | 18..=31 | 33..=44 | 200..=204 | 300
    ) {
        return Ok(None);
    }
    let (encoding_offset, length_offset, data_offset) = if legacy {
        (offset + 16, offset + 20, offset + 32)
    } else {
        (offset + 24, offset + 28, offset + 40)
    };
    if chunk.len().saturating_sub(encoding_offset) < 4 {
        return Ok(None);
    }
    let encoding = read_u32(chunk, encoding_offset)?;
    if encoding != 1 {
        return Ok(None);
    }
    let byte_length = usize_value(read_u32(chunk, length_offset)?, length_offset)?;
    let Some(data) = chunk.get(data_offset..data_offset + byte_length) else {
        if legacy {
            // An implausible length means a non-string mhod was misdetected;
            // treat it as not a string instead of failing the parse.
            return Ok(None);
        }
        return Err(malformed(
            data_offset,
            "string MHOD length exceeds its chunk",
        ));
    };
    let mut units = Vec::with_capacity(byte_length / 2);
    for pair in data.chunks_exact(2) {
        units.push(u16::from_le_bytes([pair[0], pair[1]]));
    }
    Ok(Some((
        String::from_utf16_lossy(&units),
        data_offset + byte_length,
    )))
}

pub(super) fn rewrite_track_dataset(
    dataset: &[u8],
    addition: &CdbTrackAddition,
    db_id_2: u64,
    album_id: u32,
    mhit_header_size: usize,
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
    let mhod_profile = track_mhod_profile(dataset)?;
    output.extend_from_slice(&build_mhit(
        addition,
        db_id_2,
        next_track_id,
        album_id,
        artist_id_ref,
        mhit_header_size,
        &mhod_profile,
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

pub(super) fn resolve_album(dataset: &[u8], addition: &CdbTrackAddition) -> Result<(u32, bool)> {
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

pub(super) fn append_album(
    dataset: &[u8],
    album_id: u32,
    addition: &CdbTrackAddition,
) -> Result<Vec<u8>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhla")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut output = dataset[..body].to_vec();
    let mut offset = body;
    // Collect existing `sql_id` values so the new album's id is unique even
    // when the RNG stream repeats within a clock tick.
    let mut sql_ids = std::collections::BTreeSet::new();
    for _ in 0..count {
        let album = chunk_header(dataset, offset, b"mhia")?;
        sql_ids.insert(read_u64(dataset, offset + 0x14)?);
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
    let sql_id = loop {
        let candidate = crate::random::next_u64();
        if candidate != 0 && !sql_ids.contains(&candidate) {
            break candidate;
        }
    };
    output.extend_from_slice(&build_mhia(
        album_id,
        &album_name,
        &album_artist,
        addition.persistent_id,
        sql_id,
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
        let (mhod_type, claimed_end, legacy) = mhod_child(chunk, offset)?;
        match parse_string_mhod(chunk, offset, legacy)? {
            Some((text, real_end)) => {
                if mhod_type == 200 {
                    name = text;
                } else if mhod_type == 201 {
                    artist = text;
                }
                offset = real_end;
            }
            None => offset = claimed_end,
        }
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

pub(super) fn rewrite_master_playlist_dataset(
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

    let mut existing_mhips = HashMap::with_capacity(mhip_count);
    for _ in 0..mhip_count {
        let mhip = chunk_header(playlist, offset, b"mhip")?;
        let existing_track_id = read_u32(playlist, offset + 0x18)?;
        existing_mhips.insert(existing_track_id, playlist[offset..mhip.end].to_vec());
        offset = mhip.end;
    }
    if offset != playlist.len() {
        return Err(malformed(offset, "trailing bytes after mhyp children"));
    }

    // Master membership and type-52 indices both use positions in type-1
    // track order. Re-emit that canonical order, retaining a byte-identical
    // existing MHIP only when its type-100 child has the complete iOpenPod
    // layout. The old libopod writer omitted the MHOD header's 8-byte gap:
    // each generated MHIP was 112 bytes but its child claimed 44 bytes. Such
    // an entry is only valid as a firmware-truncated final child and corrupts
    // the following entries when several tracks are added in one sync.
    let mut position = 0_usize;
    for existing in tracks {
        let position_u32 = u32::try_from(position)
            .map_err(|_| verification("master playlist position exceeds u32"))?;
        if let Some(mhip) = existing_mhips.remove(&existing.track_id) {
            if master_mhip_is_canonical(&mhip, existing.track_id, position_u32)? {
                output.extend_from_slice(&mhip);
            } else {
                output.extend_from_slice(&build_mhip(existing.track_id, position_u32));
            }
        } else {
            output.extend_from_slice(&build_mhip(existing.track_id, position_u32));
        }
        position += 1;
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

fn master_mhip_is_canonical(mhip: &[u8], track_id: u32, position: u32) -> Result<bool> {
    let header = chunk_header(mhip, 0, b"mhip")?;
    if header.header_length != MHIP_HEADER_SIZE
        || read_u32(mhip, 0x0c)? != 1
        || read_u32(mhip, 0x18)? != track_id
    {
        return Ok(false);
    }
    let mhod_offset = header.header_length;
    if mhip.get(mhod_offset..mhod_offset + 4) != Some(b"mhod") {
        return Ok(false);
    }
    let mhod_header = usize_value(read_u32(mhip, mhod_offset + 4)?, mhod_offset + 4)?;
    let mhod_total = usize_value(read_u32(mhip, mhod_offset + 8)?, mhod_offset + 8)?;
    Ok(mhod_header == 24
        && mhod_offset.checked_add(mhod_total) == Some(mhip.len())
        && read_u32(mhip, mhod_offset + 12)? == 100
        && read_u32(mhip, mhod_offset + 24)? == position)
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
/// Reads the ordered union of `mhod` types used by existing tracks.
///
/// A Nano 2G library commonly has a short first track profile and adds genre
/// or album-artist children only on later tracks. Mirroring only the first
/// row loses those fields; using the ordered union preserves the device's
/// established order while retaining all metadata types it already uses.
fn track_mhod_profile(dataset: &[u8]) -> Result<Vec<u32>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlt")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    if count == 0 {
        // iOpenPod's current writer order for a newly initialized classic
        // library. Filetype is supplied for MP3 additions.
        return Ok(vec![
            MHOD_TYPE_TITLE,
            MHOD_TYPE_LOCATION,
            MHOD_TYPE_ARTIST,
            MHOD_TYPE_ALBUM,
            MHOD_TYPE_GENRE,
            MHOD_TYPE_ALBUM_ARTIST,
            MHOD_TYPE_COMPOSER,
            MHOD_TYPE_FILETYPE,
        ]);
    }
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut profiles = Vec::with_capacity(count);
    let mut track_offset = body;
    for _ in 0..count {
        let track = chunk_header(dataset, track_offset, b"mhit")?;
        let child_count = usize_value(
            read_u32(dataset, track_offset + MHIT_CHILD_COUNT)?,
            track_offset + MHIT_CHILD_COUNT,
        )?;
        let mut profile = Vec::with_capacity(child_count);
        let mut offset = track_offset + track.header_length;
        for _ in 0..child_count {
            let (mhod_type, claimed_end, legacy) = mhod_child(dataset, offset)?;
            profile.push(mhod_type);
            offset = parse_string_mhod(dataset, offset, legacy)?
                .map_or(claimed_end, |(_, real_end)| real_end);
        }
        profiles.push(profile);
        track_offset = track.end;
    }
    if track_offset != dataset.len() {
        return Err(malformed(track_offset, "trailing bytes after mhlt tracks"));
    }

    // Prefer an existing MP3 profile that includes the filetype description.
    // This also heals a library produced by the old libopod writer: most new
    // rows omitted type 6, while a smaller set of working rows retained the
    // Nano 2G-compatible order. Then take
    // optional fields in the most complete row's order and include rarities.
    let mut types = profiles
        .iter()
        .filter(|profile| profile.contains(&MHOD_TYPE_FILETYPE))
        .max_by_key(|profile| profile.len())
        .or_else(|| profiles.first())
        .cloned()
        .unwrap_or_default();
    if let Some(longest) = profiles.iter().max_by_key(|profile| profile.len()) {
        for mhod_type in longest {
            if !types.contains(mhod_type) {
                types.push(*mhod_type);
            }
        }
    }
    for profile in &profiles {
        for mhod_type in profile {
            if !types.contains(mhod_type) {
                types.push(*mhod_type);
            }
        }
    }
    Ok(types)
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn build_mhit(
    addition: &CdbTrackAddition,
    db_id_2: u64,
    track_id: u32,
    album_id: u32,
    artist_id_ref: u32,
    header_size: usize,
    mhod_profile: &[u32],
) -> Vec<u8> {
    let artwork = addition.artwork;
    let mut mhods = Vec::new();
    let mut child_count = 0_u32;
    for &mhod_type in mhod_profile {
        let text = match mhod_type {
            MHOD_TYPE_TITLE => Some(addition.title.clone()),
            MHOD_TYPE_LOCATION => Some(addition.location.clone()),
            MHOD_TYPE_ALBUM => Some(addition.album.clone().unwrap_or_default()),
            MHOD_TYPE_ARTIST => Some(addition.artist.clone().unwrap_or_default()),
            MHOD_TYPE_GENRE => addition.genre.clone(),
            MHOD_TYPE_FILETYPE => Some("MPEG audio file".to_owned()),
            MHOD_TYPE_ALBUM_ARTIST => Some(
                addition
                    .album_artist
                    .clone()
                    .or_else(|| addition.artist.clone())
                    .unwrap_or_default(),
            ),
            MHOD_TYPE_COMPOSER => addition.composer.clone(),
            // Sort/other mhods cannot be rebuilt faithfully; omit them.
            _ => None,
        };
        if let Some(text) = text {
            mhods.extend_from_slice(&build_mhod_string(mhod_type, &text));
            child_count += 1;
        }
    }
    if child_count < 2 {
        return Vec::new();
    }
    let total = header_size + mhods.len();
    let mut header = vec![0_u8; header_size];
    header[..4].copy_from_slice(b"mhit");
    write_u32(
        &mut header,
        4,
        u32::try_from(header_size).unwrap_or(u32::MAX),
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
    if header.len() > 0xa4 {
        header[0xa4] = if artwork.is_some() { 1 } else { 2 }; // has_artwork
    }
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
    if header.len() >= 0x13a {
        header[0x134..0x13a].copy_from_slice(&[0x80; 6]); // sort indicators, no sort MHODs
    }
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
    output.extend_from_slice(&[0_u8; 8]);
    output.extend_from_slice(&position.to_le_bytes());
    output.extend_from_slice(&[0_u8; 16]);
    debug_assert_eq!(output.len(), 44);
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

fn build_mhia(
    album_id: u32,
    name: &str,
    artist: &str,
    representative: PersistentId,
    sql_id: u64,
) -> Vec<u8> {
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
    write_u64(&mut output, 0x14, sql_id).ok(); // sql_id
    write_u16(&mut output, 0x1c, 2).ok(); // platform_flag
    write_u64(&mut output, 0x20, representative.to_bits()).ok();
    output.extend_from_slice(&mhods);
    output
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
#[allow(clippy::items_after_test_module)]
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

    #[test]
    fn builds_a_complete_master_playlist_position_child() {
        let mhip = build_mhip(42, 7);
        assert_eq!(mhip.len(), 120);
        assert_eq!(read_u32(&mhip, 8).unwrap(), 120);
        assert_eq!(&mhip[MHIP_HEADER_SIZE..MHIP_HEADER_SIZE + 4], b"mhod");
        assert_eq!(read_u32(&mhip, MHIP_HEADER_SIZE + 8).unwrap(), 44);
        assert_eq!(read_u32(&mhip, MHIP_HEADER_SIZE + 12).unwrap(), 100);
        assert_eq!(read_u32(&mhip, MHIP_HEADER_SIZE + 24).unwrap(), 7);
        assert!(master_mhip_is_canonical(&mhip, 42, 7).unwrap());
    }

    #[test]
    fn parses_a_nano2_sized_mhit_for_index_rebuilds() {
        let addition = CdbTrackAddition {
            persistent_id: PersistentId::from_bits(0x1122_3344_5566_7788),
            location: ":iPod_Control:Music:F00:TEST.mp3".to_owned(),
            title: "Nano 2G header".to_owned(),
            artist: Some("Artist".to_owned()),
            album: Some("Album".to_owned()),
            album_artist: None,
            genre: None,
            composer: None,
            file_size: 1_000,
            length_ms: 60_000,
            bitrate: 128,
            sample_rate: 44_100,
            track_number: 1,
            total_tracks: 1,
            disc_number: 1,
            total_discs: 1,
            year: 2024,
            compilation: false,
            date_mac: 0,
            artwork: None,
        };
        let bytes = build_mhit(
            &addition,
            0,
            1,
            0,
            0,
            0x148,
            &[
                MHOD_TYPE_TITLE,
                MHOD_TYPE_ARTIST,
                MHOD_TYPE_ALBUM,
                MHOD_TYPE_FILETYPE,
                MHOD_TYPE_LOCATION,
            ],
        );
        let parsed = parse_track(&bytes, 0).expect("parse Nano 2G-sized mhit");
        assert_eq!(parsed.persistent_id, addition.persistent_id);
        assert_eq!(parsed.title, addition.title);
        assert_eq!(parsed.artist_id_ref, 0);
    }

    #[test]
    fn addition_round_trips_the_copied_device_cdb() {
        // The copied device CDB contains the firmware-rewritten master
        // playlist, including its truncated trailing mhip. The adder must
        // append a new track without tripping over it, and the result must
        // re-parse for a later removal.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("iTunesCDB");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let addition = CdbTrackAddition {
            persistent_id: PersistentId::from_bits(0x7A11_0000_0000_0001),
            location: "F00/ZZZZ.mp3".to_owned(),
            title: "Device State Test".to_owned(),
            artist: Some("Artist".to_owned()),
            album: Some("Album".to_owned()),
            album_artist: None,
            genre: None,
            composer: None,
            file_size: 1000,
            length_ms: 60_000,
            bitrate: 128,
            sample_rate: 44_100,
            track_number: 1,
            total_tracks: 1,
            disc_number: 1,
            total_discs: 1,
            year: 2024,
            compilation: false,
            date_mac: 0,
            artwork: None,
        };
        let added = super::add_track_to_cdb(&bytes, [0u8; 8], &addition)
            .unwrap_or_else(|error| panic!("addition to the copied device CDB failed: {error:?}"));
        // The rewritten CDB must re-parse: remove the track we just added.
        let removed = super::super::cdb_edit::remove_tracks_from_cdb(
            &added,
            [0u8; 8],
            &[addition.persistent_id],
        );
        assert!(
            removed.is_ok(),
            "removal from rewritten device CDB failed: {removed:?}"
        );
    }

    #[test]
    fn fixture_addition_preserves_original_chunks() {
        // Opposite side of the byte-preservation harness: adding a track must
        // leave every original chunk byte-identical; new chunks append after.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("backup_7g/iPod_Control/iTunes/iTunesCDB");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let header_length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let payload = super::super::cdb::decode_payload(&bytes, header_length).unwrap();
        let addition = CdbTrackAddition {
            persistent_id: PersistentId::from_bits(0x7B22_0000_0000_0002),
            location: "F40/ZZZZ.mp3".to_owned(),
            title: "Harness Addition".to_owned(),
            artist: Some("Harness Artist".to_owned()),
            album: Some("Harness Album".to_owned()),
            album_artist: None,
            genre: None,
            composer: None,
            file_size: 1000,
            length_ms: 60_000,
            bitrate: 128,
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
        let rewritten = super::add_track_to_cdb(&bytes, [0u8; 8], &addition)
            .unwrap_or_else(|error| panic!("addition to the fixture CDB failed: {error:?}"));
        let rewritten_header = u32::from_le_bytes(rewritten[4..8].try_into().unwrap()) as usize;
        let rewritten_payload =
            super::super::cdb::decode_payload(&rewritten, rewritten_header).unwrap();
        super::assert_original_chunks_preserved(&payload, &rewritten_payload);
    }

    #[test]
    fn parses_legacy_pre_gap_string_mhods() {
        // The pre-gap Nano 7G writer stored encoding at +16, length at +20,
        // data at +32, and claimed `40 + len` total while writing only
        // `32 + len` bytes. The parser must decode the string and return the
        // real end so a child walk stays aligned.
        let text = "AudioTestAlbum";
        let mut chunk = vec![0u8; 32 + text.len() * 2];
        chunk[0..4].copy_from_slice(b"mhod");
        chunk[4..8].copy_from_slice(&24u32.to_le_bytes());
        chunk[8..12].copy_from_slice(&u32::try_from(40 + text.len() * 2).unwrap().to_le_bytes());
        chunk[12..16].copy_from_slice(&200u32.to_le_bytes());
        chunk[16..20].copy_from_slice(&1u32.to_le_bytes()); // encoding
        chunk[20..24].copy_from_slice(&u32::try_from(text.len() * 2).unwrap().to_le_bytes()); // length
        chunk[24..28].copy_from_slice(&1u32.to_le_bytes()); // legacy unk
        for (i, unit) in text.encode_utf16().enumerate() {
            chunk[32 + i * 2..34 + i * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let parsed = parse_string_mhod(&chunk, 0, true).unwrap().unwrap();
        assert_eq!(parsed.0, text);
        assert_eq!(parsed.1, 32 + text.len() * 2);
    }
}

/// Addition byte-preservation harness: adding a track must leave every
/// original chunk byte-identical; new chunks are appended after the original
/// ones. `original`/`rewritten` are decoded payloads before/after the add.
/// Original-chunk byte-preservation harness (addition side): every original
/// mhit/mhia/mhod/mhip must stay byte-identical and in order; new chunks
/// append after. Exposed to the edit test harness.
#[cfg(test)]
pub(crate) fn assert_original_chunks_preserved(original: &[u8], rewritten: &[u8]) {
    let input = super::cdb_edit::split_payload(original);
    let output = super::cdb_edit::split_payload(rewritten);
    assert_eq!(input.len(), output.len(), "dataset count must not change");
    for (input_dataset, output_dataset) in input.iter().zip(output.iter()) {
        let (input, input_kind) = *input_dataset;
        let (output, _) = *output_dataset;
        match input_kind {
            1 => {
                // Original mhits are a byte-identical prefix of the output.
                let mut in_off = super::cdb_edit::dataset_list_body(input);
                let mut out_off = super::cdb_edit::dataset_list_body(output);
                let mut compared = 0;
                while in_off < input.len() {
                    let in_track = chunk_header(input, in_off, b"mhit").unwrap();
                    let out_track = chunk_header(output, out_off, b"mhit").unwrap();
                    assert_eq!(
                        &input[in_off..in_track.end],
                        &output[out_off..out_track.end],
                        "original track chunk changed during addition"
                    );
                    in_off = in_track.end;
                    out_off = out_track.end;
                    compared += 1;
                }
                assert!(compared > 0, "no original tracks compared");
                assert!(
                    out_off <= output.len(),
                    "appended track walked past the output dataset"
                );
            }
            2 | 3 | 5 => {
                // Each original mhyp is byte-identical except the rebuilt
                // 52/53 indices; the appended mhip may follow.
                let mut in_off = super::cdb_edit::dataset_list_body(input);
                let mut out_off = super::cdb_edit::dataset_list_body(output);
                while in_off < input.len() {
                    let in_hyp = chunk_header(input, in_off, b"mhyp").unwrap();
                    let out_hyp = chunk_header(output, out_off, b"mhyp").unwrap();
                    let in_playlist = &input[in_off..in_hyp.end];
                    let out_playlist = &output[out_off..out_hyp.end];
                    let header_length = usize_value(read_u32(in_playlist, 4).unwrap(), 4).unwrap();
                    let mhod_count = usize_value(read_u32(in_playlist, 12).unwrap(), 12).unwrap();
                    let mhip_count = usize_value(read_u32(in_playlist, 16).unwrap(), 16).unwrap();
                    let mut in_child = header_length;
                    let mut out_child = header_length;
                    for _ in 0..mhod_count {
                        let in_mhod = chunk_header(in_playlist, in_child, b"mhod").unwrap();
                        let mhod_type = read_u32(in_playlist, in_child + 12).unwrap();
                        if mhod_type == 52 || mhod_type == 53 {
                            let out_mhod = chunk_header(out_playlist, out_child, b"mhod").unwrap();
                            in_child = in_mhod.end;
                            out_child = out_mhod.end;
                            continue;
                        }
                        let out_mhod = chunk_header(out_playlist, out_child, b"mhod").unwrap();
                        assert_eq!(
                            &in_playlist[in_child..in_mhod.end],
                            &out_playlist[out_child..out_mhod.end],
                            "original mhod changed during addition"
                        );
                        in_child = in_mhod.end;
                        out_child = out_mhod.end;
                    }
                    for _ in 0..mhip_count {
                        let in_mhip = chunk_header(in_playlist, in_child, b"mhip").unwrap();
                        let out_mhip = chunk_header(out_playlist, out_child, b"mhip").unwrap();
                        assert_eq!(
                            &in_playlist[in_child..in_mhip.end],
                            &out_playlist[out_child..out_mhip.end],
                            "original mhip changed during addition"
                        );
                        in_child = in_mhip.end;
                        out_child = out_mhip.end;
                    }
                    in_off = in_hyp.end;
                    out_off = out_hyp.end;
                }
            }
            4 => {
                // A new album appends an mhia; originals are a prefix.
                let mut in_off = super::cdb_edit::dataset_list_body(input);
                let mut out_off = super::cdb_edit::dataset_list_body(output);
                while in_off < input.len() {
                    let in_album = chunk_header(input, in_off, b"mhia").unwrap();
                    let out_album = chunk_header(output, out_off, b"mhia").unwrap();
                    assert_eq!(
                        &input[in_off..in_album.end],
                        &output[out_off..out_album.end],
                        "original album chunk changed during addition"
                    );
                    in_off = in_album.end;
                    out_off = out_album.end;
                }
            }
            _ => {
                assert_eq!(input, output, "untouched dataset changed during addition");
            }
        }
    }
}
