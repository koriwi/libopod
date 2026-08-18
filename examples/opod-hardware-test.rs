use std::{error::Error, ffi::OsString, io::Error as IoError, path::PathBuf, process::ExitCode};

use libopod::{
    recover_interrupted_transaction, Device, TrackToAdd, NANO7_ADDITION_HARDWARE_TEST_CONFIRMATION,
    NANO7_ARTWORK_REMOVAL_HARDWARE_TEST_CONFIRMATION, NANO7_NOOP_HARDWARE_TEST_CONFIRMATION,
    NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION,
};

type CliResult<T> = Result<T, Box<dyn Error>>;

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match run(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[OsString]) -> CliResult<()> {
    match arguments {
        [command, mount, staging, confirmation] if command == "noop" => {
            let confirmation = confirmation
                .to_str()
                .ok_or_else(|| invalid_input("confirmation must be UTF-8"))?;
            run_noop(mount, staging, confirmation)
        }
        [command, mount, index, staging, confirmation] if command == "remove" => {
            let index = parse_index(index)?;
            let confirmation = confirmation
                .to_str()
                .ok_or_else(|| invalid_input("confirmation must be UTF-8"))?;
            run_removal(mount, index, staging, confirmation)
        }
        [command, mount, source, title, artist, album, length_ms, staging, confirmation]
            if command == "add" =>
        {
            let length_ms = parse_u32(length_ms, "LENGTH_MS")?;
            let confirmation = confirmation
                .to_str()
                .ok_or_else(|| invalid_input("confirmation must be UTF-8"))?;
            run_addition(mount, source, title, artist, album, length_ms, staging, confirmation)
        }
        [command, mount] if command == "recover" => run_recovery(mount),
        _ => Err(invalid_input(&format!(
            "usage:\n  opod-hardware-test noop MOUNT EMPTY_HOST_DIRECTORY '{NANO7_NOOP_HARDWARE_TEST_CONFIRMATION}'\n  opod-hardware-test remove MOUNT TRACK_INDEX EMPTY_HOST_DIRECTORY '{NANO7_REMOVAL_HARDWARE_TEST_CONFIRMATION}'\n  opod-hardware-test remove MOUNT TRACK_INDEX EMPTY_HOST_DIRECTORY '{NANO7_ARTWORK_REMOVAL_HARDWARE_TEST_CONFIRMATION}'  (for an artwork-bearing track)\n  opod-hardware-test add MOUNT SOURCE_MP3 TITLE ARTIST ALBUM LENGTH_MS EMPTY_HOST_DIRECTORY '{NANO7_ADDITION_HARDWARE_TEST_CONFIRMATION}'\n  opod-hardware-test recover MOUNT"
        ))),
    }
}

fn run_noop(mount: &OsString, staging: &OsString, confirmation: &str) -> CliResult<()> {
    let device = Device::open(PathBuf::from(mount))?;
    let staged = device.stage_noop_preview(PathBuf::from(staging))?;
    device.install_noop_hardware_test(&staged, confirmation)?;
    println!("Nano 7G no-op transaction completed and read back successfully.");
    println!("No tracks, artwork, or media files were changed.");
    println!("Keep the host bundle, safely eject, reboot, and verify browsing/playback.");
    Ok(())
}

fn run_removal(
    mount: &OsString,
    index: usize,
    staging: &OsString,
    confirmation: &str,
) -> CliResult<()> {
    let device = Device::open(PathBuf::from(mount))?;
    let track = device
        .library()
        .and_then(|library| library.tracks().get(index))
        .ok_or_else(|| invalid_input("TRACK_INDEX is not present in the opened library"))?;
    let mut edit = device.edit()?;
    edit.remove_track(track.id)?;
    let staged = edit.stage_sqlite_preview(PathBuf::from(staging))?;
    if track.has_artwork {
        device.install_single_artwork_removal_hardware_test(&staged, confirmation)?;
        println!(
            "Nano 7G one-track artwork removal transaction completed and read back successfully."
        );
        println!(
            "ArtworkDB records now: {}",
            704 - staged.removed_artwork_tracks()
        );
        println!("The .ithmb slots remain as unreferenced data; the MP3 remains on disk.");
    } else {
        device.install_single_removal_hardware_test(&staged, confirmation)?;
        println!("Nano 7G one-track removal transaction completed and read back successfully.");
        println!("The media file remains on the iPod as an unreferenced safety copy.");
    }
    println!("removed database index: {index}");
    println!("remaining tracks: {}", staged.remaining_tracks());
    println!("Keep the host bundle, safely eject, reboot, and validate before continuing.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_addition(
    mount: &OsString,
    source: &OsString,
    title: &OsString,
    artist: &OsString,
    album: &OsString,
    length_ms: u32,
    staging: &OsString,
    confirmation: &str,
) -> CliResult<()> {
    let device = Device::open(PathBuf::from(mount))?;
    let text = |value: &OsString| value.to_str().map(str::to_owned);
    let track = TrackToAdd {
        source_path: PathBuf::from(source),
        title: text(title).unwrap_or_default(),
        artist: text(artist).filter(|value| !value.is_empty()),
        album: text(album).filter(|value| !value.is_empty()),
        album_artist: None,
        genre: None,
        composer: None,
        year: 0,
        track_number: 1,
        total_tracks: 1,
        disc_number: 1,
        total_discs: 1,
        bitrate: 192,
        sample_rate: 44_100,
        length_ms,
        compilation: false,
        reuse_album_art: false,
    };
    if track.title.is_empty() {
        return Err(invalid_input("TITLE must be non-empty"));
    }
    let mut edit = device.edit()?;
    edit.add_track(track)?;
    let staged = edit.stage_sqlite_preview(PathBuf::from(staging))?;
    device.install_single_addition_hardware_test(&staged, confirmation)?;
    println!("Nano 7G one-track addition transaction completed and read back successfully.");
    println!("added database index: {}", staged.remaining_tracks() - 1);
    println!("total tracks: {}", staged.remaining_tracks());
    println!("installed media: {}", staged.added_media()[0]);
    println!("Keep the host bundle, safely eject, reboot, and validate before continuing.");
    Ok(())
}

fn run_recovery(mount: &OsString) -> CliResult<()> {
    if recover_interrupted_transaction(PathBuf::from(mount))? {
        println!("Verified transaction backups were restored or committed cleanup completed.");
    } else {
        println!("No interrupted libopod transaction was present.");
    }
    Ok(())
}

fn parse_index(value: &OsString) -> CliResult<usize> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid_input("TRACK_INDEX must be a non-negative decimal integer"))
}

fn parse_u32(value: &OsString, name: &str) -> CliResult<u32> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid_input(&format!("{name} must be a non-negative decimal integer")))
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(IoError::other(message.to_owned()))
}
