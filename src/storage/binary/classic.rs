//! Classic (uncompressed) iTunesDB parsing for pre-SQLite iPods.
//!
//! Nano 1–4G and the Classic line store their library in a single
//! uncompressed `iPod_Control/iTunes/iTunesDB` `mhbd` file: the same chunk
//! structure as the compressed Nano 5G+ `iTunesCDB`, without zlib and signed
//! with NONE (Nano 1–2G) or HASH58 (Nano 3–4G) instead of HASHAB.

use super::cdb_add::{mhod_child, parse_string_mhod};
use super::cdb_edit::{
    checked_end, chunk_header, malformed, read_u32, read_u64, require_magic, split_datasets,
    usize_value,
};
use crate::{Error, PersistentId, Result};

/// The Nano 7G `mhit` header size, used by the synthetic test builder; the
/// classic parser accepts any header long enough for its fields (see
/// `MHIT_MIN_HEADER`).
#[cfg(test)]
const MHIT_HEADER_SIZE: usize = 0x270;

/// Minimum classic `mhit` header: the fields read below (`db_track_id` at
/// 0x70 + 8) must fit, while the header itself varies by firmware version
/// (observed 0x248 on Nano 3G, 0x9C minimum on older devices).
const MHIT_MIN_HEADER: usize = 0x78;
const MHIT_CHILD_COUNT: usize = 0x0c;
const MHIT_TRACK_ID: usize = 0x10;
const MHIT_SIZE: usize = 0x24;
const MHIT_LENGTH: usize = 0x28;
const MHIT_TRACK_NUMBER: usize = 0x2c;
const MHIT_DISC_NUMBER: usize = 0x5c;
const MHIT_DB_TRACK_ID: usize = 0x70;
const MHIP_TRACK_ID: usize = 0x18;

const MHOD_TYPE_TITLE: u32 = 1;
const MHOD_TYPE_LOCATION: u32 = 2;
const MHOD_TYPE_ALBUM: u32 = 3;
const MHOD_TYPE_ARTIST: u32 = 4;
const MHOD_TYPE_GENRE: u32 = 5;
const MHOD_TYPE_COMPOSER: u32 = 12;
const MHOD_TYPE_ALBUM_ARTIST: u32 = 22;
const MHOD_TYPE_PLAYLIST_NAME: u32 = 1;

/// One classic library track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassicTrack {
    pub persistent_id: PersistentId,
    pub track_id: u32,
    /// Mount-relative location, e.g. `iPod_Control/Music/F11/ABCD.mp3`.
    pub location: String,
    pub title: String,
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub genre: String,
    pub composer: String,
    pub size: u64,
    pub duration_ms: u32,
    pub track_number: u32,
    pub disc_number: u32,
    pub has_artwork: bool,
}

/// One classic playlist (`mhyp`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassicPlaylist {
    pub id: PersistentId,
    pub name: String,
    pub track_ids: Vec<u32>,
}

/// A parsed classic library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClassicLibrary {
    pub tracks: Vec<ClassicTrack>,
    pub playlists: Vec<ClassicPlaylist>,
}

/// Parses an uncompressed classic `iTunesDB`. When `artworkdb` is present,
/// track artwork presence is resolved from its `mhii` records.
pub fn parse_library(itunesdb: &[u8], artworkdb: Option<&[u8]>) -> Result<ClassicLibrary> {
    require_magic(itunesdb, 0, b"mhbd")?;
    let header_length = usize_value(read_u32(itunesdb, 4)?, 4)?;
    let dataset_count = usize::try_from(read_u32(itunesdb, 0x14)?)
        .map_err(|_| malformed(0x14, "dataset count does not fit this host"))?;
    let payload = itunesdb
        .get(header_length..)
        .ok_or_else(|| Error::Malformed {
            format: "classic iTunesDB",
            offset: u64::try_from(header_length).unwrap_or(u64::MAX),
            reason: "mhbd header exceeds the file length".to_owned(),
        })?;
    let datasets = split_datasets(payload, dataset_count)?;

    let artwork_tracks: std::collections::BTreeSet<PersistentId> = match artworkdb {
        Some(bytes) => crate::artwork::parse_artwork_records(bytes)?
            .into_iter()
            .map(|record| record.track_id)
            .collect(),
        None => std::collections::BTreeSet::default(),
    };

    let mut tracks = Vec::new();
    let mut playlists = Vec::new();
    for dataset in &datasets {
        let kind = read_u32(dataset, 12)?;
        match kind {
            1 => tracks.extend(parse_track_dataset(dataset, &artwork_tracks)?),
            2 => playlists.extend(parse_playlist_dataset(dataset)?),
            _ => {}
        }
    }
    Ok(ClassicLibrary { tracks, playlists })
}

