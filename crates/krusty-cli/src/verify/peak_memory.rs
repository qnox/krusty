//! The process's peak resident set size, read once a compilation is done and before its output is
//! compared, so the figure is the compilation's own.

/// The most memory this process has had resident, in bytes, or `None` where the platform does not
/// say.
///
/// On Linux this is the address space's high-water mark (`VmHWM`). `getrusage` is not: its
/// `ru_maxrss` survives `exec`, so a krusty that a JVM build tool started through `vfork` or
/// `posix_spawn` would report the build tool's own resident set.
#[cfg(target_os = "linux")]
pub fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(kibibytes * 1024)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the `rusage` it is given and reports failure by its return value.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: the call succeeded, so the structure is initialized.
    let maximum = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    // macOS reports bytes; the BSDs report kibibytes.
    Some(if cfg!(target_os = "macos") {
        maximum
    } else {
        maximum * 1024
    })
}

#[cfg(not(unix))]
pub fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_running_process_has_resident_memory() {
        let peak = super::peak_rss_bytes().expect("unix reports its peak resident set");
        assert!(peak > 1024 * 1024, "{peak} bytes");
    }
}
