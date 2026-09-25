//! Fetching one jar over HTTP within a bounded time. A test binary runs under the harness deadline
//! (120 seconds by default), so provisioning a missing jar must give up well before it: every attempt
//! is capped, retries stop at an overall deadline, and the whole process shares one download budget,
//! so several missing jars cannot add up to the suite timeout either.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one download may take, and how often it is retried.
#[derive(Clone, Copy, Debug)]
pub(super) struct RetryPolicy {
    pub(super) connect_timeout: Duration,
    pub(super) attempt_timeout: Duration,
    pub(super) retry_delay: Duration,
    pub(super) max_attempts: u32,
}

/// Maven Central refuses or drops a connection now and then; a missing jar silently leaves its
/// library off every test's classpath, so a failed attempt is retried a few times.
pub(super) const MAVEN_CENTRAL_RETRIES: RetryPolicy = RetryPolicy {
    connect_timeout: Duration::from_secs(5),
    attempt_timeout: Duration::from_secs(20),
    retry_delay: Duration::from_secs(2),
    max_attempts: 5,
};

/// The default time one process may spend downloading, well below the harness's 120-second
/// deadline. `KRUSTY_DEPS_DEADLINE_SECONDS` overrides it.
const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

/// What a download did: how many transfers it started and whether one completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DownloadOutcome {
    pub(super) attempts: u32,
    pub(super) completed: bool,
}

/// Download `url` into `dest`, retrying under `policy` until `deadline`. No attempt starts after the
/// deadline, and a running attempt is cut off at it.
pub(super) fn download(
    url: &str,
    dest: &Path,
    policy: RetryPolicy,
    deadline: Instant,
) -> DownloadOutcome {
    let mut attempts = 0;
    while attempts < policy.max_attempts {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        attempts += 1;
        let max_time = policy.attempt_timeout.min(remaining);
        let status = std::process::Command::new("curl")
            .args(["-sfL", "--connect-timeout"])
            .arg(seconds(policy.connect_timeout.min(max_time)))
            .arg("--max-time")
            .arg(seconds(max_time))
            .arg("-o")
            .arg(dest)
            .arg(url)
            .status();
        if status.is_ok_and(|status| status.success()) && dest.is_file() {
            return DownloadOutcome {
                attempts,
                completed: true,
            };
        }
        let _ = std::fs::remove_file(dest);
        if attempts == policy.max_attempts || Instant::now() + policy.retry_delay >= deadline {
            break;
        }
        std::thread::sleep(policy.retry_delay);
    }
    DownloadOutcome {
        attempts,
        completed: false,
    }
}

fn seconds(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64())
}

/// The time a process may still spend downloading. Every download charges its wall time, including
/// ones that succeed, so the total a process blocks on the network stays within the budget.
pub(super) struct DownloadBudget {
    remaining: Mutex<Duration>,
}

impl DownloadBudget {
    pub(super) const fn new(budget: Duration) -> Self {
        DownloadBudget {
            remaining: Mutex::new(budget),
        }
    }

    /// Run `fetch` with the deadline the remaining budget allows, then charge the time it took.
    /// Returns `None` without calling `fetch` once the budget is spent.
    pub(super) fn spend<T>(&self, fetch: impl FnOnce(Instant) -> T) -> Option<T> {
        let start = Instant::now();
        let remaining = *self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        if remaining.is_zero() {
            return None;
        }
        let result = fetch(start + remaining);
        let mut left = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        *left = left.saturating_sub(start.elapsed());
        Some(result)
    }
}

/// The process-wide download budget, read once from `KRUSTY_DEPS_DEADLINE_SECONDS`.
pub(super) fn process_budget() -> &'static DownloadBudget {
    static BUDGET: std::sync::OnceLock<DownloadBudget> = std::sync::OnceLock::new();
    BUDGET.get_or_init(|| {
        let budget = std::env::var("KRUSTY_DEPS_DEADLINE_SECONDS")
            .ok()
            .map(|seconds| {
                seconds.trim().parse::<u64>().unwrap_or_else(|_| {
                    panic!("KRUSTY_DEPS_DEADLINE_SECONDS must be whole seconds, got {seconds:?}")
                })
            })
            .map_or(DEFAULT_BUDGET, Duration::from_secs);
        DownloadBudget::new(budget)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A local HTTP endpoint that answers each request with the next of `responses` (the last one
    /// repeats), or holds the connection open without answering when that response is `None`.
    fn endpoint(responses: Vec<Option<&'static str>>) -> (String, Arc<AtomicU32>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local endpoint");
        let url = format!(
            "http://{}/dep.jar",
            listener.local_addr().expect("local address")
        );
        let requests = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let index = seen.fetch_add(1, Ordering::SeqCst) as usize;
                match responses[index.min(responses.len() - 1)] {
                    Some(response) => {
                        let _ = stream.write_all(response.as_bytes());
                    }
                    None => held.push(stream),
                }
            }
        });
        (url, requests)
    }

    const SERVER_ERROR: &str =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const JAR: &str = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\njar";

    fn policy(attempt_timeout: Duration) -> RetryPolicy {
        RetryPolicy {
            connect_timeout: Duration::from_secs(1),
            attempt_timeout,
            retry_delay: Duration::from_millis(100),
            max_attempts: 5,
        }
    }

    fn destination(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krusty-maven-download-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("download directory");
        dir.join("dep.jar")
    }

    #[test]
    fn failing_endpoint_is_tried_max_attempts_times_then_given_up() {
        let (url, requests) = endpoint(vec![Some(SERVER_ERROR)]);
        let dest = destination("failing");
        let deadline = Instant::now() + Duration::from_secs(30);

        let outcome = download(&url, &dest, policy(Duration::from_secs(5)), deadline);

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 5,
                completed: false
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 5);
        assert!(!dest.exists());
    }

    #[test]
    fn a_retry_after_a_failure_completes_the_download() {
        let (url, requests) = endpoint(vec![Some(SERVER_ERROR), Some(JAR)]);
        let dest = destination("recovering");
        let deadline = Instant::now() + Duration::from_secs(30);

        let outcome = download(&url, &dest, policy(Duration::from_secs(5)), deadline);

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 2,
                completed: true
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(&dest).expect("downloaded jar"), b"jar");
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn a_hanging_endpoint_is_cut_off_at_the_deadline_without_a_retry() {
        let (url, requests) = endpoint(vec![None]);
        let dest = destination("hanging");
        let start = Instant::now();
        let deadline = start + Duration::from_secs(2);

        let outcome = download(&url, &dest, policy(Duration::from_secs(30)), deadline);
        let elapsed = start.elapsed();

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 1,
                completed: false
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(
            (Duration::from_secs(2)..Duration::from_secs(3)).contains(&elapsed),
            "the attempt ran {elapsed:?} against a 2s deadline"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn a_spent_budget_refuses_further_downloads() {
        let budget = DownloadBudget::new(Duration::from_millis(200));
        let (url, requests) = endpoint(vec![None]);
        let dest = destination("budget");

        let first = budget
            .spend(|deadline| download(&url, &dest, policy(Duration::from_secs(30)), deadline));
        let second = budget.spend(|_| unreachable!("the budget is spent"));

        assert_eq!(
            first,
            Some(DownloadOutcome {
                attempts: 1,
                completed: false
            })
        );
        assert_eq!(second, None::<DownloadOutcome>);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
}
