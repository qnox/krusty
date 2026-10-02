//! One ownership transition for an analysis-worker process and its stdout reader.
//!
//! `release_worker` kills, polls, and joins against a single deadline. `try_wait` reporting an
//! exit, or an operating-system error that means this process is not a child, is the only
//! proof the child is gone. Any other wait error, or a child that is still running at the
//! deadline, stays in a one-slot park together with its reader. That park outlives the thread
//! that filled it. A replacement is not spawned while the slot is occupied, and the slot never
//! drops or forgets the last child and reader handles.

use std::io::{self, ErrorKind};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::{Arc, MutexGuard};

/// How long one release may spend killing the child, polling it, and joining its reader.
pub(crate) const WORKER_REAP_GRACE: Duration = Duration::from_secs(2);

const WAIT_POLL: Duration = Duration::from_millis(5);

/// Unix `ECHILD`: `waitpid` says this pid is not our child.
#[cfg(unix)]
const NOT_A_CHILD: i32 = 10;
/// Unix `ESRCH`: `kill` says the pid does not exist.
#[cfg(unix)]
const NO_SUCH_PROCESS: i32 = 3;
/// Windows `ERROR_INVALID_HANDLE`.
#[cfg(windows)]
const NOT_A_CHILD: i32 = 6;
/// Windows `ERROR_PROC_NOT_FOUND`.
#[cfg(windows)]
const NO_SUCH_PROCESS: i32 = 127;

fn park_slot() -> &'static Mutex<Option<Parked>> {
    static PARK: Mutex<Option<Parked>> = Mutex::new(None);
    &PARK
}

pub(crate) enum WaitClass {
    Exited(ExitStatus),
    Running,
    NotAChild,
    Failed(io::Error),
}

pub(crate) fn classify_wait(result: io::Result<Option<ExitStatus>>) -> WaitClass {
    match result {
        Ok(Some(status)) => WaitClass::Exited(status),
        Ok(None) => WaitClass::Running,
        Err(error) if error.raw_os_error() == Some(NOT_A_CHILD) => WaitClass::NotAChild,
        Err(error) => WaitClass::Failed(error),
    }
}

pub(crate) enum KillClass {
    Signaled,
    AlreadyGone,
    Failed(io::Error),
}

pub(crate) fn classify_kill(result: io::Result<()>) -> KillClass {
    match result {
        Ok(()) => KillClass::Signaled,
        Err(error) if error.raw_os_error() == Some(NO_SUCH_PROCESS) => KillClass::AlreadyGone,
        Err(error) => KillClass::Failed(error),
    }
}

pub(crate) fn still_live() -> io::Error {
    io::Error::new(
        ErrorKind::TimedOut,
        "analysis worker is still live after the reap deadline",
    )
}

pub(crate) fn replacement_permitted() -> bool {
    with_slot(|slot| !slot_occupied(slot))
}

enum Poll {
    Exited,
    Running,
    NotAChild,
    Failed(io::Error),
}

fn poll_child(child: &mut Child) -> Poll {
    match classify_wait(child.try_wait()) {
        WaitClass::Exited(_) => Poll::Exited,
        WaitClass::Running => Poll::Running,
        WaitClass::NotAChild => Poll::NotAChild,
        WaitClass::Failed(error) => Poll::Failed(error),
    }
}

enum OwnedChild {
    Os(Child),
    #[cfg(test)]
    Simulated(Simulated),
}

impl OwnedChild {
    fn signal_kill(&mut self) -> io::Result<()> {
        match self {
            OwnedChild::Os(child) => child.kill(),
            #[cfg(test)]
            OwnedChild::Simulated(child) => child.signal_kill(),
        }
    }

    fn poll(&mut self) -> Poll {
        match self {
            OwnedChild::Os(child) => poll_child(child),
            #[cfg(test)]
            OwnedChild::Simulated(child) => child.poll(),
        }
    }

    #[cfg(test)]
    fn force_finish(&mut self) {
        match self {
            OwnedChild::Os(child) => {
                let _ = child.kill();
            }
            OwnedChild::Simulated(child) => child.running.store(false, Ordering::SeqCst),
        }
    }
}

struct ReaderWait {
    join: JoinHandle<()>,
    done: mpsc::Receiver<()>,
    #[cfg(test)]
    unblock: Option<mpsc::Sender<()>>,
}

type FrameOutcome<R> = (R, io::Result<Option<Vec<u8>>>);

struct Parked {
    child: Option<OwnedChild>,
    reader: Option<ReaderWait>,
}

