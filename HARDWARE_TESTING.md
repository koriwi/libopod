# Hardware testing

Never run a write test without a complete, verified backup and an explicit
operator confirmation that identifies the mounted volume.

The initial Nano 7G backup at `backup_7g/` is private and immutable. It is a
file-level development input, not a write target.

## Current gate: media-deletion removal (gate 7) and artwork-delete removal (gate 8)

Gates 1–6 (no-op, both removals, both additions, both artwork paths) have
passed on hardware. The device is at 727 tracks / 705 ArtworkDB records.

The media-deletion policy: when the operator asks for a delete (distinct
confirmation phrase), the media file is deleted **as part of the removal
transaction** — no post-reboot confirmation phase, mirroring iTunes. The file
is backed up inside the transaction and restored on rollback, so a failed or
interrupted commit never loses it.

Artwork-bearing removals now **rewrite and reindex the `.ithmb` files**
instead of leaving unreferenced slots: the remaining images are packed into
fresh contiguous slots (shared slots deduplicated) and every `mhii` record is
repointed. This applies to both the keep-orphan and delete variants.

- Gate 7: remove one no-artwork track and delete its media file.
- Gate 8: remove one artwork-bearing track, delete its media, and reindex
  the four `.ithmb` files.

The edit stage can read a mounted iPod and write modified SQLite and signed
CDB copies to separate host storage. It does not write to the iPod unless an
explicit hardware-gate command with the confirmation phrase is used.
Prefer a read-only mount; create an empty directory outside the mount.

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
inspection changes are one more track than before the gate (725 → 726 on a
device that already ran gate 2; the pristine-fixture virtual tests expect
727 = 726 + 1), valid SQLite and CBK, and `Some(Valid)`
for the newly generated CDB HASHAB signature. ArtworkDB should still have 704
records.

Stop if the firmware or Apple software requests a restore. Do not run copyPod
or repeat the gate until the reboot checks pass. CopyPod synchronization
remains disabled until this addition gate and subsequent artwork gates pass.
The full matrix is specified in `plan.md` sections 10 and 12.

### Gate 3 result

The real Nano 7G addition transaction completed, read back successfully, and
survived safe eject/reboot: the operator found the newly added track
immediately in the browse lists, played it without issue, and the device and
CLI both reported 726 tracks (725 after gate 2, plus the one added track). This
qualifies the staged media allocation and installation, the SQLite `item`/
`avformat_info`/`location`/container insertion, album/artist/composer/genre
resolution, the rebuilt CBK, the new CDB MHIT with rebuilt type-52/53 library
indices and master-playlist MHIP, and the exact-final-byte HASHAB signature on
real firmware. All three Nano 7G qualification gates (no-op, removal,
addition) have now passed on hardware.

## Nano 7G gate 4: one artwork-bearing database removal

This gate removes exactly one track that has artwork. The matching `mhii`
record is dropped from `ArtworkDB`, while `.ithmb` slot payloads stay in place
as unreferenced data (mirroring the orphaned-media policy). SQLite, CBK, and
CDB behave exactly as in gate 2. Keep both the complete external backup and
the new host transaction bundle.

Choose an index whose artwork column is `yes`:

```console
cargo run --release --example opod-stage-remove -- /path/to/ipod list
```

Create another empty host directory and run with the artwork confirmation:

```console
cargo run --release --example opod-hardware-test -- \
  remove /path/to/ipod TRACK_INDEX /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; REMOVE ONE ARTWORK-BEARING TRACK AND KEEP ITS MEDIA FILE'
```

The same `recover` command applies if interrupted. On success, safely eject and
reboot. Confirm the library has one fewer track, browse the remaining tracks of
the removed track's album (their shared art should still display), reconnect,
and run `opod-inspect`: expect one fewer track, valid SQLite and CBK,
`Some(Valid)` for the CDB HASHAB signature, and 703 `ArtworkDB` records.

Stop if the firmware or Apple software requests a restore. Media and artwork
slot deletion remain disabled until later gates pass. The full matrix is
specified in `plan.md` sections 10 and 12.

### Gate 4 result

Passed on hardware 2025-08-18: one artwork-bearing track removed. `ArtworkDB`
records 704 → 703, tracks 726 → 725, media file retained as an orphan, `.ithmb`
slot payloads retained as unreferenced data. CLI and `opod-inspect` agreed;
SQLite/CBK valid, CDB HASHAB signature `Some(Valid)`.

Note: this run also exposed two device-state tolerances that were fixed
before gate 5 could stage (both host-side; the device was never at risk):

