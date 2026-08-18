# Third-party provenance

libopod is MIT licensed. Implementation code must not be copied from LGPL
libgpod. Format behavior may be independently implemented from observations,
documentation, and permissively licensed sources.

## Included HASHAB implementation and vectors

`src/crypto/hashab/` is a safe-Rust port of dstaley/hashab snapshot
`f80d46432204c6238cad7d8ca3b3dd52ea66836b`. Algorithm structure and constants
were transcribed from `src/calcHashAB.c`, `cipher.c`, `cipher_tables.c`,
`expand.c`, `hash.c`, and `reduce.c` under The Unlicense; see
`LICENSES/hashab-Unlicense.txt`. The embedded EDON-R compression function was
ported from `src/edonr.c`, copyright Aleksey Kravchenko and ISC licensed; see
`LICENSES/edonr-ISC.txt`. No C, native blob, WASM module, or unsafe code is used
at runtime.

`tests/fixtures/hashab-vectors.json` is an exact copy of dstaley/hashab's
`test-data.json` at that revision. `tests/hashab_vectors.rs` checks all 100
published vectors with the upstream fixed random field.

## Research references

Except for the HASHAB material documented above, no source code from these
projects is included in libopod.

| Project | Snapshot | License / use |
| --- | --- | --- |
| iOpenPod | `3f6e20f66c203abe900812b64d90a5e5270d2bb3` | MIT; format and device-behavior reference |
| copyPod | `8580517ba98c45c8e8615ba3fc87d714c6d1c541` | MIT; consumer API requirements |
| dstaley/hashab | `f80d46432204c6238cad7d8ca3b3dd52ea66836b` | The Unlicense plus ISC EDON-R file; included as documented above |
| ipodsync | `28174c7f2f8ded61d19782fd2b2ee4922414db71` | Permissive research reference; modern Nano behavior |
| Ithmb-Codec | `6809cf55f728f6f0a662584d9c46006b1f84504c` | MIT; artwork format research |
| libgpod | `4a8a33ef4bc58eee1baca6793618365f75a5c3fa` | LGPL; behavioral research only, no implementation code copied |

Future vectors, constants, tables, or implementation ports must record the
exact source file, revision, license, and local destination here.

## Rust dependencies

Dependency licenses are tracked in `Cargo.lock` and must be checked before a
release with a license-auditing tool such as `cargo-deny`.