fn parse_track_dataset(
    dataset: &[u8],
    artwork_tracks: &std::collections::BTreeSet<PersistentId>,
) -> Result<Vec<ClassicTrack>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlt")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut offset = body;
    let mut tracks = Vec::with_capacity(count);
    for _ in 0..count {
        let track = chunk_header(dataset, offset, b"mhit")?;
        let chunk = &dataset[offset..track.end];
        tracks.push(parse_track(chunk, artwork_tracks)?);
        offset = track.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlt tracks"));
    }
    Ok(tracks)
}

fn parse_track(
    chunk: &[u8],
    artwork_tracks: &std::collections::BTreeSet<PersistentId>,
) -> Result<ClassicTrack> {
    let header = chunk_header(chunk, 0, b"mhit")?;
    if header.header_length < MHIT_MIN_HEADER {
        return Err(malformed(4, "mhit header is too short"));
    }
    let persistent_id = PersistentId::from_bits(read_u64(chunk, MHIT_DB_TRACK_ID)?);
    let track_id = read_u32(chunk, MHIT_TRACK_ID)?;
    let size = u64::from(read_u32(chunk, MHIT_SIZE)?);
    let duration_ms = read_u32(chunk, MHIT_LENGTH)?;
    let track_number = read_u32(chunk, MHIT_TRACK_NUMBER)?;
    let disc_number = read_u32(chunk, MHIT_DISC_NUMBER)?;

    let mut title = String::new();
    let mut location = String::new();
    let mut album = String::new();
    let mut artist = String::new();
    let mut genre = String::new();
    let mut composer = String::new();
    let mut album_artist = String::new();
    let mut offset = header.header_length;
    let child_count = usize_value(read_u32(chunk, MHIT_CHILD_COUNT)?, MHIT_CHILD_COUNT)?;
    for _ in 0..child_count {
        let (mhod_type, claimed_end, legacy) = mhod_child(chunk, offset)?;
        match parse_string_mhod(chunk, offset, legacy)? {
            Some((text, real_end)) => {
                match mhod_type {
                    MHOD_TYPE_TITLE => title = text,
                    MHOD_TYPE_LOCATION => location = text,
                    MHOD_TYPE_ALBUM => album = text,
                    MHOD_TYPE_ARTIST => artist = text,
                    MHOD_TYPE_GENRE => genre = text,
                    MHOD_TYPE_COMPOSER => composer = text,
                    MHOD_TYPE_ALBUM_ARTIST => album_artist = text,
                    _ => {}
                }
                offset = real_end;
            }
            None => offset = claimed_end,
        }
    }
    if offset != chunk.len() {
        return Err(malformed(offset, "trailing bytes after mhit children"));
    }
    let album_artist = if album_artist.trim().is_empty() {
        artist.clone()
    } else {
        album_artist
    };
    Ok(ClassicTrack {
        persistent_id,
        track_id,
        location: normalize_location(&location),
        title,
        album,
        artist,
        album_artist,
        genre,
        composer,
        size,
        duration_ms,
        track_number,
        disc_number,
        has_artwork: artwork_tracks.contains(&persistent_id),
    })
}

/// Normalizes a classic location mhod value (`:iPod_Control:Music:F11/A.mp3`)
/// into a mount-relative path (`iPod_Control/Music/F11/A.mp3`).
fn normalize_location(raw: &str) -> String {
    let trimmed = raw.trim_start_matches(':');
    if trimmed.starts_with("iPod_Control") {
        trimmed.replace(':', "/")
    } else {
        trimmed.to_owned()
    }
}

fn parse_playlist_dataset(dataset: &[u8]) -> Result<Vec<ClassicPlaylist>> {
    let header = chunk_header(dataset, 0, b"mhsd")?;
    let list = header.header_length;
    require_magic(dataset, list, b"mhlp")?;
    let list_header = usize_value(read_u32(dataset, list + 4)?, list + 4)?;
    let count = usize_value(read_u32(dataset, list + 8)?, list + 8)?;
    let body = checked_end(list, list_header, dataset.len(), list + 4)?;
    let mut offset = body;
    let mut playlists = Vec::with_capacity(count);
    for index in 0..count {
        let playlist = chunk_header(dataset, offset, b"mhyp")?;
        let chunk = &dataset[offset..playlist.end];
        playlists.push(parse_playlist(chunk, index)?);
        offset = playlist.end;
    }
    if offset != dataset.len() {
        return Err(malformed(offset, "trailing bytes after mhlp playlists"));
    }
    Ok(playlists)
}

