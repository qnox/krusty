//! Stdout queue for the async language-server loop.
//!
//! The analysis engine publishes on the same channel that carries client input. A `write` to a
//! client that is not reading used to run on that loop, so a full pipe stalled both the engine and
//! the stdin reader. Frames are handed to a writer thread instead. The thread holds at most one
//! legal LSP frame of unsent bytes ([`MAX_MESSAGE_BYTES`] plus its `Content-Length` header). The
//! next frame that does not fit, a larger frame, or a writer error fails the transport. Accepted
//! frames are not discarded to make room.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::implementation::MAX_MESSAGE_BYTES;

/// Frames the writer thread can hold before `pending` stops being delivered.
///
/// A flush that finds this channel full leaves the frame in `pending` and returns success.
/// That frame is not written until a later flush can `try_send` it. Shutdown drops `pending`
/// when the channel is still full, so a burst is delivered in full only when it fits here.
pub(super) const OUTPUT_CHANNEL_FRAMES: usize = 32;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);

const fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

const fn content_length_header(body_len: usize) -> usize {
    "Content-Length: ".len() + decimal_digits(body_len) + "\r\n\r\n".len()
}

/// One framed LSP message at the read limit. A larger write is not a response this server sends.
const OUTPUT_STAGED_BYTES: usize = MAX_MESSAGE_BYTES + content_length_header(MAX_MESSAGE_BYTES);

/// The writer from a shutdown that timed out. At most one: a second spawn is refused until it
/// finishes, so a stuck client cannot accumulate writer threads.
static STUCK_WRITER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

pub(super) struct OutputQueue {
    current: Vec<u8>,
    pending: Vec<u8>,
    held: Arc<AtomicUsize>,
    staged_limit: usize,
    tx: Option<SyncSender<Vec<u8>>>,
    done: Option<Receiver<()>>,
    join: Option<JoinHandle<()>>,
    writer_error: Arc<Mutex<Option<String>>>,
    failed: Option<String>,
    drain_grace: Duration,
}

