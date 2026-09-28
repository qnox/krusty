//! Cancel an analysis that a newer edit has already made obsolete.
//!
//! Pending `Analyze` commands coalesce, but the engine thread stays inside the worker until that
//! pass returns. A pass that has already been running is the one a later edit should preempt;
//! killing every keystroke would restart the classpath more often than it would save work.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How long an in-flight pass may run before a newer edit cancels it.
const DEFAULT_PREEMPT_AFTER_MS: u64 = 500;

fn monotonic_ms() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    // Anchor behind the first sample so a pass that started earlier in this process still has a
    // representable start time. Comparisons only use differences, so the anchor is not an epoch.
    let origin = ORIGIN.get_or_init(|| {
        Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now)
    });
    origin.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// Shared between the command queue and the analysis that is currently inside the worker.
pub struct AnalysisGate {
    running: AtomicBool,
    started_ms: AtomicU64,
    cancel: AtomicBool,
    preempt_after_ms: u64,
}

impl Default for AnalysisGate {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisGate {
    pub fn new() -> Self {
        Self::with_preempt_after(DEFAULT_PREEMPT_AFTER_MS)
    }

    pub(crate) fn with_preempt_after(preempt_after_ms: u64) -> Self {
        Self {
            running: AtomicBool::new(false),
            started_ms: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            preempt_after_ms,
        }
    }

    /// Publish this pass. `cancel` is cleared before `running` becomes visible, so a newer edit
    /// cannot have its cancel flag wiped by the store that opens the pass.
    pub(crate) fn begin(&self) {
        self.cancel.store(false, Ordering::Release);
        self.started_ms.store(monotonic_ms(), Ordering::Release);
        self.running.store(true, Ordering::Release);
    }

    pub(crate) fn end(&self) {
        self.running.store(false, Ordering::Release);
    }

    pub(crate) fn note_newer_analysis(&self) {
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        let started = self.started_ms.load(Ordering::Acquire);
        let elapsed = monotonic_ms().saturating_sub(started);
        if elapsed >= self.preempt_after_ms {
            self.cancel.store(true, Ordering::Release);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// Ends the pass on drop, including when analysis returns early or panics.
pub(crate) struct AnalysisRun {
    gate: Arc<AnalysisGate>,
}

impl AnalysisRun {
    pub(crate) fn begin(gate: Arc<AnalysisGate>) -> Self {
        gate.begin();
        Self { gate }
    }
}

impl Drop for AnalysisRun {
    fn drop(&mut self) {
        self.gate.end();
    }
}

pub fn analysis_was_superseded(kind: io::ErrorKind) -> bool {
    kind == io::ErrorKind::WouldBlock
}

pub(crate) fn superseded_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "analysis superseded by a newer edit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_pass_that_already_started_is_cancelled_by_a_newer_edit() {
        let gate = AnalysisGate::with_preempt_after(500);
        gate.begin_elapsed(1_000);
        gate.note_newer_analysis();
        assert!(gate.is_cancelled());
    }

    #[test]
    fn a_pass_that_just_started_is_not_cancelled() {
        let gate = AnalysisGate::with_preempt_after(500);
        gate.begin();
        gate.note_newer_analysis();
        assert!(!gate.is_cancelled());
    }

    #[test]
    fn an_idle_gate_ignores_a_newer_edit() {
        let gate = AnalysisGate::with_preempt_after(0);
        gate.note_newer_analysis();
        assert!(!gate.is_cancelled());
    }

    #[test]
    fn ending_the_pass_stops_later_edits_from_cancelling_it() {
        let gate = Arc::new(AnalysisGate::with_preempt_after(0));
        {
            let _run = AnalysisRun::begin(Arc::clone(&gate));
            gate.note_newer_analysis();
            assert!(gate.is_cancelled());
        }
        gate.note_newer_analysis();
        {
            let _run = AnalysisRun::begin(Arc::clone(&gate));
            assert!(
                !gate.is_cancelled(),
                "the next pass starts clear of an edit that arrived after the previous one ended"
            );
        }
    }

    impl AnalysisGate {
        fn begin_elapsed(&self, elapsed_ms: u64) {
            self.cancel.store(false, Ordering::Release);
            self.started_ms
                .store(monotonic_ms().saturating_sub(elapsed_ms), Ordering::Release);
            self.running.store(true, Ordering::Release);
        }
    }
}
