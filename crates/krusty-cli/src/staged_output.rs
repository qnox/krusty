//! Compiler output written as it is emitted, published only once the compilation has succeeded.
//!
//! kotlinc leaves `-d` untouched when a compilation fails, and a later source file can still fail
//! after earlier ones were emitted. Holding every class file in memory until the end keeps that
//! promise at the cost of the module's whole output (over a hundred megabytes for a large generated
//! client). Instead each artifact goes to a staging location beside the destination: a directory
//! for `-d dir`, a jar file for `-d foo.jar`. [`StagedOutput::commit`] moves it into place;
//! dropping an uncommitted output removes the staging location.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

pub struct StagedOutput {
    dest: PathBuf,
    staging: PathBuf,
    target: Target,
    /// The first write failure. Emission continues so the compilation still reports its own
    /// diagnostics; the failure is returned by `commit`.
    error: Option<io::Error>,
    committed: bool,
}

enum Target {
    /// Relative paths written under the staging directory. A path emitted twice (a source listed
    /// twice) is staged once, the later bytes replacing the earlier, as a direct write would.
    Directory { written: BTreeSet<String> },
    /// Opened on the first artifact, so a compilation that emits nothing creates nothing.
    Jar {
        writer: Option<Box<ZipWriter<File>>>,
    },
}

impl StagedOutput {
    pub fn new(dest: &Path) -> Self {
        let target = if dest.extension().is_some_and(|extension| extension == "jar") {
            Target::Jar { writer: None }
        } else {
            Target::Directory {
                written: BTreeSet::new(),
            }
        };
        Self {
            dest: dest.to_path_buf(),
            staging: staging_path(dest),
            target,
            error: None,
            committed: false,
        }
    }

    pub fn write(&mut self, rel: &str, bytes: &[u8]) {
        if self.error.is_some() {
            return;
        }
        let written = match &mut self.target {
            Target::Directory { written } => {
                let path = self.staging.join(rel);
                let result = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, bytes));
                if result.is_ok() {
                    written.insert(rel.to_string());
                }
                result
            }
            Target::Jar { writer } => {
                let writer = match writer {
                    Some(writer) => Ok(writer),
                    None => {
                        start_jar(&self.staging).map(|started| writer.insert(Box::new(started)))
                    }
                };
                writer.and_then(|writer| write_jar_entry(writer, rel, bytes))
            }
        };
        if let Err(error) = written {
            self.error = Some(error);
        }
    }

    /// Publish everything written so far at the destination.
    pub fn commit(mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        match &mut self.target {
            Target::Directory { written } => {
                for rel in written.iter() {
                    let to = self.dest.join(rel);
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    move_file(&self.staging.join(rel), &to)?;
                }
            }
            Target::Jar { writer } => {
                // A jar destination exists even when nothing was emitted.
                let writer = match writer.take() {
                    Some(writer) => *writer,
                    None => start_jar(&self.staging)?,
                };
                writer.finish().map_err(io::Error::other)?;
                if let Some(parent) = self.dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                move_file(&self.staging, &self.dest)?;
            }
        }
        self.committed = true;
        self.remove_staging();
        Ok(())
    }

    fn remove_staging(&self) {
        // Best effort: a leftover staging entry never affects the destination.
        let _ = if self.staging.is_dir() {
            std::fs::remove_dir_all(&self.staging)
        } else {
            std::fs::remove_file(&self.staging)
        };
    }
}

impl Drop for StagedOutput {
    fn drop(&mut self) {
        if !self.committed {
            self.remove_staging();
        }
    }
}

/// A hidden sibling of the destination, so publishing it is a rename on the same file system.
fn staging_path(dest: &Path) -> PathBuf {
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    let name = dest
        .file_name()
        .map_or_else(|| "out".into(), |name| name.to_string_lossy());
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staging = format!(".{name}.krusty-{}-{sequence}.tmp", std::process::id());
    match dest.parent() {
        Some(parent) => parent.join(staging),
        None => PathBuf::from(staging),
    }
}

