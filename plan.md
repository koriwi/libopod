# libopod research and implementation plan

## 1. Goal

Build **libopod** as an MIT-licensed, safe, documented Rust library for
mounted, storage-backed iPods. Its first consumer will be
[copyPod](https://github.com/koriwi/copyPod), but the public API should support
other sync tools without exposing iTunesDB internals. Version 1.0 is Linux-first
and exposes a Rust API only.

Target devices:

- Full-size iPods, including the later “iPod Classic” models
- iPod Mini 1G–2G
- iPod Nano 1G–7G, including the non-click-wheel Nano 6G/7G
- Apple firmware and mounted mass-storage mode

Explicit non-goals for the first release:

- iPod Shuffle (`iTunesSD` is a different database family)
- iPod Touch/iPhone/iPad device protocols
- Rockbox database management
- Audio transcoding, tag extraction, mounting, and ejecting; callers own those
  concerns

The release must support Nano 5G/6G/7G as real devices, not merely recognize
the model names.

## 2. Research completed

The following upstream snapshots were reviewed:

| Project | Snapshot | Relevant findings |
| --- | --- | --- |
| [iOpenPod](https://github.com/TheRealSavi/iOpenPod) | `3f6e20f66c203abe900812b64d90a5e5270d2bb3` | MIT implementation of device identification, binary iTunesDB/CDB parsing and writing, ArtworkDB/ithmb, checksums, and the SQLite-era databases |
| [copyPod](https://github.com/koriwi/copyPod) | `8580517ba98c45c8e8615ba3fc87d714c6d1c541` | The immediate API contract: open by mount point, enumerate tracks, copy/add/remove tracks, inspect/set artwork, identify the device/GUID, and commit |
| [libgpod](https://github.com/gtkpod/libgpod) | `4a8a33ef4bc58eee1baca6793618365f75a5c3fa` | Legacy format reference, compressed DB and SQLite paths, device tables, and the reason modern support is unreliable in practice |
| [dstaley/hashab](https://github.com/dstaley/hashab) | `f80d46432204c6238cad7d8ca3b3dd52ea66836b` | Public-domain/ISC-compatible C implementation and 100 vectors for Nano 6G/7G HASHAB |
| [ipodsync](https://github.com/buldiei/ipodsync) | `28174c7f2f8ded61d19782fd2b2ee4922414db71` | Independently hardware-tested Nano 6G/7G SQLite edits, CBK signing, and Nano 7G artwork behavior |
| [Ithmb-Codec](https://github.com/B67687/Ithmb-Codec) | `6809cf55f728f6f0a662584d9c46006b1f84504c` | Useful MIT Rust artwork codecs and profiles; the currently exposed PhotoDB API is mainly a parser, so it does not replace libopod’s ArtworkDB writer |
| Existing Rust iPod projects | current default branches | `rust-libgpod` is only an FFI experiment, `rPod` is effectively empty, and `iTunesDB-Parser` is a partial read-only extractor without CDB support |

### Important conclusions

1. **Modern Nano support is more than a checksum fix.** Nano 5G–7G have
   `iTunes Library.itlp` SQLite databases (`Library.itdb`, `Locations.itdb`,
   `Dynamic.itdb`, `Extras.itdb`, `Genius.itdb`) and a signed
   `Locations.itdb.cbk`. Nano 6G/7G use these as the effective library. A port
   that only writes `iTunesCDB` is incomplete.
2. **There are two storage backends behind one domain model:**
   traditional binary `iTunesDB`/compressed `iTunesCDB`, and the SQLite-era
   `.itlp` set.
3. **The signing matrix is device-specific:**
   - no checksum: full-size iPod through 5.5G, Mini 1G/2G, Nano 1G/2G
   - HASH58: iPod Classic 6G/6.5G/7G and Nano 3G/4G
   - HASH72: Nano 5G; needs `HashInfo` material or a valid existing signature
   - HASHAB: Nano 6G/7G; needs the 8-byte FireWire GUID
4. HASHAB has two easily confused values: its checksum enum / MHBD scheme is
   observed as `3`, while the separate header indicator at `0x70` is `4`.
   Some iOpenPod prose/constants conflate them, while its final writer and
   libgpod use `3` at `0x30`. Golden files and hardware, not comments, must
   settle every such conflict.
5. `iTunesCDB` is an uncompressed `mhbd` header plus a level-1 zlib stream.
   Signing occurs **after** compression. A CDB write also leaves a zero-byte
   `iTunesDB` marker.
6. Artwork is profile-specific. Format IDs, dimensions, row stride, pixel
   layout, sparse behavior, and even the meaning of an ID can vary. Nano 7G,
   for example, has device-specific interpretations of 1013/1015/1016.
7. libgpod contains nominal Nano 6G/HASHAB hooks, but dynamically loads an
   external hash library and lacks complete current model/profile handling.
   This explains why source-level “support” does not translate into a usable
   distribution.
8. iOpenPod’s relevant device/database/artwork code is roughly 38k lines. A
   literal Python-to-Rust translation would carry UI/application assumptions
   into the crate. libopod should port verified behavior and fixtures into a
   smaller backend-oriented design.
9. iOpenPod and copyPod are MIT. libgpod is LGPL-family code. To keep libopod
   permissive, implementation code should come from MIT/public-domain sources
   or be independently written from documented behavior and vectors. Every
   imported table/vector needs provenance in `THIRD_PARTY.md`.

## 3. Proposed product boundary

### First stable release

Version 1.0 is deliberately scoped to copyPod's requirements. It will:

- Open an existing, Apple/iTunes-initialized mounted iPod safely.
- Resolve an unambiguous device profile.
- Read tracks from the authoritative database backend.
- Add/copy MP3 tracks from caller-supplied metadata.
- Remove tracks and all playlist/artwork references to them.
- Add, replace, and remove cover artwork from JPEG/PNG input.
- Preserve playlists and metadata not edited by the caller.
- Write every required binary, compressed, SQLite, artwork, and checksum file.
- Back up, stage, validate, install, and read back a commit.
- Work through one Rust API on every target generation.

This is enough to replace `src/gpod.rs`, `gpod_shim.c`, `gpod_shim.h`,
`build.rs`, and the system libgpod dependency in copyPod.

### Follow-up parity

- Playlist CRUD and folders
- Smart playlists
- Podcasts/audiobooks/video-specific metadata
- Play Counts, ratings, bookmarks, OTG playlists, and chapters
- Photo database management
- Device bootstrap/initialization from an entirely empty volume
- macOS and Windows qualification
- Optional device discovery/USB identity adapters
- Optional C ABI if non-Rust consumers need one

The parser should retain these fields from day one even if the first public API
does not edit all of them.

## 4. Architecture

Start with one public crate rather than a workspace of tiny crates. Keep format
modules private until their APIs prove stable.

```text
libopod/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── device/       # SysInfo, model catalog, evidence, capabilities
│   ├── library/      # public Track/Playlist model and edit session
│   ├── storage/
│   │   ├── binary/   # iTunesDB and iTunesCDB parser/writer
│   │   └── sqlite/   # .itlp reader/editor/writer
│   ├── artwork/      # ArtworkDB, profile registry, ithmb encoding
│   ├── crypto/       # HASH58, HASH72, HASHAB, CBK
│   └── fs/           # safe paths, locking, staging, recovery, durability
├── tests/
│   ├── fixtures/
│   ├── format/
│   ├── differential/
│   └── virtual_device/
├── fuzz/
├── examples/
├── THIRD_PARTY.md
└── HARDWARE_TESTING.md
```

### Public API shape

Use stable IDs rather than pointer-like handles that become invalid when a
collection changes.

```rust
let mut device = libopod::Device::open("/run/media/user/IPOD")?;

println!("{}", device.profile().display_name());
for track in device.library().tracks() {
    println!("{}: {}", track.id(), track.title());
}

let mut edit = device.edit()?;
edit.remove_track(track_id)?;
edit.add_track_from_path(source, metadata, artwork)?;
let report = edit.commit(libopod::CommitOptions::default())?;
```

Principal types:

- `Device`, `DeviceProfile`, `DeviceCapabilities`, `IdentityEvidence`
- `Library`, `Track`, `TrackId`/`PersistentId`, `Playlist`, `PlaylistId`
- `TrackMetadata`, `ArtworkInput`, `AddedTrack`
- `EditSession`, `CommitOptions`, `CommitReport`, `BackupManifest`
- a non-exhaustive typed `Error`; library code must not expose `anyhow::Error`

Design rules:

- Opening and reading may work with partial identity; writing requires one
  unambiguous profile or an explicit, typed override.
- All on-device paths use a validated relative `IpodPath`. Host paths cannot
  escape the mount through `..` or symlinks.
- Metadata parsing remains in copyPod/other callers. libopod accepts normalized
  metadata and does not depend on `lofty`.
- Track copying and Fxx allocation belong in libopod because they must commit
  consistently with database location records.
- No mutation happens on `Drop`; the caller explicitly commits.
- Unknown fields/chunks are retained as opaque data wherever possible.

## 5. Device identification and profiles

Use source-ranked evidence rather than one overloaded “serial” string:

1. Exact model/order number from SysInfo/SysInfoExtended
2. Apple product serial suffix lookup
3. Board/family IDs and trusted SysInfoExtended capabilities
4. Normal-mode USB PID, when supplied by a caller/optional platform adapter
5. Existing database format/checksum and mounted capacity as corroboration only

The FireWire GUID/USB transport serial is a signing key, not the Apple product
serial.

A profile controls:

- backend selection and companion DB requirements
- checksum and HashInfo requirements
- CDB compression and database version
- number of `Music/Fxx` directories
- artwork formats and strides
- supported media capabilities
- practical database and file-size limits

Unknown or conflicting write-affecting evidence produces an actionable error,
never a “best guess” write.

## 6. Storage backends

### 6.1 Binary iTunesDB/iTunesCDB

Implement a checked, bounds-limited parser for at least:

- `mhbd`, `mhsd`
- `mhlt`, `mhit`, `mhod`
- `mhlp`, `mhyp`, `mhip`
- `mhla`, `mhia`
- `mhli`, `mhii`
- smart-playlist payloads and opaque datasets

The writer must:

- preserve stable persistent IDs and unknown values
- keep standard, podcast, and smart-playlist datasets distinct
- maintain playlist membership when a track is deleted
- emit the profile-correct database version and dataset set/order
- support standard DB and CDB zlib wrapping
- sign final on-disk bytes in the correct order
- validate its own output by reparsing it

A no-op rewrite does not need byte-identical compression, but it must be
semantically equivalent and preserve opaque data. Golden tests will identify
fields where byte identity is required by firmware.

### 6.2 SQLite `.itlp`

For Nano 5G–7G, prefer the SQLite library as the read source when the device
profile and on-disk evidence say it is authoritative. Do not derive the live
library from a potentially stale CDB.

Edit staged copies of the existing databases rather than replacing every file
with one hard-coded Nano 6G schema. This preserves unknown tables, columns,
indexes, and firmware-specific values. The backend updates related rows in:

- `Library.itdb`
- `Locations.itdb`
- `Dynamic.itdb`
- `Extras.itdb`
- `Genius.itdb` when required
- `Locations.itdb.cbk`

It must retain the same database persistent ID across the binary companion and
SQLite files. It must produce profile-correct CBK block SHA-1s and HASH72 or
HASHAB signatures. Bootstrap schemas can be added later as explicit,
profile-versioned assets rather than guessed from one generation.

For Nano 5G, and wherever existing device evidence requires it, write both the
SQLite set and a valid companion `iTunesCDB`. For Nano 6G/7G, SQLite is the
source of truth; CDB maintenance is a compatibility operation, not a substitute
for SQLite writes.

## 7. Checksums

Implement all algorithms behind one internal trait and test them separately
from database serialization.

- **HASH58:** safe Rust implementation with independent vectors and differential
  checks against known-good output.
- **HASH72:** AES/SHA-1 implementation; read `HashInfo`, and support extracting
  its reusable IV/random material from a known-good existing signature. Refuse
  to write if required material is unavailable.
- **HASHAB:** safe Rust port of the public-domain `dstaley/hashab` algorithm.
  Include its 100 vectors and intermediate-phase tests. Avoid a runtime WASM
  engine or opaque native blob.
- **CBK:** golden-test complete files, including exact block treatment and the
  fixed 23-byte random input used by compatible writers.

Checksum fields must be named by semantic role, not only offset. Tests must
explicitly distinguish MHBD scheme `3` from the separate indicator value `4`
for HASHAB.

## 8. Artwork

Port the model-aware profile table from the MIT sources with provenance and
hardware confirmation. The implementation needs:

- ArtworkDB parse/write with preservation of unknown records
- ithmb frame allocation, deduplication, replacement, and compaction
- exact row strides/padding and RGB565/RGB555/YUV variants used by target iPods
- updates to binary `mhit` artwork links and SQLite `artwork_status`,
  `artwork_cache_id`, and album representative links
- sparse-artwork behavior where supported
- encoded JPEG/PNG input plus a lower-level RGBA input API

Evaluate `ithmb-core` for pixel conversion/profile reuse, but do not make the
first release depend on its claimed container writer unless that writer is
present, public, and passes libopod’s fixtures. A small internal encoder may be
safer for the limited cover formats copyPod needs.

## 9. Commit and recovery model

Multi-file SQLite/artwork commits cannot be made truly atomic on FAT/HFS+, so
libopod must make interruption recoverable:

1. Verify mount root, profile, free space, filesystem behavior, and required
   signing material.
2. Acquire a per-volume advisory lock and record a generation fingerprint of
   every live DB/artwork file.
3. Build all outputs in a host or same-volume staging directory.
4. Reparse outputs, verify counts/IDs/paths, run SQLite integrity checks, and
   verify checksums before touching live data.
5. Create a manifest and durable backups of every file that may be replaced.
6. Revalidate volume identity and generation before each install phase.
7. Install artwork first (an interruption can leave harmless unreferenced art),
   database files next, and signature/marker files in a documented order.
8. Read back the committed authoritative backend and verify that every track
   location exists.
9. Delete media for removed tracks only after the database no longer references
   it. A deletion failure leaves an orphan file, not a broken library.
10. Flush where the platform permits it and retain enough manifest state for
    the next open to resume or roll back an interrupted commit.

New audio files are copied to unique temporary names and promoted before their
database rows become live. If a DB commit fails, libopod removes those new
copies. Existing media is not destroyed during a database preflight.

## 10. Validation strategy

### Automated tests

- Unit tests for every field/chunk and overflow/truncation case
- Golden HASH58/HASH72/HASHAB and CBK vectors
- CDB compress-sign-decompress tests proving operation order
- Parser/writer semantic round trips for every backend/profile
- Differential snapshots against pinned iOpenPod behavior
- Artwork pixel/stride and ArtworkDB link golden tests
- SQLite schema-preservation and `PRAGMA integrity_check` tests
- Virtual mounted-iPod end-to-end add/artwork/remove/reopen tests
- Property tests for chunk lengths, IDs, and paths
- `cargo-fuzz` targets for binary DB, CDB, ArtworkDB, SysInfo/plist, and artwork
- Failure injection at every commit/install step followed by recovery tests
- Linux CI for the supported 1.0 surface, including system and bundled SQLite
  builds; portability checks for macOS/Windows can run without making those
  hosts part of the 1.0 support promise

Production parser code should be safe Rust with explicit allocation and nesting
limits. Malformed device files must return errors rather than panic or allocate
from untrusted lengths.

### Hardware qualification

At minimum, qualify one device for every materially different profile:

- un-hashed legacy DB: Mini or Nano 1G/2G
- legacy artwork: color/photo/video iPod or Nano 1G/2G
- HASH58: Classic and Nano 3G/4G
- HASH72 + CDB/SQLite: Nano 5G
- HASHAB + SQLite: Nano 6G
- HASHAB + Nano 7G artwork profile: Nano 7G

For each: back up, no-op rewrite, add without art, add with art, update art,
remove, safe eject/reboot, browse/play on-device, reconnect/reparse, and—where
available—check that iTunes/Finder does not demand a restore.

No target generation will be advertised from model-table recognition alone.

## 11. Delivery milestones

### M0 — contracts, provenance, and corpus

- Resolve the questions below.
- Establish MIT-compatible provenance rules.
- Obtain consented/sanitized fixtures and record model/firmware/filesystem data.
- Freeze the copyPod-required API as an integration test.

### M1 — high-risk format/crypto spikes

- HASH58, HASH72, and pure-Rust HASHAB vectors
- CBK golden fixture
- CDB compression/signing round trip
- Read-only inspection of one legacy and one modern fixture

This milestone prevents building the public API on an invalid modern-Nano
assumption.

### M2 — crate foundation and read path

- Public domain model and typed errors
- Device evidence/profile resolver
- Safe filesystem paths
- Binary/CDB and SQLite read adapters
- `opod-inspect` example for fixture/hardware diagnostics

### M3 — legacy edits

- Track add/copy/remove
- Playlist-reference preservation
- Standard/CDB writer with NONE/HASH58/HASH72
- Read-back verification

### M4 — artwork

- ArtworkDB/ithmb writer
- Generation-specific profiles
- Existing artwork preservation and track/album links

### M5 — modern Nano edits

- Schema-preserving SQLite edits
- all companion databases
- CBK/HASHAB
- Nano 5G/6G/7G backend rules

### M6 — transactional safety and hardware gate

- Locking, generation checks, durable staging, manifests, rollback/recovery
- fault-injection suite and fuzzing
- complete hardware matrix with documented results

### M7 — copyPod migration and release

- Replace C shim with `libopod` dependency
- Preserve copyPod’s dry-run/full-mirror behavior
- Back up all relevant modern DB/artwork files, not only `iTunesDB`
- Remove libgpod/pkg-config/GLib build requirements
- Publish API docs, examples, support matrix, and crate after hardware gates pass

## 12. Definition of done for 1.0

- copyPod uses no libgpod or C shim.
- Every advertised target passes add/artwork/remove/reboot testing on hardware.
- Nano 5G/6G/7G writes include their authoritative SQLite files and valid
  signatures.
- An interrupted commit is detected and recoverable.
- Unknown profile evidence can never silently select a destructive writer.
- All committed output reparses and all media references stay inside the mount.
- `cargo fmt`, Clippy with warnings denied, tests, docs, dependency/license
  checks, fuzz smoke tests, and supported-platform CI pass.
- Public API and on-disk compatibility policy are documented.

## 13. Confirmed decisions

- Name: **libopod** (crate/package name `libopod`).
- License: MIT. Do not import LGPL implementation code into the crate.
- 1.0 scope: copyPod's needs—music track enumeration/add/remove, cover artwork,
  model/signing support, safe commits, and preservation of existing playlists.
- API: Rust only for 1.0.
- Host support: Linux first.
- Device range includes full-size iPod 1G–3G despite those models predating the
  click wheel.
- A FAT32 Nano 7G with an existing music library is available for repeated,
  fully backed-up add/artwork/remove/reboot validation and is writable on Linux.
- `rusqlite` with bundled SQLite is acceptable. Make bundled SQLite the default
  for reproducible end-user builds and retain a system-SQLite feature for Linux
  distributions that require centralized security updates or prohibit vendored
  native libraries.

## 14. Remaining input and recommended bootstrap decision

Initializing a blank device is substantially more work than editing an existing
one, especially for Nano 5G–7G. Existing-device support can preserve and mutate
Apple-created schemas and opaque records. Blank-device support needs separately
validated, firmware-specific SQLite schemas, seed rows, companion databases,
preferences, artwork structures, and initialization behavior. It also expands
the destructive hardware test matrix.

**Recommendation:** require an Apple/iTunes-initialized device for 1.0 and add
blank-device initialization as a later milestone. This does not block copyPod.

The intended reduced Nano 7G development fixture is a known subset of real
database files used as parser/writer test input, stored outside the public Git
repository (for example under a gitignored `tests/fixtures-private/` directory).
The reduced fixture should contain no audio files, but its SQLite library can
still expose track metadata and SysInfo can expose device identifiers. The
available full backup described below does contain audio; keep it immutable and
private, and copy only the required database subset for tests. Derive a
sanitized synthetic fixture for public CI once the writer can replace metadata
and regenerate signatures safely.

Relevant private inputs are `iPod_Control/iTunes/iTunes Library.itlp/`, the
binary `iTunesDB`/`iTunesCDB` files if present,
`iPod_Control/Artwork/ArtworkDB`, and `iPod_Control/Device/SysInfo*`. Large
`.ithmb` files are only needed for focused artwork tests. The collection tool
must preview every file, exclude audio, record hashes/profile information, and
never modify the device.

## 15. Available Nano 7G private fixture

A complete file-level backup is available at repository-relative
`backup_7g/`. It is about 4.7 GiB and contains personal audio, cover images,
track metadata, product serials, and the FireWire GUID. `/backup_7g/` is listed
in `.gitignore`: **never add, modify, sanitize in place, or commit this
directory**. Tests should copy only required files into a temporary directory.
Normal parser tests do not need to open the MP3 payloads.

Observed non-personal structure:

- Nano 7G, 16 GB, normal USB PID `0x1267`, FAT32, writable on Linux.
- SysInfo and SysInfoExtended contain both a product serial and FireWire GUID.
  Never log their values. SysInfoExtended advertises `SQLiteDB=true`, sparse
  artwork, a 4 GiB max file size, and 65,534 max tracks.
- Firmware/build strings differ between the two SysInfo sources. Device
  evidence must retain provenance and must not silently combine conflicting
  version fields.
- There are 726 MP3 location files allocated across `Music/F00`–`F19`.
  `F20`–`F49` also exist but are empty, proving that directory existence alone
  must not select the allocation count.

Current database fixture:

| File | Size | Structural observation |
| --- | ---: | --- |
| `iTunes/iTunesCDB` | 140,169 | `mhbd` header 244 bytes; CDB version 110; 5 children; scheme at `0x30` = 3; indicator at `0x70` = 4; compression flag = 1; HASHAB starts `03 00` |
| decompressed CDB payload | 1,093,398 | Exact top-level dataset order/types: 4, 1, 3, 2, 5; no trailing bytes |
| `Library.itdb` | 647,168 | SQLite, 4,096-byte pages, 158 pages |
| `Locations.itdb` | 69,632 | SQLite, 4,096-byte pages, 17 pages / 68 CBK blocks |
| `Dynamic.itdb` | 45,056 | SQLite, 4,096-byte pages, 11 pages |
| `Extras.itdb` | 12,288 | SQLite, 4,096-byte pages, 3 pages |
| `Genius.itdb` | 20,480 | SQLite, 4,096-byte pages, 5 pages |
| `Locations.itdb.cbk` | 1,437 | 57-byte HASHAB + 20-byte master SHA-1 + 68 block SHA-1s |

The CBK's stored master and all 68 stored block hashes have been independently
recomputed and match. Its HASHAB signature also verifies with the private GUID
using libopod's safe-Rust implementation. The HASHAB prefix is `03 00`. This
fixture directly confirms the plan's distinction between MHBD scheme 3 and
indicator 4.

The current CDB exposes a historical iOpenPod signing-order bug: its HASHAB
verifies only when the digest is computed with MHBD scheme 4 and the field is
then changed to the stored value 3. It does not match the exact final on-disk
bytes. libopod reports this as `LegacyScheme4Then3`, accepts it for read-only
inspection, and must never reproduce it. New signatures set scheme 3 before
computing the digest. Hardware must confirm the corrected CDB path; SQLite and
the independently valid CBK remain authoritative on this Nano 7G.

Artwork fixture:

- `ArtworkDB`: 570,024 bytes, `mhfd` header 132 bytes, 3 datasets.
- Dataset 1 has 704 `mhii` records; dataset 2 has no album-list records;
  dataset 3 declares 4 files; next image ID is 804.
- `F1010_1.ithmb`, `F1013_1.ithmb`, `F1015_1.ithmb`, and
  `F1016_1.ithmb` each contain exactly 140 frame slots with no remainder,
  using slot sizes 115,200, 5,000, 6,728, and 6,612 bytes respectively.
- The 704 artwork records sharing 140 physical frame slots are a real sparse /
  deduplicated-artwork case. libopod must preserve shared offsets and must not
  assume one physical frame per track record.

The `.backup` database files in the same directories are older/smaller states;
use the unsuffixed files as the current working fixture unless a test explicitly
covers migration or recovery.

## 16. Implementation status and session handoff

The MIT Rust 2021 crate now exists. The initial M0/M1/M2 foundation includes:

- `Cargo.toml` with bundled SQLite by default and an opt-in system SQLite build,
  `LICENSE`, `README.md`, `THIRD_PARTY.md`, and `HARDWARE_TESTING.md`.
- A non-exhaustive typed error, validated `IpodPath`, canonical `MountRoot`,
  bounded reads, and read-only symlink-escape rejection.
- Redaction-safe SysInfo/SysInfoExtended evidence parsing and conservative Nano
  7G profile resolution. FireWire GUID and product serial values are never
  exposed by public accessors or `Debug`.
- Checked CDB zlib/header/dataset inspection, SQLite header plus
  `PRAGMA integrity_check`, complete CBK block/master digest verification,
  ArtworkDB dataset inspection, and profile-specific ithmb slot arithmetic.
- A redacted `opod-inspect` example and a private Nano 7G structural integration
  test that does not read MP3 payloads.
- A safe-Rust HASHAB port with explicit Unlicense/ISC provenance, all 100 public
  vectors, final-byte CDB signing, CBK generation, and private CDB/CBK signature
  classification. No native blob, C, unsafe code, or runtime WASM is used.
- A playlist read model and in-memory `EditSession`. Its explicitly incomplete
  `stage_sqlite_preview` copies all five databases to an empty host directory,
  removes queued tracks and direct references, preserves playlist/container
  metadata, repairs order and aggregate rows, preserves unknown schema objects,
  regenerates HASHAB CBK, runs integrity checks, and semantically reparses the
  result. It now also removes matching CDB tracks and playlist references,
  repairs type-52 sorted indices and type-53 jump tables, preserves dataset
  order and opaque chunks, recompresses at level 1, and creates an exact-final-
  bytes HASHAB signature. Device open now records SHA-256 generation state for
  SysInfo, all database companions, ArtworkDB, and each profile-specific ithmb;
  staging revalidates this before and after work and carries the source
  generation in its result. Each preview now includes verified host backups of
  every generation input plus a durable, self-reparsed JSON manifest containing
  source and output SHA-256 digests and target paths. A transaction engine now
  checks free space, uses exclusive transaction-directory creation as a lock,
  creates and verifies same-volume backups, durably journals intent before each
  replacement, installs through flushed sibling files, validates every output,
  reopens the library, and supports strict interrupted-state rollback. Virtual
  Nano tests pass for no-op installation, one-track installation, and injected
  interruptions at every durable boundary: mid-backup, before any install,
  mid-install with the journal already counting the next file, during output
  validation, and after the committed journal write (which must survive
  recovery untouched); a corrupt journal must refuse recovery and leave the
  transaction in place. A staged one-track addition is now also implemented:
  it allocates a free `Music/Fxx/XXXX.mp3` name in the least-populated media
  directory, stages a verified copy in the bundle, inserts the `item`,
  `avformat_info`, `location`, and master `item_to_container` rows, creates or
  updates album/artist/track_artist/composer/genre rows with shared-counter
  PIDs and rank-based order fields, rebuilds the CBK, and rewrites the CDB
  with a new MHIT (0x270 header + MHOD children), an appended MHLA album when
  needed, rebuilt type-52/53 library indices, a new master-playlist MHIP, and
  an exact-final-byte HASHAB signature. The transaction engine now treats new
  media files as absent-original outputs: it verifies their absence before
  installation, skips backups, and deletes them on rollback. Virtual tests
  install the addition onto a copy of the fixture, read back 727 tracks with
  the media file present, and recover an interrupted addition back to 726
  tracks with the media file removed. The byte-identical no-op, the single
  no-artwork removal, and the single no-artwork addition hardware gates are
  publicly enabled; media deletion, artwork mutation, and copyPod
  synchronization remain disabled. The `opod-stage-remove` example makes the
  host-only gate testable from a mounted, preferably read-only iPod without
  exposing track metadata or device IDs.

The current code passes:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo test --no-default-features --features system-sqlite
cargo doc --no-deps --all-features
```

The first two real Nano 7G hardware gates have passed. The no-op gate
validated the byte-identical transaction path (read back, reboot, playback).
The removal gate then validated the first semantic mutation on hardware: the
explicitly confirmed one-track no-artwork removal completed, read back, and
both the CLI and the device firmware reported 725 tracks after reboot. This
qualifies the schema-preserving SQLite removal, the CBK rebuild, and the
corrected exact-final-byte HASHAB CDB signature on real firmware. The orphaned
MP3 remains on disk as intended.

The third gate then validated the first media-installing mutation on
hardware: the explicitly confirmed one-track no-artwork addition completed,
read back, and the new track was immediately findable in the browse lists and
played after reboot. This qualifies staged media allocation and installation,
the SQLite `item`/`avformat_info`/`location`/container insertion,
album/artist/composer/genre resolution, the rebuilt CBK, the new CDB MHIT
with rebuilt type-52/53 library indices and master-playlist MHIP, and the
exact-final-byte HASHAB signature on real firmware.

The private tests verify all structural observations in section 15 and stage a
one-track removal into a temporary host directory. The staged set has 725
tracks, all five schemas remain identical, every database passes
`integrity_check`, playlist membership excludes only the removed track, and the
new CBK verifies. A synthetic test also covers direct references, derived rows,
artwork representatives, opaque schema data, and Genius cleanup. The example
was run successfully against `backup_7g/`. The backup remained ignored and was
only opened read-only; its essential file hashes remain unchanged.

Next implementation work:

1. Treat `backup_7g/` as immutable and private; confirm it remains ignored.
2. All three Nano 7G qualification gates (no-op, removal, addition) have now
   passed on hardware. Next: ArtworkDB/ithmb updates (needed before removing or
   adding artwork-bearing tracks and before cover-art sync), then connect
   copyPod mutation methods. Defer HASH58/HASH72 and legacy writers until this
   Nano 7G path is fully qualified.

## 17. copyPod migration spike

A fresh copyPod clone at snapshot
`8580517ba98c45c8e8615ba3fc87d714c6d1c541` was migrated in a separate working
tree to consume libopod as a Rust path dependency. The spike removed `build.rs`,
the C shim/header, build-time `cc`/`pkg-config`, and every libgpod/GLib call.

To make the integration useful, libopod now has a normalized read-only
`Library`, `Track`, `Playlist`, and `PersistentId` API backed by the Nano 7G authoritative
`Library.itdb` plus `Locations.itdb`. It validates every database location as an
`IpodPath`, joins locations by persistent ID, and exposes redaction-safe host
path resolution through `Device::track_path`.

Verified against the private fixture:

- 726 song rows join one-to-one with 726 primary locations.
- 704 tracks report artwork, matching the ArtworkDB record count.
- The migrated copyPod builds and its tests and strict Clippy pass without
  libgpod installed.
- A real copyPod `--dry-run` against `backup_7g/` reads all 726 tracks through
  libopod and produces a complete mirror plan.
- A non-dry invocation refuses at libopod's write preflight before deleting or
  copying media. Essential database hashes remain unchanged.
- FireWire GUID presence is checked without exposing or printing its value.

The migration is therefore complete for read-only planning, but deliberately
not for synchronization. The remaining blocker is libopod's staged
add/remove/artwork/commit implementation; do not reintroduce the old
single-`iTunesDB` backup/write behavior because it is unsafe and incomplete for
modern Nanos.
