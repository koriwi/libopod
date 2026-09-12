use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::Path,
};

use super::generation::fingerprint_host_file;
use crate::{error::io_error, Device, Error, IpodPath, Result};

/// One directory inventory per staging batch, including names reserved by
/// earlier additions in that batch. Never cache it across device generations.
pub(super) struct MediaAllocator {
    directories: BTreeMap<String, BTreeSet<String>>,
}

impl MediaAllocator {
    pub(super) fn scan(device: &Device) -> Result<Self> {
        let music = device
            .mount()
            .resolve_existing(&IpodPath::new("iPod_Control/Music")?)?;
        let mut directories = BTreeMap::new();
        for entry in
            fs::read_dir(&music).map_err(|error| io_error("read Music directory", &music, error))?
        {
            let entry = entry.map_err(|error| io_error("read Music entry", &music, error))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.len() != 3
                || !name.starts_with('F')
                || !name[1..]
                    .chars()
                    .all(|character| character.is_ascii_digit())
                || !entry
                    .file_type()
                    .map_err(|error| io_error("inspect Music entry", entry.path(), error))?
                    .is_dir()
            {
                continue;
            }
            let path = entry.path();
            let mut names = BTreeSet::new();
            for file in fs::read_dir(&path)
                .map_err(|error| io_error("read media directory", &path, error))?
            {
                let file = file.map_err(|error| io_error("read media entry", &path, error))?;
                names.insert(file.file_name().to_string_lossy().to_ascii_uppercase());
            }
            directories.insert(name.into_owned(), names);
        }
        if directories.is_empty() {
            return Err(Error::Unsupported {
                feature: "media allocation",
                reason: "the device has no Music/Fxx media directories".to_owned(),
            });
        }
        Ok(Self { directories })
    }

    /// Stage a verified host copy in the least-populated directory, counting
    /// queued additions as well as live entries. Installation still checks
    /// that each allocated target is absent before publishing any new file.
    pub(super) fn stage_copy(&mut self, destination: &Path, source: &Path) -> Result<String> {
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("mp3");
        if !extension.eq_ignore_ascii_case("mp3") {
            return Err(Error::Unsupported {
                feature: "media allocation",
                reason: "only MP3 sources are supported by the first addition gate".to_owned(),
            });
        }
        let (folder, names) = self
            .directories
            .iter_mut()
            .min_by_key(|(_, names)| names.len())
            .ok_or_else(|| Error::Verification {
                format: "media allocation",
                reason: "no media directory could be selected".to_owned(),
            })?;
        let filename = reserve_name(names, random_media_name)?;
        let staged_dir = destination.join("iPod_Control").join("Music").join(folder);
        fs::create_dir_all(&staged_dir)
            .map_err(|error| io_error("create staged media directory", &staged_dir, error))?;
        let staged = staged_dir.join(&filename);
        let mut input =
            File::open(source).map_err(|error| io_error("open media source", source, error))?;
        // Never overwrite another staged payload, even if the bundle changed
        // unexpectedly after allocation. Source and copy hashes remain strict.
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(|error| io_error("create staged media file", &staged, error))?;
        std::io::copy(&mut input, &mut output)
            .map_err(|error| io_error("stage media file", &staged, error))?;
        drop(output);
        let expected = fingerprint_host_file(source)?;
        let actual = fingerprint_host_file(&staged)?;
        if actual != expected || actual.0 == 0 {
            let _ = fs::remove_file(&staged);
            return Err(Error::Verification {
                format: "staged media file",
                reason: if actual == expected {
                    "the source audio file is empty"
                } else {
                    "staged copy did not verify against its source"
                }
                .to_owned(),
            });
        }
        Ok(format!("{folder}/{filename}"))
    }
}

fn reserve_name(
    names: &mut BTreeSet<String>,
    mut generate: impl FnMut() -> String,
) -> Result<String> {
    for _ in 0..64 {
        let filename = format!("{}.mp3", generate());
        // Compare complete filenames (including .mp3), case-insensitively for
        // FAT, and reserve immediately so this batch cannot reuse the name.
        if names.insert(filename.to_ascii_uppercase()) {
            return Ok(filename);
        }
    }
    Err(Error::Verification {
        format: "media allocation",
        reason: "could not find a free media filename".to_owned(),
    })
}

fn random_media_name() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut output = String::with_capacity(4);
    for _ in 0..4 {
        output.push(char::from(
            ALPHABET[(crate::random::next_u64() % 36) as usize],
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::classic_tests::virtual_classic;
    use tempfile::tempdir;

    #[test]
    fn names_include_extension_and_reserve_case_insensitively_within_batch() {
        let mut names = BTreeSet::from(["TAKN.MP3".to_owned()]);
        let mut sequence = ["takn", "FREE", "free", "NEXT"].into_iter();
        assert_eq!(
            reserve_name(&mut names, || sequence.next().unwrap().to_owned()).unwrap(),
            "FREE.mp3"
        );
        assert_eq!(
            reserve_name(&mut names, || sequence.next().unwrap().to_owned()).unwrap(),
            "NEXT.mp3"
        );
        assert_eq!(names.len(), 3);
        assert!(reserve_name(&mut names, || "takn".to_owned()).is_err());
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn cached_inventory_balances_additions_without_rescanning_live_directories() {
        let directory = virtual_classic("ModelNumStr: MC293", false);
        let music = directory.path().join("iPod_Control/Music");
        fs::write(music.join("F00/TAKN.mp3"), b"existing").unwrap();
        let device = Device::open(directory.path()).unwrap();
        let mut allocator = MediaAllocator::scan(&device).unwrap();
        // Only a disposable test mount: make live rescans impossible after
        // capturing the inventory. Installation would refuse these targets
        // until their parent directories are available again.
        let hidden = directory.path().join("hidden-music");
        fs::rename(&music, &hidden).unwrap();
        let source = directory.path().join("source.MP3");
        fs::write(&source, b"new synthetic audio").unwrap();
        let bundle = tempdir().unwrap();
        for expected_folder in ["F01/", "F02/", "F03/"] {
            let relative = allocator.stage_copy(bundle.path(), &source).unwrap();
            assert!(relative.starts_with(expected_folder), "{relative}");
            let staged = bundle.path().join("iPod_Control/Music").join(relative);
            assert_eq!(fs::read(staged).unwrap(), b"new synthetic audio");
        }
        fs::rename(&hidden, &music).unwrap();
        assert_eq!(fs::read(music.join("F00/TAKN.mp3")).unwrap(), b"existing");
        assert_eq!(fs::read_dir(music.join("F01")).unwrap().count(), 0);
    }
}
