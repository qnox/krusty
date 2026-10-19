//! Putting verified files into the cache. Everything is assembled beside its final path and renamed
//! into place, so a reader never sees a partial file and concurrent builds never corrupt each other:
//! whichever finishes first wins, and the others discard their copy.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::{Digest, Sha256};

/// Why a file is not the pinned one.
#[derive(Debug)]
pub(crate) enum Mismatch {
    Size { actual: u64 },
    Digest { actual: String },
    Unreadable(std::io::Error),
}

pub(crate) fn verify(path: &Path, sha256: &str, size: u64) -> Result<(), Mismatch> {
    let actual = std::fs::metadata(path).map_err(Mismatch::Unreadable)?.len();
    if actual != size {
        return Err(Mismatch::Size { actual });
    }
    let actual = sha256_file(path).map_err(Mismatch::Unreadable)?;
    if actual != sha256 {
        return Err(Mismatch::Digest { actual });
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// A path beside `dest`, unique to this process and call, to assemble `dest` in.
pub(crate) fn staging_path(dest: &Path) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".partial-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    dest.with_file_name(name)
}

/// Renames a staged file or directory to `dest`. A rename fails when `dest` exists, which callers
/// rule out by removing a broken earlier entry first, so a failure means another process published
/// `dest` meanwhile. Its copy is kept, and the staged one removed, only when `valid` accepts it.
pub(crate) fn publish(
    staged: &Path,
    dest: &Path,
    valid: impl Fn(&Path) -> bool,
) -> std::io::Result<()> {
    match std::fs::rename(staged, dest) {
        Ok(()) => Ok(()),
        Err(error) => {
            remove(staged);
            if valid(dest) {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

/// Removes a file or a directory tree. One that is already gone is not an error.
pub(crate) fn remove_entry(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    }
}

/// Removes this process's own staging leftovers, which nothing reads, so a failure is harmless.
pub(crate) fn remove(path: &Path) {
    let _ = remove_entry(path);
}

/// Unpacks the JDK archive `name`, staged at `archive`, into the directory `into`, which it creates:
/// a zip with krusty's own reader, a gzip tarball with the system `tar`.
pub(crate) fn unpack(archive: &Path, name: &str, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|error| format!("{}: {error}", into.display()))?;
    if name.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(|error| error.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
        return zip.extract(into).map_err(|error| error.to_string());
    }
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|error| format!("could not run tar: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
