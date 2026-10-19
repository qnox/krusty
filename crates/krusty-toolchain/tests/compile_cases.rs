//! `krusty-toolchain task :<module>:compileJvm` gives krusty exactly the arguments JetBrains'
//! `kotlin task` gives the Kotlin compiler, for each module the task compiles; what it does not
//! implement it refuses, before compiling anything. Both run as `support/compiling.rs` lays a run
//! out, with a krusty that records its arguments.
//!
//! A case in `tests/cases/compile` holds the project, its local Maven repository (`m2/<path>`
//! sections) and the task to run (`task`), added to the shared repository of
//! `tests/cases/dependencies/repository`. Its `krusty-argv` section is krusty-toolchain's ledger of
//! the recorded invocations, in the order it runs them, each run's directory written
//! `<directory>`; a `krusty-refusal` section instead holds the errors it refuses with, which must be
//! all it prints. Both ledgers are checked without the oracle. Where the oracle runs, the
//! toolchain's recorded invocations must be the `krusty-argv` ledger's, compared module by module
//! (the toolchain may compile independent modules in either order), the sources of each as a set.
//!
//! One test compiles with the real krusty instead of the recorder: a library and an application
//! that uses it, whose outputs are listed and whose program is run on the JDK it compiled against.

mod support;

#[path = "support/compiling.rs"]
mod compiling;

use std::path::{Path, PathBuf};
use std::process::Command;

use support::kotlin::{self, Invocation, Resolving};
use support::oracle::Output;
use support::rendering::problems;
use support::{ExpectedDiagnostic, ExpectedSeverity, KrustyExpected};

const REPOSITORY_SECTION: &str = "m2/";
const TASK_SECTION: &str = "task";
const ARGV_SECTION: &str = "krusty-argv";
const PLACEHOLDER: &str = "<directory>";

type Files = Vec<(String, String)>;

/// A case, its sections sorted by what they are.
struct CompileCase {
    name: String,
    project: Files,
    repository: Files,
    task: String,
    argv: Option<String>,
    refusal: Option<Vec<ExpectedDiagnostic>>,
}

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

fn compile_cases() -> Vec<CompileCase> {
    let mut shared = Vec::new();
    files_below(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/dependencies/repository"),
        "",
        &mut shared,
    );
    support::cases("compile")
        .into_iter()
        .map(|case| {
            let mut compiled = CompileCase {
                name: case.name.clone(),
                project: Vec::new(),
                repository: shared.clone(),
                task: String::new(),
                argv: None,
                refusal: match case.krusty {
                    Some(KrustyExpected::Refusal(refused)) => Some(refused),
                    Some(KrustyExpected::Difference(_)) => {
                        panic!("{}: a compile case refuses or matches", case.name)
                    }
                    None => None,
                },
            };
            for (file, text) in case.files {
                if let Some(file) = file.strip_prefix(REPOSITORY_SECTION) {
                    compiled.repository.push((file.to_string(), text));
                } else if file == TASK_SECTION {
                    compiled.task = text.trim().to_string();
                } else if file == ARGV_SECTION {
                    compiled.argv = Some(text);
                } else {
                    compiled.project.push((file, text));
                }
            }
            assert!(
                !compiled.task.is_empty(),
                "{}: the case names no task",
                compiled.name
            );
            assert!(
                compiled.argv.is_some() != compiled.refusal.is_some(),
                "{}: a case holds either a `krusty-argv` or a `krusty-refusal` section",
                compiled.name
            );
            compiled
        })
        .collect()
}

/// What krusty-toolchain did for `task` in `case`'s project, and what the recorder recorded.
struct Ran {
    output: Output,
    record: String,
    directory: PathBuf,
    /// Removes the run's directory when dropped.
    _temp: support::TempDir,
}

fn krusty_toolchain(
    run: &str,
    case: &CompileCase,
    task: &str,
    configure: impl Fn(&mut Command),
) -> Ran {
    let temp = support::TempDir::new(&format!("compile-{run}"));
    let directory = temp.0.clone();
    let root = support::materialize(&temp, &case.project);
    Resolving::write_repository(&directory, &case.repository);
    compiling::lay_out(&directory);
    let output = run_task(&directory, &root, task, configure);
    let record = std::fs::read_to_string(directory.join(compiling::RECORD)).unwrap_or_default();
    Ran {
        output,
        record: placed(&directory.display().to_string(), record.as_bytes()),
        directory,
        _temp: temp,
    }
}

