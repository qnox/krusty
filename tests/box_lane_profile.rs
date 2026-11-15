//! Where a runnable box lane spends its time.
//!
//! The lane's report line gives one wall-clock total. These thread-sum totals split it into the
//! JVM oracle the owned fixtures require, the target's own reference oracle, krusty's compilation
//! and link, and running the programs. The slowest cases are listed so a run that is slow on one
//! host but not another can be attributed to stages or to particular cases.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One measured part of a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The JVM reference compile and `box()` run an owned fixture needs first.
    JvmOracle,
    /// The target's own reference compiler: the oracle a lane requires, and the reference KLIB
    /// a scored case's bytes are compared with.
    TargetOracle,
    /// The lane's compile, link and run of the case.
    Lane,
    /// Running the compiled program, inside [`Stage::Lane`].
    Run,
}

const STAGE_COUNT: usize = 4;

impl Stage {
    fn index(self) -> usize {
        match self {
            Stage::JvmOracle => 0,
            Stage::TargetOracle => 1,
            Stage::Lane => 2,
            Stage::Run => 3,
        }
    }
}

struct StageTotal {
    nanos: AtomicU64,
    count: AtomicU64,
}

static TOTALS: [StageTotal; STAGE_COUNT] = [const {
    StageTotal {
        nanos: AtomicU64::new(0),
        count: AtomicU64::new(0),
    }
}; STAGE_COUNT];

/// Run `work` and add its duration to `stage`'s total.
pub fn timed<T>(stage: Stage, work: impl FnOnce() -> T) -> T {
    let _progress = ActiveStage::enter(stage);
    let started = Instant::now();
    let result = work();
    let total = &TOTALS[stage.index()];
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    total.nanos.fetch_add(nanos, Ordering::Relaxed);
    total.count.fetch_add(1, Ordering::Relaxed);
    result
}

const MAX_REPORTED_ACTIVE_CASES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressPhase {
    Preparing,
    JvmOracle,
    TargetOracle,
    CompileAndLink,
    Run,
}

impl ProgressPhase {
    fn from_stage(stage: Stage) -> Self {
        match stage {
            Stage::JvmOracle => Self::JvmOracle,
            Stage::TargetOracle => Self::TargetOracle,
            Stage::Lane => Self::CompileAndLink,
            Stage::Run => Self::Run,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::JvmOracle => "JVM oracle",
            Self::TargetOracle => "target oracle",
            Self::CompileAndLink => "compile/link",
            Self::Run => "program run",
        }
    }
}

struct ActiveCase {
    name: String,
    started: Instant,
    phase: ProgressPhase,
    phase_started: Instant,
}

struct ProgressState {
    started: Instant,
    completed: usize,
    active: BTreeMap<u64, ActiveCase>,
    stopped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveCaseReport {
    name: String,
    phase: &'static str,
    elapsed: Duration,
    phase_elapsed: Duration,
}

struct ProgressSnapshot {
    elapsed: Duration,
    completed: usize,
    active: Vec<ActiveCaseReport>,
    omitted_active: usize,
}

impl ProgressState {
    fn new(started: Instant) -> Self {
        Self {
            started,
            completed: 0,
            active: BTreeMap::new(),
            stopped: false,
        }
    }

    fn start_case(&mut self, id: u64, name: String, now: Instant) {
        let previous = self.active.insert(
            id,
            ActiveCase {
                name,
                started: now,
                phase: ProgressPhase::Preparing,
                phase_started: now,
            },
        );
        debug_assert!(previous.is_none(), "progress case id reused while active");
    }

    fn set_phase(
        &mut self,
        id: u64,
        phase: ProgressPhase,
        now: Instant,
    ) -> Option<(ProgressPhase, Instant)> {
        let active = self.active.get_mut(&id)?;
        let previous = (active.phase, active.phase_started);
        active.phase = phase;
        active.phase_started = now;
        Some(previous)
    }

    fn restore_phase(&mut self, id: u64, previous: (ProgressPhase, Instant)) {
        if let Some(active) = self.active.get_mut(&id) {
            (active.phase, active.phase_started) = previous;
        }
    }

    fn finish_case(&mut self, id: u64) {
        if self.active.remove(&id).is_some() {
            self.completed += 1;
        }
    }