pub(crate) struct FrameReader<R> {
    join: Option<JoinHandle<()>>,
    outcome: Option<mpsc::Receiver<FrameOutcome<R>>>,
    done: Option<mpsc::Receiver<()>>,
}

impl<R> FrameReader<R> {
    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<FrameOutcome<R>, RecvTimeoutError> {
        self.outcome
            .as_ref()
            .expect("frame reader outcome")
            .recv_timeout(timeout)
    }

    pub(crate) fn join(mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    fn into_wait(mut self) -> ReaderWait {
        ReaderWait {
            join: self.join.take().expect("frame reader join"),
            done: self.done.take().expect("frame reader done"),
            #[cfg(test)]
            unblock: None,
        }
    }
}

impl<R> Drop for FrameReader<R> {
    fn drop(&mut self) {
        // `into_wait` and `join` take the handle first. Dropping a still-running handle detaches
        // the thread, so a forgotten one stays owned by the process instead.
        if let Some(join) = self.join.take() {
            if join.is_finished() {
                let _ = join.join();
            } else {
                std::mem::forget(join);
            }
        }
    }
}

pub(crate) fn spawn_frame_reader<R, F>(read: F) -> FrameReader<R>
where
    R: Send + 'static,
    F: FnOnce() -> FrameOutcome<R> + Send + 'static,
{
    let (outcome_tx, outcome) = mpsc::sync_channel(1);
    let (done_tx, done) = mpsc::channel();
    let join = std::thread::spawn(move || {
        let read = read();
        let _ = outcome_tx.send(read);
        let _ = done_tx.send(());
    });
    FrameReader {
        join: Some(join),
        outcome: Some(outcome),
        done: Some(done),
    }
}

pub(crate) fn release_worker<R>(
    child: &mut Option<Child>,
    reader: &mut Option<FrameReader<R>>,
    grace: Duration,
) -> io::Result<()> {
    if child.is_none() && reader.is_none() {
        return Ok(());
    }
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(Instant::now);
    with_slot(|slot| {
        if slot_occupied(slot) {
            return Err(still_live());
        }
        let owned = child.take().map(OwnedChild::Os);
        let reader = reader.take().map(FrameReader::into_wait);
        dispose(slot, owned, reader, deadline)
    })
}

fn dispose(
    slot: &mut Option<Parked>,
    child: Option<OwnedChild>,
    reader: Option<ReaderWait>,
    deadline: Instant,
) -> io::Result<()> {
    let Some(mut child) = child else {
        return finish_reader(slot, reader, deadline);
    };
    let kill = classify_kill(child.signal_kill());
    let mut poll = await_exit(&mut child, deadline);
    if matches!(poll, Poll::Running) {
        poll = child.poll();
    }
    match poll {
        Poll::Exited | Poll::NotAChild => finish_reader(slot, reader, deadline),
        Poll::Running => {
            *slot = Some(Parked {
                child: Some(child),
                reader,
            });
            Err(kill_failure(kill))
        }
        Poll::Failed(error) => {
            *slot = Some(Parked {
                child: Some(child),
                reader,
            });
            Err(error)
        }
    }
}

fn kill_failure(kill: KillClass) -> io::Error {
    match kill {
        KillClass::Failed(error) => io::Error::new(
            ErrorKind::TimedOut,
            format!("analysis worker kill failed: {error}"),
        ),
        KillClass::Signaled | KillClass::AlreadyGone => still_live(),
    }
}

fn await_exit(child: &mut OwnedChild, deadline: Instant) -> Poll {
    loop {
        match child.poll() {
            Poll::Running if Instant::now() >= deadline => return Poll::Running,
            Poll::Running => std::thread::sleep(WAIT_POLL),
            other => return other,
        }
    }
}

fn finish_reader(
    slot: &mut Option<Parked>,
    reader: Option<ReaderWait>,
    deadline: Instant,
) -> io::Result<()> {
    let Some(reader) = reader else {
        return Ok(());
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    match reader.done.recv_timeout(remaining) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            let _ = reader.join.join();
            Ok(())
        }
        Err(RecvTimeoutError::Timeout) => {
            *slot = Some(Parked {
                child: None,
                reader: Some(reader),
            });
            Err(still_live())
        }
    }
}

fn slot_occupied(slot: &mut Option<Parked>) -> bool {
    let Some(parked) = slot.as_mut() else {
        return false;
    };
    if reclaim(parked) {
        *slot = None;
        false
    } else {
        true
    }
}

