//! Differential cases: a project's files, given to JetBrains' `kotlin` (the oracle, [`oracle`]) and
//! to krusty-toolchain, whose reports must agree.
//!
//! A case file holds the files after `--- <path>` lines. Lines before the first section describe
//! the case. A `--- krusty` section holds the exact typed diagnostic ledger krusty-toolchain
//! reports instead where it deliberately differs. `--- krusty-refusal` is the same ledger for an
//! explicit unsupported-feature refusal. Each line is `<severity>\t<rendered diagnostic>`; the
//! toolchain's own output is never written into the repository.
//!
//! A case spells the Kotlin release krusty compiles against `{kotlin}`: the release the harness
//! runs the lane under ([`kotlin_version`]), so moving krusty to another release changes no case.

pub mod kotlin;
pub mod oracle;
pub mod rendering;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// Sections that hold krusty-toolchain's deliberate difference rather than a file.
const KRUSTY: &str = "krusty";
const KRUSTY_REFUSAL: &str = "krusty-refusal";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpectedSeverity {
    Error,
    Warning,
    WeakWarning,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedDiagnostic {
    pub severity: ExpectedSeverity,
    pub rendered: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KrustyExpected {
    Difference(Vec<ExpectedDiagnostic>),
    Refusal(Vec<ExpectedDiagnostic>),
}

pub struct Case {
    pub name: String,
    /// The project's files, by `/`-separated path below its root.
    pub files: Vec<(String, String)>,
    /// What krusty-toolchain reports instead, where it deliberately refuses what the toolchain
    /// accepts, or rejects for another reason.
    pub krusty: Option<KrustyExpected>,
}

/// The Kotlin release the harness runs the lane under: the release a project krusty compiles pins,
/// since the toolchain's own default is one krusty does not support.
pub fn kotlin_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| krusty_core::kotlin_version::environment_release().to_string())
}

/// `text` with every `{kotlin}` spelled as [`kotlin_version`].
pub fn with_kotlin_version(text: &str) -> String {
    text.replace("{kotlin}", kotlin_version())
}

/// The cases in `tests/cases/<directory>`, by file name.
pub fn cases(directory: &str) -> Vec<Case> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/cases")
        .join(directory);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "case")
        })
        .collect();
    paths.sort();
    paths.iter().map(|path| read_case(path)).collect()
}

fn read_case(path: &Path) -> Case {
    let text = with_kotlin_version(&std::fs::read_to_string(path).unwrap());
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix("--- ") {
            Some(name) => sections.push((name.to_string(), Vec::new())),
            None => {
                if let Some((_, lines)) = sections.last_mut() {
                    lines.push(line);
                }
            }
        }
    }
    let mut case = Case {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        files: Vec::new(),
        krusty: None,
    };
    for (name, lines) in sections {
        if matches!(name.as_str(), KRUSTY | KRUSTY_REFUSAL) {
            let diagnostics = lines
                .iter()
                .map(|line| {
                    let (severity, rendered) = line.split_once('\t').unwrap_or_else(|| {
                        panic!(
                            "{}: `{name}` diagnostic has no tab-separated severity: {line}",
                            path.display()
                        )
                    });
                    let severity = match severity {
                        "error" => ExpectedSeverity::Error,
                        "warning" => ExpectedSeverity::Warning,
                        "weak-warning" => ExpectedSeverity::WeakWarning,
                        other => panic!(
                            "{}: `{name}` diagnostic has unknown severity `{other}`",
                            path.display()
                        ),
                    };
                    ExpectedDiagnostic {
                        severity,
                        rendered: rendered.to_string(),
                    }
                })
                .collect();
            case.krusty = Some(if name == KRUSTY_REFUSAL {
                KrustyExpected::Refusal(diagnostics)
            } else {
                KrustyExpected::Difference(diagnostics)
            });
            continue;
        }
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        case.files.push((name, text));
    }
    case
}

/// A directory removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// A fresh, empty directory named after `name` and this process.
    pub fn new(name: &str) -> Self {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "krusty-toolchain-{name}-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path.canonicalize().unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temporary directory {}: {error}", path.display()),
            }
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write `files` below a directory named `project` in `temp`, the name the oracle's project has
/// too, since the root module is named after its directory. Returns the project root.
pub fn materialize(temp: &TempDir, files: &[(String, String)]) -> PathBuf {
    let root = temp.0.join("project");
    std::fs::create_dir_all(&root).unwrap();
    for (file, text) in files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}
