use std::{
    error::Error,
    ffi::OsString,
    io::{Error as IoError, ErrorKind},
    path::PathBuf,
    process::ExitCode,
};

type CliResult<T> = Result<T, Box<dyn Error>>;

use libopod::Device;

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
        [mount, command] if command == "list" => list(mount),
        [mount, command, index, destination] if command == "stage" => {
            let index = parse_index(index)?;
            stage(mount, index, destination)
        }
        _ => Err(invalid_input(
            "usage: opod-stage-remove MOUNT list | opod-stage-remove MOUNT stage INDEX EMPTY_HOST_DIRECTORY",
        )),
    }
}

fn list(mount: &OsString) -> CliResult<()> {
    let device = Device::open(PathBuf::from(mount))?;
    let library = device
        .library()
        .ok_or_else(|| invalid_input("the authoritative library is unavailable"))?;
    println!("index\tartwork\tbytes\ttitle\tartist\talbum");
    for (index, track) in library.tracks().iter().enumerate() {
        println!(
            "{index}\t{}\t{}\t{}\t{}\t{}",
            if track.has_artwork { "yes" } else { "no" },
            track.size,
            track.title,
            track.artist,
            track.album,
        );
    }
    Ok(())
}

fn stage(mount: &OsString, index: usize, destination: &OsString) -> CliResult<()> {
    let device = Device::open(PathBuf::from(mount))?;
    let track = device
        .library()
        .and_then(|library| library.tracks().get(index))
        .ok_or_else(|| invalid_input("INDEX is not present in the opened library"))?;
    let mut edit = device.edit()?;
    edit.remove_track(track.id)?;
    let staged = edit.stage_sqlite_preview(PathBuf::from(destination))?;
    println!("staged removals: {}", staged.removed_tracks());
    println!("remaining tracks: {}", staged.remaining_tracks());
    println!("output: {}", staged.directory().display());
    println!("WARNING: this host-only preview is incomplete; never copy it to an iPod.");
    Ok(())
}

fn parse_index(value: &OsString) -> CliResult<usize> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            invalid_input("INDEX must be a non-negative decimal integer from the list command")
        })
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(IoError::new(ErrorKind::InvalidInput, message))
}