    fn snapshot(&self, now: Instant) -> ProgressSnapshot {
        let active = self
            .active
            .values()
            .take(MAX_REPORTED_ACTIVE_CASES)
            .map(|case| ActiveCaseReport {
                name: case.name.clone(),
                phase: case.phase.label(),
                elapsed: now.saturating_duration_since(case.started),
                phase_elapsed: now.saturating_duration_since(case.phase_started),
            })
            .collect::<Vec<_>>();
        ProgressSnapshot {
            elapsed: now.saturating_duration_since(self.started),
            completed: self.completed,
            active,
            omitted_active: self.active.len().saturating_sub(MAX_REPORTED_ACTIVE_CASES),
        }
    }
}

fn format_progress(label: &str, total: usize, snapshot: &ProgressSnapshot) -> String {
    let seconds = |duration: Duration| duration.as_secs_f64();
    let mut line = format!(
        "{label} conformance progress: {}/{} cases, elapsed {:.0}s; active:",
        snapshot.completed,
        total,
        seconds(snapshot.elapsed)
    );
    if snapshot.active.is_empty() {
        line.push_str(" none");
    } else {
        for case in &snapshot.active {
            line.push_str(&format!(
                " {} [{} {:.0}s; total {:.0}s]",
                case.name,
                case.phase,
                seconds(case.phase_elapsed),
                seconds(case.elapsed)
            ));
        }
        if snapshot.omitted_active > 0 {
            line.push_str(&format!(" +{} more", snapshot.omitted_active));
        }
    }
    line
}

struct ProgressInner {
    label: String,
    total: usize,
    interval: Duration,
    next_id: AtomicU64,
    state: Mutex<ProgressState>,
    changed: Condvar,
}

struct CurrentCase {
    inner: Weak<ProgressInner>,
    id: u64,
}

thread_local! {
    static CURRENT_CASE: std::cell::RefCell<Option<CurrentCase>> = const {
        std::cell::RefCell::new(None)
    };
}

/// A rate-limited progress heartbeat for a parallel box-corpus sweep.
pub struct ProgressReporter {
    inner: Arc<ProgressInner>,
    reporter: Option<JoinHandle<()>>,
}

impl ProgressReporter {
    pub fn new(label: &str, total: usize, interval: Duration) -> Self {
        Self::spawn(label, total, interval, None)
    }

    #[cfg(test)]
    fn spawn_observed(
        label: &str,
        total: usize,
        interval: Duration,
    ) -> (Self, std::sync::mpsc::Receiver<()>) {
        let (ready, observed) = std::sync::mpsc::sync_channel(0);
        (Self::spawn(label, total, interval, Some(ready)), observed)
    }

    fn spawn(
        label: &str,
        total: usize,
        interval: Duration,
        ready: Option<std::sync::mpsc::SyncSender<()>>,
    ) -> Self {
        let inner = Arc::new(ProgressInner {
            label: label.to_string(),
            total,
            interval,
            next_id: AtomicU64::new(0),
            state: Mutex::new(ProgressState::new(Instant::now())),
            changed: Condvar::new(),
        });
        let reporter_inner = Arc::clone(&inner);
        let reporter = thread::Builder::new()
            .name("box-conformance-progress".to_string())
            .spawn(move || report_progress(reporter_inner, ready));
        let reporter = match reporter {
            Ok(reporter) => Some(reporter),
            Err(error) => {
                eprintln!(
                    "{} conformance progress reporter unavailable: {error}",
                    inner.label
                );
                None
            }
        };
        Self { inner, reporter }
    }

    pub fn start_case(&self, name: String) -> CaseProgress {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start_case(id, name, Instant::now());
        let previous = CURRENT_CASE.with(|current| {
            current.replace(Some(CurrentCase {
                inner: Arc::downgrade(&self.inner),
                id,
            }))
        });
        CaseProgress {
            inner: Arc::clone(&self.inner),
            id,
            previous,
        }
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        self.inner.changed.notify_all();
        if let Some(reporter) = self.reporter.take() {
            let _ = reporter.join();
        }
    }
}

pub struct CaseProgress {
    inner: Arc<ProgressInner>,
    id: u64,
    previous: Option<CurrentCase>,
}

impl Drop for CaseProgress {
    fn drop(&mut self) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_case(self.id);
        let previous = self.previous.take();
        CURRENT_CASE.with(|current| *current.borrow_mut() = previous);
    }
}

struct ActiveStage {
    inner: Arc<ProgressInner>,
    id: u64,
    previous: Option<(ProgressPhase, Instant)>,
}

impl ActiveStage {
    fn enter(stage: Stage) -> Option<Self> {
        CURRENT_CASE.with(|current| {
            let current = current.borrow();
            let current = current.as_ref()?;
            let inner = current.inner.upgrade()?;
            let previous = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .set_phase(current.id, ProgressPhase::from_stage(stage), Instant::now());
            previous.map(|previous| Self {
                inner,
                id: current.id,
                previous: Some(previous),
            })
        })
    }
}

impl Drop for ActiveStage {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .restore_phase(self.id, previous);
        }
    }
}

