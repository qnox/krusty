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
//! all it prints. Both ledgers are checked without the oracle; the invocations of the `krusty-argv`
//! ledger are compared module by module, as krusty-toolchain compiles independent modules together
//! (the order it starts them in is checked by its own tests). Where the oracle runs, the
//! toolchain's recorded invocations must be the `krusty-argv` ledger's, compared module by module
//! (the toolchain may compile independent modules in either order), the sources of each as a set.
//! A case whose modules enable a toolchain plugin names the plugin's module in a
//! `toolchain-plugin-module` section: the toolchain compiles that module for real to run the
//! plugin's tasks (`compiling::RecordingKrustyCompilingPlugin`), and its invocation, which
//! krusty-toolchain does not make, must be recorded once and is left out of the comparison. A case
//! with neither ledger is a project both reject: krusty-toolchain must refuse it, and where the
//! oracle runs, with exactly the problems the toolchain reports.
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
const PLUGIN_SECTION: &str = "toolchain-plugin-module";
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
    /// The toolchain plugin module the toolchain compiles, and krusty-toolchain does not, to run
    /// its tasks, when there is one.
    plugin_module: Option<String>,
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
                plugin_module: None,
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
                } else if file == PLUGIN_SECTION {
                    compiled.plugin_module = Some(text.trim().to_string());
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
                compiled.argv.is_none() || compiled.refusal.is_none(),
                "{}: a case holds a `krusty-argv` or a `krusty-refusal` section, not both",
                compiled.name
            );
            compiled
        })
        .collect()
}

/// What krusty-toolchain did for `tasks` in `case`'s project, and what the recorder recorded.
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
    tasks: &[&str],
    configure: impl Fn(&mut Command),
) -> Ran {
    let temp = support::TempDir::new(&format!("compile-{run}"));
    let directory = temp.0.clone();
    let root = support::materialize(&temp, &case.project);
    Resolving::write_repository(&directory, &case.repository);
    compiling::lay_out(&directory);
    let output = run_task(&directory, &root, tasks, configure);
    let record = std::fs::read_to_string(directory.join(compiling::RECORD)).unwrap_or_default();
    Ran {
        output,
        record: placed(&directory.display().to_string(), record.as_bytes()),
        directory,
        _temp: temp,
    }
}

