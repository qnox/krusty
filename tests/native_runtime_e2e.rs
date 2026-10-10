//! The native runtime, run on the host.
//!
//! The runtime under `src/native/runtime/` is freestanding C that `build.rs` compiles for every
//! target and krusty links into a user's program. Compiling it proves only that it compiles; this
//! harness RUNS it. Each driver under `tests/native_runtime/` is a small freestanding program that
//! supplies `kt_program_entry`, calls into the runtime, and prints `OK` when what it checked held.
//! The harness links the driver with every runtime source on the branch — with the host's clang,
//! `-nostdlib -static`, so nothing but the runtime itself answers its symbols — and runs it. The
//! runtime's sources are compiled once per test run, with the drivers' own flags, and every driver
//! links all of their objects.
//!
//! A driver reports failure by exiting non-zero with a message on stderr (`KT_SYS_FAIL`), or by
//! crashing; either fails the test with what it printed. A driver that checks the runtime ENDS the
//! program, as it does on exhausted memory, is instead expected to exit with the runtime's failure
//! status and exactly the runtime's message; anything the driver prints itself fails the test.
//!
//! A driver whose expected answers are Kotlin's prints them as a transcript instead of comparing
//! them with values copied into C, and the harness compares that transcript with the one the Kotlin
//! program beside it (`<driver>.kt`) answers under the reference kotlinc
//! (`run_driver_against_kotlin`). Where the native runtime answers differently from the JVM on
//! purpose, the test declares the line (`run_driver_against_kotlin_with`, `Divergence`). kotlinc
//! compiles every such program once per test run, in one invocation (`kotlin_programs`).
//!
//! The drivers need a C compiler for the host. CI has one and must run them; a local build without
//! clang is told why they did not run rather than failing on a missing tool.

use super::common;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

/// Whether every function the runtime sources on this branch call is defined by them. The runtime
/// lands in tiers, and a tier below the last one calls functions a later tier defines; those links
/// leave the missing symbols unresolved (a driver that reaches one crashes, it does not pass). The
/// tier that completes the runtime turns this on, and from then on a missing definition fails the
/// link.
const RUNTIME_COMPLETE: bool = true;

/// Warnings a tier below the last one cannot help giving. Such a tier DECLARES the internal
/// functions a later tier defines, and defines helpers only a later tier's code calls; the tier that
/// completes the runtime turns `RUNTIME_COMPLETE` on and with it every one of these back into an
/// error.
const INCOMPLETE_RUNTIME_WARNINGS: &[&str] = &[
    "-Wno-undefined-internal",
    "-Wno-unused-function",
    "-Wno-unused-const-variable",
];

fn runtime_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/runtime")
}

fn driver_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_runtime")
}

/// Whether the host is a target the runtime supports and a clang is there to build for it. Asked
/// once per test run.
fn host_can_run() -> bool {
    static CAN_RUN: OnceLock<bool> = OnceLock::new();
    *CAN_RUN.get_or_init(|| {
        let supported = cfg!(target_os = "linux")
            && cfg!(any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            ));
        let clang = Command::new("clang")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if supported && clang {
            return true;
        }
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must run the native runtime drivers, but this host cannot (supported target: \
             {supported}, clang: {clang})"
        );
        eprintln!(
            "native runtime drivers skipped: this host has no clang or is not a runtime target"
        );
        false
    })
}

/// The flags the runtime and every driver compile with, the same for both. Linking adds only
/// `-Wl,--unresolved-symbols=ignore-all` while the runtime is incomplete.
fn compile_flags() -> Vec<&'static str> {
    let mut flags = vec![
        "-std=c11",
        "-ffreestanding",
        "-nostdlib",
        "-static",
        "-fno-pic",
        "-fno-stack-protector",
        "-fno-asynchronous-unwind-tables",
        "-O2",
        "-Wall",
        "-Wextra",
        "-Werror",
        // Runtime descriptors name the fields they define and intentionally leave the rest
        // zero-initialized. Keep every other warning an error.
        "-Wno-missing-field-initializers",
        // Signed overflow is undefined in C, and Kotlin's `Int` and `Long` wrap or raise; a
        // driver that drives a runtime counter past its maximum must see an overflow the
        // runtime left signed, so every one traps (SIGILL) instead of wrapping quietly.
        "-fsanitize=signed-integer-overflow",
        "-fsanitize-trap=signed-integer-overflow",
    ];
    if !RUNTIME_COMPLETE {
        flags.extend_from_slice(INCOMPLETE_RUNTIME_WARNINGS);
    }
    flags
}