fn reclaim(parked: &mut Parked) -> bool {
    let child_done = match parked.child.as_mut() {
        None => true,
        Some(child) => match child.poll() {
            Poll::Exited | Poll::NotAChild => {
                parked.child = None;
                true
            }
            Poll::Running | Poll::Failed(_) => false,
        },
    };
    let reader_done = parked
        .reader
        .as_ref()
        .is_none_or(|reader| reader.join.is_finished());
    if child_done && reader_done {
        if let Some(reader) = parked.reader.take() {
            let _ = reader.join.join();
        }
        true
    } else {
        false
    }
}

fn with_slot<T>(body: impl FnOnce(&mut Option<Parked>) -> T) -> T {
    let mut slot = park_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    body(&mut slot)
}

/// Last-resort shutdown for a child the one slot could not take during `release`.
///
/// A normal child dies on `kill` and is reaped here. A child that is still running, or whose wait
/// failed, is moved into the process-lifetime slot when that slot has no child. The handle is not
/// forgotten. When the slot already retains a different live child, this one is waited so the
/// operating system can reap it instead of being dropped unreaped.
pub(crate) fn quarantine_child(mut child: Child) {
    let _ = child.kill();
    let deadline = Instant::now()
        .checked_add(WORKER_REAP_GRACE)
        .unwrap_or_else(Instant::now);
    loop {
        match classify_wait(child.try_wait()) {
            WaitClass::Exited(_) | WaitClass::NotAChild => return,
            WaitClass::Running if Instant::now() < deadline => std::thread::sleep(WAIT_POLL),
            WaitClass::Running | WaitClass::Failed(_) => {
                retain_os_child(child);
                return;
            }
        }
    }
}

fn retain_os_child(child: Child) {
    let waiting = with_slot(|slot| {
        if !slot_occupied(slot) {
            *slot = Some(Parked {
                child: Some(OwnedChild::Os(child)),
                reader: None,
            });
            return None;
        }
        if let Some(parked) = slot.as_mut() {
            if parked.child.is_none() {
                parked.child = Some(OwnedChild::Os(child));
                return None;
            }
        }
        Some(child)
    });
    if let Some(mut child) = waiting {
        let _ = child.wait();
    }
}

#[cfg(test)]
struct Simulated {
    running: Arc<AtomicBool>,
    kill: SimulatedKill,
    wait: SimulatedWait,
}

#[cfg(test)]
enum SimulatedKill {
    Signal,
    Fail,
}

#[cfg(test)]
enum SimulatedWait {
    RunningFlag,
    NotAChild,
    Fail,
}

#[cfg(test)]
impl Simulated {
    fn survives(running: Arc<AtomicBool>) -> Self {
        Self {
            running,
            kill: SimulatedKill::Signal,
            wait: SimulatedWait::RunningFlag,
        }
    }

    fn kill_fails(running: Arc<AtomicBool>) -> Self {
        Self {
            running,
            kill: SimulatedKill::Fail,
            wait: SimulatedWait::RunningFlag,
        }
    }

    fn wait_fails() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(true)),
            kill: SimulatedKill::Signal,
            wait: SimulatedWait::Fail,
        }
    }

    fn not_a_child() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            kill: SimulatedKill::Signal,
            wait: SimulatedWait::NotAChild,
        }
    }

    fn signal_kill(&mut self) -> io::Result<()> {
        match self.kill {
            SimulatedKill::Signal => Ok(()),
            SimulatedKill::Fail => Err(io::Error::new(ErrorKind::PermissionDenied, "kill failed")),
        }
    }

    fn poll(&mut self) -> Poll {
        match self.wait {
            SimulatedWait::RunningFlag => {
                if self.running.load(Ordering::SeqCst) {
                    Poll::Running
                } else {
                    Poll::Exited
                }
            }
            SimulatedWait::NotAChild => Poll::NotAChild,
            SimulatedWait::Fail => {
                Poll::Failed(io::Error::new(ErrorKind::PermissionDenied, "wait failed"))
            }
        }
    }
}

#[cfg(test)]
fn release_simulated(
    child: &mut Option<Simulated>,
    reader: &mut Option<ReaderWait>,
    grace: Duration,
) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(Instant::now);
    with_slot(|slot| {
        if slot_occupied(slot) {
            return Err(still_live());
        }
        let owned = child.take().map(OwnedChild::Simulated);
        let reader = reader.take();
        dispose(slot, owned, reader, deadline)
    })
}

#[cfg(test)]
fn parked_processes() -> usize {
    with_slot(|slot| usize::from(slot.as_ref().is_some_and(|parked| parked.child.is_some())))
}

#[cfg(test)]
struct LiveCount(Arc<AtomicUsize>);

