/// An observation from staging, installing or recovering an edit.
///
/// Events arrive synchronously, immediately before the named work starts.
/// Item counters are one-based and local to each operation, not an overall
/// completion percentage. A successful method return signals completion.
/// Names can contain track metadata or device-relative paths; unlike the
/// inspector, this opt-in API is not redacted. Callbacks must not panic or
/// modify the source device, staging bundle or recovery journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProgressEvent<'a> {
    /// Work without a meaningful per-item counter, such as integrity checks.
    Phase(&'static str),
    /// The current track or file in a bounded operation.
    Item {
        operation: &'static str,
        current: usize,
        total: usize,
        name: &'a str,
    },
}

pub(crate) type Progress<'a> = dyn FnMut(ProgressEvent<'_>) + 'a;
