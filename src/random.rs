//! Process-wide xorshift64 random state.
//!
//! Media file names, track persistent IDs, and album `sql_id` values are all
//! generated from one per-thread xorshift64 state that is seeded exactly once
//! (clock and process ID entropy, with a non-zero guard against the
//! xorshift zero fixed point). Callers that need uniqueness still check or
//! retry; this module only guarantees the stream is not degenerate or
//! collision-prone within a clock tick.

use std::cell::Cell;

thread_local! {
    static STATE: Cell<u64> = Cell::new(seed());
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
    let pid = u64::from(std::process::id());
    let mut state = nanos ^ (pid << 32) ^ 0x9e37_79b9_7f4a_7c15;
    if state == 0 {
        state = 0x9e37_79b9_7f4a_7c15;
    }
    state
}

/// Returns the next value from the per-thread xorshift64 stream.
#[must_use]
pub(crate) fn next_u64() -> u64 {
    STATE.with(|cell| {
        let mut state = cell.get();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        cell.set(state);
        state
    })
}

#[cfg(test)]
mod tests {
    use super::next_u64;

    #[test]
    fn stream_is_nonzero_and_varied() {
        let first = next_u64();
        assert_ne!(first, 0);
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(first);
        for _ in 0..10_000 {
            let value = next_u64();
            assert_ne!(value, 0);
            assert!(seen.insert(value), "xorshift64 cycle too short");
        }
    }
}
