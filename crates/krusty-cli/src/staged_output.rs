//! Compiler output written as it is emitted, published only once the compilation has succeeded.
//!
//! kotlinc leaves `-d` untouched when a compilation fails, and a later source file can still fail
//! after earlier ones were emitted. Holding every class file in memory until the end keeps that
//! promise at the cost of the module's whole output (over a hundred megabytes for a large generated
//! client). Instead each artifact goes to a staging location beside the destination: a directory
//! for `-d dir`, a jar file for `-d foo.jar`. [`StagedOutput::commit`] moves it into place;
//! dropping an uncommitted output removes the staging location.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

pub struct StagedOutput {
    dest: PathBuf,
    target: Target,
    /// The staging entry this output created, reserved by the first artifact. Only an entry this
    /// instance created is ever published or removed; one left by another process is never
    /// adopted, even when its name would match.
    staging: Option<PathBuf>,
    /// The first write failure. Emission continues so the compilation still reports its own
    /// diagnostics; the failure is returned by `commit`.
    error: Option<io::Error>,
    committed: bool,
}

enum Target {
    /// Files staged under the staging directory. Commit publishes what is on disk there rather than
    /// a ledger of written names: a path emitted twice (a source listed twice), or two names one
    /// case-insensitive file system folds together, is one staged file holding the later bytes, as
    /// a direct write would have left it.
    Directory,
    /// Started together with the staging file.
    Jar {
        writer: Option<Box<ZipWriter<File>>>,
    },
}

impl StagedOutput {
    pub fn new(dest: &Path) -> Self {
        let target = if dest.extension().is_some_and(|extension| extension == "jar") {
            Target::Jar { writer: None }
        } else {
            Target::Directory
        };
        Self {
            dest: dest.to_path_buf(),
            target,
            staging: None,
            error: None,
            committed: false,
        }
    }

