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
//! `krusty-refusal` section holds the errors krusty-toolchain reports, read back from its rendering
//! as the toolchain's problems are, which must be all it prints: its complete stderr, with exit
//! status 1, and not what the toolchain does.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use support::kotlin::{self, Invocation, Resolving};
use support::oracle::Output;
use support::rendering::problems;
use support::{ExpectedDiagnostic, ExpectedSeverity, KrustyExpected};

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

/// What krusty-toolchain does for a case, laid out as the oracle lays out `kotlin`'s run, in a
/// directory named after `run`: the tests run at once, each in its own directories.
fn krusty_toolchain(run: &str, inputs: &Inputs) -> Output {
    let temp = support::TempDir::new(&format!("dependency-{run}"));
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

/// The problems krusty-toolchain refused with, read back as the toolchain's are, each run's
/// directory replaced by the placeholder; `None` unless it exited 1 with nothing on stdout and
/// nothing but problems on stderr.
fn refusal(actual: &Output) -> Option<Vec<ExpectedDiagnostic>> {
    let project_root = format!("{}/{}", actual.root, Resolving::PROJECT);
    let (refused, unread) = problems(&project_root, &actual.stderr);
    (actual.code == 1 && actual.stdout.is_empty() && unread.is_empty()).then(|| {
        refused
            .into_iter()
            .map(|problem| ExpectedDiagnostic {
                rendered: placed(&actual.root, problem.rendered.as_bytes()),
                ..problem
            })
            .collect()
    })
}

/// krusty-toolchain refusals are repository-owned behavior and remain runnable without the cached
/// external oracle. Their complete status and streams still have to match the typed case ledger.
#[test]
fn every_dependency_refusal_is_exact() {
    let cases = support::cases("dependencies");
    let mut shared = Vec::new();
    files_below(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/dependencies/repository"),
        "",
        &mut shared,
    );
    let mut failures = Vec::new();
    for case in &cases {
        let Some(KrustyExpected::Refusal(refused)) = &case.krusty else {
            continue;
        };
        let mut repository = shared.clone();
        let mut project = Vec::new();
        for (file, text) in &case.files {
            match file.strip_prefix(REPOSITORY_SECTION) {
                Some(file) => repository.push((file.to_string(), text.clone())),
                None => project.push((file.clone(), text.clone())),
            }
        }
        let actual = krusty_toolchain(
            &format!("refusal-{}", case.name),
            &Inputs {
                project,
                repository,
            },
        );
        let read = refusal(&actual);
        if read.as_ref() != Some(refused) {
            failures.push(format!(
                "{}: got status {} with stdout\n{}stderr\n{}read back as {read:#?}\nexpected status 1, no stdout and {refused:#?}",
                case.name,
                actual.code,
                placed(&actual.root, &actual.stdout),
                placed(&actual.root, &actual.stderr),
            ));
        }
        let severities: Vec<ExpectedSeverity> =
            refused.iter().map(|problem| problem.severity).collect();
        let errors = vec![ExpectedSeverity::Error; refused.len()];
        if refused.is_empty() || severities != errors {
            failures.push(format!(
                "{}: the `krusty-refusal` section must be errors only: {refused:#?}",
                case.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
        let actual = krusty_toolchain(&format!("case-{name}"), inputs);
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
        let (warnings, _) = problems(&project_root, &toolchain.stdout);
        // Each stream holds one kind of problem.
        let mixed: Vec<&ExpectedDiagnostic> = errors
            .iter()
            .filter(|problem| problem.severity != ExpectedSeverity::Error)
            .chain(
                warnings
                    .iter()
                    .filter(|problem| problem.severity == ExpectedSeverity::Error),
            )
            .collect();
        if !mixed.is_empty() {
            failures.push(format!(
                "{name}: the toolchain wrote warnings to stderr or errors to stdout: {mixed:#?}"
            ));
            continue;
        }
        let expected = (
            toolchain.code,
            placed(&toolchain.root, &toolchain.stdout),
            placed(&toolchain.root, &toolchain.stderr),
        );
        let printed = (
            actual.code,
            placed(&actual.root, &actual.stdout),
            placed(&actual.root, &actual.stderr),
        );
        match &case.krusty {
            Some(KrustyExpected::Refusal(refused)) => {
                let read = refusal(&actual);
                if read.as_ref() != Some(refused) {
                    failures.push(format!(
                        "{name}: krusty-toolchain exited {} with stdout\n{}\nstderr\n{}\nread back as {read:#?}\nthe `krusty-refusal` section expects exit 1, no stdout and {refused:#?}",
                        printed.0, printed.1, printed.2
                    ));
                }
                let not_errors: Vec<&ExpectedDiagnostic> = refused
                    .iter()
                    .filter(|problem| problem.severity != ExpectedSeverity::Error)
                    .collect();
                if refused.is_empty() || !not_errors.is_empty() {
                    failures.push(format!(
                        "{name}: the `krusty-refusal` section must be errors only: {refused:#?}"
                    ));
                }
                if expected == printed {
                    failures.push(format!(
                        "{name}: the `krusty-refusal` section is what the toolchain does"
                    ));
                }
                continue;
            }
            Some(KrustyExpected::Difference(_)) => {
                failures.push(format!(
                    "{name}: krusty-toolchain differs from the toolchain only by refusing"
                ));
                continue;
            }
            None => {}
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
