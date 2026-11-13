# PR #1 regression checks

These tests use synthetic databases only. They do not need private device dumps
or physical hardware. PR #1 is merged in `c59d2f5`; the follow-up artist fixes
make all **21 checks pass**, with no ignored checks.

Run the checks:

```sh
cargo test --locked --lib edit::add::regression_tests
```

## Historical comparison before the artist follow-up

The original 18 checks compared `main` at `40725d7` plus the empty-last album-row
fix against the same source plus PR #1 head `81ebf88`:

| Case | Main + album fix | Same source + PR #1 |
| --- | --- | --- |
| NULL-name rank queries and insertions (6 tests) | Fail | Pass |
| Preserve an artist while an album needs it | Fail | Pass |
| PR's empty-last key/catalog/item order rule (5 tests) | Fail | Pass |
| Named-key order with no empty keys | Pass | Pass |
| Remove both artists and their album in one edit | Pass | Pass |
| Refresh retained artist's direct-track aggregates | Fail | Fail |
| Remove the retained artist after its last album disappears in a later edit | Pass | Fail |
| Empty album rows last, with distinct slots and PID tie order (2 tests) | Pass | Pass |

Those totals were **5 pass / 13 fail** without PR #1 and **16 pass / 2 fail**
with PR #1. The two failures motivated the artist follow-up below.

## Artist follow-up fixes

The fixture has two tracks on one album. Track 1 directly names artist 20;
track 2 directly names artist 21. The album refers to artist 20.

1. Remove track 1. Keep artist 20 because the album still needs it.
2. Refresh artist 20's flags and counts according to the existing production
   UPDATE expressions, which use direct `item.artist_pid` references. Keep its
   artwork link to the surviving album. The update now includes affected artists
   with no direct tracks.
3. Remove track 2 in a separate edit. No track or album now needs artist 20.
   The edit captures the album's artist before deleting the album, so cleanup
   now includes artist 20 as well as the removed track's artist 21.

Three additional tests check that:

- Cleanup includes an album owner even if no track directly names that artist.
- Another surviving album keeps its artist alive and supplies the new artwork
  link; removing that last album then permits artist cleanup.
- Cleanup does not refresh or delete unrelated NULL-name firmware placeholders.

Before the follow-up, the expanded suite had **17 passing and 4 failing**
checks. After the follow-up, all **21 pass**. These checks exercise database
consistency rules. They do not prove Apple's intended artist-aggregate semantics
or a specific visible firmware fault.

## Chosen album-order policy

The maintainer chose empty album rows last as an explicit libopod rule. Both
album-row tests run normally. They failed before the comparator fix and pass
after it. The fix puts empty effective sort keys after named keys, keeps one
scaled rank per album row, breaks equal-key ties by PID, and updates
`name_order` and `sort_order` together.

The shared key-order function and the separate album-row comparator both follow
this policy. Run the album checks alone:

```sh
cargo test --locked --lib album_rows_
```

These tests define our chosen policy. They do not independently prove the
behavior of every iPod firmware or browse category. No test changes a mounted
iPod or the original private fixtures.
