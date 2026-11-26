//! The Java half of a mixed `jvm_library` action.
//!
//! intellij-community's `jvm-inc-builder` compiles a target's Kotlin first, reading its `.java`
//! files as declarations only, then runs javac over the `.java` files with the Kotlin classes on
//! the classpath, and writes both into the one module jar. The worker does the same: krusty compiles
//! the Kotlin with the Java sources as declarations, and this module runs the JDK's javac with the
//! options the builder derives from the work request (`BuildContextImpl.buildJavaOptions`), then
//! appends javac's output to the jar.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Java sources of a work request and the javac options its flags select.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct JavaCompilation {
    pub sources: Vec<PathBuf>,
    /// `--jvm_target`. javac gets `--release` when it differs from javac's own feature release and
    /// `-source`/`-target` otherwise, as the builder decides.
    pub jvm_target: Option<String>,
    /// `--warn off`, unless a `--x_warning_level` entry raises some category back to a warning or
    /// an error.
    pub no_warn: bool,
    /// `--no-proc`: no annotation processing.
    pub no_proc: bool,
    /// `--add-export` values, each passed as `--add-exports`.
    pub add_exports: Vec<String>,
}

impl JavaCompilation {
    /// The javac options for a javac of feature release `javac_release`, without classpath and
    /// output directories.
    pub fn options(&self, javac_release: u32) -> Vec<String> {
        let mut options = vec!["-g".to_string(), "-deprecation".to_string()];
        if self.no_warn {
            options.push("-nowarn".to_string());
        }
        if self.no_proc {
            options.push("-proc:none".to_string());
        }
        options.push("-encoding".to_string());
        options.push("UTF-8".to_string());
        if let Some(target) = &self.jvm_target {
            match target_release(target) {
                // javac's `--release` takes the feature number only: `1.8` is refused, `8` is not.
                Some(release) if release != javac_release => {
                    options.push("--release".to_string());
                    options.push(release.to_string());
                }
                _ => {
                    options.extend(["-source".to_string(), target.clone()]);
                    options.extend(["-target".to_string(), target.clone()]);
                }
            }
        }
        for export in &self.add_exports {
            options.push("--add-exports".to_string());
            options.push(export.clone());
        }
        options
    }

    /// Compile the Java sources with the javac of `java_home` against the Kotlin classes already in
    /// `jar`, then `classpath`, and append every class file javac writes to `jar`.
    pub fn compile_into(
        &self,
        java_home: &Path,
        classpath: &[PathBuf],
        jar: &Path,
    ) -> Result<(), String> {
        let javac = java_home
            .join("bin")
            .join(if cfg!(windows) { "javac.exe" } else { "javac" });
        if !javac.is_file() {
            return Err(format!(
                "JAVA_HOME ({}) has no bin/javac, so the target's Java sources cannot be compiled",
                java_home.display()
            ));
        }
        let release = javac_release(java_home)?;
        let scratch = Scratch::new(jar)?;
        let classes = scratch.path.join("classes");
        let generated = scratch.path.join("generated");
        for directory in [&classes, &generated] {
            std::fs::create_dir_all(directory)
                .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        }
        let classpath = std::iter::once(jar.to_path_buf())
            .chain(classpath.iter().cloned())
            .map(|entry| entry.display().to_string())
            .collect::<Vec<_>>()
            .join(&crate::cli::CLASSPATH_SEPARATOR.to_string());
        let mut arguments = self.options(release);
        arguments.extend(["-classpath".to_string(), classpath]);
        arguments.extend(["-d".to_string(), classes.display().to_string()]);
        // Generated sources go beside the classes and, as in the builder, not into the jar.
        arguments.extend(["-s".to_string(), generated.display().to_string()]);
        arguments.extend(self.sources.iter().map(|path| path.display().to_string()));
        // An argument file keeps a large target under the platform's command-line limit.
        let argfile = scratch.path.join("javac.args");
        std::fs::write(
            &argfile,
            arguments
                .iter()
                .map(|argument| quote_argument(argument))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .map_err(|error| format!("cannot write {}: {error}", argfile.display()))?;
        let ran = Command::new(&javac)
            .arg(format!("@{}", argfile.display()))
            .output()
            .map_err(|error| format!("cannot run {}: {error}", javac.display()))?;
        if !ran.status.success() {
            return Err(format!(
                "javac failed ({}):\n{}{}",
                ran.status,
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr)
            ));
        }
        let mut entries = Vec::new();
        collect_entries(&classes, &classes, &mut entries)?;
        entries.sort();
        super::append_entries(jar, entries)
    }
}

