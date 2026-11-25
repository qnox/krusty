//! Fetching one artifact over HTTP within a bounded time. A test binary runs under the harness deadline
//! (120 seconds by default), so provisioning a missing jar must give up well before it: every attempt
//! is capped, retries stop at an overall deadline, and the whole process shares one download budget,
//! so several missing jars cannot add up to the suite timeout either.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long one download may take, and how often it is retried.
#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub connect_timeout: Duration,
    pub attempt_timeout: Duration,
    pub retry_delay: Duration,
    pub max_attempts: u32,
    /// The least time between the starts of two requests from this process. Maven Central answers
    /// back-to-back artifact requests with `429` and extends the block for every request made
    /// during it, so a download never starts within this gap of another.
    pub request_gap: Duration,
}

/// Maven Central refuses or drops a connection now and then; a missing jar silently leaves its
/// library off every test's classpath, so a failed attempt is retried a few times.
pub const MAVEN_CENTRAL_RETRIES: RetryPolicy = RetryPolicy {
    connect_timeout: Duration::from_secs(5),
    attempt_timeout: Duration::from_secs(20),
    retry_delay: Duration::from_secs(2),
    max_attempts: 5,
    request_gap: Duration::from_secs(3),
};

/// The default time one process may spend downloading, well below the harness's 120-second
/// deadline. `KRUSTY_DEPS_DEADLINE_SECONDS` overrides it.
const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

/// What a download did: how many transfers it started and whether one completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub attempts: u32,
    pub completed: bool,
}

/// Download `url` into `dest`, retrying under `policy` until `deadline`. No attempt starts after the
/// deadline, and a running attempt is cut off at it. When no attempt completed, the second value is
/// why the last one failed: curl's own message, the status, server address and final URL of the
/// refusal, or the deadline that stopped the retries.
pub fn download(
    url: &str,
    dest: &Path,
    policy: RetryPolicy,
    deadline: Instant,
) -> (DownloadOutcome, Option<String>) {
    let mut attempts = 0;
    let mut failure = None;
    let mut retry_after = Duration::ZERO;
    while attempts < policy.max_attempts {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            failure.get_or_insert_with(|| "the download deadline passed".to_string());
            break;
        }
        REQUESTS.pace(policy.request_gap, deadline);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            failure.get_or_insert_with(|| "the download deadline passed".to_string());
            break;
        }
        attempts += 1;
        let max_time = policy.attempt_timeout.min(remaining);
        let output = std::process::Command::new("curl")
            .args(["-sSfL", "--connect-timeout"])
            .arg(seconds(policy.connect_timeout.min(max_time)))
            .arg("--max-time")
            .arg(seconds(max_time))
            .args([
                "-w",
                "%{http_code} %{remote_ip} %{url_effective}\n%header{retry-after}",
            ])
            .arg("-o")
            .arg(dest)
            .arg(url)
            .output();
        let mut refused_for_good = false;
        match output {
            Ok(output) if output.status.success() && dest.is_file() => {
                return (
                    DownloadOutcome {
                        attempts,
                        completed: true,
                    },
                    None,
                );
            }
            Ok(output) => {
                retry_after = retry_after_of(&output.stdout, SystemTime::now());
                refused_for_good = is_final_refusal(&output.stdout);
                let message = String::from_utf8_lossy(&output.stderr);
                let message = message.trim();
                let message = if message.is_empty() {
                    format!("curl exited with {}", output.status)
                } else {
                    message.to_string()
                };
                failure = Some(match answered_by(&output.stdout) {
                    Some(answer) => format!("{message} ({answer})"),
                    None => message,
                });
            }
            Err(error) => failure = Some(format!("curl could not start: {error}")),
        }
        let _ = std::fs::remove_file(dest);
        if refused_for_good {
            break;
        }
        // A server that says how long to wait is obeyed, within the deadline: retrying sooner only
        // extends the block.
        let delay = policy.retry_delay.max(retry_after);
        if attempts == policy.max_attempts || Instant::now() + delay >= deadline {
            break;
        }
        std::thread::sleep(delay);
    }
    (
        DownloadOutcome {
            attempts,
            completed: false,
        },
        failure,
    )
}

/// Spaces request starts at least a gap apart; callers take turns.
struct Pacer {
    last: Mutex<Option<Instant>>,
}

impl Pacer {
    const fn new() -> Self {
        Pacer {
            last: Mutex::new(None),
        }
    }

    /// How long a request starting at `now` must wait; it counts as started once it has waited.
    fn delay(&self, now: Instant, gap: Duration) -> Duration {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let wait = last
            .map(|last| (last + gap).saturating_duration_since(now))
            .unwrap_or_default();
        *last = Some(now + wait);
        wait
    }