    pub fn write(&mut self, rel: &str, bytes: &[u8]) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.reserve().and_then(|staging| match &mut self.target {
            Target::Directory => {
                let path = staging.join(rel);
                path.parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::write(&path, bytes))
            }
            Target::Jar { writer } => write_jar_entry(
                writer.as_mut().expect("a reserved jar stage has a writer"),
                rel,
                bytes,
            ),
        }) {
            self.error = Some(error);
        }
    }

    /// The staging entry, created exclusively on first use.
    fn reserve(&mut self) -> io::Result<PathBuf> {
        if let Some(staging) = &self.staging {
            return Ok(staging.clone());
        }
        let jar = matches!(self.target, Target::Jar { .. });
        let (staging, file) = reserve_staging(&self.dest, jar)?;
        // Owned from here on, so a failure below still removes it.
        self.staging = Some(staging.clone());
        if let (Target::Jar { writer }, Some(file)) = (&mut self.target, file) {
            *writer = Some(Box::new(start_jar(file)?));
        }
        Ok(staging)
    }

    /// Publish everything written so far at the destination.
    pub fn commit(mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        match &self.target {
            Target::Directory => {
                // Nothing staged means nothing emitted; the destination is not created.
                if let Some(staging) = &self.staging {
                    publish_tree(staging, &self.dest)?;
                }
            }
            Target::Jar { .. } => {
                // A jar destination exists even when nothing was emitted.
                let staging = self.reserve()?;
                let Target::Jar { writer } = &mut self.target else {
                    unreachable!("the target was matched as a jar")
                };
                let writer = writer.take().expect("a reserved jar stage has a writer");
                writer.finish().map_err(io::Error::other)?;
                move_file(&staging, &self.dest)?;
            }
        }
        self.committed = true;
        self.remove_staging();
        Ok(())
    }

    fn remove_staging(&self) {
        let Some(staging) = &self.staging else {
            return;
        };
        // Best effort: a leftover staging entry never affects the destination.
        let _ = if staging.is_dir() {
            std::fs::remove_dir_all(staging)
        } else {
            std::fs::remove_file(staging)
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

/// Create a fresh hidden sibling of the destination, so publishing it is a rename on the same file
/// system. The entry is created with exclusive semantics: a name another process left behind (its
/// pid since reused) is skipped, never reused.
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[cfg(test)]
fn reserve_sequence_peek() -> u32 {
    SEQUENCE.load(Ordering::Relaxed)
}

fn reserve_staging(dest: &Path, jar: bool) -> io::Result<(PathBuf, Option<File>)> {
    let name = dest
        .file_name()
        .map_or_else(|| "out".into(), |name| name.to_string_lossy());
    let parent = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    loop {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staging = parent.join(format!(
            ".{name}.krusty-{}-{sequence}.tmp",
            std::process::id()
        ));
        let created = if jar {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)
                .map(Some)
        } else {
            std::fs::create_dir(&staging).map(|()| None)
        };
        match created {
            Ok(file) => return Ok((staging, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn start_jar(file: File) -> io::Result<ZipWriter<File>> {
    let mut writer = ZipWriter::new(file);
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

/// Move every file under `from` to the same relative path under `to`.
fn publish_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            publish_tree(&entry.path(), &target)?;
        } else {
            move_file(&entry.path(), &target)?;
        }
    }
    Ok(())
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
    fn a_directory_with_nothing_emitted_is_not_created() {
        let root = scratch("dir-empty");
        StagedOutput::new(&root.join("classes")).commit().unwrap();
        assert!(entries(&root).is_empty());
    }

    /// Leave entries under every staging name this process can pick next, as an interrupted
    /// compilation whose pid was later reused would have.
    fn plant_stale_stages(dest: &Path, jar: bool) -> Vec<PathBuf> {
        let name = dest.file_name().unwrap().to_string_lossy().into_owned();
        let first = reserve_sequence_peek();
        (first..first + 64)
            .map(|sequence| {
                let stale = dest.parent().unwrap().join(format!(
                    ".{name}.krusty-{}-{sequence}.tmp",
                    std::process::id()
                ));
                if jar {
                    std::fs::write(&stale, b"stale jar").unwrap();
                } else {
                    std::fs::create_dir_all(&stale).unwrap();
                    std::fs::write(stale.join("Stale.class"), b"stale").unwrap();
                }
                stale
            })
            .collect()
    }

    #[test]
    fn a_stale_directory_stage_is_neither_published_nor_removed() {
        let root = scratch("dir-stale");
        let dest = root.join("classes");
        let stale = plant_stale_stages(&dest, false);
        let mut output = StagedOutput::new(&dest);
        output.write("Fresh.class", b"fresh");
        output.commit().unwrap();
        assert_eq!(entries(&dest), vec!["Fresh.class".to_string()]);
        let failed = StagedOutput::new(&dest);
        drop(failed);
        let mut dropped = StagedOutput::new(&dest);
        dropped.write("Other.class", b"other");
        drop(dropped);
        for stage in &stale {
            assert_eq!(entries(stage), vec!["Stale.class".to_string()]);
        }
        assert_eq!(entries(&root).len(), stale.len() + 1);
    }

    #[test]
    fn a_stale_jar_stage_is_neither_published_nor_removed() {
        let root = scratch("jar-stale");
        let dest = root.join("lib.jar");
        let stale = plant_stale_stages(&dest, true);
        let mut output = StagedOutput::new(&dest);
        output.write("Fresh.class", b"fresh");
        output.commit().unwrap();
        assert_eq!(
            jar_names(&dest),
            vec!["META-INF/MANIFEST.MF", "Fresh.class"]
        );
        let mut dropped = StagedOutput::new(&dest);
        dropped.write("Other.class", b"other");
        drop(dropped);
        for stage in &stale {
            assert_eq!(std::fs::read(stage).unwrap(), b"stale jar");
        }
        assert_eq!(entries(&root).len(), stale.len() + 1);
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
