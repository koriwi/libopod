# libopod

Experimental Rust library for managing storage-mounted iPods running Apple firmware.

[![CI](https://github.com/koriwi/libopod/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/koriwi/libopod/actions/workflows/ci.yml)

CI runs tests, documentation tests, and Clippy for bundled and system SQLite on
pushes and fork PRs. See [CI setup](.github/CI.md) for required checks, trusted
`main`-workflow runs of existing PRs, and the limits of public-fixture coverage.

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
| iPod Nano 2G | ✅ Music tested; artwork and podcasts need playback verification |
| iPod Nano 3G | ✅ Music tested; podcasts need playback verification |
| iPod Nano 7G | ✅ Tested |
| iPod Nano 1G, 4G | 🧪 Music and podcasts implemented; hardware testing needed |
| iPod Nano 5G, 6G | 🧪 Testers wanted |
| iPod Classic 160 GB revision B (7G) | ✅ Music sync tested; podcasts need testing |
| iPod Classic 6G, 120 GB revision A | 🧪 Implemented; hardware testing needed |

Testing on unlisted hardware is greatly appreciated. See [HARDWARE_TESTING.md](HARDWARE_TESTING.md) before writing to a device.

## iPod Nano 2G artwork

Nano 2G now supports cover additions, reuse, removal/reindexing, and recovery.
Its native cover formats are **1031 (42×42, 3,528 bytes)** and **1027 (100×100,
20,000 bytes)**, RGB565 little-endian without row padding. These match
[libgpod's device table](https://github.com/fadingred/libgpod/blob/master/src/itdb_device.c)
and read-only measurements of the operator's Nano 2G files. Existing reused
cover records retain their padding/crop and opaque child metadata.

The writer requires initialized `iTunesDB` and `ArtworkDB` files. It supports
legacy 0x148-byte track headers, linking covers through track persistent IDs.
Nano 2G uses verified full-file replacements with rename backups, not incremental
thumbnail appends. Synthetic tests and edits on a disposable copy of native
files pass; physical display/playback qualification remains outstanding.
Nano 1G artwork writing remains disabled; its music edits leave artwork alone.

## Click-wheel Nano podcast support

Nano 1G through 4G support podcast additions and removals through their binary
`iTunesDB` backend. Podcast tracks get the media-kind, resume, shuffle, unplayed,
and Now Playing flags, plus a protected Podcasts playlist with dataset-3 show
groups. Nano 1G and 2G use the same version-0x13, 0x148-byte track layout;
Nano 3G and 4G use their HASH58-signed layout. Synthetic tests cover all four
generations, but podcast playback still needs hardware verification.

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

## Repeated commits and I/O

`install_and_open(device, mode, callback)` returns the device handle from the
mandatory installation read-back after the transaction has committed and its
journal is gone. Batched callers can use it for their next edit without
reopening and fingerprinting all database/artwork files again.

Staging verifies the initial generation through its host backup rather than
hashing every live input before copying it. Staging reads the main databases
from those host snapshots. Thumbnail staging copies the existing host prefix
once per format, then appends new frames and flushes once. Installation verifies
the host snapshots and rechecks the live generation, but no longer copies those
snapshots back to USB as rollback backups. For replacements it writes, flushes and verifies
a replacement sibling first, then renames the original into the recovery
directory, flushes both directories and verifies the preserved original before
publishing the replacement. Full original files remain on-device until commit.
Rollback also uses verified renames rather than allocating another full copy.
Keep the entire staging bundle, including `original`, intact until installation
finishes. Recovery still needs no host bundle and keeps full byte verification.

### Incremental thumbnail installation

On supported Unix file geometry, growing `F1055_1.ithmb`, `F1068_1.ithmb`
(Classic, Nano 3G/4G) and `F1060_1.ithmb` (Classic, Nano 3G) can append only new
frames. Eligibility requires whole aligned slots and a staged prefix whose
SHA-256 matches the original generation. Read-only or multiply linked files,
unaligned formats, reindexed/changed prefixes and unsupported hosts retain full
replacement. The optimizer also falls back when two suffix writes would exceed
the cost of one complete replacement.

Before append intent, installation writes, flushes and verifies an on-device
**suffix spool** containing only the new bytes. Those bytes are then appended to
the live thumbnail and fully verified. The old prefix stays in the live file;
it is not copied to another on-device backup. Recovery verifies the entire old
prefix, compares any partial suffix byte-for-byte with the spool, and binds the
spool to the expected complete output hash before truncating back to the old
length. Required spools remain until terminal cleanup. A small, physically
allocated journal-space reserve supports rollback if an append fills the volume.
Full file verification and generation checks remain enabled in both install modes.
Host staging still builds complete preview files.

New transactions use journal **version 4**; the directory name remains
`.libopod-transaction-v1`. Recovery also accepts version 2 copy-backup and
version 3 rename-backup journals. Older binaries cannot recover version 4:
do not downgrade with a pending transaction. Recover pending append transactions
on a supported Unix host before moving to another host. A power cut between
renames can leave a live file temporarily absent; run recovery before using the
iPod. Terminal journals remain until cleanup finishes, making cleanup retryable.
Rename/append boundaries have synthetic fault-injection coverage, not hardware
power-loss qualification. Keep an independent verified backup.
Media allocation inventories `Music/Fxx` once per staging batch, not per song,
and reserves complete filenames case-insensitively before copying.

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