fn start_jar(path: &Path) -> io::Result<ZipWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = ZipWriter::new(File::create(path)?);
    write_jar_entry(
        &mut writer,
        "META-INF/MANIFEST.MF",
        b"Manifest-Version: 1.0\r\nCreated-By: krusty\r\n\r\n",
    )?;
    Ok(writer)
}

fn write_jar_entry(writer: &mut ZipWriter<File>, rel: &str, bytes: &[u8]) -> io::Result<()> {
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file(rel, options).map_err(io::Error::other)?;
    writer.write_all(bytes)
}

/// Rename, or copy when the destination is on another file system.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krusty-staged-output-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn jar_names(path: &Path) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .unwrap()
                    .name()
                    .unwrap()
                    .into_owned()
            })
            .collect()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn a_committed_directory_holds_every_artifact_and_no_staging() {
        let root = scratch("dir-commit");
        let dest = root.join("classes");
        let mut output = StagedOutput::new(&dest);
        output.write("p/A.class", b"a");
        output.write("META-INF/m.kotlin_module", b"m");
        assert!(!dest.exists(), "nothing is published before commit");
        output.commit().unwrap();
        assert_eq!(std::fs::read(dest.join("p/A.class")).unwrap(), b"a");
        assert_eq!(
            std::fs::read(dest.join("META-INF/m.kotlin_module")).unwrap(),
            b"m"
        );
        assert_eq!(entries(&root), vec!["classes".to_string()]);
    }

    #[test]
    fn a_path_emitted_twice_is_published_once_with_the_later_bytes() {
        let root = scratch("dir-twice");
        let dest = root.join("classes");
        let mut output = StagedOutput::new(&dest);
        output.write("A.class", b"first");
        output.write("A.class", b"second");
        output.commit().unwrap();
        assert_eq!(std::fs::read(dest.join("A.class")).unwrap(), b"second");
        assert_eq!(entries(&root), vec!["classes".to_string()]);
    }

    #[test]
    fn an_uncommitted_directory_leaves_the_destination_untouched() {
        let root = scratch("dir-drop");
        let dest = root.join("classes");
        std::fs::create_dir_all(dest.join("p")).unwrap();
        std::fs::write(dest.join("p/A.class"), b"old").unwrap();
        let mut output = StagedOutput::new(&dest);
        output.write("p/A.class", b"new");
        output.write("p/B.class", b"b");
        drop(output);
        assert_eq!(std::fs::read(dest.join("p/A.class")).unwrap(), b"old");
        assert!(!dest.join("p/B.class").exists());
        assert_eq!(entries(&root), vec!["classes".to_string()]);
    }

    #[test]
    fn a_committed_jar_lists_the_manifest_then_artifacts_in_emission_order() {
        let root = scratch("jar-commit");
        let dest = root.join("out/lib.jar");
        let mut output = StagedOutput::new(&dest);
        output.write("p/B.class", b"b");
        output.write("p/A.class", b"a");
        output.commit().unwrap();
        assert_eq!(
            jar_names(&dest),
            vec!["META-INF/MANIFEST.MF", "p/B.class", "p/A.class"]
        );
        assert_eq!(entries(&root.join("out")), vec!["lib.jar".to_string()]);
    }

    #[test]
    fn an_empty_jar_is_still_written_and_an_uncommitted_one_is_not() {
        let root = scratch("jar-empty");
        let dest = root.join("empty.jar");
        StagedOutput::new(&dest).commit().unwrap();
        assert_eq!(jar_names(&dest), vec!["META-INF/MANIFEST.MF"]);
        let failed = root.join("failed.jar");
        let mut output = StagedOutput::new(&failed);
        output.write("p/A.class", b"a");
        drop(output);
        assert_eq!(entries(&root), vec!["empty.jar".to_string()]);
    }
}