impl OutputQueue {
    pub(super) fn spawn<W>(inner: W) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        Self::spawn_limited(
            inner,
            OUTPUT_CHANNEL_FRAMES,
            OUTPUT_STAGED_BYTES,
            OUTPUT_DRAIN_GRACE,
        )
    }

    fn spawn_limited<W>(
        inner: W,
        channel_frames: usize,
        staged_limit: usize,
        drain_grace: Duration,
    ) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        Self::spawn_limited_held(inner, channel_frames, staged_limit, drain_grace, None)
    }

    /// `hold` blocks the writer before it receives. The flag is set once the thread is waiting.
    /// Tests fill the channel while no frame has been taken. Production passes `None`.
    fn spawn_limited_held<W>(
        inner: W,
        channel_frames: usize,
        staged_limit: usize,
        drain_grace: Duration,
        hold: Option<Arc<(Mutex<bool>, Condvar, AtomicBool)>>,
    ) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        // Other modules' tests spawn queues in parallel. Refusing them while this module parks
        // a writer would flake. Production still refuses a second writer; the check is tested
        // through `reclaim_finished_writer`.
        if cfg!(test) {
            let _ = reclaim_finished_writer();
        } else {
            reclaim_finished_writer()?;
        }
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(channel_frames.max(1));
        let (done_tx, done_rx) = mpsc::channel();
        let held = Arc::new(AtomicUsize::new(0));
        let writer_error = Arc::new(Mutex::new(None));
        let held_for_writer = Arc::clone(&held);
        let error_for_writer = Arc::clone(&writer_error);
        let join = std::thread::spawn(move || {
            if let Some(hold) = hold {
                let (lock, cv, waiting) = &*hold;
                let mut open = lock.lock().expect("writer hold");
                waiting.store(true, Ordering::SeqCst);
                while !*open {
                    open = cv.wait(open).expect("writer hold");
                }
            }
            let mut inner = inner;
            while let Ok(frame) = rx.recv() {
                let write = inner.write_all(&frame).and_then(|()| inner.flush());
                if let Err(error) = write {
                    *error_for_writer.lock().expect("writer error") = Some(error.to_string());
                    break;
                }
                held_for_writer.fetch_sub(frame.len(), Ordering::SeqCst);
            }
            let _ = done_tx.send(());
        });
        Ok(Self {
            current: Vec::new(),
            pending: Vec::new(),
            held,
            staged_limit,
            tx: Some(tx),
            done: Some(done_rx),
            join: Some(join),
            writer_error,
            failed: None,
            drain_grace,
        })
    }

    fn poll_writer_failure(&mut self) -> io::Result<()> {
        if let Some(message) = &self.failed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, message.clone()));
        }
        if let Some(message) = self.writer_error.lock().expect("writer error").clone() {
            self.failed = Some(message.clone());
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, message));
        }
        Ok(())
    }

    fn accept(&mut self, frame: Vec<u8>) -> io::Result<()> {
        self.poll_writer_failure()?;
        if !self.pending.is_empty() {
            let message = "stdout queue exceeded the transport budget";
            self.failed = Some(message.to_string());
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, message));
        }
        let len = frame.len();
        if len > self.staged_limit {
            let message = "stdout frame exceeds the transport budget";
            self.failed = Some(message.to_string());
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, message));
        }
        let mut held = self.held.load(Ordering::SeqCst);
        loop {
            if held.saturating_add(len) > self.staged_limit {
                let message = "stdout queue exceeded the transport budget";
                self.failed = Some(message.to_string());
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, message));
            }
            match self.held.compare_exchange_weak(
                held,
                held + len,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => held = observed,
            }
        }
        self.pending = frame;
        Ok(())
    }

    fn pump(&mut self) -> io::Result<()> {
        let Some(tx) = self.tx.as_ref() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"));
        };
        if self.pending.is_empty() {
            return Ok(());
        }
        let frame = std::mem::take(&mut self.pending);
        let len = frame.len();
        match tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(frame)) => {
                self.pending = frame;
                Ok(())
            }
            Err(TrySendError::Disconnected(frame)) => {
                self.held.fetch_sub(len, Ordering::SeqCst);
                self.pending = frame;
                let message = self
                    .writer_error
                    .lock()
                    .expect("writer error")
                    .clone()
                    .unwrap_or_else(|| "stdout closed".to_string());
                self.failed = Some(message.clone());
                Err(io::Error::new(io::ErrorKind::BrokenPipe, message))
            }
        }
    }

    pub(super) fn finish(&mut self, outcome: io::Result<i32>) -> io::Result<i32> {
        let delivery = self.shutdown();
        match outcome {
            Err(error) => Err(error),
            Ok(code) => delivery.map(|()| code),
        }
    }

    fn shutdown(&mut self) -> io::Result<()> {
        let flushed = if self.tx.is_some() {
            self.flush()
        } else {
            Ok(())
        };
        self.tx.take();
        let Some(join) = self.join.take() else {
            return flushed;
        };
        let done = self.done.take();
        match done.and_then(|done| match done.recv_timeout(self.drain_grace) {
            Ok(()) => Some(()),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(()),
        }) {
            Some(()) => {
                let _ = join.join();
                self.poll_writer_failure()?;
                flushed
            }
            None => {
                park_stuck_writer(join);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "stdout writer did not finish",
                ))
            }
        }
    }
}

fn reclaim_finished_writer() -> io::Result<()> {
    let mut slot = STUCK_WRITER.lock().expect("stuck writer");
    let Some(handle) = slot.take() else {
        return Ok(());
    };
    if handle.is_finished() {
        let _ = handle.join();
        Ok(())
    } else {
        *slot = Some(handle);
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "stdout writer is still blocked",
        ))
    }
}

fn park_stuck_writer(handle: JoinHandle<()>) {
    let mut slot = STUCK_WRITER.lock().expect("stuck writer");
    if let Some(previous) = slot.take() {
        if previous.is_finished() {
            let _ = previous.join();
        } else {
            *slot = Some(previous);
            // `spawn` refuses a second writer while this one is parked, so a live previous
            // handle is not expected. Joining it here would wait forever; keep it and join the
            // new handle only after the owner is free by blocking the caller instead of detaching.
            drop(slot);
            let _ = handle.join();
            return;
        }
    }
    *slot = Some(handle);
}

