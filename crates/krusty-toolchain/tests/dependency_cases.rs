//! `krusty-toolchain show dependencies --all-modules --include-tests` does exactly what JetBrains'
//! `kotlin` does for each case: the same exit status, stdout and stderr, byte for byte, once each
//! run's own directory is replaced by one placeholder. Both resolve from the same local Maven
//! repository and nothing else (`support::kotlin::Resolving`): `tests/cases/dependencies/repository`
//! (the standard library and the test framework) with the case's `m2/<path>` sections added. The
//! toolchain's output comes from the cached oracle (`tests/support/oracle.rs`).
//!
//! Where an artifact cannot be read completely (it is in no repository, its metadata is not
//! well-formed, no variant or more than one matches, its coordinates name no file), the toolchain
//! downloads, or logs and prints the graph anyway; krusty-toolchain refuses. Such a case's
//! `krusty` section holds the complete stderr krusty-toolchain prints, which must be all it
//! prints, with exit status 1, and must not be what the toolchain prints.

#[path = "support/rendering.rs"]
mod rendering;
mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use rendering::problems;
use support::kotlin::{self, Invocation, Resolving};
use support::oracle::Output;

const ARGS: &[&str] = &["show", "dependencies", "--all-modules", "--include-tests"];
/// A case's sections below this directory are files of its local Maven repository.
const REPOSITORY_SECTION: &str = "m2/";
const PLACEHOLDER: &str = "<directory>";

/// Files by `/`-separated path.
type Files = Vec<(String, String)>;

/// A case's inputs: its project, and its local Maven repository.
struct Inputs {
    project: Files,
    repository: Files,
}

/// The files below `directory`, by `/`-separated path.
fn files_below(directory: &Path, prefix: &str, files: &mut Files) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type().unwrap().is_dir() {
            files_below(&entry.path(), &format!("{name}/"), files);
        } else {
            files.push((name, std::fs::read_to_string(entry.path()).unwrap()));
        }
    }
}

/// What krusty-toolchain does for a case, laid out as the oracle lays out `kotlin`'s run.
fn krusty_toolchain(name: &str, inputs: &Inputs) -> Output {
    let temp = support::TempDir::new(&format!("dependency-case-{name}"));
    let directory: PathBuf = temp.0.clone();
    let root = support::materialize(&temp, &inputs.project);
    Resolving::write_repository(&directory, &inputs.repository);
    let ran = Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"))
        .args(ARGS)
        .current_dir(&root)
        .env("HOME", directory.join(Resolving::HOME))
        .env("KOTLIN_SHARED_CACHE_DIR", directory.join(Resolving::CACHE))
        .env_remove("AMPER_SHARED_CACHE_DIR")
        .env_remove("M2_HOME")
        .output()
        .expect("run krusty-toolchain");
    Output {
        root: directory.display().to_string(),
        code: ran
            .status
            .code()
            .expect("krusty-toolchain exits with a code"),
        stdout: ran.stdout,
        stderr: ran.stderr,
    }
}

/// `bytes` with every mention of the run's `directory` replaced by the placeholder.
fn placed(directory: &str, bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace(directory, PLACEHOLDER)
}

#[test]
fn every_dependency_case_is_resolved_as_the_toolchain_resolves_it() {
    let cases = support::cases("dependencies");
    assert!(cases.len() >= 6, "the cases are missing");
    let mut shared = Vec::new();
    files_below(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/dependencies/repository"),
        "",
        &mut shared,
    );
    let inputs: Vec<Inputs> = cases
        .iter()
        .map(|case| {
            let mut repository = shared.clone();
            let mut project = Vec::new();
            for (file, text) in &case.files {
                match file.strip_prefix(REPOSITORY_SECTION) {
                    Some(file) => repository.push((file.to_string(), text.clone())),
                    None => project.push((file.clone(), text.clone())),
                }
            }
            Inputs {
                project,
                repository,
            }
        })
        .collect();
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .zip(&inputs)
        .map(|(case, inputs)| Invocation {
            case: &case.name,
            files: &inputs.project,
            args: ARGS,
            repository: Some(&inputs.repository),
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let mut failures = Vec::new();
    for ((case, inputs), toolchain) in cases.iter().zip(&inputs).zip(outputs) {
        let name = &case.name;
        let actual = krusty_toolchain(name, inputs);
        // The toolchain's problems, read back from its rendering: errors on stderr, which holds
        // nothing else, and warnings on stdout before the graphs.
        let project_root = format!("{}/{}", toolchain.root, Resolving::PROJECT);
        let (errors, unread) = problems(&project_root, &toolchain.stderr);
        if !unread.is_empty() {
            failures.push(format!(
                "{name}: the toolchain's stderr holds more than problems:\n{}",
                placed(&toolchain.root, unread)
            ));
            continue;
        }
        let (warnings, result) = problems(&project_root, &toolchain.stdout);
        let lines = |problems: &[String]| -> String {
            problems
                .iter()
                .map(|problem| format!("{problem}\n"))
                .collect()
        };
        let expected = (
            toolchain.code,
            format!("{}{}", lines(&warnings), placed(&toolchain.root, result)),
            lines(&errors),
        );
        // krusty-toolchain's, with paths relative to the project as the toolchain's are read back.
        let relative = |bytes: &[u8]| {
            let project = format!("{}/{}/", actual.root, Resolving::PROJECT);
            placed(
                &actual.root,
                String::from_utf8_lossy(bytes)
                    .replace(&project, "")
                    .as_bytes(),
            )
        };
        let printed = (
            actual.code,
            relative(&actual.stdout),
            relative(&actual.stderr),
        );
        if let Some(krusty) = &case.krusty {
            let refusal = (1, String::new(), format!("{}\n", krusty.join("\n")));
            if printed != refusal {
                failures.push(format!(
                    "{name}: krusty-toolchain exited {} with stdout\n{}\nstderr\n{}\nthe `krusty` section expects exit 1, no stdout and stderr\n{}",
                    printed.0, printed.1, printed.2, refusal.2
                ));
            }
            if expected == refusal {
                failures.push(format!(
                    "{name}: the `krusty` section is what the toolchain does"
                ));
            }
            continue;
        }
        if printed != expected {
            failures.push(format!(
                "{name}: krusty-toolchain exited {} with stdout\n{}\nstderr\n{}\nthe toolchain exited {} with stdout\n{}\nstderr\n{}",
                printed.0, printed.1, printed.2, expected.0, expected.1, expected.2
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
