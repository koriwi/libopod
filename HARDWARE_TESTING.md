# Hardware testing

Never run a write test without a complete, verified backup and an explicit
operator confirmation that identifies the mounted volume.

The initial Nano 7G backup at `backup_7g/` is private and immutable. It is a
file-level development input, not a write target.

## Current gate: host-only removal preview

The current edit stage can read a mounted iPod and write modified SQLite and
signed CDB copies to separate host storage. It does not write to the iPod.
Prefer a read-only mount, create an empty directory outside the mount, and run:

```console
cargo run --release --example opod-stage-remove -- /path/to/ipod list
cargo run --release --example opod-stage-remove -- \
  /path/to/ipod stage TRACK_INDEX /path/to/empty-host-directory
```

The list intentionally shows only database order, artwork presence, and byte
size. The stage command reparses all output and verifies its regenerated CBK.
Do not copy preview files to an iPod manually.

## Nano 7G gate 1: byte-identical no-op transaction

This is the only enabled hardware write. It replaces the seven database files
with byte-identical copies. It does not change tracks, artwork, playlists, or
media. Before running it:

1. Make and verify a complete independent backup.
2. Confirm the device resolves specifically as Nano 7G with `opod-inspect`.
3. Charge the iPod and prevent host suspend or cable disconnection.
4. Mount it writable and create a new empty directory on host storage.
5. Keep that host directory until reboot and playback checks succeed.

Run the exact confirmation command:

```console
cargo run --release --example opod-hardware-test -- \
  noop /path/to/ipod /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; RUN NANO 7G NO-OP WRITE TEST'
```

libopod revalidates every source generation, checks free space, writes verified
on-device backups and a durable journal, installs through flushed sibling
files, reads the resulting library back, and removes the journal only after a
successful commit. If the process, host, or device is interrupted, do not run
another sync. Reconnect and run:

```console
cargo run --release --example opod-hardware-test -- \
  recover /path/to/ipod
```

Recovery refuses to write unless volume identity inputs, the journal, current
mixed state, and all backups verify. After a successful no-op test, safely
eject, reboot, browse and play multiple tracks, reconnect, and run
`opod-inspect` again. Record whether the firmware or Apple software requests a
restore.

### Gate 1 result

The real Nano 7G no-op transaction completed, read back successfully, survived
safe eject/reboot, and playback continued to work. This validates the
byte-identical transaction path on this device. It does not validate semantic
CDB changes.

## Nano 7G gate 2: one no-artwork database removal

This gate removes exactly one no-artwork track from SQLite and CDB while
leaving its MP3 on disk as an orphaned safety copy. It does not modify
ArtworkDB or ithmb files. Keep both the complete external backup and the new
host transaction bundle.

First list redacted candidates and choose an index whose artwork column is
`no`:

```console
cargo run --release --example opod-stage-remove -- /path/to/ipod list
```

Create another empty host directory and run:

```console
cargo run --release --example opod-hardware-test -- \
  remove /path/to/ipod TRACK_INDEX /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; REMOVE ONE NO-ARTWORK TRACK AND KEEP ITS MEDIA FILE'
```

If interrupted, run the same `recover` command documented above. On success,
safely eject and reboot. Confirm that the library has one fewer track, browse
albums/playlists, play multiple remaining tracks, reconnect, and run
`opod-inspect`. Expected inspection changes are 725 tracks, valid SQLite and
CBK, and `Some(Valid)` for the newly generated CDB HASHAB signature. ArtworkDB
should still have 704 records.

Stop if the firmware or Apple software requests a restore. Do not run copyPod
or delete the orphan MP3 yet.

### Gate 2 result

The real Nano 7G removal transaction completed, read back successfully, and
both the CLI and the device firmware reported 725 tracks after reboot: the
semantic SQLite removal, CBK rebuild, exact-final-byte HASHAB CDB signature,
and recoverable transaction path are now qualified on hardware. The orphaned
MP3 remains on disk as intended.

## Nano 7G gate 3: one no-artwork MP3 addition

This gate adds exactly one MP3 track without artwork: it allocates a free
`Music/Fxx/XXXX.mp3` name, stages a verified copy in the bundle, inserts the
`item`/`avformat_info`/`location`/master `item_to_container` rows, creates or
updates album/artist/composer/genre rows, rebuilds the CBK, rewrites the CDB
with a new MHIT and rebuilt library indices, and installs everything
recoverably. Bitrate is recorded as 192 kbps and the sample rate as 44100 Hz;
supply the duration in milliseconds. Keep both the complete external backup
and the new host transaction bundle.

Create another empty host directory and run (SOURCE_MP3 must be an MP3 file on
host storage; use `LENGTH_MS` from the file's actual duration):

```console
cargo run --release --example opod-hardware-test -- \
  add /path/to/ipod /path/to/source.mp3 'TITLE' 'ARTIST' 'ALBUM' LENGTH_MS \
  /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; ADD ONE NO-ARTWORK MP3 TRACK'
```

If interrupted, run the same `recover` command documented above. On success,
safely eject and reboot. Confirm the library has one more track, play the new
track and browse its album/artist, reconnect, and run `opod-inspect`. Expected
inspection changes are 727 tracks, valid SQLite and CBK, and `Some(Valid)`
for the newly generated CDB HASHAB signature. ArtworkDB should still have 704
records.

Stop if the firmware or Apple software requests a restore. Do not run copyPod
or repeat the gate until the reboot checks pass. CopyPod synchronization
remains disabled until this addition gate and subsequent artwork gates pass.
The full matrix is specified in `plan.md` sections 10 and 12.

### Gate 3 result

The real Nano 7G addition transaction completed, read back successfully, and
survived safe eject/reboot: the operator found the newly added track
immediately in the browse lists and played it without issue. This qualifies
the staged media allocation and installation, the SQLite `item`/
`avformat_info`/`location`/container insertion, album/artist/composer/genre
resolution, the rebuilt CBK, the new CDB MHIT with rebuilt type-52/53 library
indices and master-playlist MHIP, and the exact-final-byte HASHAB signature on
real firmware. All three Nano 7G qualification gates (no-op, removal,
addition) have now passed on hardware.
