use std::{error::Error, ffi::OsString, io::Error as IoError, path::PathBuf, process::ExitCode};

use libopod::{recover_interrupted_transaction, Device, NANO7_NOOP_HARDWARE_TEST_CONFIRMATION};

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
        [command, mount] if command == "recover" => run_recovery(mount),
        _ => Err(invalid_input(&format!(
            "usage:\n  opod-hardware-test noop MOUNT EMPTY_HOST_DIRECTORY '{NANO7_NOOP_HARDWARE_TEST_CONFIRMATION}'\n  opod-hardware-test recover MOUNT"
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

fn run_recovery(mount: &OsString) -> CliResult<()> {
    if recover_interrupted_transaction(PathBuf::from(mount))? {
        println!("Verified transaction backups were restored or committed cleanup completed.");
    } else {
        println!("No interrupted libopod transaction was present.");
    }
    Ok(())
}

fn invalid_input(message: &str) -> Box<dyn Error> {
    Box::new(IoError::other(message.to_owned()))
}
