//! Moving a file from a URL to disk. Provisioning verifies what arrives, so a fetcher only transfers.

use std::path::Path;
use std::time::{Duration, Instant};

use krusty_download::{download, RetryPolicy};

pub trait Fetcher {
    /// Writes the content at `url` to `dest`, or says why it could not.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String>;
}

/// Downloads with the system `curl`, which every supported platform ships and which already honors
/// the user's proxy variables and CA configuration. The shared fetcher paces the requests and
/// retries transient failures, including the `429 Too Many Requests` Maven Central answers under
/// load. A JDK archive is large, so an attempt may run long.
pub struct Curl;

const POLICY: RetryPolicy = RetryPolicy {
    connect_timeout: Duration::from_secs(30),
    attempt_timeout: Duration::from_secs(30 * 60),
    retry_delay: Duration::from_secs(10),
    max_attempts: 8,
    request_gap: Duration::from_secs(3),
};

/// The whole provisioning of one file, retries included.
const DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

impl Fetcher for Curl {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        let (outcome, failure) = download(url, dest, POLICY, Instant::now() + DEADLINE);
        if outcome.completed {
            Ok(())
        } else {
            Err(failure.unwrap_or_else(|| "curl failed".to_string()))
        }
    }
}