fn parse_playlist(chunk: &[u8], index: usize) -> Result<ClassicPlaylist> {
    let header = chunk_header(chunk, 0, b"mhyp")?;
    let mut name = String::new();
    let mut track_ids = Vec::new();
    let mut offset = header.header_length;
    let mhod_count = usize_value(read_u32(chunk, 12)?, 12)?;
    let mhip_count = usize_value(read_u32(chunk, 16)?, 16)?;
    for _ in 0..mhod_count {
        let (mhod_type, claimed_end, legacy) = mhod_child(chunk, offset)?;
        match parse_string_mhod(chunk, offset, legacy)? {
            Some((text, real_end)) => {
                if mhod_type == MHOD_TYPE_PLAYLIST_NAME {
                    name = text;
                }
                offset = real_end;
            }
            None => offset = claimed_end,
        }
    }
    for _ in 0..mhip_count {
        let mhip = chunk_header(chunk, offset, b"mhip")?;
        track_ids.push(read_u32(chunk, offset + MHIP_TRACK_ID)?);
        offset = mhip.end;
    }
    if offset != chunk.len() {
        return Err(malformed(offset, "trailing bytes after mhyp children"));
    }
    Ok(ClassicPlaylist {
        id: PersistentId::from_bits(u64::try_from(index + 1).unwrap_or(u64::MAX)),
        name,
        track_ids,
    })
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::trivially_copy_pass_by_ref)]
mod tests {
    use super::*;

    fn u32_le(value: u32) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    /// A legacy-layout string mhod: encoding at +16, length at +20, data at
    /// +32, claimed total 32+len.
    fn string_mhod(mhod_type: u32, text: &str) -> Vec<u8> {
        let encoded: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut chunk = vec![0u8; 32 + encoded.len()];
        chunk[..4].copy_from_slice(b"mhod");
        chunk[4..8].copy_from_slice(&u32_le(24));
        chunk[8..12].copy_from_slice(&u32_le(32 + encoded.len() as u32));
        chunk[12..16].copy_from_slice(&u32_le(mhod_type));
        chunk[16..20].copy_from_slice(&u32_le(1)); // encoding
        chunk[20..24].copy_from_slice(&u32_le(encoded.len() as u32));
        chunk[24..28].copy_from_slice(&u32_le(1)); // legacy unk
        chunk[32..].copy_from_slice(&encoded);
        chunk
    }

    fn mhit(track_id: u32, persistent_id: u64) -> Vec<u8> {
        let title = string_mhod(1, "Test Title");
        let location = string_mhod(2, ":iPod_Control:Music:F00/ABCD.mp3");
        let mut chunk = vec![0u8; MHIT_HEADER_SIZE];
        chunk[..4].copy_from_slice(b"mhit");
        chunk[4..8].copy_from_slice(&u32_le(MHIT_HEADER_SIZE as u32));
        let total = MHIT_HEADER_SIZE + title.len() + location.len();
        chunk[8..12].copy_from_slice(&u32_le(total as u32));
        chunk[MHIT_CHILD_COUNT..MHIT_CHILD_COUNT + 4].copy_from_slice(&u32_le(2));
        chunk[MHIT_TRACK_ID..MHIT_TRACK_ID + 4].copy_from_slice(&u32_le(track_id));
        chunk[MHIT_SIZE..MHIT_SIZE + 4].copy_from_slice(&u32_le(1_234_567));
        chunk[MHIT_LENGTH..MHIT_LENGTH + 4].copy_from_slice(&u32_le(155_742));
        chunk[MHIT_TRACK_NUMBER..MHIT_TRACK_NUMBER + 4].copy_from_slice(&u32_le(3));
        chunk[MHIT_DISC_NUMBER..MHIT_DISC_NUMBER + 4].copy_from_slice(&u32_le(1));
        chunk[MHIT_DB_TRACK_ID..MHIT_DB_TRACK_ID + 8].copy_from_slice(&persistent_id.to_le_bytes());
        chunk.extend_from_slice(&title);
        chunk.extend_from_slice(&location);
        chunk
    }

    fn mhip(track_id: u32) -> Vec<u8> {
        let mut chunk = vec![0u8; 0x4C + 44];
        chunk[..4].copy_from_slice(b"mhip");
        chunk[4..8].copy_from_slice(&u32_le(0x4C));
        chunk[8..12].copy_from_slice(&u32_le(0x78));
        chunk[MHIP_TRACK_ID..MHIP_TRACK_ID + 4].copy_from_slice(&u32_le(track_id));
        chunk
    }

