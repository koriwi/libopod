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
| All iPod Classic models | 🧪 Testers wanted |

Testing on unlisted hardware is greatly appreciated. See [HARDWARE_TESTING.md](HARDWARE_TESTING.md) before writing to a device.

## Reference

Database layouts and device behavior are based on the [iOpenPod](https://github.com/TheRealSavi/iOpenPod) reference implementation.

libopod preserves existing device data where possible instead of rebuilding everything from scratch.

## Inspect an iPod

```console
cargo run --example opod-inspect -- /path/to/ipod/mount
```

The inspector hides serial numbers, `FireWire` GUIDs, and track metadata.

## More information

- [Hardware testing](HARDWARE_TESTING.md)
- [Architecture and safety plan](plan.md)
- License: MIT