#[cfg(test)]
impl Drop for LiveCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn spawn_blocked_reader(
    gate: mpsc::Receiver<()>,
    unblock: mpsc::Sender<()>,
    live: Arc<AtomicUsize>,
) -> ReaderWait {
    let (done_tx, done) = mpsc::channel();
    live.fetch_add(1, Ordering::SeqCst);
    let count = LiveCount(Arc::clone(&live));
    let join = std::thread::spawn(move || {
        let _count = count;
        let _ = gate.recv();
        let _ = done_tx.send(());
    });
    ReaderWait {
        join,
        done,
        unblock: Some(unblock),
    }
}

#[cfg(test)]
fn shutdown_parked_for_test() {
    let parked = with_slot(|slot| slot.take());
    let Some(mut parked) = parked else {
        return;
    };
    if let Some(child) = parked.child.as_mut() {
        child.force_finish();
        let _ = child.signal_kill();
    }
    if let Some(reader) = parked.reader.as_mut() {
        if let Some(unblock) = reader.unblock.take() {
            let _ = unblock.send(());
        }
    }
    if let Some(child) = parked.child.as_mut() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let _ = await_exit(child, deadline);
    }
    drop(parked.child.take());
    if let Some(reader) = parked.reader.take() {
        let _ = reader.done.recv_timeout(Duration::from_secs(1));
        let _ = reader.join.join();
    }
}

#[cfg(test)]
struct TestGuard {
    _lock: MutexGuard<'static, ()>,
}

#[cfg(test)]
fn test_lock() -> TestGuard {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    let guard = TestGuard {
        _lock: TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner()),
    };
    shutdown_parked_for_test();
    guard
}

#[cfg(test)]
impl Drop for TestGuard {
    fn drop(&mut self) {
        shutdown_parked_for_test();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, ErrorKind, Read};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn wait_and_kill_errors_keep_their_meaning() {
        let exited = io::Error::from_raw_os_error(NOT_A_CHILD);
        assert!(matches!(classify_wait(Err(exited)), WaitClass::NotAChild));
        let denied = io::Error::new(ErrorKind::PermissionDenied, "denied");
        assert!(matches!(classify_wait(Err(denied)), WaitClass::Failed(_)));
        assert!(matches!(classify_wait(Ok(None)), WaitClass::Running));
        assert!(matches!(classify_kill(Ok(())), KillClass::Signaled));
        let gone = io::Error::from_raw_os_error(NO_SUCH_PROCESS);
        assert!(matches!(classify_kill(Err(gone)), KillClass::AlreadyGone));
        let kill_denied = io::Error::new(ErrorKind::PermissionDenied, "denied");
        assert!(matches!(
            classify_kill(Err(kill_denied)),
            KillClass::Failed(_)
        ));

        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn true");
        let status = child.wait().expect("wait true");
        assert!(status.success());
        assert!(matches!(
            classify_wait(child.try_wait()),
            WaitClass::Exited(_)
        ));
    }

    #[test]
    fn a_child_that_survives_kill_is_parked_and_blocks_replacement() {
        let _guard = test_lock();
        let running = Arc::new(AtomicBool::new(true));
        let readers = Arc::new(AtomicUsize::new(0));
        let (unblock, gate) = mpsc::channel();
        let mut child = Some(Simulated::survives(Arc::clone(&running)));
        let mut reader = Some(spawn_blocked_reader(
            gate,
            unblock.clone(),
            Arc::clone(&readers),
        ));
        let started = Instant::now();

        let error = release_simulated(&mut child, &mut reader, Duration::from_millis(40))
            .expect_err("surviving child");

        assert!(child.is_none(), "the parked child is owned by the slot");
        assert!(reader.is_none(), "the parked reader is owned by the slot");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "analysis worker is still live after the reap deadline"
        );
        assert!(!replacement_permitted());
        assert_eq!(parked_processes(), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 1);

        let (extra_unblock, extra_gate) = mpsc::channel();
        let mut extra_child = Some(Simulated::survives(Arc::new(AtomicBool::new(true))));
        let mut extra_reader = Some(spawn_blocked_reader(
            extra_gate,
            extra_unblock.clone(),
            Arc::clone(&readers),
        ));
        let refused = release_simulated(
            &mut extra_child,
            &mut extra_reader,
            Duration::from_millis(40),
        )
        .expect_err("second child");
        assert_eq!(refused.kind(), ErrorKind::TimedOut);
        assert!(
            extra_child.is_some(),
            "a refused child stays with the caller"
        );
        assert!(
            extra_reader.is_some(),
            "a refused reader stays with the caller"
        );
        assert_eq!(parked_processes(), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 2);

