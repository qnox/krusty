//! Running a plan's steps as JetBrains' toolchain runs its task graph: a step starts as soon as
//! every step it waits for has succeeded, at most `jobs` at a time, the plan's order deciding
//! between steps that are ready together. Once a step fails no step begins; the running ones are
//! waited for.
//!
//! Beginning a step and recording a failure are one synchronization boundary ([`Gate`]): a step
//! either began before the failure was recorded, or does nothing at all, however late it was
//! scheduled.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{mpsc, Mutex, MutexGuard, PoisonError};

/// Whether the plan has stopped, consulted by every step before it does anything and set by the
/// first step that fails.
pub(super) struct Gate {
    stopped: Mutex<bool>,
}

impl Gate {
    fn state(&self) -> MutexGuard<'_, bool> {
        // A step that panicked holding the lock had nothing half-done to the flag.
        self.stopped.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the step calling it may begin: false once a step has failed. A step does nothing,
    /// and touches nothing, before it has begun.
    pub(super) fn begin(&self) -> bool {
        !*self.state()
    }

    /// Record that a step failed: no step begins after this.
    pub(super) fn fail(&self) {
        *self.state() = true;
    }

    fn stopped(&self) -> bool {
        *self.state()
    }
}

/// Run step `0..waits.len()` with `work`, each after the earlier steps `waits` names for it, on up
/// to `jobs` threads. `work` begins a step through the [`Gate`] it is given, returning `None`
/// without doing anything when the gate refuses, and records through it a failure before it
/// returns. `finished` sees each begun step's result on the calling thread as soon as it arrives.
pub(super) fn schedule<R: Send>(
    waits: &[Vec<usize>],
    jobs: NonZeroUsize,
    work: impl Fn(usize, &Gate) -> Option<R> + Sync,
    mut finished: impl FnMut(usize, R),
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
    let gate = Gate {
        stopped: Mutex::new(false),
    };
    let (sender, receiver) = mpsc::channel();
    let work = &work;
    let gate = &gate;
    std::thread::scope(|scope| loop {
        // Only spares threads: whether a scheduled step does anything is the gate's to decide
        // when the step begins.
        while running < jobs.get() && !gate.stopped() {
            let Some(step) = ready.pop_first() else { break };
            let sender = sender.clone();
            scope.spawn(move || {
                // A panic stops the plan and is carried back, so the caller does not wait for a
                // result never sent.
                let result = panic::catch_unwind(AssertUnwindSafe(|| work(step, gate)));
                if result.is_err() {
                    gate.fail();
                }
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
        // A step the gate refused did nothing; the plan has stopped, so nothing waits for it.
        let Some(result) = result else { continue };
        finished(step, result);
        // A failed step's dependents are released too: once a step fails nothing begins.
        for &dependent in &dependents[step] {
            remaining[dependent] -= 1;
            if remaining[dependent] == 0 {
                ready.insert(dependent);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::schedule;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Condvar, Mutex};
    use std::time::Duration;

    /// 0 is the base; 1 and 2 wait for it; 3 waits for both.
    fn diamond() -> Vec<Vec<usize>> {
        vec![vec![], vec![0], vec![0], vec![1, 2]]
    }

    fn jobs(count: usize) -> NonZeroUsize {
        NonZeroUsize::new(count).unwrap()
    }

    /// The steps' starts and ends, as `+step` and `-step`, and the order `finished` saw them in;
    /// a step fails when `work` returns false.
    fn events(
        waits: &[Vec<usize>],
        jobs: NonZeroUsize,
        work: impl Fn(usize) -> bool + Sync,
    ) -> (Vec<String>, Vec<usize>) {
        events_seen(waits, jobs, work, |_| {})
    }

    /// [`events`], with `seen` called as each result arrives.
    fn events_seen(
        waits: &[Vec<usize>],
        jobs: NonZeroUsize,
        work: impl Fn(usize) -> bool + Sync,
        mut seen: impl FnMut(usize),
    ) -> (Vec<String>, Vec<usize>) {
        let events = Mutex::new(Vec::new());
        let mut finished = Vec::new();
        schedule(
            waits,
            jobs,
            |step, gate| {
                if !gate.begin() {
                    return None;
                }
                events.lock().unwrap().push(format!("+{step}"));
                let succeeded = work(step);
                events.lock().unwrap().push(format!("-{step}"));
                if !succeeded {
                    gate.fail();
                }
                Some(())
            },
            |step, ()| {
                finished.push(step);
                seen(step);
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

    #[test]
    fn a_failure_not_yet_received_starts_nothing() {
        // 0 and 1 run with two jobs while 2 waits for one. 1 fails while 0's success is being
        // handled, before 1's result arrives: 2 must not take 0's job.
        let (handling_zero, at_one) = mpsc::channel();
        let (one_failed, failure) = mpsc::channel();
        let at_one = Mutex::new(at_one);
        let one_failed = Mutex::new(one_failed);
        let started = Mutex::new(Vec::new());
        let mut finished = Vec::new();
        schedule(
            &[Vec::new(), Vec::new(), Vec::new()],
            jobs(2),
            |step, gate| {
                if !gate.begin() {
                    return None;
                }
                started.lock().unwrap().push(step);
                if step == 1 {
                    at_one
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(30))
                        .expect("0's result is handled");
                    gate.fail();
                    one_failed.lock().unwrap().send(()).unwrap();
                }
                Some(())
            },
            |step, ()| {
                finished.push(step);
                if step == 0 {
                    handling_zero.send(()).unwrap();
                    failure
                        .recv_timeout(Duration::from_secs(30))
                        .expect("1 fails");
                }
            },
        );
        let mut started = started.into_inner().unwrap();
        started.sort();
        assert_eq!((started, finished), (vec![0, 1], vec![0, 1]));
    }

    #[test]
    fn a_step_scheduled_before_a_failure_but_beginning_after_it_does_nothing() {
        // 0 and 1 run with two jobs while 2 waits for one. 0 succeeds, so 2 is scheduled while
        // nothing has failed; 1 fails only then, and 2 tries to begin only after that.
        let (two_scheduled, at_one) = mpsc::channel();
        let (one_failed, at_two) = mpsc::channel();
        let at_one = Mutex::new(at_one);
        let at_two = Mutex::new(at_two);
        let two_scheduled = Mutex::new(two_scheduled);
        let one_failed = Mutex::new(one_failed);
        let wait = |receiver: &Mutex<mpsc::Receiver<()>>| {
            receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("the other step arrives");
        };
        let began = Mutex::new(Vec::new());
        let mut finished = Vec::new();
        schedule(
            &[Vec::new(), Vec::new(), Vec::new()],
            jobs(2),
            |step, gate| {
                if step == 2 {
                    two_scheduled.lock().unwrap().send(()).unwrap();
                    wait(&at_two);
                }
                if !gate.begin() {
                    return None;
                }
                began.lock().unwrap().push(step);
                if step == 1 {
                    wait(&at_one);
                    gate.fail();
                    one_failed.lock().unwrap().send(()).unwrap();
                }
                Some(())
            },
            |step, ()| finished.push(step),
        );
        let mut began = began.into_inner().unwrap();
        began.sort();
        assert_eq!((began, finished), (vec![0, 1], vec![0, 1]));
    }
}
