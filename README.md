# libopod

Experimental Rust library for managing storage-mounted iPods running Apple firmware.

> **Vibe-coded with AI assistance.** Keep a verified backup. Expect bugs.

## Installation

```console
cargo add libopod-rs
```

The package name is `libopod-rs`; the Rust library and import path remain
`libopod`:

```rust,no_run
use libopod::Device;

let device = Device::open("/path/to/ipod/mount")?;
println!("{} tracks", device.library().map_or(0, |library| library.track_count()));
# Ok::<(), libopod::Error>(())
```

## Hardware status

| Device | Status |
|---|---|
| iPod Nano 2G | ✅ Tested |
| iPod Nano 3G | ✅ Tested |
| iPod Nano 7G | ✅ Tested |
| iPod Nano 1G, 4G, 5G, 6G | 🧪 Testers wanted |
| iPod Classic 160 GB revision B (7G) | ✅ Music sync tested; podcasts need testing |
| iPod Classic 6G, 120 GB revision A | 🧪 Implemented; hardware testing needed |

Testing on unlisted hardware is greatly appreciated. See [HARDWARE_TESTING.md](HARDWARE_TESTING.md) before writing to a device.

## iPod Classic support

Classic profiles support binary `iTunesDB` reads, track additions/removals,
standard playlists, HASH58 signing, cover art, and transaction recovery.
Podcast additions include the flagged Podcasts playlist, dataset-3 show
groups, resume/shuffle flags, and removal of empty show groups.
The 2009 160 GB revision B (often called 7G) is identified by `MC293`/`MC297`
(or `C293`/`C297`) model numbers. USB PID `0x1261` identifies the Classic
family, not its revision; all three revisions share the same writer.
Linux can obtain this PID and the required `FireWire` GUID from USB when
`SysInfo` is empty. Other platforms need on-device identity/signing evidence.

Initialize the library with iTunes or another compatible manager first:
libopod edits an existing `iPod_Control/iTunes/iTunesDB`; it does not bootstrap
a blank device. Cover-art writes also require an existing `ArtworkDB`.
Synthetic tests cover music, playlists, artwork, signing and interrupted
transaction recovery. The operator confirmed music sync on Classic 7G;
podcast playback and other Classic revisions still need hardware verification. Keep a complete backup and verify playback after safely ejecting.

## Reference

Database layouts and device behavior are based on the [iOpenPod](https://github.com/TheRealSavi/iOpenPod) reference implementation.

libopod preserves existing device data where possible instead of rebuilding everything from scratch.

## Inspect an iPod

```console
cargo run --example opod-inspect -- /path/to/ipod/mount
```

The inspector hides serial numbers, `FireWire` GUIDs, and track metadata.

## Installation verification modes

`StagedSqliteEdit::install` and `install_with_progress` keep full verification
by default. `install_with_mode(device, InstallMode::Fast, callback)` avoids
repeated MP3 reads: it verifies newly allocated MP3s against their manifest
hash while copying, flushes them, and checks the installed sizes instead of
reading their contents back. Same-size destination corruption can go
undetected in fast mode. Database/artwork verification, signing, backups,
journals and recovery remain unchanged. Fast mode does not reduce source
scanning, staging or database-rewrite work.

## Progress callbacks

The existing staging, installation and recovery methods remain silent. For
live UI or logging, use `EditSession::stage_sqlite_preview_with_progress`,
`StagedSqliteEdit::install_with_progress`, or
`recover_interrupted_transaction_with_progress`, each with an
`FnMut(ProgressEvent)` callback. Installation callbacks also receive automatic
rollback progress. Recovery reports paths without opening the inconsistent
library, and it validates the interrupted state before any destructive work. Events run synchronously before the named work; item counters are
one-based and per operation. Events can contain track titles or paths, so they
are not redacted. Callbacks must not panic or mutate the device/bundle.

## More information

- [Hardware testing](HARDWARE_TESTING.md)
- [Architecture and safety plan](plan.md)
- License: MIT