fn report_progress(inner: Arc<ProgressInner>, mut ready: Option<std::sync::mpsc::SyncSender<()>>) {
    loop {
        let state = inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped {
            return;
        }
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }
        let (state, timeout) = inner
            .changed
            .wait_timeout(state, inner.interval)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stopped {
            return;
        }
        if timeout.timed_out() && state.completed < inner.total {
            let snapshot = state.snapshot(Instant::now());
            drop(state);
            eprintln!("{}", format_progress(&inner.label, inner.total, &snapshot));
        }
    }
}

/// The time and number of calls of one stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTime {
    pub time: Duration,
    pub count: u64,
}

/// Every stage's total, in this process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTimes {
    pub jvm_oracle: StageTime,
    pub target_oracle: StageTime,
    pub lane: StageTime,
    pub run: StageTime,
}

pub fn stage_times() -> StageTimes {
    let read = |stage: Stage| {
        let total = &TOTALS[stage.index()];
        StageTime {
            time: Duration::from_nanos(total.nanos.load(Ordering::Relaxed)),
            count: total.count.load(Ordering::Relaxed),
        }
    };
    StageTimes {
        jvm_oracle: read(Stage::JvmOracle),
        target_oracle: read(Stage::TargetOracle),
        lane: read(Stage::Lane),
        run: read(Stage::Run),
    }
}

/// How many of the slowest cases the report names.
const SLOWEST: usize = 10;

/// The lane's timing lines: the thread-sum split of every case's time, then the slowest cases.
pub fn timing_report(
    wall: Duration,
    threads: usize,
    cases: &[(String, Duration)],
    stages: StageTimes,
) -> String {
    let secs = |duration: Duration| duration.as_secs_f64();
    let case_total: Duration = cases.iter().map(|(_, time)| *time).sum();
    let compile = stages.lane.time.saturating_sub(stages.run.time);
    let other = case_total
        .saturating_sub(stages.jvm_oracle.time)
        .saturating_sub(stages.target_oracle.time)
        .saturating_sub(stages.lane.time);
    let mut report = format!(
        "timing (thread-sum, {threads} threads, wall {:.1}s): cases {} {:.1}s | \
         jvm oracle {} {:.1}s | target oracle {} {:.1}s | compile and link {} {:.1}s | \
         run {} {:.1}s | other {:.1}s\n",
        secs(wall),
        cases.len(),
        secs(case_total),
        stages.jvm_oracle.count,
        secs(stages.jvm_oracle.time),
        stages.target_oracle.count,
        secs(stages.target_oracle.time),
        stages.lane.count,
        secs(compile),
        stages.run.count,
        secs(stages.run.time),
        secs(other),
    );
    let mut slowest = cases.to_vec();
    slowest.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (file, time) in slowest.iter().take(SLOWEST) {
        report.push_str(&format!("  slow {:.1}s {file}\n", secs(*time)));
    }
    report
}

#[test]
fn timing_report_splits_case_time_and_names_the_slowest_cases() {
    let cases = (1..=12)
        .map(|index| {
            (
                format!("case{index:02}.kt"),
                Duration::from_millis(index * 100),
            )
        })
        .collect::<Vec<_>>();
    let stages = StageTimes {
        jvm_oracle: StageTime {
            time: Duration::from_millis(1_000),
            count: 2,
        },
        target_oracle: StageTime {
            time: Duration::from_millis(500),
            count: 1,
        },
        lane: StageTime {
            time: Duration::from_millis(5_000),
            count: 10,
        },
        run: StageTime {
            time: Duration::from_millis(1_200),
            count: 8,
        },
    };
    assert_eq!(
        timing_report(Duration::from_millis(2_500), 4, &cases, stages),
        "timing (thread-sum, 4 threads, wall 2.5s): cases 12 7.8s | jvm oracle 2 1.0s | \
         target oracle 1 0.5s | compile and link 10 3.8s | run 8 1.2s | other 1.3s\n\
         \x20 slow 1.2s case12.kt\n\
         \x20 slow 1.1s case11.kt\n\
         \x20 slow 1.0s case10.kt\n\
         \x20 slow 0.9s case09.kt\n\
         \x20 slow 0.8s case08.kt\n\
         \x20 slow 0.7s case07.kt\n\
         \x20 slow 0.6s case06.kt\n\
         \x20 slow 0.5s case05.kt\n\
         \x20 slow 0.4s case04.kt\n\
         \x20 slow 0.3s case03.kt\n"
    );
}

#[test]
fn a_timed_stage_counts_each_call_and_returns_its_result() {
    let before = stage_times().target_oracle.count;
    assert_eq!(timed(Stage::TargetOracle, || 7), 7);
    assert!(stage_times().target_oracle.count > before);
}

