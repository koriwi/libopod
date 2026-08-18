# libopod

`libopod` is an MIT-licensed Rust library for reading and managing iPods that
run Apple firmware and are mounted as storage.

The project is under active development. The current code provides read-only
device evidence, safe mount-relative paths, structural inspection of
`iTunesCDB`, `SQLite` `.itlp`, CBK, and `ArtworkDB` files, plus normalized Nano
7G track enumeration from its authoritative `SQLite` library. It also includes
a safe-Rust HASHAB implementation validated by all 100 public vectors and the
private Nano's CBK signature. Playlist reading and schema-preserving
`SQLite`-only removal previews are available in separate empty host directories;
these previews deliberately cannot be installed manually. The enabled device
mutations are explicitly confirmed Nano 7G qualification gates: a
byte-identical transaction, one no-artwork removal that retains its media
file, and one no-artwork MP3 addition. All three have passed their virtual
read-back and recovery tests **and have qualified on real Nano 7G hardware**
(byte-identical transaction, one-track removal with media retained, and
one-track addition that was immediately browsable and playable after reboot).
General semantic writes and synchronization remain disabled.

See [`plan.md`](plan.md) for scope, architecture, safety rules, and the hardware
qualification matrix.

## Private-device inspection

```console
cargo run --example opod-inspect -- /path/to/ipod/mount
```

The inspector reports structural information and presence flags only. It does
not print product serials, `FireWire` GUIDs, or track metadata.

A mounted iPod can also drive a host-only removal preview or the narrowly gated
Nano 7G no-op, removal, and addition hardware tests. See
[`HARDWARE_TESTING.md`](HARDWARE_TESTING.md). Never install preview output
manually.

## License

MIT
