//! Long-lived child processes of this test binary that compile corpus cases for a parent test.
//!
//! A compiler crash (a stack overflow aborts the whole process) or a case that never finishes would
//! otherwise end an in-process corpus run with no verdict for any case. Each worker is this binary
//! running one ignored worker test: it pays the compiler's startup once, then answers one request
//! line with one reply line until its stdin closes. The parent gives every request a deadline;
//! a worker that dies or misses it is killed, that case gets the matching [`Reply`], and the next
//! request on that thread starts a fresh worker.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

/// Marks a reply on the worker's stdout, which libtest shares with its own report lines.
const REPLY: &str = "@@krusty-case-reply\t";

/// How a parent addresses its workers: the exact ignored test that serves requests, and the
/// variable that tells that test it is a worker rather than an ordinary ignored run.
pub(super) struct Workers {
    pub(super) test: &'static str,
    pub(super) role: &'static str,
    pub(super) deadline: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reply {
    Line(String),
    /// The worker missed the deadline and was killed.
    TimedOut(Duration),
    /// The worker ended before replying, with this exit status.
    Died(String),
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<String>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

thread_local! {
    /// The worker this parent thread talks to; each thread of the parent's pool owns one.
    static WORKER: RefCell<Option<Worker>> = const { RefCell::new(None) };
}

impl Workers {
    /// Send `request` (one line) to this thread's worker and wait for its reply.
    pub(super) fn request(&self, request: &str) -> Reply {
        assert!(!request.contains('\n'), "a request is one line");
        WORKER.with(|slot| {
            let mut slot = slot.borrow_mut();
            let worker = match slot.as_mut() {
                Some(worker) => worker,
                None => slot.insert(self.spawn()),
            };
            let sent = writeln!(worker.stdin, "{request}").and_then(|()| worker.stdin.flush());
            let reply = match sent {
                Ok(()) => worker.replies.recv_timeout(self.deadline),
                Err(_) => Err(RecvTimeoutError::Disconnected),
            };
            match reply {
                Ok(line) => Reply::Line(line),
                Err(RecvTimeoutError::Timeout) => {
                    *slot = None;
                    Reply::TimedOut(self.deadline)
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let mut worker = slot.take().expect("the worker that was asked");
                    let status = worker
                        .child
                        .wait()
                        .map(|status| status.to_string())
                        .unwrap_or_else(|error| format!("unknown exit: {error}"));
                    Reply::Died(status)
                }
            }
        })
    }

    fn spawn(&self) -> Worker {
        let binary = std::env::current_exe().expect("the running test binary");
        let mut child = Command::new(&binary)
            .args(["--ignored", "--exact", self.test, "--nocapture"])
            .args(["--test-threads", "1"])
            .env(self.role, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|error| panic!("start a worker {}: {error}", binary.display()));
        let stdin = child.stdin.take().expect("a piped stdin");
        let stdout = child.stdout.take().expect("a piped stdout");
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                // libtest may print its own report on the same line before the first reply.
                if let Some((_, reply)) = line.split_once(REPLY) {
                    if sender.send(unescape(reply)).is_err() {
                        return;
                    }
                }
            }
        });
        Worker {
            child,
            stdin,
            replies,
        }
    }
}

/// Serve requests from stdin with `handle` when this process is a worker for `role`, and exit
/// when stdin closes; otherwise return at once, so the worker test is a no-op in an ordinary
/// ignored run. A worker exits rather than return to libtest, whose report would write to a parent
/// that has stopped reading.
pub(super) fn serve(role: &str, handle: impl Fn(&str) -> String) {
    if std::env::var_os(role).is_none() {
        return;
    }
    let stdout = std::io::stdout();
    for request in std::io::stdin().lock().lines() {
        let request = request.expect("read a request");
        let reply = escape(&handle(&request));
        let mut out = stdout.lock();
        writeln!(out, "{REPLY}{reply}")
            .and_then(|()| out.flush())
            .expect("send a reply");
    }
    std::process::exit(0);
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_ROLE: &str = "KRUSTY_CASE_WORKER_FIXTURE";

    /// A worker that echoes a request over two lines, never answers `hang`, and aborts on `die`.
    #[test]
    #[ignore = "a fixture worker process of the worker-pool tests"]
    fn fixture_worker() {
        serve(FIXTURE_ROLE, |request| match request {
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(3600));
            },
            "die" => std::process::abort(),
            other => format!("echo {other}\\\nline"),
        });
    }

    #[test]
    fn a_worker_answers_and_is_replaced_after_a_hang_or_a_crash() {
        let deadline = Duration::from_secs(2);
        let workers = Workers {
            test: "krusty_case_workers::tests::fixture_worker",
            role: FIXTURE_ROLE,
            deadline,
        };
        let echo = |request: &str| Reply::Line(format!("echo {request}\\\nline"));
        assert_eq!(workers.request("a"), echo("a"));
        assert_eq!(workers.request("b"), echo("b"));
        assert_eq!(workers.request("hang"), Reply::TimedOut(deadline));
        assert_eq!(workers.request("c"), echo("c"));
        assert!(
            matches!(workers.request("die"), Reply::Died(_)),
            "an aborted worker is reported as dead"
        );
        assert_eq!(workers.request("d"), echo("d"));
    }

    #[test]
    fn a_reply_with_newlines_and_backslashes_round_trips() {
        for text in ["", "a\nb", "C:\\path\\n", "trailing\\", "\\\\n\n"] {
            assert_eq!(unescape(&escape(text)), text);
            assert!(!escape(text).contains('\n'));
        }
    }
}