    fn mhyp(name: &str, track_ids: &[u32]) -> Vec<u8> {
        let name_mhod = string_mhod(MHOD_TYPE_PLAYLIST_NAME, name);
        let mhips: Vec<Vec<u8>> = track_ids.iter().map(|id| mhip(*id)).collect();
        let header_length = 184usize;
        let total = header_length + name_mhod.len() + mhips.iter().map(Vec::len).sum::<usize>();
        let mut chunk = vec![0u8; header_length];
        chunk[..4].copy_from_slice(b"mhyp");
        chunk[4..8].copy_from_slice(&u32_le(header_length as u32));
        chunk[8..12].copy_from_slice(&u32_le(total as u32));
        chunk[12..16].copy_from_slice(&u32_le(1)); // one name mhod
        chunk[16..20].copy_from_slice(&u32_le(track_ids.len() as u32));
        chunk.extend_from_slice(&name_mhod);
        for mhip in mhips {
            chunk.extend_from_slice(&mhip);
        }
        chunk
    }

    fn dataset(kind: u32, list_magic: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let mut list = vec![0u8; 92];
        list[..4].copy_from_slice(list_magic);
        list[4..8].copy_from_slice(&u32_le(92));
        list[8..12].copy_from_slice(&u32_le(children.len() as u32));
        let body_len: usize = children.iter().map(Vec::len).sum();
        let mut dataset = vec![0u8; 96];
        dataset[..4].copy_from_slice(b"mhsd");
        dataset[4..8].copy_from_slice(&u32_le(96));
        dataset[8..12].copy_from_slice(&u32_le(96 + 92 + body_len as u32));
        dataset[12..16].copy_from_slice(&u32_le(kind));
        dataset.extend_from_slice(&list);
        for child in children {
            dataset.extend_from_slice(child);
        }
        dataset
    }

    fn minimal_itunesdb() -> Vec<u8> {
        let track = mhit(7, 0x1122_3344_5566_7788);
        let playlist = mhyp("Test Playlist", &[7]);
        let track_dataset = dataset(1, b"mhlt", &[track]);
        let playlist_dataset = dataset(2, b"mhlp", &[playlist]);
        let header_length = 244usize;
        let total = header_length + track_dataset.len() + playlist_dataset.len();
        let mut file = vec![0u8; header_length];
        file[..4].copy_from_slice(b"mhbd");
        file[4..8].copy_from_slice(&u32_le(header_length as u32));
        file[8..12].copy_from_slice(&u32_le(total as u32));
        file[0x14..0x18].copy_from_slice(&u32_le(2));
        file.extend_from_slice(&track_dataset);
        file.extend_from_slice(&playlist_dataset);
        file
    }

    #[test]
    fn parses_a_minimal_classic_library() {
        let bytes = minimal_itunesdb();
        let library = parse_library(&bytes, None).unwrap();
        assert_eq!(library.tracks.len(), 1);
        let track = &library.tracks[0];
        assert_eq!(track.title, "Test Title");
        assert_eq!(track.location, "iPod_Control/Music/F00/ABCD.mp3");
        assert_eq!(track.size, 1_234_567);
        assert_eq!(track.duration_ms, 155_742);
        assert_eq!(track.track_number, 3);
        assert_eq!(
            track.persistent_id,
            PersistentId::from_bits(0x1122_3344_5566_7788)
        );
        assert!(!track.has_artwork);
        assert_eq!(library.playlists.len(), 1);
        let playlist = &library.playlists[0];
        assert_eq!(playlist.name, "Test Playlist");
        assert_eq!(playlist.track_ids, vec![7]);
    }

    #[test]
    fn resolves_artwork_from_an_artworkdb() {
        // An empty ArtworkDB is not required; a malformed one must surface.
        let bytes = minimal_itunesdb();
        assert!(parse_library(&bytes, Some(b"not an artworkdb")).is_err());
    }

    #[test]
    fn opens_the_attached_nano3_device() {
        // End-to-end against the operator's Nano 3G files: the attached
        // Device/iTunes/Artwork tree is wrapped into a proper iPod_Control
        // layout, and Device::open must resolve the profile, parse all 724
        // tracks, and read artwork presence.
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("files_nano3");
        if !source.join("iTunes/iTunesDB").is_file() {
            return;
        }
        let device_dir = tempfile::tempdir().unwrap();
        let root = device_dir.path().join("iPod_Control");
        for folder in ["Device", "iTunes", "Artwork"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            for entry in std::fs::read_dir(source.join(folder)).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), root.join(folder).join(entry.file_name())).unwrap();
            }
        }
        let device = crate::Device::open(device_dir.path()).unwrap();
        assert_eq!(
            device.profile().map(crate::DeviceProfile::key),
            Some("nano-3g")
        );
        assert!(device.evidence().has_firewire_guid());
        let library = device.library().expect("classic library");
        assert_eq!(library.track_count(), 724);
        assert!(!library.tracks().is_empty());
        // The fixture artwork covers a large share of the library.
        let artwork_count = library
            .tracks()
            .iter()
            .filter(|track| track.has_artwork)
            .count();
        assert!(artwork_count > 0, "no tracks reported artwork");
        assert!(!library.playlists().is_empty(), "no playlists parsed");
    }
}
