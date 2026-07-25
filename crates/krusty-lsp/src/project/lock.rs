use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use super::fingerprint::Hasher;
use fs2::FileExt;

pub struct WorkspaceProbeLock {
    file: File,
}

impl WorkspaceProbeLock {
    pub fn acquire(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut hasher = Hasher::default();
        hasher.write_str(&root.to_string_lossy());
        let path = std::env::temp_dir().join(format!(
            "krusty-project-sync-{:016x}.lock",
            hasher.finish().as_u64()
        ));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock_exclusive().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "waiting for project sync lock for {}: {error}",
                    root.display()
                ),
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for WorkspaceProbeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    #[test]
    fn a_second_probe_waits_for_the_workspace_lock() {
        let root = std::env::temp_dir().join(format!(
            "krusty-lock-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let first = WorkspaceProbeLock::acquire(&root).expect("first lock");
        let (sender, receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let second = WorkspaceProbeLock::acquire(&root).expect("second lock");
            sender.send(()).unwrap();
            drop(second);
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout)
        );
        drop(first);
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("second lock proceeds after release");
        waiter.join().unwrap();
    }
}
