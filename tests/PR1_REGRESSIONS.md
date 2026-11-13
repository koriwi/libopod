# PR #1 regression checks

These tests use synthetic databases only. They do not need private device dumps
or physical hardware. The local changes include the chosen empty-last album-row
policy. The other production fixes remain separate in PR #1.

Run the active checks:

```sh
cargo test --locked --lib edit::add::regression_tests
```

Comparison performed against:

- `main` at `40725d7` plus the local empty-last album-row fix
- The same source plus PR #1 head `81ebf88`

| Case | Main + local album fix | Same source + PR #1 |
| --- | --- | --- |
| NULL-name rank queries and insertions (6 tests) | Fail | Pass |
| Preserve an artist while an album needs it | Fail | Pass |
| PR's empty-last key/catalog/item order rule (5 tests) | Fail | Pass |
| Named-key order with no empty keys | Pass | Pass |
| Remove both artists and their album in one edit | Pass | Pass |
| Refresh retained artist's direct-track aggregates | Fail | Fail |
| Remove the retained artist after its last album disappears in a later edit | Pass | Fail |
| Empty album rows last, with distinct slots and PID tie order (2 tests) | Pass | Pass |

Totals: **5 pass / 13 fail / 0 ignored** without PR #1;
**16 pass / 2 fail / 0 ignored** with PR #1.

## Artist cases that need more work

The fixture has two tracks on one album. Track 1 directly names artist 20;
track 2 directly names artist 21. The album refers to artist 20.

1. Remove track 1. Keep artist 20 because the album still needs it.
2. Refresh artist 20's flags and counts according to the existing production
   UPDATE expressions, which use direct `item.artist_pid` references. Keep its
   artwork link to the surviving album. The PR preserves the row but skips the
   UPDATE, so this check still fails.
3. Remove track 2 in a separate edit. No track or album now needs artist 20.
   The PR leaves that record behind because the second edit only queues artist
   21 for cleanup. This check passes on main only because main prematurely
   deletes artist 20 in the first edit; the preservation test catches that bug.

These checks exercise database consistency rules. They do not claim a specific
visible firmware fault or prove Apple's intended artist-aggregate semantics.

## Chosen album-order policy

The maintainer chose empty album rows last as an explicit libopod rule. Both
album-row tests now run normally. They failed before the local comparator fix
and pass after it. The fix puts empty effective sort keys after named keys,
keeps one scaled rank per album row, breaks equal-key ties by PID, and updates
`name_order` and `sort_order` together.

The PR changes the shared key-order function, but album rows use this separate
comparator. Keep the local album fix when testing or merging the PR.

Run the album checks alone:

```sh
cargo test --locked --lib album_rows_
```

These tests define our chosen policy. They do not independently prove the
behavior of every iPod firmware or browse category.

The active tests intentionally leave the suite red until the corresponding
fixes meet the checks. Review or amend the criteria before committing them to
main. No test changes the mounted iPod or the original private fixtures.