    /// Waits for the gap, but never past `deadline`.
    fn pace(&self, gap: Duration, deadline: Instant) {
        let now = Instant::now();
        let wait = self
            .delay(now, gap)
            .min(deadline.saturating_duration_since(now));
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// Every download in the process shares one pacer.
static REQUESTS: Pacer = Pacer::new();

/// Who refused: the status, server address and final URL curl reports through `-w` on its first
/// line (the address and URL after redirects are what tell a mirror or proxy from Central itself).
/// `None` when the request never reached a server, where curl prints a zero status.
fn answered_by(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let mut fields = text.lines().next()?.split(' ');
    let (status, address, url) = (fields.next()?, fields.next()?, fields.next()?);
    (status != "000").then(|| format!("HTTP {status} from {address} for {url}"))
}

/// Whether the server answered with a client error that asking again cannot change (a 404 or a 403
/// names a file or a right the server will not give). Timeouts, 425, 429 and every 5xx are
/// transient and stay retryable, as does a request that never reached a server.
fn is_final_refusal(stdout: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stdout);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split(' ').next())
        .and_then(|status| status.parse::<u16>().ok());
    matches!(status, Some(400..=499) if !matches!(status, Some(408 | 425 | 429)))
}

/// How long the `Retry-After` header on curl's second `-w` line asks to wait: its whole seconds, or
/// the time until its HTTP-date. Zero when the server sent none or something unreadable. The value
/// comes last because an HTTP-date holds spaces.
fn retry_after_of(stdout: &[u8], now: SystemTime) -> Duration {
    let text = String::from_utf8_lossy(stdout);
    let value = text.lines().nth(1).unwrap_or("").trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    http_date(value).map_or(Duration::ZERO, |at| {
        at.duration_since(now).unwrap_or(Duration::ZERO)
    })
}