#[test]
fn progress_snapshot_tracks_completion_and_nested_phases() {
    let started = Instant::now();
    let mut state = ProgressState::new(started);
    state.start_case(4, "stdlib/foo.kt".to_string(), started);
    let phase_started = started + Duration::from_secs(3);
    assert_eq!(
        state.set_phase(4, ProgressPhase::CompileAndLink, phase_started),
        Some((ProgressPhase::Preparing, started))
    );
    let snapshot = state.snapshot(started + Duration::from_secs(11));
    assert_eq!(snapshot.completed, 0);
    assert_eq!(snapshot.active.len(), 1);
    assert_eq!(snapshot.active[0].phase, "compile/link");
    assert_eq!(snapshot.active[0].elapsed, Duration::from_secs(11));
    assert_eq!(snapshot.active[0].phase_elapsed, Duration::from_secs(8));
    assert_eq!(
        format_progress("wasm-js", 2, &snapshot),
        "wasm-js conformance progress: 0/2 cases, elapsed 11s; active: \
         stdlib/foo.kt [compile/link 8s; total 11s]"
    );

    state.finish_case(4);
    let complete = state.snapshot(started + Duration::from_secs(12));
    assert_eq!(complete.completed, 1);
    assert!(complete.active.is_empty());
    assert_eq!(
        format_progress("wasm-js", 2, &complete),
        "wasm-js conformance progress: 1/2 cases, elapsed 12s; active: none"
    );
}

#[test]
fn progress_snapshot_bounds_active_case_names() {
    let started = Instant::now();
    let mut state = ProgressState::new(started);
    for id in 0..(MAX_REPORTED_ACTIVE_CASES as u64 + 3) {
        state.start_case(id, format!("case-{id}.kt"), started);
    }
    let snapshot = state.snapshot(started + Duration::from_secs(60));
    assert_eq!(snapshot.active.len(), MAX_REPORTED_ACTIVE_CASES);
    assert_eq!(snapshot.omitted_active, 3);
    let line = format_progress("Native", 20, &snapshot);
    assert!(line.contains("+3 more"));
    assert!(!line.contains("case-10.kt"));
}

fn observed_phase(reporter: &ProgressReporter, id: u64) -> Option<&'static str> {
    reporter
        .inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .active
        .get(&id)
        .map(|case| case.phase.label())
}

#[test]
fn reporter_tracks_real_case_and_nested_timed_phases() {
    let (reporter, ready) =
        ProgressReporter::spawn_observed("test-lane", 1, Duration::from_secs(60));
    ready
        .recv_timeout(Duration::from_secs(2))
        .expect("reporter starts");
    let case = reporter.start_case("owned/example.kt".to_string());
    let id = case.id;
    assert_eq!(observed_phase(&reporter, id), Some("preparing"));
    timed(Stage::Lane, || {
        assert_eq!(observed_phase(&reporter, id), Some("compile/link"));
        timed(Stage::Run, || {
            assert_eq!(observed_phase(&reporter, id), Some("program run"));
        });
        assert_eq!(observed_phase(&reporter, id), Some("compile/link"));
    });
    assert_eq!(observed_phase(&reporter, id), Some("preparing"));
    drop(case);
    let state = reporter
        .inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(state.completed, 1);
    assert!(state.active.is_empty());
    CURRENT_CASE.with(|current| assert!(current.borrow().is_none()));
    drop(state);
    drop(reporter);
}

#[test]
fn case_and_stage_guards_restore_state_during_unwind() {
    let (reporter, ready) =
        ProgressReporter::spawn_observed("test-lane", 1, Duration::from_secs(60));
    ready
        .recv_timeout(Duration::from_secs(2))
        .expect("reporter starts");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let case = reporter.start_case("owned/panic.kt".to_string());
        let id = case.id;
        timed(Stage::Lane, || {
            assert_eq!(observed_phase(&reporter, id), Some("compile/link"));
            let _keep_case_alive_until_unwind = &case;
            panic!("injected stage panic");
        });
    }));
    assert!(result.is_err());
    let state = reporter
        .inner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(state.completed, 1);
    assert!(state.active.is_empty());
    CURRENT_CASE.with(|current| assert!(current.borrow().is_none()));
    drop(state);
    drop(reporter);
}

#[test]
fn reporter_drop_wakes_and_joins_the_worker() {
    let (reporter, ready) =
        ProgressReporter::spawn_observed("test-lane", 1, Duration::from_secs(60));
    ready
        .recv_timeout(Duration::from_secs(2))
        .expect("reporter starts");
    // Acquiring the state lock proves the worker has released it into its condition wait (or is
    // handling a spurious wake); the long interval makes a missing shutdown notification visible.
    drop(
        reporter
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let (finished, completion) = std::sync::mpsc::sync_channel(0);
    thread::spawn(move || {
        drop(reporter);
        let _ = finished.send(());
    });
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("reporter shutdown does not wait for its interval");
}