        let extra = extra_reader.take().expect("refused reader");
        let _ = extra_unblock.send(());
        let _ = extra.done.recv_timeout(Duration::from_secs(1));
        let _ = extra.join.join();
        assert_eq!(readers.load(Ordering::SeqCst), 1);
        assert!(!replacement_permitted());

        running.store(false, Ordering::SeqCst);
        let _ = unblock.send(());
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && !replacement_permitted() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(replacement_permitted());
        assert_eq!(parked_processes(), 0);
        assert_eq!(readers.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_parked_worker_outlives_the_thread_that_parked_it() {
        let _guard = test_lock();
        let running = Arc::new(AtomicBool::new(true));
        let readers = Arc::new(AtomicUsize::new(0));
        let (unblock, gate) = mpsc::channel();
        let parked = std::thread::spawn({
            let running = Arc::clone(&running);
            let readers = Arc::clone(&readers);
            let unblock = unblock.clone();
            move || {
                let mut child = Some(Simulated::survives(running));
                let mut reader = Some(spawn_blocked_reader(gate, unblock, readers));
                let error = release_simulated(&mut child, &mut reader, Duration::from_millis(30))
                    .expect_err("surviving child stays parked");
                assert_eq!(error.kind(), ErrorKind::TimedOut);
                assert!(child.is_none(), "the slot owns the child");
                assert!(reader.is_none(), "the slot owns the reader");
            }
        });
        parked.join().expect("parking thread returns");

        assert!(
            !replacement_permitted(),
            "the park remains occupied after the parking thread exits"
        );
        assert_eq!(parked_processes(), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 1);

        running.store(false, Ordering::SeqCst);
        unblock.send(()).expect("unblock the parked reader");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && !replacement_permitted() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(replacement_permitted());
        assert_eq!(parked_processes(), 0);
        assert_eq!(readers.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_kill_failure_is_not_treated_as_exit() {
        let _guard = test_lock();
        let readers = Arc::new(AtomicUsize::new(0));
        let (unblock, gate) = mpsc::channel();
        let mut child = Some(Simulated::kill_fails(Arc::new(AtomicBool::new(true))));
        let mut reader = Some(spawn_blocked_reader(gate, unblock, Arc::clone(&readers)));
        let started = Instant::now();

        let error = release_simulated(&mut child, &mut reader, Duration::from_millis(40))
            .expect_err("kill failure");

        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "analysis worker kill failed: kill failed"
        );
        assert!(!replacement_permitted());
        assert_eq!(parked_processes(), 1);
        assert_eq!(readers.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_wait_error_is_not_treated_as_exit() {
        let _guard = test_lock();
        let mut child = Some(Simulated::wait_fails());
        let mut reader = None;

        let error = release_simulated(&mut child, &mut reader, Duration::from_secs(2))
            .expect_err("wait failure");

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "wait failed");
        assert!(child.is_none());
        assert!(!replacement_permitted());
        assert_eq!(parked_processes(), 1);
    }

    #[test]
    fn not_a_child_releases_the_slot() {
        let _guard = test_lock();
        let readers = Arc::new(AtomicUsize::new(0));
        let (unblock, gate) = mpsc::channel();
        let mut reader = Some(spawn_blocked_reader(
            gate,
            unblock.clone(),
            Arc::clone(&readers),
        ));
        let _ = unblock.send(());
        let mut child = Some(Simulated::not_a_child());

        release_simulated(&mut child, &mut reader, Duration::from_secs(1))
            .expect("not a child is reaped");

        assert!(replacement_permitted());
        assert_eq!(parked_processes(), 0);
        assert_eq!(readers.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_finished_stdout_reader_is_joined() {
        let _guard = test_lock();
        let (unblock, gate) = mpsc::channel();
        let reader = spawn_frame_reader(move || {
            let mut input = GateRead { gate, done: false };
            let response = read_eof(&mut input);
            ((), response)
        });
        let _ = unblock.send(());
        let started = Instant::now();
        let ((), response) = reader.recv_timeout(Duration::from_secs(1)).expect("frame");
        reader.join();
        assert!(response.expect("eof").is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(replacement_permitted());
    }

    struct GateRead {
        gate: mpsc::Receiver<()>,
        done: bool,
    }

    impl Read for GateRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            if self.done {
                return Ok(0);
            }
            let _ = self.gate.recv();
            self.done = true;
            Ok(0)
        }
    }

    fn read_eof(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
        let mut buffer = [0; 1];
        match reader.read(&mut buffer)? {
            0 => Ok(None),
            _ => Ok(Some(buffer.to_vec())),
        }
    }
}