- The master playlist's firmware-rewritten mhips embed a type-100 position
  mhod child; the firmware truncates the trailing mhip to 112 bytes while the
  embedded mhod still claims 44. The mhip child walk now tolerates the
  truncation and reads the position at +24 (standard) or +16 (truncated).
- The gate-3-added album on the device still carries the pre-gap string mhod
  layout (data at +32, claimed total 40+len vs 32+len actual). The string
  mhod parser now detects both layouts and walks by the real end.

## Nano 7G gate 5: addition with reused album artwork

Adds one track whose album already exists on the device with artwork; the new
track inherits the album's existing `.ithmb` slots (no image decoding).

```console
cargo run --release --example opod-hardware-test -- \
  add-reuse /path/to/ipod /path/to/source.mp3 'TITLE' 'ARTIST' 'ALBUM' LENGTH_MS \
  /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; ADD ONE TRACK WITH REUSED ALBUM ART'
```

ALBUM must already exist on the device with artwork (e.g. any existing album).
Expect one more track (726 total), the same artwork as its album-mates, 704
ArtworkDB records (703 + the new track's reused record), valid signatures,
and no restore warning after reboot.

### Gate 5 result

Passed on hardware 2025-08-18: one track added with reused album artwork.
Tracks 725 → 726, ArtworkDB records 703 → 704, the new track appears under
the correct album and displays the shared artwork. Signatures valid, no
restore warning after reboot.

## Nano 7G gate 6: addition with new encoded cover art

Adds one track with a fresh cover image (JPEG/PNG) encoded into all four Nano
7G cover formats and written into new `.ithmb` slots.

```console
cargo run --release --example opod-hardware-test -- \
  add-art /path/to/ipod /path/to/source.mp3 /path/to/cover.png \
  'TITLE' 'ARTIST' 'ALBUM' LENGTH_MS /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; ADD ONE TRACK WITH NEW COVER ART'
```

Expect one more track (727 total, 726 + 1), 705 ArtworkDB records (704 + 1),
the four `.ithmb` files each one slot longer, the new artwork visible in Now
Playing/browse, valid signatures, and no restore warning after reboot.

### Gate 6 result

Passed on hardware 2025-08-18: one track added with fresh encoded cover art.
Tracks 726 → 727, ArtworkDB records 704 → 705, all four `.ithmb` files grew
by one slot, the new album displays its artwork and the track is playable.
Signatures valid, no restore warning after reboot.

All six Nano 7G qualification gates (no-op, no-artwork removal, no-artwork
addition, artwork-bearing removal, reused-art addition, new-art addition)
have now passed on hardware.

Observed gap (artist browse art): the new artist row showed no artwork at the
artist browse level, even though its album displayed art. Apple's data sets
`artist.artwork_album_pid` (a representative album) and `album.artwork_item_pid`
(a representative item) for browse-level art; libopod wrote only the item's
`artwork_cache_id`, which the firmware derives for album art but not for the
artist row. Fixed in the SQLite add path (`link_artwork_rows`): when a track
with artwork is inserted, the album and artist rows get their artwork
references set unless already present. Virtual tests assert the linkage; a
future hardware add should show artist-level art.

## Nano 7G gate 7: no-artwork removal with immediate media deletion

Removes exactly one no-artwork track and deletes its media file inside the
same transaction (no post-reboot confirmation). Pick a no-artwork index from
`opod-stage-remove list`, then:

```console
cargo run --release --example opod-hardware-test -- \
  remove /path/to/ipod TRACK_INDEX /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; REMOVE ONE NO-ARTWORK TRACK AND DELETE ITS MEDIA FILE'
```

Expect: one fewer track (727 → 726), the media file gone from
`iPod_Control/Music/`, valid signatures, no restore warning after reboot. An
interrupted install restores the file (transaction rollback).

## Nano 7G gate 8: artwork-bearing removal with media deletion and reindex

Removes exactly one artwork-bearing track, deletes its media, drops its `mhii`
record, and rebuilds all four `.ithmb` files with the remaining images packed
into contiguous slots (shared slots deduplicated):

```console
cargo run --release --example opod-hardware-test -- \
  remove /path/to/ipod TRACK_INDEX /path/to/empty-host-directory \
  'I HAVE A VERIFIED BACKUP; REMOVE ONE ARTWORK-BEARING TRACK AND DELETE ITS MEDIA FILE'
```

Expect: one fewer track (727 → 725), ArtworkDB records 705 → 704, the four
`.ithmb` files whole slots and no larger than before, the media file gone,
remaining albums still display their artwork, valid signatures, no restore
warning after reboot.
