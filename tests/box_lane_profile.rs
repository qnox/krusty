//! Where a runnable box lane spends its time.
//!
//! The lane's report line gives one wall-clock total. These thread-sum totals split it into the
//! JVM oracle the owned fixtures require, the target's own reference oracle, krusty's compilation
//! and link, and running the programs. The slowest cases are listed so a run that is slow on one
//! host but not another can be attributed to stages or to particular cases.

use std::sync::atomic::{AtomicU64, Ordering};
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
    let started = Instant::now();
    let result = work();
    let total = &TOTALS[stage.index()];
    let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    total.nanos.fetch_add(nanos, Ordering::Relaxed);
    total.count.fetch_add(1, Ordering::Relaxed);
    result
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