#[cfg(test)]
pub(super) fn stuck_writer_parked() -> bool {
    STUCK_WRITER
        .lock()
        .expect("stuck writer")
        .as_ref()
        .is_some_and(|handle| !handle.is_finished())
}

#[cfg(test)]
pub(super) fn reclaim_stuck_writer() {
    if let Some(handle) = STUCK_WRITER.lock().expect("stuck writer").take() {
        let _ = handle.join();
    }
}

impl Write for OutputQueue {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.poll_writer_failure()?;
        self.current.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.poll_writer_failure()?;
        self.pump()?;
        let accepted = if self.current.is_empty() {
            Ok(())
        } else {
            let frame = std::mem::take(&mut self.current);
            self.accept(frame)
        };
        let pumped = self.pump();
        self.poll_writer_failure()?;
        accepted?;
        pumped
    }
}

impl Drop for OutputQueue {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.shutdown();
        }
    }
}

#[cfg(test)]
pub(super) struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl SharedWriter {
    pub(super) fn recording() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        (Self(Arc::clone(&bytes)), bytes)
    }
}

#[cfg(test)]
impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("writer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    struct GatedWriter {
        started: Arc<Mutex<usize>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        captured: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for GatedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            {
                let mut started = self.started.lock().expect("started lock");
                *started += 1;
            }
            let (lock, cv) = &*self.gate;
            let mut open = lock.lock().expect("gate lock");
            while !*open {
                open = cv.wait(open).expect("gate wait");
            }
            self.captured
                .lock()
                .expect("captured")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "client gone"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wait_until(pred: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !pred() {
            assert!(Instant::now() < deadline, "writer thread did not start");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn open_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, cv) = &**gate;
        *lock.lock().expect("open gate") = true;
        cv.notify_all();
    }

    #[test]
    fn frames_reach_a_reading_client_in_order() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn(SharedWriter(Arc::clone(&bytes))).unwrap();
        queue.write_all(b"one").unwrap();
        queue.flush().unwrap();
        queue.write_all(b"two").unwrap();
        queue.flush().unwrap();
        queue.shutdown().unwrap();

        assert_eq!(&bytes.lock().expect("bytes")[..], b"onetwo");
    }

    #[test]
    fn a_maximum_message_frame_is_delivered() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let body = vec![b'z'; MAX_MESSAGE_BYTES];
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        assert_eq!(frame.len(), OUTPUT_STAGED_BYTES);
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn(SharedWriter(Arc::clone(&bytes))).unwrap();
        queue.write_all(&frame).unwrap();
        queue.flush().unwrap();
        queue.shutdown().unwrap();
        assert_eq!(&bytes.lock().expect("bytes")[..], frame);
    }

    #[test]
    fn a_frame_over_the_budget_fails_and_keeps_the_earlier_frame() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn_limited(
            SharedWriter(Arc::clone(&bytes)),
            1,
            8,
            Duration::from_millis(50),
        )
        .unwrap();
        queue.write_all(b"kept").unwrap();
        queue.flush().unwrap();
        queue.write_all(b"too-large").unwrap();
        let error = queue.flush().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        let again = queue.flush().unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::BrokenPipe);
        drop(queue);
        assert_eq!(&bytes.lock().expect("bytes")[..], b"kept");
    }

    #[test]
    fn a_blocked_stdout_stops_at_the_budget_and_keeps_order() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let started = Arc::new(Mutex::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn_limited(
            GatedWriter {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
                captured: Arc::clone(&captured),
            },
            4,
            10,
            Duration::from_millis(50),
        )
        .unwrap();
        queue.write_all(b"12345").unwrap();
        queue.flush().unwrap();
        wait_until(|| *started.lock().expect("started") >= 1);

        let began = Instant::now();
        queue.write_all(b"abcde").unwrap();
        queue.flush().unwrap();
        let overflow = queue.write_all(b"Z").and_then(|_| queue.flush());
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(overflow.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert!(queue.flush().is_err());

        open_gate(&gate);
        let _ = queue.shutdown();
        assert_eq!(&captured.lock().expect("captured")[..], b"12345abcde");
    }

    #[test]
    fn a_writer_error_fails_later_flushes() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let mut queue =
            OutputQueue::spawn_limited(FailingWriter, 1, 64, Duration::from_millis(50)).unwrap();
        queue.write_all(b"x").unwrap();
        let first = queue.flush();
        wait_until(|| queue.writer_error.lock().expect("writer error").is_some());
        let error = match first {
            Err(error) => error,
            Ok(()) => queue.flush().unwrap_err(),
        };
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(error.to_string().contains("client gone"));
        queue.write_all(b"y").unwrap_err();
        let _ = queue.shutdown();
    }

    #[test]
    fn a_blocked_shutdown_parks_one_writer_and_refuses_another() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let started = Arc::new(Mutex::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn_limited(
            GatedWriter {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
                captured: Arc::clone(&captured),
            },
            1,
            32,
            Duration::from_millis(50),
        )
        .unwrap();
        queue.write_all(b"block").unwrap();
        queue.flush().unwrap();
        wait_until(|| *started.lock().expect("started") >= 1);

        let began = Instant::now();
        let error = queue.shutdown().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_millis(500));
        assert!(stuck_writer_parked());
        let refused = reclaim_finished_writer();
        assert_eq!(
            refused.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe,
            "a second writer must not start while the first is still blocked"
        );

        open_gate(&gate);
        reclaim_stuck_writer();
        assert!(!stuck_writer_parked());
    }

    #[test]
    fn async_loop_returns_while_stdout_stays_blocked() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        use crate::server::engine::{AnalysisEngine, EngineBackend};
        use crate::server::implementation::{run_async_loop, Analysis, Incoming, LspService};
        use crate::{DocumentAnalysis, IndexOutcome};

        struct Mock;
        impl Analysis for Mock {
            fn index_workspace_files(&mut self, _uris: &[&str]) -> IndexOutcome {
                IndexOutcome::default()
            }
            fn analyze(&mut self, sources: &[&str]) -> Vec<DocumentAnalysis> {
                sources.iter().map(|_| DocumentAnalysis::empty()).collect()
            }
        }

        let started = Arc::new(Mutex::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (sender, incoming) = mpsc::sync_channel(4);
        let engine = AnalysisEngine::spawn(Mock, sender.clone());
        let service = LspService::with_backend(EngineBackend::new(engine, false));
        sender.send(Incoming::ParseError).unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let gate_for_loop = Arc::clone(&gate);
        let started_for_loop = Arc::clone(&started);
        std::thread::spawn(move || {
            let code = run_async_loop(
                service,
                GatedWriter {
                    started: started_for_loop,
                    gate: gate_for_loop,
                    captured: Arc::new(Mutex::new(Vec::new())),
                },
                incoming,
            );
            let _ = done_tx.send(code);
        });
        wait_until(|| *started.lock().expect("started") >= 1);
        sender.send(Incoming::Eof).unwrap();

        let began = Instant::now();
        let code = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a blocked stdout write stalled the input loop");
        let error = code.expect_err("undelivered stdout fails the connection");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(5));

        open_gate(&gate);
        reclaim_stuck_writer();
    }

    #[test]
    fn a_full_channel_is_delivered_and_the_pending_frame_is_not() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let hold = Arc::new((Mutex::new(false), Condvar::new(), AtomicBool::new(false)));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn_limited_held(
            SharedWriter(Arc::clone(&captured)),
            2,
            64,
            Duration::from_millis(50),
            Some(Arc::clone(&hold)),
        )
        .unwrap();
        wait_until(|| hold.2.load(Ordering::SeqCst));
        queue.write_all(b"A").unwrap();
        queue.flush().unwrap();
        queue.write_all(b"B").unwrap();
        queue.flush().unwrap();
        queue.write_all(b"C").unwrap();
        queue.flush().unwrap();

        let error = queue.shutdown().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        {
            let (lock, cv, _) = &*hold;
            *lock.lock().expect("release hold") = true;
            cv.notify_all();
        }
        reclaim_stuck_writer();
        assert_eq!(&captured.lock().expect("captured")[..], b"AB");
    }
}
