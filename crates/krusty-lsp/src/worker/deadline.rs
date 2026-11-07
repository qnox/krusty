//! How long the supervisor waits for one analysis worker response.

use std::time::Duration;

/// Startup, including the classpath load of a large project.
pub(super) const READINESS: Duration = Duration::from_secs(30 * 60);
/// An interactive request: an open document, a dump, a library materialization.
pub(super) const INTERACTIVE: Duration = Duration::from_secs(30 * 60);
/// One background index chunk. The sweep is serial and an interactive command waits for the chunk
/// in flight, so the interactive deadline let one chunk that drives the checker into a pathological
/// case stall the sweep, and every edit, for half an hour. A large module's chunk finishes in
/// seconds; one that runs this long is abandoned: the worker restarts and the chunk is reported
/// inconclusive, so the store keeps what it already had.
pub(super) const INDEX_CHUNK: Duration = Duration::from_secs(2 * 60);

/// The deadline for an analysis request; `diagnostics_only` marks a background index chunk.
pub(super) fn analysis(diagnostics_only: bool) -> Duration {
    if diagnostics_only {
        INDEX_CHUNK
    } else {
        INTERACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_chunk_gets_the_short_deadline_and_interactive_work_the_long_one() {
        assert_eq!(analysis(true), Duration::from_secs(120));
        assert_eq!(analysis(false), Duration::from_secs(30 * 60));
    }
}
