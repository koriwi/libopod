# Hardware testing

Never run a write test without a complete, verified backup and an explicit
operator confirmation that identifies the mounted volume.

The initial Nano 7G backup at `backup_7g/` is private and immutable. It is a
file-level development input, not a write target.

## Current gate: host-only removal preview

The current edit stage can read a mounted iPod and write modified SQLite and
signed CDB copies to separate host storage. It does not write to the iPod. Prefer a read-only
mount, create an empty directory outside the mount, and run:

```console
cargo run --release --example opod-stage-remove -- /path/to/ipod list
cargo run --release --example opod-stage-remove -- \
  /path/to/ipod stage TRACK_INDEX /path/to/empty-host-directory
```

The list intentionally shows only database order, artwork presence, and byte
size. The stage command reparses all output and verifies its regenerated CBK.
Do not copy the output to an iPod: although source generations are now checked,
it does not yet include ArtworkDB handling, a commit manifest, backups, or
recovery.

Actual write qualification procedures will be added before write support. The
required profile matrix and test sequence are specified in `plan.md` sections
10 and 12.
