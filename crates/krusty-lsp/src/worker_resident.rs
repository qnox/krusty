//! Resident-set ceiling for the analysis worker.
//!
//! The supervisor owns the limit. [`DEFAULT_WORKER_RSS_BYTES`] is that bound when the supervisor
//! has no tighter one: 1 GiB of retained compiler state between requests, not a reading of the
//! host or container budget. A smaller cgroup can still OOM-kill the child, and the sample is
//! taken before a request, so one compile can grow past the ceiling and die before the next
//! sample. Both of those deaths use the existing crash restart. An unreadable sample leaves the
//! worker in place.

/// Default resident ceiling the supervisor applies between analysis requests.
pub const DEFAULT_WORKER_RSS_BYTES: u64 = 1024 * 1024 * 1024;

/// One observation of a worker process's resident set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResidentSample {
    /// Resident bytes from the platform accounting source.
    Bytes(u64),
    /// The probe failed, or this platform has no resident-set source. The worker stays up.
    Unavailable,
}

/// Ceiling and observation the supervisor applies to its analysis worker.
pub struct WorkerResidentPolicy {
    limit_bytes: u64,
    sample: fn(u32) -> ResidentSample,
}

impl WorkerResidentPolicy {
    /// Sample Linux `VmRSS` when `/proc/<pid>/status` can be read. Every other platform, and a
    /// failed read, is [`ResidentSample::Unavailable`].
    pub fn platform(limit_bytes: u64) -> Self {
        Self {
            limit_bytes,
            sample: platform_resident_sample,
        }
    }

    /// Compare `sample(pid)` with `limit_bytes`. A test supplies the samples; production uses
    /// [`Self::platform`].
    pub fn observe(limit_bytes: u64, sample: fn(u32) -> ResidentSample) -> Self {
        Self {
            limit_bytes,
            sample,
        }
    }

    pub(crate) fn over_budget(&self, pid: u32) -> bool {
        over_budget((self.sample)(pid), self.limit_bytes)
    }
}

fn over_budget(sample: ResidentSample, limit_bytes: u64) -> bool {
    match sample {
        ResidentSample::Bytes(bytes) => bytes > limit_bytes,
        ResidentSample::Unavailable => false,
    }
}

pub(crate) fn parse_vm_rss_bytes(status: &str) -> Option<u64> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix("VmRSS:")?;
        let mut parts = rest.split_whitespace();
        let kib: u64 = parts.next()?.parse().ok()?;
        // procfs reports this field in kilobytes. Any other unit is not the value we account.
        (parts.next() == Some("kB")).then_some(kib.saturating_mul(1024))
    })
}

fn platform_resident_sample(pid: u32) -> ResidentSample {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok();
        match status.as_deref().and_then(parse_vm_rss_bytes) {
            Some(bytes) => ResidentSample::Bytes(bytes),
            None => ResidentSample::Unavailable,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        ResidentSample::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_rss_is_kilobytes_from_proc_status() {
        let status = "Name:\tkrusty-lsp\nVmSize:\t  8192 kB\nVmRSS:\t  2048 kB\n";
        assert_eq!(parse_vm_rss_bytes(status), Some(2048 * 1024));
        assert_eq!(parse_vm_rss_bytes("Name:\tkrusty-lsp\n"), None);
        assert_eq!(parse_vm_rss_bytes("VmRSS:\t  10 mB\n"), None);
    }

    #[test]
    fn exactly_at_the_limit_keeps_the_worker_and_past_it_restarts() {
        let limit = DEFAULT_WORKER_RSS_BYTES;
        assert!(!over_budget(ResidentSample::Bytes(limit), limit));
        assert!(over_budget(ResidentSample::Bytes(limit + 1), limit));
        assert!(!over_budget(ResidentSample::Unavailable, limit));
    }

    #[test]
    fn a_missing_process_sample_is_unavailable() {
        let sample = platform_resident_sample(u32::MAX);
        assert_eq!(sample, ResidentSample::Unavailable);
        assert!(
            !over_budget(sample, DEFAULT_WORKER_RSS_BYTES),
            "an unreadable sample must not force a restart"
        );
    }

    #[test]
    fn this_process_sample_matches_the_platform() {
        let sample = platform_resident_sample(std::process::id());
        if cfg!(target_os = "linux") {
            assert!(
                matches!(sample, ResidentSample::Bytes(bytes) if bytes > 0),
                "Linux reads VmRSS for a live process, got {sample:?}"
            );
        } else {
            assert_eq!(sample, ResidentSample::Unavailable);
        }
    }
}
