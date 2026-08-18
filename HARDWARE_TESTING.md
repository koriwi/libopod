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

Semantic hardware writes and copyPod synchronization remain disabled until this
no-op gate passes and the removal/artwork gates are completed. The full matrix
is specified in `plan.md` sections 10 and 12.