/// The feature release a `--jvm_target` names (`1.8` is 8), or `None` when it names none.
fn target_release(target: &str) -> Option<u32> {
    let target = target.trim();
    target
        .rsplit_once('.')
        .map_or(target, |(_, last)| last)
        .parse()
        .ok()
}

/// The feature release of the JDK at `java_home`, from the `JAVA_VERSION` line of its `release`
/// file (`"25.0.1"` is 25, `"1.8.0_402"` is 8).
fn javac_release(java_home: &Path) -> Result<u32, String> {
    let path = java_home.join("release");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse_java_version(&text)
        .ok_or_else(|| format!("{} names no JAVA_VERSION release", path.display()))
}

fn parse_java_version(release_file: &str) -> Option<u32> {
    let version = release_file
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim()
        .trim_matches('"');
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// One javac argument-file entry. javac splits an argument file on whitespace, so an argument
/// holding whitespace, a quote, or a backslash is double-quoted with those escaped.
fn quote_argument(argument: &str) -> String {
    if !argument.is_empty()
        && !argument
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '#'))
    {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// Every file below `directory`, named by its `/`-separated path relative to `root`.
fn collect_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let listing = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in listing {
        let path = entry
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
            .path();
        if path.is_dir() {
            collect_entries(root, &path, entries)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| format!("javac wrote a non-UTF-8 path {}", path.display()))?
                .replace('\\', "/");
            entries.push((relative, path));
        }
    }
    Ok(())
}

/// A directory beside the output jar for javac's output, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(jar: &Path) -> Result<Self, String> {
        let name = jar
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = jar.with_file_name(format!(".{name}.javac.{}", std::process::id()));
        // A directory left by an earlier, killed request of this same worker holds stale classes.
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("cannot clear {}: {error}", path.display()))?;
        }
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compilation(jvm_target: Option<&str>) -> JavaCompilation {
        JavaCompilation {
            sources: vec![PathBuf::from("B.java")],
            jvm_target: jvm_target.map(str::to_string),
            ..JavaCompilation::default()
        }
    }

    /// The builder passes `--release` only when javac would otherwise cross-compile, because
    /// `--release` cannot be combined with exporting a package of a system module.
    #[test]
    fn the_target_selects_release_only_when_javac_cross_compiles() {
        assert_eq!(
            compilation(Some("17")).options(25),
            [
                "-g",
                "-deprecation",
                "-encoding",
                "UTF-8",
                "--release",
                "17"
            ]
        );
        assert_eq!(
            compilation(Some("25")).options(25),
            [
                "-g",
                "-deprecation",
                "-encoding",
                "UTF-8",
                "-source",
                "25",
                "-target",
                "25"
            ]
        );
        assert_eq!(
            compilation(Some("1.8")).options(25),
            ["-g", "-deprecation", "-encoding", "UTF-8", "--release", "8"]
        );
        assert_eq!(
            compilation(None).options(25),
            ["-g", "-deprecation", "-encoding", "UTF-8"]
        );
    }

    #[test]
    fn warning_processing_and_export_flags_reach_javac() {
        let unit = JavaCompilation {
            no_warn: true,
            no_proc: true,
            add_exports: vec!["java.desktop/sun.awt=ALL-UNNAMED".to_string()],
            ..compilation(None)
        };
        assert_eq!(
            unit.options(21),
            [
                "-g",
                "-deprecation",
                "-nowarn",
                "-proc:none",
                "-encoding",
                "UTF-8",
                "--add-exports",
                "java.desktop/sun.awt=ALL-UNNAMED"
            ]
        );
    }

    #[test]
    fn the_jdk_release_file_names_the_feature_release() {
        assert_eq!(
            parse_java_version("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"25.0.1\"\n"),
            Some(25)
        );
        assert_eq!(parse_java_version("JAVA_VERSION=\"21\"\n"), Some(21));
        assert_eq!(parse_java_version("JAVA_VERSION=\"1.8.0_402\"\n"), Some(8));
        assert_eq!(parse_java_version("IMPLEMENTOR=\"x\"\n"), None);
    }

    #[test]
    fn argument_file_entries_quote_whitespace_and_escapes() {
        assert_eq!(quote_argument("a/B.java"), "a/B.java");
        assert_eq!(
            quote_argument("dir with space/B.java"),
            "\"dir with space/B.java\""
        );
        assert_eq!(quote_argument("C:\\x\\B.java"), "\"C:\\\\x\\\\B.java\"");
    }
}
