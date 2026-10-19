//! Running a plan's steps as JetBrains' toolchain runs its task graph: a step starts as soon as
//! every step it waits for has succeeded, at most `jobs` at a time, the plan's order deciding
//! between steps that are ready together. Once a step fails no step starts; the running ones are
//! waited for.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc;

/// Run step `0..waits.len()` with `work`, each after the earlier steps `waits` names for it, on up
/// to `jobs` threads. `finished` sees each step's result on the calling thread, in the order the
/// steps finish, and says whether the step succeeded.
pub(super) fn schedule<R: Send>(
    waits: &[Vec<usize>],
    jobs: NonZeroUsize,
    work: impl Fn(usize) -> R + Sync,
    mut finished: impl FnMut(usize, R) -> bool,
) {
    let mut remaining: Vec<usize> = waits.iter().map(Vec::len).collect();
    let mut dependents = vec![Vec::new(); waits.len()];
    for (step, waited) in waits.iter().enumerate() {
        for &dependency in waited {
            dependents[dependency].push(step);
        }
    }
    let mut ready: BTreeSet<usize> = (0..waits.len())
        .filter(|&step| remaining[step] == 0)
        .collect();
    let mut running = 0;
    let mut stopped = false;
    let (sender, receiver) = mpsc::channel();
    let work = &work;
    std::thread::scope(|scope| loop {
        while !stopped && running < jobs.get() {
            let Some(step) = ready.pop_first() else { break };
            let sender = sender.clone();
            scope.spawn(move || {
                // A panic is carried back so the caller does not wait for a result never sent.
                let result = panic::catch_unwind(AssertUnwindSafe(|| work(step)));
                // The receiver lives until every spawned step has reported.
                let _ = sender.send((step, result));
            });
            running += 1;
        }
        if running == 0 {
            break;
        }
        let (step, result) = receiver
            .recv()
            .expect("the scheduler holds a sender while steps run");
        running -= 1;
        let result = result.unwrap_or_else(|payload| panic::resume_unwind(payload));
        if finished(step, result) {
            for &dependent in &dependents[step] {
                remaining[dependent] -= 1;
                if remaining[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        } else {
            stopped = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::schedule;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    /// 0 is the base; 1 and 2 wait for it; 3 waits for both.
    fn diamond() -> Vec<Vec<usize>> {
        vec![vec![], vec![0], vec![0], vec![1, 2]]
    }

    fn jobs(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
    }

    /// The steps' starts and ends, as `+step` and `-step`, and the order `finished` saw them in.
    fn events(
        waits: &[Vec<usize>],
        jobs: NonZeroUsize,
        work: impl Fn(usize) -> bool + Sync,
    ) -> (Vec<String>, Vec<usize>) {
        let events = Mutex::new(Vec::new());
        let mut finished = Vec::new();
        schedule(
            waits,
            jobs,
            |step| {
                events.lock().unwrap().push(format!("+{step}"));
                let succeeded = work(step);
                events.lock().unwrap().push(format!("-{step}"));
                succeeded
            },
            |step, succeeded| {
                finished.push(step);
                succeeded
            },
        );
        (events.into_inner().unwrap(), finished)
    }

    #[test]
    fn one_job_runs_the_steps_one_after_another_in_plan_order() {
        assert_eq!(
            events(&diamond(), jobs(1), |_| true),
            (
                ["+0", "-0", "+1", "-1", "+2", "-2", "+3", "-3"]
                    .map(String::from)
                    .to_vec(),
                vec![0, 1, 2, 3]
            )
        );
    }

    /// Steps that each wait, up to half a minute, until `count` of them have arrived.
    struct Rendezvous {
        arrived: Mutex<usize>,
        changed: Condvar,
        count: usize,
    }

    impl Rendezvous {
        fn new(count: usize) -> Rendezvous {
            Rendezvous {
                arrived: Mutex::new(0),
                changed: Condvar::new(),
                count,
            }
        }

        /// Arrive, and say whether the others did too.
        fn meet(&self) -> bool {
            let mut arrived = self.arrived.lock().unwrap();
            *arrived += 1;
            self.changed.notify_all();
            let (arrived, _) = self
                .changed
                .wait_timeout_while(arrived, Duration::from_secs(30), |arrived| {
                    *arrived < self.count
                })
                .unwrap();
            *arrived >= self.count
        }
    }

    #[test]
    fn independent_steps_run_together() {
        // Steps 1 and 2 meet only when both run at once.
        let rendezvous = Rendezvous::new(2);
        let met = Mutex::new(Vec::new());
        let (mut events, finished) = events(&diamond(), jobs(2), |step| {
            if step == 1 || step == 2 {
                let together = rendezvous.meet();
                met.lock().unwrap().push(together);
            }
            true
        });
        assert_eq!(met.into_inner().unwrap(), [true, true]);
        // 1 and 2 start, then end, in either order.
        events[2..4].sort();
        events[4..6].sort();
        assert_eq!(
            events,
            ["+0", "-0", "+1", "+2", "-1", "-2", "+3", "-3"].map(String::from)
        );
        assert_eq!((finished[0], finished[3]), (0, 3));
    }

    #[test]
    fn no_more_than_jobs_steps_run_at_once() {
        let running = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let waits = vec![Vec::new(); 6];
        events(&waits, jobs(2), |_| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            running.fetch_sub(1, Ordering::SeqCst);
            true
        });
        assert!(most.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn a_failure_starts_nothing_more_and_waits_for_the_running_steps() {
        assert_eq!(
            events(&diamond(), jobs(1), |step| step != 1),
            (
                ["+0", "-0", "+1", "-1"].map(String::from).to_vec(),
                vec![0, 1]
            )
        );
        // With two jobs, 2 is running when 1 fails: it is waited for, and 3 never starts.
        let rendezvous = Rendezvous::new(2);
        let (mut events, finished) = events(&diamond(), jobs(2), |step| match step {
            1 => !rendezvous.meet(),
            2 => {
                rendezvous.meet();
                std::thread::sleep(Duration::from_millis(200));
                true
            }
            _ => true,
        });
        events[2..4].sort();
        assert_eq!(
            (events, finished),
            (
                ["+0", "-0", "+1", "+2", "-1", "-2"]
                    .map(String::from)
                    .to_vec(),
                vec![0, 1, 2]
            )
        );
    }
}