/// The instant of an IMF-fixdate (`Wed, 21 Oct 2015 07:28:00 GMT`), the form HTTP servers send.
fn http_date(value: &str) -> Option<SystemTime> {
    let mut parts = value.split_whitespace();
    let (_weekday, day, month, year, time, zone) = (
        parts.next()?,
        parts.next()?.parse::<i64>().ok()?,
        parts.next()?,
        parts.next()?.parse::<i64>().ok()?,
        parts.next()?,
        parts.next()?,
    );
    if zone != "GMT" || parts.next().is_some() {
        return None;
    }
    let month = 1 + [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| *name == month)? as i64;
    let mut clock = time.split(':').map(|field| field.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if clock.next().is_some() || !(1..=31).contains(&day) {
        return None;
    }
    // Days since the epoch of a proleptic Gregorian date (Howard Hinnant's civil-days algorithm).
    let shifted_year = year - i64::from(month <= 2);
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

fn seconds(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64())
}

/// The time a process may still spend downloading. Every download charges its wall time, including
/// ones that succeed, so the total a process blocks on the network stays within the budget.
pub struct DownloadBudget {
    remaining: Mutex<Duration>,
}

impl DownloadBudget {
    pub const fn new(budget: Duration) -> Self {
        DownloadBudget {
            remaining: Mutex::new(budget),
        }
    }

    /// Run `fetch` with the deadline the remaining budget allows, then charge the time it took.
    /// Returns `None` without calling `fetch` once the budget is spent. The budget stays locked for
    /// the whole fetch, so concurrent callers take turns and each sees what the previous one left.
    pub fn spend<T>(&self, fetch: impl FnOnce(Instant) -> T) -> Option<T> {
        let mut remaining = self.remaining.lock().unwrap_or_else(|e| e.into_inner());
        if remaining.is_zero() {
            return None;
        }
        let start = Instant::now();
        let result = fetch(start + *remaining);
        *remaining = remaining.saturating_sub(start.elapsed());
        Some(result)
    }
}

/// The process-wide download budget, read once from `KRUSTY_DEPS_DEADLINE_SECONDS`.
pub fn process_budget() -> &'static DownloadBudget {
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

    /// The outcome of [`super::download`], without the failure reason.
    fn download(url: &str, dest: &Path, policy: RetryPolicy, deadline: Instant) -> DownloadOutcome {
        super::download(url, dest, policy, deadline).0
    }

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

    const NOT_FOUND: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const TOO_MANY: &str =
        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const SERVER_ERROR: &str =
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const JAR: &str = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\njar";

    fn policy(attempt_timeout: Duration) -> RetryPolicy {
        RetryPolicy {
            connect_timeout: Duration::from_secs(1),
            attempt_timeout,
            retry_delay: Duration::from_millis(100),
            max_attempts: 5,
            request_gap: Duration::ZERO,
        }
    }

    /// curl prints `<status> <address> <url>`, a newline, then the `Retry-After` value, which may
    /// hold spaces (an HTTP-date) and so comes last.
    #[test]
    fn a_retry_after_header_is_read_as_seconds() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(
            retry_after_of(b"429 1.2.3.4 https://repo/a.jar\n45", now),
            Duration::from_secs(45)
        );
        assert_eq!(
            retry_after_of(b"429 1.2.3.4 https://repo/a.jar\n", now),
            Duration::ZERO
        );
        assert_eq!(
            retry_after_of(b"429 1.2.3.4 https://repo/a.jar\nsoon", now),
            Duration::ZERO
        );
    }

    #[test]
    fn a_retry_after_date_is_the_wait_until_that_instant() {
        // 2015-10-21T07:28:00Z is 1445412480 seconds after the epoch.
        let before = UNIX_EPOCH + Duration::from_secs(1_445_412_480 - 90);
        let after = UNIX_EPOCH + Duration::from_secs(1_445_412_480 + 5);
        let stdout = b"503 1.2.3.4 https://repo/a.jar\nWed, 21 Oct 2015 07:28:00 GMT";
        assert_eq!(retry_after_of(stdout, before), Duration::from_secs(90));
        assert_eq!(retry_after_of(stdout, after), Duration::ZERO);
    }

    #[test]
    fn a_refusal_names_the_url_whatever_the_retry_after_looks_like() {
        let stdout = b"503 1.2.3.4 https://repo/a.jar\nWed, 21 Oct 2015 07:28:00 GMT";
        assert_eq!(
            answered_by(stdout).as_deref(),
            Some("HTTP 503 from 1.2.3.4 for https://repo/a.jar")
        );
        assert_eq!(answered_by(b"000 0.0.0.0 https://repo/a.jar\n"), None);
    }

    #[test]
    fn requests_start_a_gap_apart() {
        let pacer = Pacer::new();
        let gap = Duration::from_secs(3);
        let start = Instant::now();
        assert_eq!(pacer.delay(start, gap), Duration::ZERO);
        assert_eq!(pacer.delay(start, gap), gap);
        assert_eq!(pacer.delay(start, gap), gap * 2);
        assert_eq!(pacer.delay(start + gap * 10, gap), Duration::ZERO);
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
    fn a_refusal_is_reported_with_the_status_the_server_gave() {
        let (url, _requests) = endpoint(vec![Some(SERVER_ERROR)]);
        let dest = destination("explained");
        let deadline = Instant::now() + Duration::from_secs(30);

        let (outcome, failure) =
            super::download(&url, &dest, policy(Duration::from_secs(5)), deadline);

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 5,
                completed: false
            }
        );
        assert_eq!(
            failure,
            Some(format!(
                "curl: (22) The requested URL returned error: 500 \
                 (HTTP 500 from 127.0.0.1 for {url})"
            ))
        );
    }

    #[test]
    fn a_missing_file_is_not_asked_for_again() {
        let (url, requests) = endpoint(vec![Some(NOT_FOUND)]);
        let dest = destination("missing");
        let deadline = Instant::now() + Duration::from_secs(30);

        let (outcome, failure) =
            super::download(&url, &dest, policy(Duration::from_secs(5)), deadline);

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 1,
                completed: false
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            failure,
            Some(format!(
                "curl: (22) The requested URL returned error: 404 \
                 (HTTP 404 from 127.0.0.1 for {url})"
            ))
        );
    }

    #[test]
    fn a_throttled_request_is_asked_for_again() {
        let (url, requests) = endpoint(vec![Some(TOO_MANY), Some(JAR)]);
        let dest = destination("throttled");
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
    fn a_server_that_asks_for_a_wait_is_waited_for() {
        const THROTTLED: &str = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\n\
                                 Content-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, requests) = endpoint(vec![Some(THROTTLED), Some(JAR)]);
        let dest = destination("throttled");
        let start = Instant::now();

        let outcome = download(
            &url,
            &dest,
            policy(Duration::from_secs(5)),
            start + Duration::from_secs(30),
        );

        assert_eq!(
            outcome,
            DownloadOutcome {
                attempts: 2,
                completed: true
            }
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert!(
            start.elapsed() >= Duration::from_secs(1),
            "the retry came after {:?}, before the 1s the server asked for",
            start.elapsed()
        );
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

    #[test]
    fn concurrent_callers_share_one_budget() {
        let budget = DownloadBudget::new(Duration::from_millis(300));
        let barrier = std::sync::Barrier::new(2);
        let fetches = AtomicU32::new(0);
        let start = Instant::now();

        let outcomes: Vec<Option<()>> = std::thread::scope(|scope| {
            let callers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        budget.spend(|deadline| {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                        })
                    })
                })
                .collect();
            callers
                .into_iter()
                .map(|caller| caller.join().expect("caller thread"))
                .collect()
        });

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_some()).count(),
            1
        );
        assert!(
            start.elapsed() < Duration::from_millis(600),
            "two callers took {:?} of a 300ms budget",
            start.elapsed()
        );
    }
}
