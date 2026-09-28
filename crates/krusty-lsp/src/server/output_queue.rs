//! Bounded stdout queue for the async language-server loop.
//!
//! The analysis engine publishes on the same channel that carries client input. A `write` to a
//! client that is not reading used to run on that loop, so a full channel stalled both the engine
//! and the stdin reader. Frames are handed to a writer thread instead. When the client stops
//! reading, extra frames are dropped once the staged budget is full, and the loop keeps running.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::Duration;

const OUTPUT_CHANNEL_FRAMES: usize = 32;
const OUTPUT_STAGED_BYTES: usize = 8 * 1024 * 1024;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(2);

pub(super) struct OutputQueue {
    current: Vec<u8>,
    staged: VecDeque<Vec<u8>>,
    staged_bytes: usize,
    staged_limit: usize,
    tx: Option<SyncSender<Vec<u8>>>,
    join: Option<JoinHandle<()>>,
}

impl OutputQueue {
    pub(super) fn spawn<W>(inner: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self::spawn_limited(inner, OUTPUT_CHANNEL_FRAMES, OUTPUT_STAGED_BYTES)
    }

    fn spawn_limited<W>(inner: W, channel_frames: usize, staged_limit: usize) -> Self
    where
        W: Write + Send + 'static,
    {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(channel_frames.max(1));
        let join = std::thread::spawn(move || {
            let mut inner = inner;
            while let Ok(frame) = rx.recv() {
                if inner.write_all(&frame).is_err() || inner.flush().is_err() {
                    break;
                }
            }
        });
        Self {
            current: Vec::new(),
            staged: VecDeque::new(),
            staged_bytes: 0,
            staged_limit,
            tx: Some(tx),
            join: Some(join),
        }
    }

    fn stage(&mut self, frame: Vec<u8>) {
        if frame.len() > self.staged_limit {
            return;
        }
        while self.staged_bytes.saturating_add(frame.len()) > self.staged_limit {
            let Some(old) = self.staged.pop_front() else {
                break;
            };
            self.staged_bytes = self.staged_bytes.saturating_sub(old.len());
        }
        self.staged_bytes = self.staged_bytes.saturating_add(frame.len());
        self.staged.push_back(frame);
    }

    fn pump(&mut self) -> io::Result<()> {
        let Some(tx) = self.tx.as_ref() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"));
        };
        while let Some(frame) = self.staged.pop_front() {
            let len = frame.len();
            match tx.try_send(frame) {
                Ok(()) => self.staged_bytes = self.staged_bytes.saturating_sub(len),
                Err(TrySendError::Full(frame)) => {
                    self.staged.push_front(frame);
                    break;
                }
                Err(TrySendError::Disconnected(frame)) => {
                    self.staged.push_front(frame);
                    return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"));
                }
            }
        }
        Ok(())
    }
}

impl Write for OutputQueue {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.current.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.current.is_empty() {
            let frame = std::mem::take(&mut self.current);
            self.stage(frame);
        }
        self.pump()
    }
}

impl Drop for OutputQueue {
    fn drop(&mut self) {
        if !self.current.is_empty() {
            let frame = std::mem::take(&mut self.current);
            self.stage(frame);
        }
        let _ = self.pump();
        // Closing the sender lets a writer that is between frames exit. One blocked in the client
        // write cannot be joined; waiting past the grace would stall shutdown the same way the
        // direct write did.
        self.tx.take();
        if let Some(join) = self.join.take() {
            let (done_tx, done_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = join.join();
                let _ = done_tx.send(());
            });
            let _ = done_rx.recv_timeout(OUTPUT_DRAIN_GRACE);
        }
    }
}

#[cfg(test)]
pub(super) struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl SharedWriter {
    pub(super) fn from_arc(bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> Self {
        Self(bytes)
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

    struct GatedWriter {
        started: Arc<Mutex<usize>>,
        gate: Arc<(Mutex<bool>, Condvar)>,
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
            Ok(buf.len())
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

    #[test]
    fn frames_reach_a_reading_client_in_order() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut queue = OutputQueue::spawn(SharedWriter(Arc::clone(&bytes)));
        queue.write_all(b"one").unwrap();
        queue.flush().unwrap();
        queue.write_all(b"two").unwrap();
        queue.flush().unwrap();
        drop(queue);

        assert_eq!(&bytes.lock().expect("bytes")[..], b"onetwo");
    }

    #[test]
    fn writes_return_while_stdout_is_blocked() {
        let started = Arc::new(Mutex::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let mut queue = OutputQueue::spawn_limited(
            GatedWriter {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
            },
            1,
            64,
        );
        queue.write_all(b"first").unwrap();
        queue.flush().unwrap();
        wait_until(|| *started.lock().expect("started") >= 1);

        let began = Instant::now();
        for _ in 0..8 {
            queue.write_all(&[b'x'; 16]).unwrap();
            queue.flush().unwrap();
        }
        assert!(began.elapsed() < Duration::from_millis(500));
        assert!(queue.staged_bytes <= 64);

        let (lock, cv) = &*gate;
        *lock.lock().expect("open gate") = true;
        cv.notify_all();
    }

    #[test]
    fn an_oversized_frame_is_dropped_instead_of_blocking() {
        let started = Arc::new(Mutex::new(0));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let mut queue = OutputQueue::spawn_limited(
            GatedWriter {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
            },
            1,
            32,
        );
        queue.write_all(b"first").unwrap();
        queue.flush().unwrap();
        wait_until(|| *started.lock().expect("started") >= 1);

        let began = Instant::now();
        queue.write_all(&[b'y'; 1_000]).unwrap();
        queue.flush().unwrap();
        assert!(began.elapsed() < Duration::from_millis(500));
        assert_eq!(queue.staged_bytes, 0);

        let (lock, cv) = &*gate;
        *lock.lock().expect("open gate") = true;
        cv.notify_all();
    }

    #[test]
    fn async_loop_reads_eof_while_stdout_is_blocked() {
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
        assert_eq!(code.unwrap(), 0);
        assert!(began.elapsed() < Duration::from_secs(5));

        let (lock, cv) = &*gate;
        *lock.lock().expect("open gate") = true;
        cv.notify_all();
    }
}
