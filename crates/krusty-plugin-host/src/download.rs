//! Moving a file from a URL to disk. Provisioning verifies what arrives, so a fetcher only transfers.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub trait Fetcher {
    /// Writes the content at `url` to `dest`, or says why it could not.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String>;
}

/// Downloads with the system `curl`, which every supported platform ships and which already honors
/// the user's proxy variables and CA configuration. Transient failures, including a repository's
/// `429 Too Many Requests` that Maven Central answers under load, are retried with curl's backoff,
/// which honors a `Retry-After`.
pub struct Curl;

/// The least time between the starts of two requests from one process. Maven Central answers
/// back-to-back artifact requests with `429` and extends the block for each request made during it,
/// so provisioning a compiler classpath must not fire its jars at once.
const REQUEST_GAP: Duration = Duration::from_secs(3);

/// Spaces request starts at least `gap` apart; callers take turns.
struct Pacer {
    last: Mutex<Option<Instant>>,
}

impl Pacer {
    const fn new() -> Self {
        Pacer {
            last: Mutex::new(None),
        }
    }

    /// How long a request starting at `now` must wait, and records it as started once it has.
    fn delay(&self, now: Instant, gap: Duration) -> Duration {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let wait = last
            .map(|last| (last + gap).saturating_duration_since(now))
            .unwrap_or_default();
        *last = Some(now + wait);
        wait
    }

    fn pace(&self, gap: Duration) {
        let wait = self.delay(Instant::now(), gap);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

static REQUESTS: Pacer = Pacer::new();

impl Fetcher for Curl {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        REQUESTS.pace(REQUEST_GAP);
        let output = Command::new("curl")
            .args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--retry", "8", "--connect-timeout", "30"])
            .arg("--output")
            .arg(dest)
            .arg(url)
            .output()
            .map_err(|error| format!("could not run curl, which krusty downloads with: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            // Each retry repeats the error; the last attempt's is the one to show.
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(stderr
                .trim()
                .lines()
                .last()
                .unwrap_or("curl failed")
                .to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
