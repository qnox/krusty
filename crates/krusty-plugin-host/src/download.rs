//! Moving a file from a URL to disk. Provisioning verifies what arrives, so a fetcher only transfers.

use std::path::Path;
use std::process::Command;

pub trait Fetcher {
    /// Writes the content at `url` to `dest`, or says why it could not.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String>;
}

/// Downloads with the system `curl`, which every supported platform ships and which already honors
/// the user's proxy variables and CA configuration. Transient failures, including a repository's
/// `429 Too Many Requests` that Maven Central answers under load, are retried with curl's backoff,
/// which honors a `Retry-After`.
pub struct Curl;

impl Fetcher for Curl {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
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