/// Runs krusty-toolchain's `tasks` in the project at `root`, laid out in `directory`.
fn run_task(
    directory: &Path,
    root: &Path,
    tasks: &[&str],
    configure: impl Fn(&mut Command),
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"));
    command
        .arg("task")
        .args(tasks)
        .current_dir(root)
        .env("HOME", directory.join(Resolving::HOME));
    compiling::environment(&mut command, directory, &compiling::toolchain_recorder());
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
    assert_eq!(cases.len(), 15, "the cases are missing");
    let mut failures = Vec::new();
    for case in &cases {
        let ran = krusty_toolchain(
            &format!("case-{}", case.name),
            case,
            &[case.task.as_str()],
            |_| {},
        );
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
                if !clean || by_module_text(&ran.record) != by_module_text(argv) {
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
            (None, None) => {
                // Checked against the toolchain's problems where the oracle runs.
                if refusal(actual).is_none() || !ran.record.is_empty() {
                    failures.push(format!(
                        "{}: krusty-toolchain did {printed}\na case with neither ledger expects status 1, no stdout, nothing recorded and nothing but problems",
                        case.name
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A record's invocations as written, ordered by the module each compiles.
fn by_module_text(record: &str) -> Vec<(String, String)> {
    let mut invocations = Vec::new();
    let mut current = String::new();
    for line in record.split_inclusive('\n') {
        current.push_str(line);
        if line.trim_end_matches('\n') == compiling::END_OF_INVOCATION {
            let module = current
                .lines()
                .find_map(|argument| argument.strip_prefix("-module-name="))
                .unwrap_or_default()
                .to_string();
            invocations.push((module, std::mem::take(&mut current)));
        }
    }
    invocations.push((String::new(), current));
    invocations.sort();
    invocations
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
    compiling::recorder();
    let arguments: Vec<[&str; 2]> = cases
        .iter()
        .map(|case| ["task", case.task.as_str()])
        .collect();
    let compilers: Vec<Box<dyn kotlin::Compiler>> = cases
        .iter()
        .map(|case| -> Box<dyn kotlin::Compiler> {
            match &case.plugin_module {
                Some(plugin) => Box::new(compiling::RecordingKrustyCompilingPlugin { plugin }),
                None => Box::new(compiling::RecordingKrusty),
            }
        })
        .collect();
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .zip(&arguments)
        .zip(&compilers)
        .map(|((case, args), compiler)| Invocation {
            case: &case.name,
            files: &case.project,
            args,
            repository: Some(&case.repository),
            compiler: Some(compiler.as_ref()),
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let mut failures = Vec::new();
    for (case, toolchain) in cases.iter().zip(outputs) {
        let record = placed(&toolchain.root, &toolchain.stdout);
        let Some(argv) = case.argv.as_deref() else {
            // Both reject the project, with the same problems.
            let ran = krusty_toolchain(
                &format!("rejected-{}", case.name),
                case,
                &[case.task.as_str()],
                |_| {},
            );
            let project_root = format!("{}/{}", toolchain.root, Resolving::PROJECT);
            let (rejected, unread) = problems(&project_root, &toolchain.stderr);
            let rejected: Vec<ExpectedDiagnostic> = rejected
                .into_iter()
                .map(|problem| ExpectedDiagnostic {
                    rendered: placed(&toolchain.root, problem.rendered.as_bytes()),
                    ..problem
                })
                .collect();
            let refused = refusal(&ran.output);
            if toolchain.code == 0
                || !unread.is_empty()
                || !record.is_empty()
                || refused.as_ref() != Some(&rejected)
            {
                failures.push(format!(
                    "{}: the toolchain exited {} having recorded\n{record}\nand rejected the project with\n{}krusty-toolchain refused it with\n{}",
                    case.name,
                    toolchain.code,
                    ledger(&rejected),
                    refused.as_deref().map(ledger).unwrap_or_default()
                ));
            }
            continue;
        };
        let mut recorded = by_module(&record);
        // The plugin module's invocation is the toolchain's alone: krusty-toolchain runs no task.
        let plugin_compiled = case.plugin_module.as_ref().is_none_or(|plugin| {
            let name = format!("-module-name={plugin}");
            let before = recorded.len();
            recorded.retain(|invocation| !invocation.contains(&name));
            recorded.len() + 1 == before
        });
        if toolchain.code != 0 || !plugin_compiled || recorded != by_module(argv) {
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
    let ran = krusty_toolchain("resources", &case, &[":app:compileJvm"], |command| {
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
    let first = krusty_toolchain("stale", &case, &[":app:compileJvm"], quiet);
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

    let second = run_task(&first.directory, &project, &[":app:compileJvm"], quiet);
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
    let ran = krusty_toolchain("failure", &case, &[":app:compileJvm"], |command| {
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
    let cases: [(&str, &[&str], bool, &str); 5] = [
        (
            "no-krusty",
            &[":lib:compileJvm"],
            true,
            "\nERROR: KRUSTY_BIN is not set: krusty-toolchain compiles with the krusty binary it names, and does not search PATH for one\n",
        ),
        (
            "unknown-module",
            &[":nope:compileJvm"],
            false,
            "\nERROR: Task ':nope:compileJvm' was not found in the project\n",
        ),
        (
            "not-a-task",
            &["compileJvm"],
            false,
            "\nERROR: Task 'compileJvm' was not found in the project\n",
        ),
        (
            // The toolchain names the last task it does not find.
            "unknown-modules",
            &[":nope:compileJvm", ":lib:compileJvm", ":zz:compileJvm"],
            false,
            "\nERROR: Task ':zz:compileJvm' was not found in the project\n",
        ),
        (
            "other-task",
            &[":app:runJvm"],
            false,
            "\nERROR: krusty-toolchain does not implement the task ':app:runJvm': it runs only `:<module>:compileJvm`\n",
        ),
    ];
    for (run, tasks, without_krusty, stderr) in cases {
        let ran = krusty_toolchain(run, &case, tasks, |command| {
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

    let ran = run_task(&directory, &root, &[":app:compileJvm"], |command| {
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

/// A diamond: `app` compiles against `left` and `right`, which both compile against `base`. The
/// project lists them so that its order is not the order they compile in.
fn diamond() -> CompileCase {
    let module = |dependencies: &[&str]| {
        let mut text = "product: jvm/lib\nrepositories:\n  - mavenLocal\n".to_string();
        if !dependencies.is_empty() {
            text.push_str("dependencies:\n");
            for dependency in dependencies {
                text.push_str(&format!("  - ../{dependency}\n"));
            }
        }
        text
    };
    let mut project = vec![(
        "project.yaml".to_string(),
        "modules:\n  - app\n  - base\n  - left\n  - right\n".to_string(),
    )];
    for (name, dependencies) in [
        ("app", &["left", "right"][..]),
        ("base", &[][..]),
        ("left", &["base"][..]),
        ("right", &["base"][..]),
    ] {
        project.push((format!("{name}/module.yaml"), module(dependencies)));
        project.push((
            format!("{name}/src/{name}.kt"),
            format!("fun {name}() = 1\n"),
        ));
    }
    let mut repository = Vec::new();
    files_below(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases/dependencies/repository"),
        "",
        &mut repository,
    );
    CompileCase {
        name: "diamond".to_string(),
        project,
        repository,
        task: ":app:compileJvm".to_string(),
        argv: None,
        refusal: None,
        plugin_module: None,
    }
}

/// Whether krusty-toolchain runs more than one module at a time on this host.
fn parallel() -> bool {
    std::thread::available_parallelism().is_ok_and(|count| count.get() > 1)
}

/// What a run did: its status, stdout, stderr, and its recorder's events.
fn timed_run(
    run: &str,
    tasks: &[&str],
    configure: impl Fn(&mut Command),
) -> (i32, String, String, Vec<String>) {
    let case = diamond();
    let temp = support::TempDir::new(&format!("compile-{run}"));
    let events = temp.0.join("events");
    let ran = krusty_toolchain(run, &case, tasks, |command| {
        command
            .env("KRUSTY_EVENTS", &events)
            .env("KRUSTY_RECORDER_SLEEP", "0.4");
        configure(command);
    });
    let events = std::fs::read_to_string(&events).unwrap_or_default();
    (
        ran.output.code,
        placed(&ran.output.root, &ran.output.stdout),
        placed(&ran.output.root, &ran.output.stderr),
        events.lines().map(str::to_string).collect(),
    )
}

/// `events` with the events from each `(start, end)` index range of `together` sorted: modules
/// that run at the same time start and end in either order.
fn settled(mut events: Vec<String>, together: &[(usize, usize)]) -> Vec<String> {
    for &(start, end) in together {
        events[start..end].sort();
    }
    events
}

fn lines(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

/// What a module the recorder sleeps in prints on stderr.
fn sleeper(module: &str) -> String {
    format!("recorder: {module} begins\nrecorder: {module} ends\n")
}

#[test]
fn independent_modules_compile_together_after_what_they_compile_against() {
    compiling::recorder();
    let (code, stdout, stderr, events) = timed_run("diamond", &[":app:compileJvm"], |_| {});
    // Each module's output stays together, in plan order whichever finishes first.
    let order = ["base", "left", "right", "app"];
    assert_eq!(
        (code, stdout, stderr),
        (
            0,
            order
                .iter()
                .map(|module| format!("recorder: compiled {module}\n"))
                .collect(),
            order.iter().map(|module| sleeper(module)).collect(),
        )
    );
    if parallel() {
        assert_eq!(
            settled(events, &[(2, 4), (4, 6)]),
            lines(&[
                "start base",
                "end base",
                "start left",
                "start right",
                "end left",
                "end right",
                "start app",
                "end app",
            ])
        );
    } else {
        assert_eq!(
            events,
            lines(&[
                "start base",
                "end base",
                "start left",
                "end left",
                "start right",
                "end right",
                "start app",
                "end app",
            ])
        );
    }
}

#[test]
fn a_failure_starts_no_module_and_waits_for_the_running_ones() {
    compiling::recorder();
    let (code, stdout, stderr, events) =
        timed_run("diamond-failure", &[":app:compileJvm"], |command| {
            command.env("KRUSTY_RECORDER_FAIL", "left");
        });
    let failed = "recorder: failing left\n";
    let error = "\nERROR: Task ':left:compileJvm' failed: krusty exited with status 3\n";
    if parallel() {
        // `right` runs while `left` fails at once: it finishes, and `app` never starts.
        assert_eq!(
            (code, stdout, stderr, settled(events, &[(2, 4)])),
            (
                3,
                "recorder: compiled base\nrecorder: compiled right\n".to_string(),
                format!("{}{failed}{}{error}", sleeper("base"), sleeper("right")),
                lines(&[
                    "start base",
                    "end base",
                    "start left",
                    "start right",
                    "end left",
                    "end right"
                ]),
            )
        );
    } else {
        assert_eq!(
            (code, stdout, stderr, events),
            (
                3,
                "recorder: compiled base\n".to_string(),
                format!("{}{failed}{error}", sleeper("base")),
                lines(&["start base", "end base", "start left", "end left"]),
            )
        );
    }
}

/// Two sibling modules fail: `left`, planned first, slowly with status 4, and `right` at once with
/// status 3. What is printed, and the status, follow the plan, not which fails first.
#[test]
fn concurrent_failures_are_reported_in_plan_order() {
    compiling::recorder();
    for attempt in 0..4 {
        let ran = timed_run(
            &format!("siblings-{attempt}"),
            &[":app:compileJvm"],
            |command| {
                command
                    .env("KRUSTY_RECORDER_FAIL", "right")
                    .env("KRUSTY_RECORDER_FAIL_LATE", "left");
            },
        );
        let base = "recorder: compiled base\n".to_string();
        let expected = if parallel() {
            (
                4,
                base,
                format!(
                    "{}{}recorder: failing left late\nrecorder: failing right\n\n\
                     ERROR: Task ':left:compileJvm' failed: krusty exited with status 4\n\n\
                     ERROR: Task ':right:compileJvm' failed: krusty exited with status 3\n",
                    sleeper("base"),
                    sleeper("left")
                ),
                lines(&[
                    "start base",
                    "end base",
                    "start left",
                    "start right",
                    "end right",
                    "end left",
                ]),
            )
        } else {
            (
                4,
                base,
                format!(
                    "{}{}recorder: failing left late\n\n\
                     ERROR: Task ':left:compileJvm' failed: krusty exited with status 4\n",
                    sleeper("base"),
                    sleeper("left")
                ),
                lines(&["start base", "end base", "start left", "end left"]),
            )
        };
        let (code, stdout, stderr, events) = ran;
        // `left` and `right` start together; `right` ends first.
        let events = if parallel() {
            settled(events, &[(2, 4)])
        } else {
            events
        };
        assert_eq!(
            (code, stdout, stderr, events),
            expected,
            "attempt {attempt}"
        );
    }
}

#[test]
fn several_tasks_compile_the_union_of_what_they_need_once() {
    compiling::recorder();
    let case = diamond();
    // A target named twice, and a dependency both targets share.
    let tasks = [":right:compileJvm", ":left:compileJvm", ":right:compileJvm"];
    let ran = krusty_toolchain("union", &case, &tasks, |_| {});
    let mut compiled = modules(&ran.record);
    // `left` and `right` compile together, in either order, after `base`.
    compiled[1..].sort();
    assert_eq!(
        (
            ran.output.code,
            placed(&ran.output.root, &ran.output.stdout),
            placed(&ran.output.root, &ran.output.stderr),
            compiled
        ),
        (
            0,
            String::new(),
            String::new(),
            lines(&["base", "left", "right"])
        )
    );
    // The toolchain, given the same tasks, compiles the same modules with the same arguments.
    let mut arguments = vec!["task"];
    arguments.extend(tasks);
    let toolchain = kotlin::kotlin(&Invocation {
        case: "diamond-union",
        files: &case.project,
        args: &arguments,
        repository: Some(&case.repository),
        compiler: Some(&compiling::RecordingKrusty),
    });
    let recorded = placed(&toolchain.root, &toolchain.stdout);
    assert_eq!(
        (toolchain.code, by_module(&recorded)),
        (0, by_module(&ran.record)),
        "the toolchain printed\n{}",
        placed(&toolchain.root, &toolchain.stderr)
    );
}