/// Runs krusty-toolchain's `task` in the project at `root`, laid out in `directory`.
fn run_task(directory: &Path, root: &Path, task: &str, configure: impl Fn(&mut Command)) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"));
    command
        .args(["task", task])
        .current_dir(root)
        .env("HOME", directory.join(Resolving::HOME));
    compiling::environment(&mut command, directory);
    configure(&mut command);
    let ran = command.output().expect("run krusty-toolchain");
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

fn placed(directory: &str, bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace(directory, PLACEHOLDER)
}

/// The problems krusty-toolchain refused with, read back as the toolchain's are; `None` unless it
/// exited 1 with nothing on stdout and nothing but problems on stderr.
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

/// A ledger line for each problem, to paste into a `krusty-refusal` section.
fn ledger(problems: &[ExpectedDiagnostic]) -> String {
    problems
        .iter()
        .map(|problem| {
            let severity = match problem.severity {
                ExpectedSeverity::Error => "error",
                ExpectedSeverity::Warning => "warning",
                ExpectedSeverity::WeakWarning => "weak-warning",
            };
            format!("{severity}\t{}\n", problem.rendered)
        })
        .collect()
}

#[test]
fn every_compile_case_is_exact() {
    compiling::recorder();
    let cases = compile_cases();
    assert_eq!(cases.len(), 11, "the cases are missing");
    let mut failures = Vec::new();
    for case in &cases {
        let ran = krusty_toolchain(&format!("case-{}", case.name), case, &case.task, |_| {});
        let actual = &ran.output;
        let printed = format!(
            "status {}, stdout\n{}stderr\n{}record\n{}",
            actual.code,
            placed(&actual.root, &actual.stdout),
            placed(&actual.root, &actual.stderr),
            ran.record
        );
        match (&case.argv, &case.refusal) {
            (Some(argv), _) => {
                let clean =
                    actual.code == 0 && actual.stdout.is_empty() && actual.stderr.is_empty();
                if !clean || ran.record != *argv {
                    failures.push(format!(
                        "{}: krusty-toolchain did {printed}\nthe `krusty-argv` section expects status 0, no output and the record\n{argv}",
                        case.name
                    ));
                }
            }
            (None, Some(refused)) => {
                let read = refusal(actual);
                if read.as_ref() != Some(refused) || !ran.record.is_empty() {
                    failures.push(format!(
                        "{}: krusty-toolchain did {printed}\nread back as\n{}the `krusty-refusal` section expects status 1, no stdout, nothing recorded and\n{}",
                        case.name,
                        read.as_deref().map(ledger).unwrap_or_default(),
                        ledger(refused)
                    ));
                }
                if refused
                    .iter()
                    .any(|problem| problem.severity != ExpectedSeverity::Error)
                {
                    failures.push(format!(
                        "{}: the `krusty-refusal` section must be errors only",
                        case.name
                    ));
                }
            }
            (None, None) => unreachable!("checked when the cases are read"),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Invocations by the module each compiles.
fn by_module(record: &str) -> Vec<Vec<String>> {
    let mut invocations = compiling::invocations(record);
    invocations.sort_by_key(|invocation| {
        invocation
            .iter()
            .find(|argument| argument.starts_with("-module-name="))
            .cloned()
    });
    invocations
}

#[test]
fn every_compile_case_passes_the_arguments_the_toolchain_passes() {
    let cases: Vec<CompileCase> = compile_cases()
        .into_iter()
        .filter(|case| case.refusal.is_none())
        .collect();
    let arguments: Vec<[&str; 2]> = cases
        .iter()
        .map(|case| ["task", case.task.as_str()])
        .collect();
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .zip(&arguments)
        .map(|(case, args)| Invocation {
            case: &case.name,
            files: &case.project,
            args,
            repository: Some(&case.repository),
            compiler: Some(&compiling::RecordingKrusty),
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let mut failures = Vec::new();
    for (case, toolchain) in cases.iter().zip(outputs) {
        let record = placed(&toolchain.root, &toolchain.stdout);
        let argv = case.argv.as_deref().unwrap_or_default();
        if toolchain.code != 0 || by_module(&record) != by_module(argv) {
            failures.push(format!(
                "{}: the toolchain exited {} having printed\n{}and recorded\n{record}\nthe `krusty-argv` section is\n{argv}",
                case.name,
                toolchain.code,
                placed(&toolchain.root, &toolchain.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

fn case(name: &str) -> CompileCase {
    compile_cases()
        .into_iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no compile case `{name}`"))
}

/// The record's invocations' module names, in order.
fn modules(record: &str) -> Vec<String> {
    compiling::invocations(record)
        .iter()
        .filter_map(|invocation| {
            invocation
                .iter()
                .find_map(|argument| argument.strip_prefix("-module-name="))
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn outputs_are_cleared_and_resources_copied() {
    compiling::recorder();
    let case = case("lib-and-app");
    let ran = krusty_toolchain("resources", &case, ":app:compileJvm", |command| {
        command.env("KRUSTY_RECORDER_FAIL", "none");
    });
    assert_eq!(
        ran.output.code,
        0,
        "{}",
        placed(&ran.output.root, &ran.output.stderr)
    );
    let artifacts = ran
        .directory
        .join(Resolving::PROJECT)
        .join("build/artifacts/CompiledJvmArtifact");
    let mut found = Vec::new();
    files_below(&artifacts, "", &mut found);
    assert_eq!(
        found,
        [(
            "libjvm/resources-output/greeting.txt".to_string(),
            "hello\n".to_string()
        )]
    );
    for module in ["appjvm", "libjvm"] {
        assert!(
            artifacts.join(module).join("kotlin-output").is_dir(),
            "{module}"
        );
    }
}

/// A module whose last source and resources directory are gone keeps nothing an earlier run put in
/// its output directories.
#[test]
fn a_rerun_leaves_no_output_of_removed_sources_or_resources() {
    compiling::recorder();
    let case = case("lib-and-app");
    let quiet = |command: &mut Command| {
        command.env("KRUSTY_RECORDER_FAIL", "none");
    };
    let first = krusty_toolchain("stale", &case, ":app:compileJvm", quiet);
    assert_eq!(
        first.output.code,
        0,
        "{}",
        placed(&first.output.root, &first.output.stderr)
    );
    let project = first.directory.join(Resolving::PROJECT);
    let artifacts = project.join("build/artifacts/CompiledJvmArtifact");
    // What krusty would have emitted for the library on the first run.
    std::fs::write(
        artifacts.join("libjvm/kotlin-output/Greeting.class"),
        b"class",
    )
    .unwrap();
    std::fs::write(
        artifacts.join("appjvm/kotlin-output/MainKt.class"),
        b"class",
    )
    .unwrap();
    std::fs::remove_file(project.join("lib/src/Greeting.kt")).unwrap();
    std::fs::remove_dir_all(project.join("lib/resources")).unwrap();

    let second = run_task(&first.directory, &project, ":app:compileJvm", quiet);
    assert_eq!(second.code, 0, "{}", placed(&second.root, &second.stderr));
    let mut found = Vec::new();
    files_below(&artifacts, "", &mut found);
    assert_eq!(found, Files::new(), "no stale classes or resources");
    for gone in ["libjvm/kotlin-output", "libjvm/resources-output"] {
        assert!(!artifacts.join(gone).exists(), "{gone}");
    }
    assert!(artifacts.join("appjvm/kotlin-output").is_dir());
}

#[test]
fn the_first_failing_compilation_stops_the_task_with_its_status() {
    compiling::recorder();
    let case = case("exported-chain");
    let ran = krusty_toolchain("failure", &case, ":app:compileJvm", |command| {
        command.env("KRUSTY_RECORDER_FAIL", "base");
    });
    assert_eq!(
        (
            ran.output.code,
            placed(&ran.output.root, &ran.output.stdout),
            placed(&ran.output.root, &ran.output.stderr),
            modules(&ran.record)
        ),
        (
            3,
            String::new(),
            "recorder: failing base\n\nERROR: Task ':base:compileJvm' failed: krusty exited with status 3\n"
                .to_string(),
            vec!["hidden".to_string(), "base".to_string()]
        )
    );
}

#[test]
fn a_task_needs_krusty_and_a_task_krusty_toolchain_runs() {
    compiling::recorder();
    let case = case("lib-and-app");
    let cases: [(&str, &str, bool, &str); 4] = [
        (
            "no-krusty",
            ":lib:compileJvm",
            true,
            "\nERROR: KRUSTY_BIN is not set: krusty-toolchain compiles with the krusty binary it names, and does not search PATH for one\n",
        ),
        (
            "unknown-module",
            ":nope:compileJvm",
            false,
            "\nERROR: Task ':nope:compileJvm' was not found in the project\n",
        ),
        (
            "not-a-task",
            "compileJvm",
            false,
            "\nERROR: Task 'compileJvm' was not found in the project\n",
        ),
        (
            "other-task",
            ":app:runJvm",
            false,
            "\nERROR: krusty-toolchain does not implement the task ':app:runJvm': it runs only `:<module>:compileJvm`\n",
        ),
    ];
    for (run, task, without_krusty, stderr) in cases {
        let ran = krusty_toolchain(run, &case, task, |command| {
            if without_krusty {
                command.env_remove("KRUSTY_BIN");
            }
        });
        assert_eq!(
            (
                ran.output.code,
                placed(&ran.output.root, &ran.output.stdout),
                placed(&ran.output.root, &ran.output.stderr),
                ran.record
            ),
            (1, String::new(), stderr.to_string(), String::new()),
            "{run}"
        );
    }
}

/// What the end-to-end compile needs beyond krusty-toolchain: the krusty binary the harness built
/// for this checkout (`KRUSTY_BIN`, as `run-tests.sh` and the coverage script export it), a JDK
/// (`JAVA_HOME`) and its major version, and the Kotlin distribution's standard library and
/// annotations jars (beside `KRUSTY_KOTLINC`'s `bin`, else in the workspace's provisioned reference
/// compiler).
struct RealCompile {
    krusty: PathBuf,
    java_home: PathBuf,
    jdk_major: u32,
    kotlin_lib: PathBuf,
}

impl RealCompile {
    fn find() -> Option<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // The toolchain runs krusty from each module's directory, so a relative `KRUSTY_BIN` (the
        // coverage harness exports one) is made absolute here.
        let krusty = std::env::var_os("KRUSTY_BIN")
            .and_then(|path| std::fs::canonicalize(path).ok())
            .filter(|path| path.is_file())?;
        let java_home = PathBuf::from(std::env::var_os("JAVA_HOME")?);
        let release = std::fs::read_to_string(java_home.join("release")).ok()?;
        let version = release
            .lines()
            .find_map(|line| line.strip_prefix("JAVA_VERSION=\""))?;
        let jdk_major = version.split(['.', '"']).next()?.parse().ok()?;
        let kotlin_lib = std::env::var_os("KRUSTY_KOTLINC")
            .and_then(|kotlinc| Some(Path::new(&kotlinc).parent()?.parent()?.join("lib")))
            .into_iter()
            .chain(
                std::fs::read_dir(workspace.join("target/cache/kotlinc"))
                    .into_iter()
                    .flatten()
                    .map(|entry| entry.unwrap().path().join("kotlinc/lib")),
            )
            .find(|lib| lib.join("kotlin-stdlib.jar").is_file())?;
        Some(Self {
            krusty,
            java_home,
            jdk_major,
            kotlin_lib,
        })
    }
}

/// The files below `directory`, by `/`-separated path, sorted.
fn listing(directory: &Path) -> Vec<String> {
    fn walk(directory: &Path, prefix: &str, names: &mut Vec<String>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), names);
            } else {
                names.push(name);
            }
        }
    }
    let mut names = Vec::new();
    walk(directory, "", &mut names);
    names.sort();
    names
}

#[test]
fn the_real_krusty_compiles_a_library_and_the_application_that_uses_it() {
    let Some(real) = RealCompile::find() else {
        eprintln!(
            "skipping the end-to-end compile: needs the krusty binary (`KRUSTY_BIN`), a JDK and the Kotlin standard library"
        );
        return;
    };
    let settings = format!(
        "settings:\n  jvm:\n    jdk:\n      version: {}\n",
        real.jdk_major
    );
    let project: Files = vec![
        ("project.yaml".into(), "modules:\n  - app\n  - lib\n".into()),
        (
            "app/module.yaml".into(),
            format!("product: jvm/app\nrepositories:\n  - mavenLocal\ndependencies:\n  - ../lib\n{settings}"),
        ),
        (
            "app/src/main.kt".into(),
            "import greeting.Greeter\n\nfun main() = println(Greeter(\"world\").greet() + \"!\")\n"
                .into(),
        ),
        (
            "lib/module.yaml".into(),
            format!("product: jvm/lib\nrepositories:\n  - mavenLocal\n{settings}"),
        ),
        (
            "lib/src/Greeter.kt".into(),
            "package greeting\n\nclass Greeter(private val name: String) {\n    fun greet() = \"hello, $name\"\n}\n"
                .into(),
        ),
    ];
    let mut repository = Vec::new();
    files_below(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/dependencies/repository"),
        "",
        &mut repository,
    );
    let temp = support::TempDir::new("compile-real-krusty");
    let directory = temp.0.clone();
    let root = support::materialize(&temp, &project);
    Resolving::write_repository(&directory, &repository);
    let local = directory.join(Resolving::HOME).join(".m2/repository");
    let stdlib = local.join("org/jetbrains/kotlin/kotlin-stdlib/2.4.20/kotlin-stdlib-2.4.20.jar");
    let annotations = local.join("org/jetbrains/annotations/13.0/annotations-13.0.jar");
    std::fs::copy(real.kotlin_lib.join("kotlin-stdlib.jar"), &stdlib).unwrap();
    std::fs::copy(real.kotlin_lib.join("annotations-13.0.jar"), &annotations).unwrap();
    compiling::lay_out(&directory);

    let ran = run_task(&directory, &root, ":app:compileJvm", |command| {
        command
            .env("JAVA_HOME", &real.java_home)
            .env("KRUSTY_BIN", &real.krusty);
    });
    assert_eq!(
        (ran.code, placed(&ran.root, &ran.stdout), placed(&ran.root, &ran.stderr)),
        (
            0,
            format!(
                "ok: emitted 1 class file(s) to {PLACEHOLDER}/project/build/artifacts/CompiledJvmArtifact/libjvm/kotlin-output\n\
                 ok: emitted 1 class file(s) to {PLACEHOLDER}/project/build/artifacts/CompiledJvmArtifact/appjvm/kotlin-output\n"
            ),
            String::new()
        ),
        "krusty-toolchain compiles the library, then the application, printing what krusty prints"
    );
    let artifacts = root.join("build/artifacts/CompiledJvmArtifact");
    let lib = artifacts.join("libjvm/kotlin-output");
    let app = artifacts.join("appjvm/kotlin-output");
    assert_eq!(
        listing(&lib),
        ["META-INF/lib.kotlin_module", "greeting/Greeter.class"]
    );
    assert_eq!(
        listing(&app),
        ["META-INF/app.kotlin_module", "MainKt.class"]
    );

    let separator = if cfg!(windows) { ";" } else { ":" };
    let classpath = [&app, &lib, &stdlib]
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(separator);
    let executed = Command::new(real.java_home.join("bin/java"))
        .args(["-cp", &classpath, "MainKt"])
        .env_remove("JAVA_TOOL_OPTIONS")
        .env_remove("JDK_JAVA_OPTIONS")
        .env_remove("_JAVA_OPTIONS")
        .output()
        .expect("run the application");
    assert_eq!(
        (
            executed.status.code(),
            String::from_utf8_lossy(&executed.stdout).into_owned(),
            String::from_utf8_lossy(&executed.stderr).into_owned()
        ),
        (Some(0), "hello, world!\n".to_string(), String::new()),
        "the application runs with the library's behavior"
    );
}