/// Every runtime source compiled to an object, once per test run and in the sources' sorted order,
/// or what clang said when one did not compile. The sources compile in parallel; each driver then
/// links all of the objects, as it would link the sources themselves.
fn runtime_objects() -> &'static Result<Vec<PathBuf>, String> {
    static OBJECTS: OnceLock<Result<Vec<PathBuf>, String>> = OnceLock::new();
    OBJECTS.get_or_init(|| {
        let mut sources: Vec<PathBuf> = fs::read_dir(runtime_dir())
            .expect("read the runtime directory")
            .map(|entry| entry.expect("runtime directory entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
            .collect();
        sources.sort();
        let scratch = common::scratch_dir().expect("scratch directory");
        let compiles: Vec<(PathBuf, std::process::Child)> = sources
            .iter()
            .map(|source| {
                let object = scratch
                    .join(source.file_name().expect("runtime source name"))
                    .with_extension("o");
                let child = Command::new("clang")
                    .args(compile_flags())
                    .arg("-I")
                    .arg(runtime_dir())
                    .arg("-c")
                    .arg(source)
                    .arg("-o")
                    .arg(&object)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("run clang");
                (object, child)
            })
            .collect();
        let mut objects = Vec::new();
        let mut errors = String::new();
        for (object, child) in compiles {
            let output = child.wait_with_output().expect("run clang");
            if !output.status.success() {
                errors.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            objects.push(object);
        }
        if errors.is_empty() {
            Ok(objects)
        } else {
            Err(errors)
        }
    })
}

/// Link `driver` with every runtime object and run it. `None` when the host cannot run drivers at
/// all.
fn build_and_run(driver: &str) -> Option<Output> {
    if !host_can_run() {
        return None;
    }
    let objects = runtime_objects().as_ref().unwrap_or_else(|errors| {
        panic!("{driver}: the driver and runtime did not build:\n{errors}")
    });
    let scratch = common::scratch_dir().expect("scratch directory");
    let executable = scratch.join(driver);
    let build = Command::new("clang")
        .args(compile_flags())
        .args((!RUNTIME_COMPLETE).then_some("-Wl,--unresolved-symbols=ignore-all"))
        .arg("-I")
        .arg(runtime_dir())
        .args(objects)
        .arg(driver_dir().join(format!("{driver}.c")))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run clang");
    assert!(
        build.status.success(),
        "{driver}: the driver and runtime did not build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    // A driver can read its own name from the environment, which is how one checks `getenv`.
    Some(
        Command::new(&executable)
            .env("KRUSTY_DRIVER", driver)
            .output()
            .expect("run the driver"),
    )
}

/// Run `driver`, which must succeed EXACTLY: exit status 0, stdout exactly `OK\n`, and nothing on
/// stderr. A driver checks what it checks and says `OK` once; any other output means it printed
/// something it should not have, which a looser test would let through.
fn run_driver(driver: &str) {
    let Some(output) = build_and_run(driver) else {
        return;
    };
    assert_ok(driver, &output);
}

fn assert_ok(driver: &str, output: &Output) {
    assert!(
        output.status.success() && output.stdout == b"OK\n" && output.stderr.is_empty(),
        "{driver}: expected status 0, stdout \"OK\\n\" and an empty stderr, got {}\n\
         stdout (first 200 bytes): {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(200)]),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `driver`, which writes a payload and then `OK\n`: it must exit 0 with nothing on stderr and
/// stdout ending in `OK\n`. Returns the payload before that `OK\n` for the test to check EXACTLY;
/// only a driver whose output is itself the subject uses this. `None` when the host cannot run
/// drivers at all.
fn run_payload_driver(driver: &str) -> Option<Vec<u8>> {
    let output = build_and_run(driver)?;
    let stdout = &output.stdout;
    assert!(
        output.status.success() && stdout.ends_with(b"OK\n") && output.stderr.is_empty(),
        "{driver}: expected status 0, stdout ending in \"OK\\n\" and an empty stderr, got {}\n\
         stdout (last 200 bytes): {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&stdout[stdout.len().saturating_sub(200)..]),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(stdout[..stdout.len() - b"OK\n".len()].to_vec())
}

/// Run `driver` against Kotlin: the program `tests/native_runtime/<driver>.kt` answers, in its
/// `box()`, the transcript of what the driver observes (one observation per line, each ending in a
/// newline), and the driver prints its own transcript before its `OK` (`transcript.h`). The program
/// is compiled by the reference kotlinc and run on the shared JVM through the persistent harness
/// (`kotlin_programs`, `common::run_box`); it must compile and answer whole lines, and the driver must
/// succeed exactly as `run_payload_driver` requires. The two transcripts must then be identical, so
/// every answer the driver prints is Kotlin's by execution, not a value copied into C. The driver's
/// own checks of what Kotlin has no counterpart for still end it on failure.
fn run_driver_against_kotlin(driver: &str) {
    run_driver_against_kotlin_with(driver, &[]);
}

/// Every Kotlin program beside a driver, compiled by the reference kotlinc once per test run: the
/// programs that take the same kotlinc arguments compile together, in ONE invocation, since a
/// compile's fixed cost outweighs a small program's. Each program compiles in a package named after
/// its driver (`package <driver>;` opens the line after its file annotations, so every line keeps
/// its number), which keeps one program's declarations from colliding with another's; its `box()`
/// is then `<driver>.MainKt`, and only a qualified name it prints could tell. Maps each driver to
/// the class directory holding its program alone. A program in no batch that compiled — kotlinc
/// rejected one of the batch, say — is missing, and its test compiles it by itself
/// (`common::kotlinc_box_result`), so kotlinc's diagnostics fail that test as they always have.
fn kotlin_programs() -> &'static HashMap<String, PathBuf> {
    static PROGRAMS: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    PROGRAMS.get_or_init(|| {
        let work = common::scratch_dir().expect("scratch directory");
        // Each batch's kotlinc arguments and class path, and its drivers' packaged programs.
        type Batch = Vec<(String, PathBuf)>;
        let mut batches: BTreeMap<(Vec<String>, Vec<PathBuf>), Batch> = BTreeMap::new();
        for entry in fs::read_dir(driver_dir()).expect("read the driver directory") {
            let program = entry.expect("driver directory entry").path();
            if program
                .extension()
                .is_none_or(|extension| extension != "kt")
            {
                continue;
            }
            let driver = program
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("a UTF-8 program name")
                .to_string();
            let Ok(source) = fs::read_to_string(&program) else {
                continue;
            };
            let Some(packaged) = in_package(&driver, &source) else {
                continue;
            };
            let file = work.join("src").join(&driver).join("Main.kt");
            fs::create_dir_all(file.parent().expect("program directory"))
                .expect("create the program directory");
            fs::write(&file, packaged).expect("write the packaged program");
            let key = (
                common::language_directives::kotlinc_args(&source),
                program_classpath(&source),
            );
            batches.entry(key).or_default().push((driver, file));
        }
        let mut programs = HashMap::new();
        for (index, ((arguments, classpath), members)) in batches.into_iter().enumerate() {
            let output = work.join(format!("out-{index}"));
            let mut kotlinc: Vec<String> = members
                .iter()
                .map(|(_, file)| file.to_string_lossy().into_owned())
                .collect();
            kotlinc.extend(["-d".to_string(), output.to_string_lossy().into_owned()]);
            kotlinc.extend(arguments);
            if !classpath.is_empty() {
                kotlinc.push("-cp".to_string());
                kotlinc.push(
                    std::env::join_paths(&classpath)
                        .expect("build the program class path")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            if !matches!(common::kotlinc_compile(&kotlinc), Some((0, _))) {
                continue;
            }
            for (driver, _) in members {
                let classes = work.join("classes").join(&driver);
                fs::create_dir_all(&classes).expect("create the program's class directory");
                fs::rename(output.join(&driver), classes.join(&driver))
                    .expect("move the program's classes");
                programs.insert(driver, classes);
            }
        }
        programs
    })
}

/// What a Kotlin program beside a driver compiles and runs against besides the stdlib: a program
/// that asserts with kotlin.test runs with kotlin-test on its class path, as a program compiled
/// against it does.
fn program_classpath(source: &str) -> Vec<PathBuf> {
    if source.contains("import kotlin.test.") {
        vec![common::kotlin_test_jar().expect("kotlin-test.jar for the Kotlin program")]
    } else {
        Vec::new()
    }
}

/// `source` in package `package`, the declaration opening the line after the file annotations so no
/// line moves; `None` for a program that names its own package.
fn in_package(package: &str, source: &str) -> Option<String> {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    if lines.iter().any(|line| line.starts_with("package ")) {
        return None;
    }
    let at = lines
        .iter()
        .rposition(|line| line.starts_with("@file:"))
        .map_or(0, |last| last + 1);
    Some(format!(
        "{}package {package}; {}",
        lines[..at].concat(),
        lines[at..].concat()
    ))
}

#[test]
fn a_program_is_packaged_after_its_file_annotations_on_the_same_line() {
    assert_eq!(
        in_package("p", "// a\nfun box() = \"\"\n").as_deref(),
        Some("package p; // a\nfun box() = \"\"\n")
    );
    assert_eq!(
        in_package("p", "// a\n@file:OptIn(X::class)\nimport a.b\n").as_deref(),
        Some("// a\n@file:OptIn(X::class)\npackage p; import a.b\n")
    );
    assert_eq!(in_package("p", "package q\nfun box() = \"\"\n"), None);
}

/// Which of the native runtime's rules a line the JVM answers differently follows. The runtime
/// BEHAVES as Kotlin/Native does -- which exception type is thrown, a class's identity and names,
/// what an `is` answers, iteration order, a collection's semantics, the order of the calls it makes
/// into the program -- and SAYS what the JVM says -- an exception's message, a diagnostic's wording
/// -- wherever that is cheap. A JVM message is therefore no divergence; the first two are, and the
/// third marks where the runtime does not yet keep the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// The runtime behaves as Kotlin/Native does, and the JVM behaves otherwise.
    NativeBehaviour,
    /// The runtime keeps Kotlin/Native's message, because the JVM's is not cheap to reproduce.
    NativeMessage,
    /// A known gap: the runtime answers as neither platform does, because Kotlin/Native's answer
    /// needs a call into the program the runtime cannot make yet. The declaration says, beside it,
    /// what Kotlin/Native answers and what closing the gap needs.
    NotYetNative,
}

/// A line of a driver's transcript on which the native runtime answers differently from the JVM,
/// by one of the runtime's rules: kotlinc's program must answer exactly `jvm` there, and the driver
/// exactly `native`. Each declaration cites, beside it, where the Kotlin/Native answer comes from,
/// since no Kotlin/Native compiler runs here.
#[derive(Clone, Copy, Debug)]
struct Divergence {
    rule: Rule,
    jvm: &'static str,
    native: &'static str,
}

impl Divergence {
    const fn native_behaviour(jvm: &'static str, native: &'static str) -> Self {
        Divergence {
            rule: Rule::NativeBehaviour,
            jvm,
            native,
        }
    }

    const fn native_message(jvm: &'static str, native: &'static str) -> Self {
        Divergence {
            rule: Rule::NativeMessage,
            jvm,
            native,
        }
    }

    const fn not_yet_native(jvm: &'static str, native: &'static str) -> Self {
        Divergence {
            rule: Rule::NotYetNative,
            jvm,
            native,
        }
    }
}

/// `run_driver_against_kotlin`, for a driver some of whose lines the native runtime answers
/// differently from the JVM on purpose. Each such line is declared: kotlinc's program must answer
/// exactly the declared JVM line and the driver exactly the declared native line, at the same place
/// in the two transcripts, so the oracle still checks the JVM's side and the driver's side is
/// pinned. Every other line must be identical; an undeclared difference fails, and so does a
/// declared one that no longer occurs, so a declaration cannot outlive the difference it explains.
fn run_driver_against_kotlin_with(driver: &str, divergences: &[Divergence]) {
    for divergence in divergences {
        assert!(
            divergence.jvm != divergence.native && !divergence.jvm.contains('\n'),
            "{driver}: a divergence must be one line that differs: {divergence:?}"
        );
    }
    let Some(native) = run_payload_driver(driver) else {
        return;
    };
    let program = driver_dir().join(format!("{driver}.kt"));
    let source = fs::read_to_string(&program)
        .unwrap_or_else(|error| panic!("{driver}: read {}: {error}", program.display()));
    let classpath = program_classpath(&source);
    let kotlin = match kotlin_programs().get(driver) {
        Some(classes) => {
            let mut runtime = vec![classes.clone()];
            runtime.extend_from_slice(&classpath);
            runtime.extend([common::stdlib_jar(), common::jdk_modules()]);
            common::run_box(&[], &format!("{driver}.MainKt"), &runtime)
                .expect("run kotlinc-built box fixture")
        }
        None => common::kotlinc_box_result_with_classpath(&source, &classpath),
    };
    assert!(
        !kotlin.starts_with("ERROR:") && kotlin.ends_with('\n'),
        "{driver}: the Kotlin program must run and answer whole lines, got {kotlin:?}"
    );
    let native = String::from_utf8(native)
        .unwrap_or_else(|error| panic!("{driver}: the native transcript is not UTF-8: {error}"));
    if let Some(difference) = transcript_difference(&kotlin, &native, divergences) {
        panic!(
            "{driver}: the native transcript differs from Kotlin's: {difference}\n\
             Kotlin:\n{kotlin}native:\n{native}"
        );
    }
}

/// Where the native transcript departs from Kotlin's other than as `divergences` declare, or a
/// declared divergence that occurs on no line; `None` when the two agree line for line.
fn transcript_difference(kotlin: &str, native: &str, divergences: &[Divergence]) -> Option<String> {
    let mut kotlin_lines = kotlin.split_inclusive('\n');
    let mut native_lines = native.split_inclusive('\n');
    let mut occurs = vec![false; divergences.len()];
    for line in 1.. {
        match (kotlin_lines.next(), native_lines.next()) {
            (None, None) => break,
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => {
                let declared = divergences.iter().position(|divergence| {
                    expected.strip_suffix('\n') == Some(divergence.jvm)
                        && actual.strip_suffix('\n') == Some(divergence.native)
                });
                match declared {
                    Some(at) => occurs[at] = true,
                    None => {
                        return Some(format!(
                            "line {line}: Kotlin {expected:?}, native {actual:?}, and no \
                             divergence declares it"
                        ))
                    }
                }
            }
            (expected, actual) => {
                return Some(format!(
                    "line {line}: Kotlin {:?}, native {:?}",
                    expected.unwrap_or("<end>"),
                    actual.unwrap_or("<end>")
                ));
            }
        }
    }
    let stale = &divergences[occurs.iter().position(|occurs| !occurs)?];
    Some(format!(
        "the declared {:?} divergence, Kotlin {:?} and native {:?}, occurs on no line",
        stale.rule, stale.jvm, stale.native
    ))
}

#[test]
fn a_transcript_differs_only_where_a_divergence_declares_it() {
    assert_eq!(transcript_difference("a\nb\n", "a\nb\n", &[]), None);
    assert_eq!(
        transcript_difference("a\nb\n", "a\nc\n", &[]).as_deref(),
        Some("line 2: Kotlin \"b\\n\", native \"c\\n\", and no divergence declares it")
    );
    assert_eq!(
        transcript_difference("a\n", "a\nb\n", &[]).as_deref(),
        Some("line 2: Kotlin \"<end>\", native \"b\\n\"")
    );
    assert_eq!(
        transcript_difference("a\n", "a", &[]).as_deref(),
        Some("line 1: Kotlin \"a\\n\", native \"a\", and no divergence declares it")
    );
    let declared = [Divergence::native_behaviour("b", "c")];
    assert_eq!(transcript_difference("a\nb\n", "a\nc\n", &declared), None);
    assert_eq!(
        transcript_difference("a\nb\n", "a\nd\n", &declared).as_deref(),
        Some("line 2: Kotlin \"b\\n\", native \"d\\n\", and no divergence declares it")
    );
    assert_eq!(
        transcript_difference("a\nb\n", "a\nb\n", &declared).as_deref(),
        Some(concat!(
            "the declared NativeBehaviour divergence, Kotlin \"b\" and native \"c\", ",
            "occurs on no line"
        ))
    );
    let message = [Divergence::native_message("x: 1", "x: one")];
    assert_eq!(transcript_difference("x: 1\n", "x: one\n", &message), None);
}

/// Run `driver`, which must end the way the runtime ends a program it cannot continue
/// (`kt_sys_fail`: status 134) with exactly `message` on stderr and nothing on stdout.
fn run_driver_expecting_failure(driver: &str, message: &str) {
    let Some(output) = build_and_run(driver) else {
        return;
    };
    assert_failed(driver, &output, message);
}

fn assert_failed(driver: &str, output: &Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() == Some(134) && stderr == message && output.stdout.is_empty(),
        "{driver}: expected status 134 and stderr {message:?}, got {}\nstdout: {:?}\n\
         stderr: {stderr}",
        output.status,
        String::from_utf8_lossy(&output.stdout)
    );
}

// ---- every architecture --------------------------------------------------------------------
//
// The thread drivers check what assembly does: which callee-saved registers a released thread
// records and where, how `clone` takes its arguments and sets up the child's stack, and that the
// kernel clears and wakes the id word when the thread ends. All of it is written once per
// architecture, and a wrong register list or offset still compiles; only running it shows it. So
// these drivers also run on every other supported architecture, built with clang and lld for that
// target and run under a QEMU user-mode emulator. CI installs both and must run them.

/// The architectures the runtime supports, by clang's and QEMU's name for each.
const ARCHITECTURES: &[&str] = &["x86_64", "aarch64", "riscv64"];

/// One architecture other than the host's, with its emulator and the runtime built for it.
struct ForeignArchitecture {
    arch: &'static str,
    emulator: PathBuf,
    objects: Result<Vec<PathBuf>, String>,
}

/// Every supported architecture but the host's, each with its emulator and runtime objects; empty
/// when the host cannot run drivers at all. Outside CI a missing emulator or linker leaves an
/// architecture out, with a note; CI must have them.
fn foreign_architectures() -> &'static [ForeignArchitecture] {
    static FOREIGN: OnceLock<Vec<ForeignArchitecture>> = OnceLock::new();
    FOREIGN.get_or_init(|| {
        if !host_can_run() {
            return Vec::new();
        }
        let lld = Command::new("ld.lld")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        let path = std::env::var_os("PATH").unwrap_or_default();
        ARCHITECTURES
            .iter()
            .filter(|arch| **arch != std::env::consts::ARCH)
            .filter_map(|arch| {
                let emulator = std::env::split_paths(&path)
                    .map(|directory| directory.join(format!("qemu-{arch}")))
                    .find(|candidate| candidate.is_file());
                let headers = Path::new("/usr")
                    .join(format!("{arch}-linux-gnu"))
                    .join("include");
                let Some(emulator) = emulator.filter(|_| lld && headers.is_dir()) else {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "CI must run the drivers on {arch}, but this host has no \
                         qemu-{arch} on PATH, no ld.lld or no {arch} glibc headers"
                    );
                    eprintln!(
                        "drivers not run on {arch}: no qemu-{arch}, no ld.lld or no glibc headers"
                    );
                    return None;
                };
                Some(ForeignArchitecture {
                    arch,
                    emulator,
                    objects: foreign_runtime_objects(arch),
                })
            })
            .collect()
    })
}

/// Compiling for `arch`: its target triple, and its own glibc headers in place of the host's, so a
/// POSIX driver checks the layer's numbers and layouts against the target's C library.
fn foreign_flags(arch: &str) -> Vec<String> {
    let mut flags = vec![
        format!("--target={arch}-linux-gnu"),
        "-nostdlibinc".to_string(),
        "-isystem".to_string(),
        format!("/usr/{arch}-linux-gnu/include"),
    ];
    flags.extend(compile_flags().iter().map(|flag| flag.to_string()));
    flags
}

/// Every runtime source compiled for `arch`, or what clang said.
fn foreign_runtime_objects(arch: &str) -> Result<Vec<PathBuf>, String> {
    let scratch = common::scratch_dir()
        .expect("scratch directory")
        .join(format!("runtime-{arch}"));
    fs::create_dir_all(&scratch).expect("create the architecture's scratch directory");
    let mut sources: Vec<PathBuf> = fs::read_dir(runtime_dir())
        .expect("read the runtime directory")
        .map(|entry| entry.expect("runtime directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    let mut objects = Vec::new();
    let mut errors = String::new();
    for source in sources {
        let object = scratch
            .join(source.file_name().expect("runtime source name"))
            .with_extension("o");
        let output = Command::new("clang")
            .args(foreign_flags(arch))
            .arg("-I")
            .arg(runtime_dir())
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()
            .expect("run clang");
        if !output.status.success() {
            errors.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        objects.push(object);
    }
    if errors.is_empty() {
        Ok(objects)
    } else {
        Err(errors)
    }
}

/// Build `driver` for `foreign`'s architecture and run it under its emulator.
fn build_and_run_on(foreign: &ForeignArchitecture, driver: &str) -> Output {
    let executable = build_on(foreign, driver);
    Command::new(&foreign.emulator)
        .arg(&executable)
        .env("KRUSTY_DRIVER", driver)
        .output()
        .expect("run the driver under its emulator")
}

/// Build `driver` for `foreign`'s architecture, against that target's own headers.
fn build_on(foreign: &ForeignArchitecture, driver: &str) -> PathBuf {
    let arch = foreign.arch;
    let objects = foreign.objects.as_ref().unwrap_or_else(|errors| {
        panic!("{driver} on {arch}: the runtime did not build:\n{errors}")
    });
    let executable = common::scratch_dir()
        .expect("scratch directory")
        .join(format!("{driver}-{arch}"));
    let build = Command::new("clang")
        .args(foreign_flags(arch))
        // The host's linker knows only the host's machine; lld links every target.
        .arg("-fuse-ld=lld")
        .arg("-I")
        .arg(runtime_dir())
        .args(objects)
        .arg(driver_dir().join(format!("{driver}.c")))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run clang");
    assert!(
        build.status.success(),
        "{driver} on {arch}: the driver and runtime did not build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    executable
}

/// `run_driver`, on the host and on every other supported architecture.
fn run_driver_everywhere(driver: &str) {
    run_driver(driver);
    for foreign in foreign_architectures() {
        assert_ok(
            &format!("{driver} on {}", foreign.arch),
            &build_and_run_on(foreign, driver),
        );
    }
}

/// `run_driver_everywhere`, except that on the architectures in `build_only` the driver is built
/// against that target's headers but not run, because its emulator does not answer as the kernel
/// does.
fn run_driver_everywhere_building_only_on(driver: &str, build_only: &[&str]) {
    run_driver(driver);
    for foreign in foreign_architectures() {
        if build_only.contains(&foreign.arch) {
            build_on(foreign, driver);
        } else {
            assert_ok(
                &format!("{driver} on {}", foreign.arch),
                &build_and_run_on(foreign, driver),
            );
        }
    }
}

/// QEMU 8.2's riscv64 user mode reads and writes `rt_sigaction`'s structure one word longer than
/// the riscv64 kernel's, which has no `sa_restorer`, so an action the kernel would install is not
/// the one QEMU installs, and a driver that installs one cannot run there. It is still built
/// against riscv64's glibc headers.
const SIGNAL_ACTIONS_NOT_EMULATED: &[&str] = &["riscv64"];

/// `run_driver_expecting_failure`, on the host and on every other supported architecture.
fn run_driver_everywhere_expecting_failure(driver: &str, message: &str) {
    run_driver_expecting_failure(driver, message);
    for foreign in foreign_architectures() {
        let output = build_and_run_on(foreign, driver);
        assert_failed(&format!("{driver} on {}", foreign.arch), &output, message);
    }
}

#[test]
fn a_program_that_returns_ends_with_status_zero() {
    run_driver("program_returns");
}

#[test]
fn a_write_to_a_full_non_blocking_pipe_keeps_writing() {
    let Some(payload) = run_payload_driver("write_nonblocking_stdout") else {
        return;
    };
    assert_eq!(payload.len(), 1 << 20, "every byte of the payload arrives");
    assert!(
        payload
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == b'a' + (index % 26) as u8),
        "the payload arrives in order"
    );
}

#[test]
fn the_collector_frees_what_is_unreachable_and_reuses_it() {
    run_driver("gc_collects_unreachable");
}

#[test]
fn an_allocation_whose_size_would_wrap_runs_out_of_memory() {
    run_driver_expecting_failure("gc_rejects_wrapping_size", "krusty: out of memory\n");
}

#[test]
fn every_registered_global_root_is_kept_past_four_thousand() {
    run_driver("gc_many_global_roots");
}

#[test]
fn only_an_arrays_end_pointer_keeps_it_alive_and_no_neighbour_is_kept() {
    run_driver("gc_end_pointer_keeps_object");
}

#[test]
fn references_kept_in_typed_fields_elements_and_globals_are_traced() {
    run_driver("gc_typed_reference_slots");
}

#[test]
fn a_collection_started_during_a_collection_fails() {
    run_driver_expecting_failure(
        "gc_reentrant_collection_fails",
        "krusty: a collection started during a collection\n",
    );
}

#[test]
fn threads_started_by_foreign_code_share_the_heap_through_collections() {
    run_driver_everywhere("threads_share_the_heap");
}

#[test]
fn each_thread_keeps_its_own_exception_in_flight() {
    run_driver_everywhere("threads_keep_their_own_exception");
}

#[test]
fn an_exception_a_callback_leaves_uncaught_ends_the_process_holding_the_lock() {
    run_driver_everywhere_expecting_failure(
        "callback_uncaught_exception",
        "Exception in thread \"Thread-0\" kotlin.IllegalStateException: leaked\n",
    );
}

#[test]
fn threads_the_runtime_starts_keep_their_argument_until_they_run() {
    run_driver_everywhere("threads_started_by_the_runtime");
}

#[test]
fn a_started_threads_stack_has_a_guard_below_it() {
    run_driver_everywhere("thread_stack_guard");
}

// The POSIX layer a static program calls instead of a C library. These drivers are compiled
// against each target's own C library headers, so each struct they pass has that glibc's layout
// and each errno its number, and linked with no C library, so the runtime answers every call.

#[test]
fn posix_files_answer_with_glibcs_layouts_and_errno() {
    run_driver_everywhere("posix_files");
}

#[test]
fn posix_process_calls_cover_the_environment_signals_and_clocks() {
    run_driver_everywhere_building_only_on("posix_process", SIGNAL_ACTIONS_NOT_EMULATED);
}

#[test]
fn posix_sockets_and_epoll_serve_a_loopback_connection() {
    run_driver_everywhere("posix_net");
}

#[test]
fn posix_threads_keep_their_own_errno_and_share_mutexes() {
    run_driver_everywhere_building_only_on("posix_threads", SIGNAL_ACTIONS_NOT_EMULATED);
}

/// The layer's hand-copied numbers, each checked by `_Static_assert` against the target's own glibc
/// and kernel headers: building the driver for a target is the check.
#[test]
fn posix_numbers_are_each_targets_own() {
    run_driver_everywhere("posix_abi");
}

#[test]
fn posix_memmove_copies_between_objects_and_within_one_either_way() {
    run_driver_everywhere("posix_string");
}

/// `abort` with SIGABRT blocked still ends the process ON SIGABRT: the status is the signal, not an
/// exit code, and nothing is written.
#[test]
fn posix_abort_ends_the_process_on_sigabrt_even_when_it_is_blocked() {
    let Some(output) = build_and_run("posix_abort") else {
        return;
    };
    assert!(
        output.status.signal() == Some(6)
            && output.status.code().is_none()
            && output.stdout.is_empty()
            && output.stderr.is_empty(),
        "posix_abort: expected termination by SIGABRT with no output, got {}\nstdout: {:?}\n\
         stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_exception_a_started_thread_leaves_uncaught_ends_the_process_on_its_name() {
    run_driver_everywhere_expecting_failure(
        "thread_uncaught_exception",
        "Exception in thread \"Thread-1\" kotlin.IllegalStateException: boom\n",
    );
}

#[test]
fn a_double_or_float_renders_as_the_jvm_renders_it() {
    run_driver("fp_render_known_answers");
}

#[test]
fn a_floating_remainder_is_exact_and_a_nan_comes_back_quiet() {
    run_driver("fp_remainder_known_answers");
}

#[test]
fn an_array_too_large_for_the_allocator_is_out_of_memory() {
    run_driver_expecting_failure("array_new_overflow", "krusty: out of memory\n");
}

#[test]
fn a_negative_string_index_is_out_of_bounds() {
    // Kotlin/Native's type: its runtime reads a `String` in `KString.cpp`
    // (`boundsCheckedIteratorAt`, `Kotlin_String_subSequence`, JetBrains/kotlin v2.4.10,
    // kotlin-native/runtime/src/main/cpp), which calls `ThrowArrayIndexOutOfBoundsException`, and
    // `RuntimeUtils.kt` throws `ArrayIndexOutOfBoundsException()`. The message is the JVM's.
    run_driver_against_kotlin_with(
        "string_get_negative_index",
        &[
            Divergence::native_behaviour(
                "\"abc\"[-1]: threw StringIndexOutOfBoundsException: Index -1 out of bounds \
                 for length 3",
                "\"abc\"[-1]: threw ArrayIndexOutOfBoundsException: Index -1 out of bounds for \
                 length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[-100]: threw StringIndexOutOfBoundsException: Index -100 out of \
                 bounds for length 3",
                "\"abc\"[-100]: threw ArrayIndexOutOfBoundsException: Index -100 out of bounds \
                 for length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[-2147483648]: threw StringIndexOutOfBoundsException: Index \
                 -2147483648 out of bounds for length 3",
                "\"abc\"[-2147483648]: threw ArrayIndexOutOfBoundsException: Index -2147483648 \
                 out of bounds for length 3",
            ),
            Divergence::native_behaviour(
                "\"abc\"[3]: threw StringIndexOutOfBoundsException: Index 3 out of bounds for \
                 length 3",
                "\"abc\"[3]: threw ArrayIndexOutOfBoundsException: Index 3 out of bounds for \
                 length 3",
            ),
        ],
    );
}

#[test]
fn a_substring_outside_the_text_is_out_of_bounds() {
    // Kotlin/Native's type: its runtime reads a `String` in `KString.cpp`
    // (`boundsCheckedIteratorAt`, `Kotlin_String_subSequence`, JetBrains/kotlin v2.4.10,
    // kotlin-native/runtime/src/main/cpp), which calls `ThrowArrayIndexOutOfBoundsException`, and
    // `RuntimeUtils.kt` throws `ArrayIndexOutOfBoundsException()`. The message is the JVM's.
    run_driver_against_kotlin_with(
        "string_substring_bounds",
        &[
            Divergence::native_behaviour(
                "substring(-1, 2): threw StringIndexOutOfBoundsException: Range [-1, 2) out of \
                 bounds for length 3",
                "substring(-1, 2): threw ArrayIndexOutOfBoundsException: Range [-1, 2) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(-1): threw StringIndexOutOfBoundsException: Range [-1, 3) out of \
                 bounds for length 3",
                "substring(-1): threw ArrayIndexOutOfBoundsException: Range [-1, 3) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(2, 1): threw StringIndexOutOfBoundsException: Range [2, 1) out of \
                 bounds for length 3",
                "substring(2, 1): threw ArrayIndexOutOfBoundsException: Range [2, 1) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(2, 10): threw StringIndexOutOfBoundsException: Range [2, 10) out of \
                 bounds for length 3",
                "substring(2, 10): threw ArrayIndexOutOfBoundsException: Range [2, 10) out of \
                 bounds for length 3",
            ),
            Divergence::native_behaviour(
                "substring(4): threw StringIndexOutOfBoundsException: Range [4, 3) out of \
                 bounds for length 3",
                "substring(4): threw ArrayIndexOutOfBoundsException: Range [4, 3) out of \
                 bounds for length 3",
            ),
        ],
    );
}

#[test]
fn whitespace_is_the_jvm_set() {
    run_driver("string_whitespace");
}

#[test]
fn a_repeat_too_long_for_memory_is_out_of_memory() {
    run_driver_expecting_failure("string_repeat_overflow", "krusty: out of memory\n");
}

#[test]
fn a_concatenation_too_long_for_memory_is_out_of_memory() {
    run_driver_expecting_failure("string_plus_overflow", "krusty: out of memory\n");
}

#[test]
fn surrogate_halves_concatenate_into_their_character() {
    run_driver("string_plus_surrogates");
}

#[test]
fn a_bound_between_surrogate_halves_cuts_the_pair_and_a_search_finds_either_half() {
    run_driver("string_surrogate_halves");
}

#[test]
fn a_builder_receiver_or_suffix_is_read_as_text_and_sliced_into_a_copy() {
    run_driver("string_builder_receivers");
}

#[test]
fn a_concatenation_stops_at_the_first_throwing_to_string() {
    run_driver("string_plus_throwing_to_string");
}

#[test]
fn unboxing_null_raises_and_comes_back() {
    run_driver("unbox_null_raises");
}

#[test]
fn a_number_conversion_of_null_raises_and_comes_back() {
    run_driver("number_conversion_null_raises");
}

#[test]
fn a_builder_joins_a_surrogate_pair_appended_unit_by_unit() {
    run_driver("builder_joins_surrogate_pair");
}

#[test]
fn a_lazy_whose_initializer_throws_stays_uninitialized() {
    run_driver("lazy_initializer_throws");
}

#[test]
fn an_append_whose_to_string_throws_leaves_the_builder_alone() {
    run_driver("builder_append_throwing_to_string");
}

#[test]
fn an_append_line_whose_to_string_throws_leaves_the_builder_alone() {
    run_driver("builder_append_line_throwing_to_string");
}

#[test]
fn a_negative_builder_capacity_throws_illegal_argument_exception() {
    // Kotlin/Native's type: 2.4.10's stdlib (the distribution's linux_x64 static cache) compiles
    // `StringBuilder(capacity)` to `AllocArrayInstance(CharArray, capacity)`, which calls
    // `ThrowIllegalArgumentException` for a negative size. The message is the JVM's.
    run_driver_against_kotlin_with(
        "builder_negative_capacity",
        &[
            Divergence::native_behaviour(
                "StringBuilder(-1): threw NegativeArraySizeException: -1",
                "StringBuilder(-1): threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "StringBuilder(-42): threw NegativeArraySizeException: -42",
                "StringBuilder(-42): threw IllegalArgumentException: -42",
            ),
            Divergence::native_behaviour(
                "StringBuilder(-2147483648): threw NegativeArraySizeException: -2147483648",
                "StringBuilder(-2147483648): threw IllegalArgumentException: -2147483648",
            ),
        ],
    );
}

#[test]
fn a_builder_made_from_a_program_char_sequence_holds_its_text() {
    run_driver("builder_from_program_char_sequence");
}

#[test]
fn a_builder_made_from_a_program_char_sequence_that_throws_is_not_made() {
    run_driver("builder_from_throwing_char_sequence");
}

#[test]
fn a_pair_stops_at_the_first_component_that_throws() {
    run_driver("pair_component_throws");
}

#[test]
fn a_builder_grown_past_the_largest_length_is_out_of_memory() {
    run_driver_expecting_failure("builder_length_overflow", "krusty: out of memory\n");
}

#[test]
fn a_class_answers_the_names_its_descriptor_publishes() {
    run_driver("class_names");
}

#[test]
fn a_class_that_publishes_no_names_fails_naming_its_descriptor() {
    run_driver_expecting_failure(
        "class_names_unpublished",
        "krusty: the class pkg.Unpublished publishes no reflection names consistent with its kind\n",
    );
}

#[test]
fn a_property_read_through_an_unknown_delegate_fails_naming_it() {
    run_driver_expecting_failure(
        "rw_property_get_unknown_delegate",
        "krusty: a ReadWriteProperty read of a pkg.CustomDelegate, which is neither \
         Delegates.notNull() nor Delegates.observable()\n",
    );
}

#[test]
fn a_property_write_through_an_unknown_delegate_fails_naming_it() {
    run_driver_expecting_failure(
        "rw_property_set_unknown_delegate",
        "krusty: a ReadWriteProperty write of a pkg.CustomDelegate, which is neither \
         Delegates.notNull() nor Delegates.observable()\n",
    );
}

#[test]
fn each_primitive_boxes_to_its_kind_and_small_values_share_a_box() {
    run_driver("boxing");
}

#[test]
fn a_number_converts_saturating_and_truncating_as_kotlin_does() {
    run_driver("number_conversions");
}

#[test]
fn a_pairs_members_answer_componentwise() {
    run_driver("pair_members");
}

#[test]
fn lazy_observable_and_not_null_delegates_keep_kotlins_order() {
    run_driver("delegates");
}

#[test]
fn a_builder_appends_copies_and_sets_its_length() {
    run_driver("builder_operations");
}

#[test]
fn a_ulong_progression_across_two_to_the_63_contains_its_members() {
    run_driver_against_kotlin("range_contains_unsigned");
}

#[test]
fn a_progression_renders_compares_and_hashes_with_its_step() {
    run_driver_against_kotlin("range_progression_members");
}

#[test]
fn a_range_of_a_program_comparable_orders_by_its_compare_to() {
    run_driver("comparable_range_program_type");
}

#[test]
fn an_empty_unsigned_until_is_the_declared_empty_range() {
    run_driver_against_kotlin("range_unsigned_until_empty");
}

#[test]
fn a_ulong_walk_across_two_to_the_63_steps_without_signed_overflow() {
    run_driver_against_kotlin("range_iterator_ulong_crosses_sign");
}

#[test]
fn a_spread_copy_that_does_not_fit_throws_and_writes_nothing() {
    run_driver_against_kotlin("array_copy_into_bounds");
}

#[test]
fn a_range_a_progression_and_their_iterators_are_kotlins_classes() {
    run_driver_against_kotlin("range_class_identity");
}

#[test]
fn a_comparable_ranges_members_call_the_program_in_kotlins_order_and_stop_at_a_throw() {
    run_driver_against_kotlin("comparable_range_members");
}

#[test]
fn a_floating_point_range_compares_by_ieee_and_answers_its_members_as_kotlin_does() {
    run_driver_against_kotlin("floating_range");
}

#[test]
fn a_spread_of_something_other_than_the_varargs_array_kind_fails() {
    run_driver_expecting_failure(
        "array_copy_into_not_an_array",
        "krusty: a spread of a value that is not an array of the vararg's kind\n",
    );
}

#[test]
fn range_behavior_matches_an_executable_kotlinc_oracle() {
    run_driver_against_kotlin("range_kotlinc_oracle");
}

#[test]
fn a_step_of_something_other_than_a_range_fails() {
    run_driver_expecting_failure(
        "range_step_not_a_range",
        "krusty: a step or reversal of a value that is not a range\n",
    );
}

#[test]
fn a_reversal_of_something_other_than_a_range_fails() {
    run_driver_expecting_failure(
        "range_reversed_not_a_range",
        "krusty: a step or reversal of a value that is not a range\n",
    );
}

#[test]
fn mapping_the_full_long_range_is_too_long_to_collect() {
    run_driver_expecting_failure(
        "range_map_full_span",
        "krusty: a range too long to collect\n",
    );
}

#[test]
fn a_list_write_out_of_bounds_raises_and_leaves_the_list_alone() {
    run_driver("mutable_list_bounds");
}

#[test]
fn a_list_read_out_of_bounds_raises_and_answers_nothing() {
    run_driver("list_read_bounds");
}

#[test]
fn a_list_added_to_itself_doubles() {
    run_driver("list_add_all_self");
}

#[test]
fn a_list_modified_during_for_each_ends_the_walk() {
    run_driver("walk_modified_during_for_each");
}

#[test]
fn a_throwing_lambda_ends_the_walk_that_called_it() {
    run_driver("walk_stops_on_throw");
}

#[test]
fn an_exhausted_iterator_raises_what_kotlin_raises_through_either_protocol() {
    // Reading text past its end raises Kotlin/Native's class with the JVM's message. A `String`'s
    // is `ArrayIndexOutOfBoundsException`, as in `a_negative_string_index_is_out_of_bounds`; a
    // `StringBuilder`'s is `IndexOutOfBoundsException`, because Kotlin/Native 2.4.10's
    // `StringBuilder#get` calls `AbstractList.Companion#checkElementIndex` (disassembly of the
    // distribution's linux_x64 stdlib cache), which throws it
    // (libraries/stdlib/src/kotlin/collections/AbstractList.kt).
    run_driver_against_kotlin_with(
        "iterator_exhausted",
        &[
            Divergence::native_behaviour(
                "\"a\" general threw StringIndexOutOfBoundsException: Index 1 out of bounds \
                 for length 1",
                "\"a\" general threw ArrayIndexOutOfBoundsException: Index 1 out of bounds for \
                 length 1",
            ),
            Divergence::native_behaviour(
                "\"a\" general again threw StringIndexOutOfBoundsException: Index 2 out of \
                 bounds for length 1",
                "\"a\" general again threw ArrayIndexOutOfBoundsException: Index 2 out of \
                 bounds for length 1",
            ),
            Divergence::native_behaviour(
                "\"a\" narrow threw StringIndexOutOfBoundsException: Index 1 out of bounds for \
                 length 1",
                "\"a\" narrow threw ArrayIndexOutOfBoundsException: Index 1 out of bounds for \
                 length 1",
            ),
            Divergence::native_behaviour(
                "\"a\" narrow again threw StringIndexOutOfBoundsException: Index 2 out of \
                 bounds for length 1",
                "\"a\" narrow again threw ArrayIndexOutOfBoundsException: Index 2 out of \
                 bounds for length 1",
            ),
            Divergence::native_behaviour(
                "\"\" general threw StringIndexOutOfBoundsException: Index 0 out of bounds for \
                 length 0",
                "\"\" general threw ArrayIndexOutOfBoundsException: Index 0 out of bounds for \
                 length 0",
            ),
            Divergence::native_behaviour(
                "\"\" narrow threw StringIndexOutOfBoundsException: Index 0 out of bounds for \
                 length 0",
                "\"\" narrow threw ArrayIndexOutOfBoundsException: Index 0 out of bounds for \
                 length 0",
            ),
            Divergence::native_behaviour(
                "StringBuilder(\"a\") general threw StringIndexOutOfBoundsException: Index 1 \
                 out of bounds for length 1",
                "StringBuilder(\"a\") general threw IndexOutOfBoundsException: Index 1 out of \
                 bounds for length 1",
            ),
            Divergence::native_behaviour(
                "StringBuilder(\"a\") general again threw StringIndexOutOfBoundsException: \
                 Index 2 out of bounds for length 1",
                "StringBuilder(\"a\") general again threw IndexOutOfBoundsException: Index 2 \
                 out of bounds for length 1",
            ),
            Divergence::native_behaviour(
                "StringBuilder(\"a\") narrow threw StringIndexOutOfBoundsException: Index 1 \
                 out of bounds for length 1",
                "StringBuilder(\"a\") narrow threw IndexOutOfBoundsException: Index 1 out of \
                 bounds for length 1",
            ),
            Divergence::native_behaviour(
                "StringBuilder(\"a\") narrow again threw StringIndexOutOfBoundsException: \
                 Index 2 out of bounds for length 1",
                "StringBuilder(\"a\") narrow again threw IndexOutOfBoundsException: Index 2 \
                 out of bounds for length 1",
            ),
        ],
    );
}

#[test]
fn an_indexed_value_hash_code_wraps() {
    run_driver("indexed_value_hash_overflow");
}

#[test]
fn an_array_list_of_negative_capacity_raises() {
    run_driver_against_kotlin("array_list_negative_capacity");
}

#[test]
fn walking_a_string_is_linear_and_yields_its_utf16_units() {
    run_driver("string_iterator_linear");
}

#[test]
fn a_programs_own_text_is_asked_its_length_once_per_step() {
    run_driver("program_text_walk_asks_length_once_per_step");
}

#[test]
fn a_list_grown_past_the_largest_capacity_is_out_of_memory() {
    run_driver_expecting_failure("mutable_list_growth_overflow", "krusty: out of memory\n");
}

#[test]
fn an_element_member_that_throws_ends_the_walk_that_called_it() {
    run_driver("list_stops_on_throwing_element");
}

#[test]
fn a_list_is_the_collection_interfaces_and_equals_a_program_list() {
    // `listOf(1, 2)` is Kotlin/Native's `Array.asList`, an anonymous read-only `AbstractList` that
    // is no `MutableList` and has no simple name
    // (kotlin-native/runtime/src/main/kotlin/generated/_ArraysNative.kt, JetBrains/kotlin v2.4.10;
    // `listOf(vararg)` calls it, per the disassembly of the distribution's linux_x64 stdlib cache).
    //
    // Known gaps: Kotlin/Native's `AbstractList.equals` is `orderedEquals`
    // (libraries/stdlib/src/kotlin/collections/AbstractList.kt), which compares the two `size`s
    // first and then walks the other list with `iterator()` and `next()` alone -- `iterator next
    // eq(1) next eq(2)` for `P(1, 2)`, and no call at all for `P(1, 2, 3)` or `P(1)` -- and
    // `EmptyList.equals` asks `isEmpty()`. The runtime reaches a program's collection only through
    // the `iterator()`, `hasNext()` and `next()` its descriptor records, so it walks the list
    // instead; closing the gap needs the compiler to record `size` (and `isEmpty`) as well. The
    // growable list has the same gap without a declaration, because the runtime answers there as
    // the JVM does: Kotlin/Native's `ArrayList.equals` compares `size` and then `get(i)`
    // (libraries/stdlib/native-wasm/src/kotlin/collections/ArrayList.kt).
    run_driver_against_kotlin_with(
        "list_identity",
        &[
            Divergence::native_behaviour(
                "listOf: true true true true true true true",
                "listOf: true true true true false false false",
            ),
            Divergence::native_behaviour(
                "listOf::class.simpleName ArrayList",
                "listOf::class.simpleName null",
            ),
            Divergence::not_yet_native(
                "listOf(1, 2) == P(1, 2) true | hasNext next eq(1) hasNext next eq(2) hasNext ",
                "listOf(1, 2) == P(1, 2) true | iterator hasNext next eq(1) hasNext next eq(2) \
                 hasNext ",
            ),
            Divergence::not_yet_native(
                "listOf(1, 2) == P(1, 2, 3) false | hasNext next eq(1) hasNext next eq(2) \
                 hasNext ",
                "listOf(1, 2) == P(1, 2, 3) false | iterator hasNext next eq(1) hasNext next \
                 eq(2) hasNext ",
            ),
            Divergence::not_yet_native(
                "listOf(1, 2) == P(1) false | hasNext next eq(1) hasNext ",
                "listOf(1, 2) == P(1) false | iterator hasNext next eq(1) hasNext ",
            ),
            Divergence::not_yet_native(
                "listOf(1, 2) == P(1, 3) false | hasNext next eq(1) hasNext next eq(2) ",
                "listOf(1, 2) == P(1, 3) false | iterator hasNext next eq(1) hasNext next \
                 eq(2) ",
            ),
            Divergence::not_yet_native(
                "listOf() == P() true | ",
                "listOf() == P() true | iterator hasNext ",
            ),
        ],
    );
}

#[test]
fn a_walk_stops_at_a_throwing_iterator_or_has_next_of_the_program() {
    run_driver_against_kotlin("walk_polls_program_calls");
}

#[test]
fn a_list_modification_count_wraps_and_is_still_noticed() {
    run_driver("list_modification_count_wraps");
}

#[test]
fn a_walk_count_past_the_largest_int_raises_kotlins_overflow() {
    run_driver_against_kotlin("walk_count_overflow");
}

#[test]
fn a_walk_index_past_the_largest_int_raises_kotlins_overflow() {
    run_driver_against_kotlin("walk_index_overflow");
}

#[test]
fn an_indexed_value_or_none_stops_at_the_programs_throwing_call() {
    run_driver("member_stops_at_throw");
}

#[test]
fn a_builder_map_sees_its_length_change_and_a_self_list_renders() {
    run_driver_against_kotlin("builder_map_and_self_list");
}

#[test]
fn the_list_walk_and_array_entry_points_answer_as_kotlin_does() {
    run_driver_against_kotlin("list_api_answers");
}

#[test]
fn a_map_or_for_each_over_a_list_its_lambda_changes_stops_where_kotlins_iterator_does() {
    // Kotlin/Native's `ArrayList` iterator answers `hasNext()` as `index < list.length`
    // (`ArrayList.kt`, `Itr.hasNext`, JetBrains/kotlin v2.4.10,
    // libraries/stdlib/native-wasm/src/kotlin/collections), so a walk whose last element removed an
    // element ends there; the JVM's answers `cursor != size` and its next `next()` throws.
    run_driver_against_kotlin_with(
        "map_mutated_source",
        &[
            Divergence::native_behaviour(
                "map removing the last at the last of [1, 2]: threw \
                 ConcurrentModificationException: null [1]",
                "map removing the last at the last of [1, 2]: [10, 20] [1]",
            ),
            Divergence::native_behaviour(
                "forEach removing the last at the last of [1, 2]: threw \
                 ConcurrentModificationException: null [1]",
                "forEach removing the last at the last of [1, 2]: kotlin.Unit [1]",
            ),
        ],
    );
}

#[test]
fn an_iterator_is_an_iterator_and_an_arrays_is_its_kinds_iterator() {
    // Kotlin/Native's classes: `kotlin.ArrayIterator` and `kotlin.IntArrayIterator` and kin
    // (kotlin-native/runtime/src/main/kotlin/kotlin/Arrays.kt, JetBrains/kotlin v2.4.10), the
    // anonymous iterator of `Array.asList`, which `listOf(1, 2)` answers
    // (kotlin-native/runtime/src/main/kotlin/generated/_ArraysNative.kt), and `ArrayList`'s `Itr`
    // (libraries/stdlib/native-wasm/src/kotlin/collections/ArrayList.kt).
    run_driver_against_kotlin_with(
        "iterator_identity",
        &[
            Divergence::native_behaviour(
                "arrayOf(1).iterator(): kotlin.jvm.internal.ArrayIterator super=kotlin.Any is \
                 Iterator",
                "arrayOf(1).iterator(): kotlin.ArrayIterator super=kotlin.Any is Iterator",
            ),
            Divergence::native_behaviour(
                "BooleanArray(1).iterator(): kotlin.jvm.internal.ArrayBooleanIterator \
                 super=kotlin.collections.BooleanIterator is Iterator BooleanIterator",
                "BooleanArray(1).iterator(): kotlin.BooleanArrayIterator \
                 super=kotlin.collections.BooleanIterator is Iterator BooleanIterator",
            ),
            Divergence::native_behaviour(
                "ByteArray(1).iterator(): kotlin.jvm.internal.ArrayByteIterator \
                 super=kotlin.collections.ByteIterator is Iterator ByteIterator",
                "ByteArray(1).iterator(): kotlin.ByteArrayIterator \
                 super=kotlin.collections.ByteIterator is Iterator ByteIterator",
            ),
            Divergence::native_behaviour(
                "CharArray(1).iterator(): kotlin.jvm.internal.ArrayCharIterator \
                 super=kotlin.collections.CharIterator is Iterator CharIterator",
                "CharArray(1).iterator(): kotlin.CharArrayIterator \
                 super=kotlin.collections.CharIterator is Iterator CharIterator",
            ),
            Divergence::native_behaviour(
                "ShortArray(1).iterator(): kotlin.jvm.internal.ArrayShortIterator \
                 super=kotlin.collections.ShortIterator is Iterator ShortIterator",
                "ShortArray(1).iterator(): kotlin.ShortArrayIterator \
                 super=kotlin.collections.ShortIterator is Iterator ShortIterator",
            ),
            Divergence::native_behaviour(
                "IntArray(1).iterator(): kotlin.jvm.internal.ArrayIntIterator \
                 super=kotlin.collections.IntIterator is Iterator IntIterator",
                "IntArray(1).iterator(): kotlin.IntArrayIterator \
                 super=kotlin.collections.IntIterator is Iterator IntIterator",
            ),
            Divergence::native_behaviour(
                "LongArray(1).iterator(): kotlin.jvm.internal.ArrayLongIterator \
                 super=kotlin.collections.LongIterator is Iterator LongIterator",
                "LongArray(1).iterator(): kotlin.LongArrayIterator \
                 super=kotlin.collections.LongIterator is Iterator LongIterator",
            ),
            Divergence::native_behaviour(
                "FloatArray(1).iterator(): kotlin.jvm.internal.ArrayFloatIterator \
                 super=kotlin.collections.FloatIterator is Iterator FloatIterator",
                "FloatArray(1).iterator(): kotlin.FloatArrayIterator \
                 super=kotlin.collections.FloatIterator is Iterator FloatIterator",
            ),
            Divergence::native_behaviour(
                "DoubleArray(1).iterator(): kotlin.jvm.internal.ArrayDoubleIterator \
                 super=kotlin.collections.DoubleIterator is Iterator DoubleIterator",
                "DoubleArray(1).iterator(): kotlin.DoubleArrayIterator \
                 super=kotlin.collections.DoubleIterator is Iterator DoubleIterator",
            ),
            Divergence::native_behaviour(
                "listOf(1, 2).iterator(): java.util.Arrays.ArrayItr super=kotlin.Any is \
                 Iterator",
                "listOf(1, 2).iterator(): null super=kotlin.Any is Iterator",
            ),
            Divergence::native_behaviour(
                "mutableListOf(1, 2).iterator(): java.util.ArrayList.Itr super=kotlin.Any is \
                 Iterator",
                "mutableListOf(1, 2).iterator(): kotlin.collections.ArrayList.Itr \
                 super=kotlin.Any is Iterator",
            ),
        ],
    );
}

#[test]
fn a_map_or_set_that_holds_itself_renders_the_marker() {
    run_driver("collection_to_string_self_reference");
}

#[test]
fn a_stdlib_thrower_whose_message_throws_propagates_that_exception() {
    run_driver("stdlib_thrower_keeps_first_exception");
}

#[test]
fn map_keys_and_entries_show_the_keys_without_comparing_them() {
    run_driver("map_views_do_not_compare_keys");
}

#[test]
fn a_map_or_set_stops_where_an_element_member_threw() {
    run_driver("map_stops_at_a_raise");
}

#[test]
fn a_default_to_string_whose_hash_code_throws_propagates_it() {
    run_driver("default_to_string_stops_at_a_raise");
}

#[test]
fn a_map_lookup_hashes_the_key_and_asks_the_stored_keys_equals() {
    // Kotlin/Native's HashMap
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt, JetBrains/kotlin
    // v2.4.10): findKey and addKey probe the open-addressed hash array and ask the STORED key's
    // equals, keysArray[index - 1] == key, with no identity check, so an asymmetric key is asked
    // the other way round and a key finds itself by equals; findValue walks the values from the
    // last index down with the stored value's equals; the hash array exists from construction, so
    // a fresh map hashes the key it is asked for.
    run_driver_against_kotlin_with(
        "map_lookup_protocol",
        &[
            Divergence::native_behaviour(
                "put b = null | hash(b) eq(b,a1) ",
                "put b = null | hash(b) eq(a1,b) ",
            ),
            Divergence::native_behaviour(
                "get a1 = 1 | hash(a1) ",
                "get a1 = 1 | hash(a1) eq(a1,a1) ",
            ),
            Divergence::native_behaviour(
                "get q = 1 | hash(q) eq(q,a1) ",
                "get q = 1 | hash(q) eq(a1,q) ",
            ),
            Divergence::native_behaviour(
                "get r = null | hash(r) eq(r,a1) eq(r,b) ",
                "get r = null | hash(r) eq(a1,r) eq(b,r) ",
            ),
            Divergence::native_behaviour(
                "containsKey c = true | hash(c) ",
                "containsKey c = true | hash(c) eq(c,c) ",
            ),
            Divergence::native_behaviour(
                "get x = 1 | hashA(x) eqA(x) ",
                "get x = null | hashA(x) eq(a1,null) eq(b,null) ",
            ),
            Divergence::native_behaviour(
                "remove b = 2 | hash(b) eq(b,a1) ",
                "remove b = 2 | hash(b) eq(a1,b) eq(b,b) ",
            ),
            Divergence::native_behaviour("get c = 3 | hash(c) ", "get c = 3 | hash(c) eq(c,c) "),
            Divergence::native_behaviour(
                "contains z = true | hashA(z) eqA(z) ",
                "contains z = false | hashA(z) eq(a1,null) ",
            ),
            Divergence::native_behaviour(
                "add d = false | hash(d) eq(d,a1) ",
                "add d = false | hash(d) eq(a1,d) ",
            ),
            Divergence::native_behaviour(
                "remove e = true | hash(e) eq(e,c) ",
                "remove e = true | hash(e) eq(c,e) ",
            ),
            Divergence::native_behaviour(
                "containsValue f = true | eq(f,a1) eq(f,c) ",
                "containsValue f = true | eq(c,f) ",
            ),
            Divergence::native_behaviour(
                "containsValue a1 = true | ",
                "containsValue a1 = true | eq(c,a1) eq(a1,a1) ",
            ),
            Divergence::native_behaviour(
                "containsValue g = true | eqA(g) eqA(g) ",
                "containsValue g = false | eq(c,null) eq(a1,null) ",
            ),
            Divergence::native_behaviour("fresh get a = null | ", "fresh get a = null | hash(a) "),
            Divergence::native_behaviour(
                "fresh containsKey b = false | ",
                "fresh containsKey b = false | hash(b) ",
            ),
            Divergence::native_behaviour(
                "fresh getOrDefault o = 5 | ",
                "fresh getOrDefault o = 5 | hash(o) ",
            ),
            Divergence::native_behaviour(
                "fresh set contains f = false | ",
                "fresh set contains f = false | hash(f) ",
            ),
        ],
    );
}

#[test]
fn every_map_and_set_iterates_in_insertion_order() {
    // Kotlin/Native's HashMap and HashSet
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt and
    // libraries/stdlib/native-wasm/src/kotlin/collections/HashSet.kt, JetBrains/kotlin v2.4.10)
    // keep keys in insertion order in keysArray and iterate by index, so every map and set
    // iterates in insertion order, a removed and re-added key going to the end; LinkedHashMap and
    // LinkedHashSet are type aliases for them.
    run_driver_against_kotlin_with(
        "map_iteration_order",
        &[
            Divergence::native_behaviour("[16, 0, 33, 1, 17, 49, 2]", "[33, 1, 17, 16, 0, 49, 2]"),
            Divergence::native_behaviour(
                "[banana, date, apple, cherry]",
                "[banana, apple, cherry, date]",
            ),
            Divergence::native_behaviour("[64, 17, 3, 100, 5]", "[100, 3, 17, 64, 5]"),
            Divergence::native_behaviour(
                "[0, 32, 64, 96, 128, 160, 192, 16, 48, 80, 112, 144, 176]",
                "[0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192]",
            ),
            Divergence::native_behaviour("[1, 15, 31, 47, 63]", "[15, 31, 47, 63, 1]"),
            Divergence::native_behaviour(
                "[0, 2, 4, 6, 8, 1, 3, 5, 7, 9]",
                "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]",
            ),
            Divergence::native_behaviour(
                "[-1, 65537, 65536, 1, -16]",
                "[-1, -16, 65536, 65537, 1]",
            ),
            Divergence::native_behaviour("[1, 17, 2, 3]", "[2, 3, 1, 17]"),
            Divergence::native_behaviour("[4, 9, 2]", "[9, 2, 4]"),
        ],
    );
}

#[test]
fn maps_sets_entries_and_views_are_kotlin_natives_classes_and_live() {
    // Kotlin/Native's classes
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt, JetBrains/kotlin
    // v2.4.10): LinkedHashMap and LinkedHashSet are type aliases for HashMap and HashSet, an entry
    // is a HashMap.EntryRef reading the map by index, and the views are HashMapKeys, HashMapValues
    // and HashMapEntrySet. Itr.hasNext is index < map.length, which a clear() sets to zero;
    // HashSet.contains asks the stored element's equals.
    run_driver_against_kotlin_with(
        "map_kinds_and_views",
        &[
            Divergence::native_behaviour(
                "java.util.LinkedHashMap.Entry Entry java.util.LinkedHashMap.LinkedKeySet \
                 LinkedKeySet java.util.LinkedHashMap.LinkedValues LinkedValues \
                 java.util.LinkedHashMap.LinkedEntrySet LinkedEntrySet",
                "kotlin.collections.HashMap.EntryRef EntryRef kotlin.collections.HashMapKeys \
                 HashMapKeys kotlin.collections.HashMapValues HashMapValues \
                 kotlin.collections.HashMapEntrySet HashMapEntrySet",
            ),
            Divergence::native_behaviour(
                "java.util.HashMap.Node Node java.util.HashMap.KeySet KeySet \
                 java.util.HashMap.Values Values java.util.HashMap.EntrySet EntrySet",
                "kotlin.collections.HashMap.EntryRef EntryRef kotlin.collections.HashMapKeys \
                 HashMapKeys kotlin.collections.HashMapValues HashMapValues \
                 kotlin.collections.HashMapEntrySet HashMapEntrySet",
            ),
            Divergence::native_behaviour(
                "true false true false true true",
                "true true true true true true",
            ),
            Divergence::native_behaviour(
                "java.util.LinkedHashMap LinkedHashMap java.util.HashMap HashMap \
                 java.util.HashSet HashSet java.util.LinkedHashSet LinkedHashSet",
                "kotlin.collections.HashMap HashMap kotlin.collections.HashMap HashMap \
                 kotlin.collections.HashSet HashSet kotlin.collections.HashSet HashSet",
            ),
            Divergence::native_behaviour(
                "hasNext after a clear: true",
                "hasNext after a clear: false",
            ),
            Divergence::native_behaviour(
                "in: true | hash(q) eq(q,b) ",
                "in: true | hash(q) eq(b,q) ",
            ),
        ],
    );
}

#[test]
fn map_of_and_set_of_nothing_are_the_shared_read_only_empty_map_and_set() {
    // Kotlin's common stdlib (libraries/stdlib/src/kotlin/collections/Maps.kt and Sets.kt,
    // JetBrains/kotlin v2.4.10), which Kotlin/Native compiles as written: mapOf(vararg) is
    // emptyMap() with no pairs and setOf(vararg) is elements.toSet(), emptySet() for none, the
    // one EmptyMap and EmptySet objects; neither is a MutableMap or MutableSet, and their
    // iterator is EmptyIterator. hashMapOf and hashSetOf are always a new HashMap or HashSet. A
    // failed cast keeps Kotlin/Native's wording (krusty_classes.c, kt_fail_cast).
    run_driver_against_kotlin_with(
        "empty_map_and_set",
        &[
            Divergence::native_message(
                "put: threw ClassCastException: kotlin.collections.EmptyMap cannot be cast to \
                 kotlin.collections.MutableMap",
                "put: threw ClassCastException: class kotlin.collections.EmptyMap cannot be cast \
                 to class kotlin.collections.MutableMap",
            ),
            Divergence::native_message(
                "add: threw ClassCastException: kotlin.collections.EmptySet cannot be cast to \
                 kotlin.collections.MutableSet",
                "add: threw ClassCastException: class kotlin.collections.EmptySet cannot be cast \
                 to class kotlin.collections.MutableSet",
            ),
            Divergence::native_behaviour(
                "hashMapOf: java.util.HashMap HashMap false true true",
                "hashMapOf: kotlin.collections.HashMap HashMap false true true",
            ),
            Divergence::native_behaviour(
                "hashSetOf: java.util.HashSet HashSet false true true",
                "hashSetOf: kotlin.collections.HashSet HashSet false true true",
            ),
        ],
    );
}

#[test]
fn a_map_changed_by_a_keys_callback_goes_on_as_kotlin_natives_does() {
    // Kotlin/Native's HashMap
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt, JetBrains/kotlin
    // v2.4.10): get reads valuesArray at the index findKey found after the stored key's equals
    // ran, so a match the callback removed answers null, and a probe that the removal emptied ends
    // the search; toString walks EntriesItr.nextAppendString, which checks for no change, so a key
    // the callback added is rendered; findValue walks from the last index down, so it compares the
    // entry the callback leaves in place.
    run_driver_against_kotlin_with(
        "map_callback_mutation",
        &[
            Divergence::native_behaviour(
                "LinkedHashMap get removing its match: A",
                "LinkedHashMap get removing its match: null",
            ),
            Divergence::native_behaviour(
                "HashMap get removing another key: B",
                "HashMap get removing another key: null",
            ),
            Divergence::native_behaviour(
                "toString adding a key: threw ConcurrentModificationException: null",
                "toString adding a key: {a=A, x=X, y=Y}",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap containsValue removing its entry: false",
                "LinkedHashMap containsValue removing its entry: true",
            ),
        ],
    );
}

#[test]
fn maps_and_sets_compare_across_classes_as_kotlin_natives_do() {
    // Kotlin/Native's HashMap.equals is contentEquals
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt, JetBrains/kotlin
    // v2.4.10): the sizes, then containsAllEntries walks the OTHER map's entries and looks each up
    // here, asking the stored key's and then the stored value's equals, and catches only
    // ClassCastException; a set's equals is AbstractSet.setEquals, this.containsAll(other), which
    // walks the other set and looks each element up here with no catch
    // (libraries/stdlib/src/kotlin/collections/AbstractSet.kt); EntryRef.equals asks other.key ==
    // key and other.value == value. So no call is identity-checked away, a NullPointerException
    // propagates, and a change to the receiver inside a comparison leaves the walk of the other
    // map untouched.
    run_driver_against_kotlin_with(
        "map_equality_across_kinds",
        &[
            Divergence::native_behaviour(
                "m1 == m2 = true | hash(a) eq(a,b2) eq(a,a2) eq(va,va2) hash(b) eq(b,b2) \
                 eq(vb,vb2) ",
                "m1 == m2 = true | hash(b2) eq(a,b2) eq(b,b2) eq(vb,vb2) hash(a2) eq(a,a2) \
                 eq(va,va2) ",
            ),
            Divergence::native_behaviour(
                "m2 == m1 = true | hash(b2) eq(b2,a) eq(b2,b) eq(vb2,vb) hash(a2) eq(a2,a) \
                 eq(va2,va) ",
                "m2 == m1 = true | hash(a) eq(b2,a) eq(a2,a) eq(va2,va) hash(b) eq(b2,b) \
                 eq(vb2,vb) ",
            ),
            Divergence::native_behaviour(
                "n1 == n2 = true | hash(a) eq(a,a2) hash(a) eq(a,a2) ",
                "n1 == n2 = true | hash(a2) eq(a,a2) ",
            ),
            Divergence::native_behaviour(
                "n1 == n3 = false | hash(a) eq(a,b2) hash(a) eq(a,b2) ",
                "n1 == n3 = false | hash(b2) eq(a,b2) ",
            ),
            Divergence::native_behaviour(
                "s1 == s2 = true | hash(b2) eq(b2,a) eq(b2,b) hash(a2) eq(a2,a) ",
                "s1 == s2 = true | hash(b2) eq(a,b2) eq(b,b2) hash(a2) eq(a,a2) ",
            ),
            Divergence::native_behaviour(
                "s2 == s1 = true | hash(a) eq(a,b2) eq(a,a2) hash(b) eq(b,b2) ",
                "s2 == s1 = true | hash(a) eq(b2,a) eq(a2,a) hash(b) eq(b2,b) ",
            ),
            Divergence::native_behaviour(
                "s1 == m1.keys = true | hash(a) hash(b) eq(b,a) ",
                "s1 == m1.keys = true | hash(a) eq(a,a) hash(b) eq(a,b) eq(b,b) ",
            ),
            Divergence::native_behaviour(
                "m1.keys == s1 = true | hash(a) hash(b) eq(b,a) ",
                "m1.keys == s1 = true | hash(a) eq(a,a) hash(b) eq(a,b) eq(b,b) ",
            ),
            Divergence::native_behaviour(
                "m1.entries == m2.entries = true | hash(b2) eq(b2,a) eq(b2,b) eq(b,b2) \
                 eq(vb,vb2) hash(a2) eq(a2,a) eq(a,a2) eq(va,va2) ",
                "m1.entries == m2.entries = true | hash(b2) eq(a,b2) eq(b,b2) eq(vb,vb2) \
                 hash(a2) eq(a,a2) eq(va,va2) ",
            ),
            Divergence::native_behaviour(
                "e1 == e2 = true | eq(a,a2) eq(va,va2) ",
                "e1 == e2 = true | eq(a2,a) eq(va2,va) ",
            ),
            Divergence::native_behaviour(
                "pm == pm2 = false | hash(x) throw(x) ",
                "pm == pm2 = threw NullPointerException: null | hash(y) throw(x) ",
            ),
            Divergence::native_behaviour(
                "ps == ps2 = false | hash(t) throw(t) ",
                "ps == ps2 = threw NullPointerException: null | hash(t) throw(s) ",
            ),
            Divergence::native_behaviour(
                "c1 == c2 = threw ConcurrentModificationException: null | ",
                "c1 == c2 = true | ",
            ),
        ],
    );
}

#[test]
fn every_built_in_companion_is_one_object_of_its_own() {
    run_driver_against_kotlin("companion_objects");
}

#[test]
fn a_map_compared_with_the_programs_map_fails_naming_it() {
    run_driver_expecting_failure(
        "map_equality_program_map",
        "krusty: comparing with the program's Map ProgramMap, whose members this runtime cannot \
         call\n",
    );
}

#[test]
fn a_map_lookup_reads_the_arrays_a_callback_cleared() {
    // Kotlin/Native's HashMap
    // (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt, JetBrains/kotlin
    // v2.4.10) has no chains: findKey and addKey read the hash array and keysArray afresh at every
    // probe, and clear() empties both, so a lookup whose first comparison cleared the map finds an
    // empty slot next and answers null, one that refilled it finds the refilled key, a removal
    // finds nothing to remove, and a put claims the slot after the cleared one, where a later
    // lookup, starting at the key's own slot, does not reach it.
    run_driver_against_kotlin_with(
        "map_chain_callback",
        &[
            Divergence::native_behaviour(
                "HashMap get clearing: B {} size=0",
                "HashMap get clearing: null {} size=0",
            ),
            Divergence::native_behaviour(
                "HashMap get clearing and refilling: B {c=C, d=D} size=2",
                "HashMap get clearing and refilling: D {c=C, d=D} size=2",
            ),
            Divergence::native_behaviour(
                "HashMap containsKey clearing: true {} size=0",
                "HashMap containsKey clearing: false {} size=0",
            ),
            Divergence::native_behaviour(
                "HashMap remove clearing: B {} size=-1",
                "HashMap remove clearing: null {} size=0",
            ),
            Divergence::native_behaviour(
                "HashMap put clearing: null {} size=1",
                "HashMap put clearing: null {c=C} size=1",
            ),
            Divergence::native_behaviour(
                "HashMap get after put clearing: null {} size=1",
                "HashMap get after put clearing: null {c=C} size=1",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap get clearing: B {} size=0",
                "LinkedHashMap get clearing: null {} size=0",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap get clearing and refilling: B {c=C, d=D} size=2",
                "LinkedHashMap get clearing and refilling: D {c=C, d=D} size=2",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap containsKey clearing: true {} size=0",
                "LinkedHashMap containsKey clearing: false {} size=0",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap remove clearing: B {} size=-1",
                "LinkedHashMap remove clearing: null {} size=0",
            ),
        ],
    );
}

#[test]
fn a_map_iterator_is_kotlin_natives_class_for_what_it_hands_out() {
    // Kotlin/Native's iterators are HashMap.KeysItr, ValuesItr and EntriesItr under the open class
    // HashMap.Itr, for either spelling of a map or a set, since LinkedHashMap and LinkedHashSet
    // are type aliases (kotlin-native/runtime/src/main/kotlin/kotlin/collections/HashMap.kt,
    // JetBrains/kotlin v2.4.10).
    run_driver_against_kotlin_with(
        "map_iterator_identity",
        &[
            Divergence::native_behaviour(
                "HashMap: java.util.HashMap.EntryIterator EntryIterator \
                 super=java.util.HashMap.HashIterator true",
                "HashMap: kotlin.collections.HashMap.EntriesItr EntriesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "HashMap.keys: java.util.HashMap.KeyIterator KeyIterator \
                 super=java.util.HashMap.HashIterator true",
                "HashMap.keys: kotlin.collections.HashMap.KeysItr KeysItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "HashMap.values: java.util.HashMap.ValueIterator ValueIterator \
                 super=java.util.HashMap.HashIterator true",
                "HashMap.values: kotlin.collections.HashMap.ValuesItr ValuesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "HashMap.entries: java.util.HashMap.EntryIterator EntryIterator \
                 super=java.util.HashMap.HashIterator true",
                "HashMap.entries: kotlin.collections.HashMap.EntriesItr EntriesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap: java.util.LinkedHashMap.LinkedEntryIterator \
                 LinkedEntryIterator super=java.util.LinkedHashMap.LinkedHashIterator true",
                "LinkedHashMap: kotlin.collections.HashMap.EntriesItr EntriesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap.keys: java.util.LinkedHashMap.LinkedKeyIterator \
                 LinkedKeyIterator super=java.util.LinkedHashMap.LinkedHashIterator true",
                "LinkedHashMap.keys: kotlin.collections.HashMap.KeysItr KeysItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap.values: java.util.LinkedHashMap.LinkedValueIterator \
                 LinkedValueIterator super=java.util.LinkedHashMap.LinkedHashIterator true",
                "LinkedHashMap.values: kotlin.collections.HashMap.ValuesItr ValuesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "LinkedHashMap.entries: java.util.LinkedHashMap.LinkedEntryIterator \
                 LinkedEntryIterator super=java.util.LinkedHashMap.LinkedHashIterator true",
                "LinkedHashMap.entries: kotlin.collections.HashMap.EntriesItr EntriesItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "HashSet: java.util.HashMap.KeyIterator KeyIterator \
                 super=java.util.HashMap.HashIterator true",
                "HashSet: kotlin.collections.HashMap.KeysItr KeysItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "LinkedHashSet: java.util.LinkedHashMap.LinkedKeyIterator LinkedKeyIterator \
                 super=java.util.LinkedHashMap.LinkedHashIterator true",
                "LinkedHashSet: kotlin.collections.HashMap.KeysItr KeysItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
            Divergence::native_behaviour(
                "empty HashMap.keys: java.util.HashMap.KeyIterator KeyIterator \
                 super=java.util.HashMap.HashIterator true",
                "empty HashMap.keys: kotlin.collections.HashMap.KeysItr KeysItr \
                 super=kotlin.collections.HashMap.Itr true",
            ),
        ],
    );
}

#[test]
fn unboxing_a_null_unsigned_records_a_null_pointer_exception_and_returns() {
    run_driver_against_kotlin("unsigned_unbox_null");
}

#[test]
fn equal_callable_references_hash_on_the_wrapping_ring() {
    run_driver("reference_hash_code");
}

#[test]
fn string_literals_share_one_root_and_stay_interned_and_alive() {
    run_driver("string_literal_roots");
}

#[test]
fn a_throwable_subclass_is_allocated_at_its_own_size() {
    run_driver_against_kotlin("throwable_subclass_size");
}

#[test]
fn integer_arithmetic_and_exceptions_answer_as_kotlin_does() {
    // Kotlin/Native names a class kotlin.X where the JVM names java.lang.X: Throwable(cause) takes
    // cause?.toString() as its message, and Throwable.toString starts with
    // this::class.qualifiedName (kotlin-native/runtime/src/main/kotlin/kotlin/Throwable.kt,
    // JetBrains/kotlin v2.4.10); assertFailsWith reports the expected KClass and the thrown
    // Throwable through their toString
    // (libraries/kotlin.test/common/src/main/kotlin/kotlin/test/Assertions.kt). The rest of each
    // message is the JVM's.
    run_driver_against_kotlin_with(
        "arithmetic_and_exceptions",
        &[
            Divergence::native_behaviour(
                "RuntimeException(bare).message = java.lang.IllegalStateException",
                "RuntimeException(bare).message = kotlin.IllegalStateException",
            ),
            Divergence::native_behaviour(
                "assertFailsWith<IllegalStateException> completing = AssertionError: Expected \
                 an exception of class java.lang.IllegalStateException to be thrown, but was \
                 completed successfully.",
                "assertFailsWith<IllegalStateException> completing = AssertionError: Expected \
                 an exception of class kotlin.IllegalStateException to be thrown, but was \
                 completed successfully.",
            ),
            Divergence::native_behaviour(
                "assertFailsWith<IllegalStateException>(\"m\") throwing = AssertionError: m. \
                 Expected an exception of class java.lang.IllegalStateException to be thrown, \
                 but was java.lang.ArithmeticException: / by zero",
                "assertFailsWith<IllegalStateException>(\"m\") throwing = AssertionError: m. \
                 Expected an exception of class kotlin.IllegalStateException to be thrown, but \
                 was kotlin.ArithmeticException: / by zero",
            ),
        ],
    );
}

#[test]
fn a_program_member_that_raises_inside_a_runtime_call_keeps_its_exception_in_flight() {
    // Kotlin/Native's KFunctionImpl.hashCode asks receiver.hashCode()
    // (kotlin-native/runtime/src/main/kotlin/kotlin/native/internal/KFunctionImpl.kt,
    // JetBrains/kotlin v2.4.10), so a bound reference whose receiver's hashCode throws propagates
    // it, where the JVM's FunctionReference.hashCode never asks the receiver.
    run_driver_against_kotlin_with(
        "user_code_raise_keeps_first_exception",
        &[Divergence::native_behaviour(
            "(u::f).hashCode() = completed",
            "(u::f).hashCode() = threw Boom",
        )],
    );
}

#[test]
fn a_print_whose_to_string_raises_writes_nothing() {
    // The driver's stdout is compared whole with Kotlin's transcript: a byte before its first line
    // is one `print` or `println` wrote after the rendering raised.
    run_driver_against_kotlin("print_of_raising_to_string_writes_nothing");
}

#[test]
fn the_uncaught_report_runs_to_string_with_nothing_in_flight() {
    run_driver_expecting_failure(
        "uncaught_report_runs_to_string_with_nothing_pending",
        "Exception in thread \"main\" Failure: disk full\n",
    );
}

#[test]
fn an_uncaught_exception_whose_to_string_raises_is_reported_as_the_jvm_reports_it() {
    run_driver_expecting_failure(
        "uncaught_report_whose_to_string_throws",
        "Exception in thread \"main\" \nException: kotlin.IllegalStateException thrown from the \
         UncaughtExceptionHandler in thread \"main\"\n",
    );
}

#[test]
fn a_negative_array_size_raises_illegal_argument_exception() {
    // Kotlin/Native's type: its array constructors allocate through AllocArrayInstance, which
    // calls ThrowIllegalArgumentException for a negative size (disassembly of the Kotlin/Native
    // 2.4.10 linux_x64 stdlib cache, as for StringBuilder(-1)). The message is the JVM's, the
    // size.
    run_driver_against_kotlin_with(
        "array_new_negative_size",
        &[
            Divergence::native_behaviour(
                "Array<Any?>(-1) threw NegativeArraySizeException: -1",
                "Array<Any?>(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "ByteArray(-1) threw NegativeArraySizeException: -1",
                "ByteArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "IntArray(-1) threw NegativeArraySizeException: -1",
                "IntArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "LongArray(-1) threw NegativeArraySizeException: -1",
                "LongArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "CharArray(-1) threw NegativeArraySizeException: -1",
                "CharArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "DoubleArray(-1) threw NegativeArraySizeException: -1",
                "DoubleArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "ULongArray(-1) threw NegativeArraySizeException: -1",
                "ULongArray(-1) threw IllegalArgumentException: -1",
            ),
            Divergence::native_behaviour(
                "ByteArray(Int.MIN_VALUE) threw NegativeArraySizeException: -2147483648",
                "ByteArray(Int.MIN_VALUE) threw IllegalArgumentException: -2147483648",
            ),
        ],
    );
}

#[test]
fn an_unmatched_exhaustive_when_raises_no_when_branch_matched_exception() {
    run_driver_against_kotlin("no_when_branch_matched");
}

#[test]
fn a_member_access_on_null_raises_null_pointer_exception() {
    run_driver_against_kotlin("null_receiver_raises");
}

#[test]
fn a_call_through_dispatch_on_null_stops_at_the_pending_check() {
    run_driver("dispatch_on_null");
}

#[test]
fn callable_references_are_equal_by_declaration_and_receiver() {
    // Kotlin/Native's KFunctionImpl.hashCode folds receiver.hashCode() into the hash
    // (kotlin-native/runtime/src/main/kotlin/kotlin/native/internal/KFunctionImpl.kt,
    // JetBrains/kotlin v2.4.10), so hashing a bound reference asks its receiver; the JVM's
    // FunctionReference.hashCode reads the owner, name and signature only.
    run_driver_against_kotlin_with(
        "reference_identity",
        &[Divergence::native_behaviour(
            "equal bound references hash alike = true []",
            "equal bound references hash alike = true [hash(1) hash(1)]",
        )],
    );
}

#[test]
fn a_failed_assertion_reports_kotlin_tests_wording() {
    run_driver_against_kotlin("assertion_wording");
}

fn compiled_build_script() -> PathBuf {
    static BUILD_SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    BUILD_SCRIPT
        .get_or_init(|| {
            let scratch = common::scratch_dir().expect("scratch directory");
            let executable = scratch.join("native-runtime-build-script");
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = Command::new(rustc)
                .args(["--edition=2021", "build.rs", "-o"])
                .arg(&executable)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("compile build.rs");
            assert_eq!(
                (output.status.success(), output.stdout, output.stderr),
                (true, Vec::new(), Vec::new()),
                "build.rs compiles as a standalone build-script executable"
            );
            executable
        })
        .clone()
}

fn run_build_script(compiler: &Path, out_dir: &Path) -> Output {
    Command::new(compiled_build_script())
        .env("OUT_DIR", out_dir)
        .env("KRUSTY_RUNTIME_CC", compiler)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run build.rs")
}

#[test]
fn a_missing_runtime_compiler_leaves_the_native_target_unavailable() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("missing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let missing = scratch.join("compiler-that-does-not-exist");

    let output = run_build_script(&missing, &out_dir);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stderr, b"");
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_maps.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_classes.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_lang.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_threads.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_thread.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_io.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_net.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_process.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_string.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-changed=src/native/target_contract.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:rustc-env=KRUSTY_RUNTIME_COMPILER={0}\n\
             cargo:warning=native runtime: compiler `{0}` was not found; no native target will be available. Install clang, or set KRUSTY_RUNTIME_CC.\n\
             cargo:rerun-if-env-changed=PATH\n",
            missing.display()
        )
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("prebuilt_runtime.rs"))
            .expect("read generated runtime table"),
        "// Generated by build.rs: the native runtime, prebuilt per target.\n\
         pub const PREBUILT: &[(crate::native::NativeTarget, &[RuntimeObject])] = &[\n\
         ];\n\
         /// Whether any target's runtime was prebuilt (a C compiler was available when krusty was built).\n\
         pub const AVAILABLE: bool = false;\n\
         /// The runtime sources every target's objects were compiled from, in the table's order. Only\n\
         /// the test that checks the table against them reads it.\n\
         #[cfg(test)]\n\
         pub const SOURCES: &[&str] = &[\"krusty_rt.c\", \"krusty_collections.c\", \"krusty_maps.c\", \"krusty_classes.c\", \"krusty_lang.c\", \"krusty_fp.c\", \"krusty_gc.c\", \"krusty_threads.c\", \"krusty_start.c\", \"krusty_posix_thread.c\", \"krusty_posix_io.c\", \"krusty_posix_net.c\", \"krusty_posix_process.c\", \"krusty_posix_string.c\"];\n"
    );
}

#[test]
fn a_failing_runtime_compiler_fails_the_build() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("failing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let compiler = scratch.join("runtime-compiler-exits-one");
    fs::write(&compiler, "#!/bin/sh\nexit 1\n").expect("write failing compiler");
    let mut permissions = fs::metadata(&compiler)
        .expect("read failing compiler metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&compiler, permissions).expect("make failing compiler executable");

    let output = run_build_script(&compiler, &out_dir);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("build-script stderr is UTF-8"),
        format!(
            "native runtime: `{}` failed compiling `krusty_rt.c` for \
             `x86_64-unknown-linux-gnu` (exit status: 1)\n",
            compiler.display()
        )
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_maps.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_classes.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_lang.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_threads.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_thread.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_io.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_net.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_process.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_string.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-changed=src/native/target_contract.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:rustc-env=KRUSTY_RUNTIME_COMPILER={}\n",
            compiler.display()
        )
    );
    assert!(
        !out_dir.join("prebuilt_runtime.rs").exists(),
        "a failed compiler must not publish a partial target table"
    );
}

#[test]
fn a_runtime_compiler_that_emits_another_machines_code_fails_the_build() {
    // A compiler wrapper that exits 0 but ignores the requested `--target`: it writes an AArch64
    // relocatable's ELF identity when asked for x86_64. The check after each compile names what it
    // wrote, and nothing is published.
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("wrong-machine-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let compiler = scratch.join("runtime-compiler-writes-aarch64");
    fs::write(
        &compiler,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         if [ \"$1\" = -o ]; then shift; printf '\\177ELF\\2\\1\\1\\0\\0\\0\\0\\0\\0\\0\\0\\0\\1\\0\\267\\0' > \"$1\"; fi\n\
         shift\n\
         done\n",
    )
    .expect("write wrong-machine compiler");
    let mut permissions = fs::metadata(&compiler)
        .expect("read wrong-machine compiler metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&compiler, permissions).expect("make wrong-machine compiler executable");

    let output = run_build_script(&compiler, &out_dir);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("build-script stderr is UTF-8"),
        format!(
            "native runtime: `{}` built `krusty_rt.c` for `x86_64-unknown-linux-gnu` as \
             code for ELF machine 183, not 62\n",
            compiler.display()
        )
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_maps.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_classes.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_lang.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_threads.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_thread.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_io.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_net.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_process.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_string.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-changed=src/native/target_contract.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:rustc-env=KRUSTY_RUNTIME_COMPILER={}\n",
            compiler.display()
        )
    );
    assert!(
        !out_dir.join("prebuilt_runtime.rs").exists(),
        "another machine's object must not publish a partial target table"
    );
}

#[test]
fn a_runtime_object_aligned_past_the_targets_page_fails_the_build() {
    // An x86_64 relocatable whose one loaded section asks for 8 KiB alignment: more than the 4 KiB
    // page krusty's linker starts x86_64's writable segment on, so no link could honour it.
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("overaligned-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let mut object = vec![0u8; 128];
    object[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x00");
    object[16..20].copy_from_slice(&[1, 0, 62, 0]); // ET_REL, EM_X86_64
    object[0x28..0x30].copy_from_slice(&64u64.to_le_bytes()); // e_shoff
    object[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
    object[0x3c..0x3e].copy_from_slice(&1u16.to_le_bytes()); // e_shnum
    object[64 + 0x08..64 + 0x10].copy_from_slice(&2u64.to_le_bytes()); // sh_flags: SHF_ALLOC
    object[64 + 0x30..64 + 0x38].copy_from_slice(&0x2000u64.to_le_bytes()); // sh_addralign
    let prepared = scratch.join("overaligned-object.o");
    fs::write(&prepared, object).expect("write over-aligned object");
    let compiler = scratch.join("runtime-compiler-writes-overaligned");
    fs::write(
        &compiler,
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do\n\
             if [ \"$1\" = -o ]; then shift; cp '{}' \"$1\"; fi\n\
             shift\n\
             done\n",
            prepared.display()
        ),
    )
    .expect("write over-aligned compiler");
    let mut permissions = fs::metadata(&compiler)
        .expect("read over-aligned compiler metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&compiler, permissions).expect("make over-aligned compiler executable");

    let output = run_build_script(&compiler, &out_dir);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("build-script stderr is UTF-8"),
        format!(
            "native runtime: `{}` built `krusty_rt.c` for `x86_64-unknown-linux-gnu` as an object \
             with a section aligned to 0x2000 bytes, more than the target's 0x1000-byte maximum \
             page size\n",
            compiler.display()
        )
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_rt.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_collections.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_maps.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_classes.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_lang.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_threads.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_thread.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_io.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_net.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_process.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix_string.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_internal.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_posix.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-changed=src/native/target_contract.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:rustc-env=KRUSTY_RUNTIME_COMPILER={}\n",
            compiler.display()
        )
    );
    assert!(
        !out_dir.join("prebuilt_runtime.rs").exists(),
        "an over-aligned object must not publish a partial target table"
    );
}
