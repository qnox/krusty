//! Running the suite's `box()` programs on the native backend.
//!
//! The reference answer comes from the JVM path in `super`; everything here is about getting the
//! same program through krusty's own code generator, its linker and the prebuilt runtime, and
//! saying precisely what came back. The four outcomes are the whole vocabulary: an ANSWER to
//! compare, a DECLINE the generator made, an abnormal END, and a build that cannot reach the
//! target at all. Which of those a given test treats as a failure is the test's business, which is
//! why each helper below is a different answer to that one question.

use super::jdk_modules;

const NATIVE_SYS_HEADER: &str = include_str!("../../src/native/runtime/krusty_sys.h");
const NATIVE_RUNTIME_HEADER: &str = include_str!("../../src/native/runtime/krusty_rt.h");

/// The runtime's syscall surface, for tests that pin the native concurrency contract.
pub fn native_sys_header() -> &'static str {
    NATIVE_SYS_HEADER
}

/// Materialize the two public runtime headers beside a C integration-test driver.
pub fn write_native_runtime_headers(directory: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(directory.join("krusty_sys.h"), NATIVE_SYS_HEADER)?;
    std::fs::write(directory.join("krusty_rt.h"), NATIVE_RUNTIME_HEADER)
}

/// The prefix every native decline carries, so a decline can be told from a wrong answer.
const NATIVE_DECLINE: &str = "krusty: the native backend does not support ";

/// One cross-checked target: the native backend, when this build has a runtime for the host.
///
/// A program the generator ACCEPTS must answer what the JVM answered; anything else — a different
/// string, a crash, a failed link — fails the test it came from, which is where the shape that
/// provoked it is already written down.
pub(super) fn also_run_natively(src: &str, stem: &str, expected: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    match native_box_outcome(src, stem, target) {
        NativeBox::Declined(_) | NativeBox::Unavailable => {}
        NativeBox::Answered(answer) if answer == expected => {}
        NativeBox::Answered(answer) => panic!(
            "{stem}: the native backend answered {answer:?} where the JVM answered {expected:?}"
        ),
        failure => panic!(
            "{stem}: the native backend ran it wrong — {}",
            failure.as_failure()
        ),
    }
}

/// Require a multi-file module to lower and answer `expected`. `sources` is `(stem, text)`.
///
/// Same rule as [`expect_native_box`]: a decline fails the test, and a build that cannot reach
/// the host target skips.
pub fn expect_native_sources(sources: &[(&str, &str)], expected: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    match native_sources_outcome(sources, target) {
        NativeBox::Unavailable => {}
        NativeBox::Answered(answer) => assert_eq!(answer, expected, "{stem}"),
        NativeBox::Declined(reason) => {
            panic!("{stem}: the native backend must lower this program — {reason}")
        }
        failure => panic!(
            "{stem}: the native backend ran it wrong — {}",
            failure.as_failure()
        ),
    }
}

/// Require the native backend to LOWER `src` and answer `expected`: a decline fails the test.
///
/// [`cross_check_backends`] treats a decline as a skip, which is right for the suite at large — a
/// young backend must not turn every JVM-side test into its to-do list. A test written FOR a native
/// construct wants the opposite: it exists to pin that the construct is lowered, so "not supported
/// yet" is the failure it is meant to catch. Skips only when this build cannot reach the target.
#[allow(dead_code)]
pub fn expect_native_box(src: &str, stem: &str, expected: &str) {
    expect_native_sources(&[(stem, src)], expected);
}

/// Require the native backend to DECLINE `src`, naming `reason` in what it says.
///
/// The mirror of [`expect_native_box`], for a construct the generator must refuse rather than
/// emit. A decline is only ever a skip elsewhere, so nothing else can tell a construct that is
/// deliberately refused from one that quietly started being accepted — and for a refusal whose
/// point is that emitting the program would be WRONG, that difference is the whole test.
#[allow(dead_code)]
pub fn expect_native_decline(src: &str, stem: &str, reason: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    match native_box_outcome(src, stem, target) {
        NativeBox::Unavailable => {}
        NativeBox::Declined(said) => assert!(
            said.contains(reason),
            "{stem}: declined for {said:?}, which does not name {reason:?}"
        ),
        NativeBox::Answered(answer) => {
            panic!("{stem}: the native backend emitted this program, answering {answer:?}")
        }
        failure => panic!(
            "{stem}: the native backend ran it — {}",
            failure.as_failure()
        ),
    }
}

/// Require the native backend to LOWER `src` and the program to END ABNORMALLY, with `code` and
/// `said` somewhere on stderr.
///
/// For a program whose whole point is that it does not finish, [`expect_native_box`] has no answer
/// to compare: the entry never prints one. This is the form those tests take — the exit code and
/// the report ARE the observable behaviour, so they are what gets pinned.
#[allow(dead_code)]
pub fn expect_native_exit(src: &str, stem: &str, code: i32, said: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    match native_box_outcome(src, stem, target) {
        NativeBox::Unavailable => {}
        NativeBox::Exited {
            code: ended,
            stderr,
            ..
        } => {
            assert_eq!(ended, Some(code), "{stem}: stderr {stderr:?}");
            assert!(
                stderr.contains(said),
                "{stem}: stderr {stderr:?} does not name {said:?}"
            );
        }
        NativeBox::Declined(reason) => {
            panic!("{stem}: the native backend must lower this program — {reason}")
        }
        NativeBox::Answered(answer) => {
            panic!("{stem}: it ran to completion, answering {answer:?}")
        }
        failure => panic!(
            "{stem}: the native backend ran it wrong — {}",
            failure.as_failure()
        ),
    }
}

pub enum NativeBox {
    /// What `box()` returned. The JVM's answer for the same program is the oracle.
    Answered(String),
    /// Declined by the generator, or refused by the frontend before it — neither is the native
    /// backend running a program wrong. Carries what said so, for the directed tests that require
    /// a construct to be lowered rather than skipped.
    Declined(String),
    /// This build cannot reach the native target at all: no host target, no linker, no stdlib jar.
    /// Every native check is a skip, not a verdict.
    Unavailable,
    /// It ran and ended ABNORMALLY. Separate from [`NativeBox::Failed`] because for a program whose
    /// point is that it ends abnormally — an uncaught throw — the exit code and what it said on
    /// stderr are the answer, not the evidence of a broken test.
    Exited {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    Failed(String),
}

impl NativeBox {
    /// How an abnormal end reads when a test did not ask for one.
    pub fn as_failure(&self) -> String {
        match self {
            NativeBox::Exited {
                code,
                stdout,
                stderr,
            } => format!(
                "it ended abnormally: exit {code:?}; stdout {stdout:?}; stderr {:?}",
                stderr.trim()
            ),
            NativeBox::Failed(reason) => reason.clone(),
            _ => unreachable!("only an abnormal end reads as a failure"),
        }
    }
}

#[allow(dead_code)]
fn native_box_outcome(src: &str, stem: &str, target: krusty::native::NativeTarget) -> NativeBox {
    native_sources_outcome(&[(stem, src)], target)
}

/// The analysis a Native compilation of `sources` (`(stem, text)`) starts from: the semantic
/// platform and language settings every Native test compiles with, and the file stems in order.
/// `None` when this build has no stdlib to analyze against.
pub fn native_analysis(
    sources: &[(&str, &str)],
    diags: &mut krusty::diag::DiagSink,
) -> Option<(krusty::frontend::StreamingSourceSetAnalysis, Vec<String>)> {
    analyze_natively(sources, &[], false, diags)
}

/// [`native_analysis`] of a build applying every compiler plugin krusty ships, with their
/// `libraries` (the plugins' runtimes) on the classpath after the stdlib and `kotlin-test`.
pub fn native_analysis_with_plugins(
    sources: &[(&str, &str)],
    libraries: &[std::path::PathBuf],
    diags: &mut krusty::diag::DiagSink,
) -> Option<(krusty::frontend::StreamingSourceSetAnalysis, Vec<String>)> {
    analyze_natively(sources, libraries, true, diags)
}

fn analyze_natively(
    sources: &[(&str, &str)],
    libraries: &[std::path::PathBuf],
    plugins: bool,
    diags: &mut krusty::diag::DiagSink,
) -> Option<(krusty::frontend::StreamingSourceSetAnalysis, Vec<String>)> {
    use krusty::source::SourceInput;

    let jar = krusty::toolchain::stdlib_jar()?;
    // Stdlib AND JDK, the same pair the JVM helpers compile against. The bridge `native/intrinsics`
    // describes runs through here: signatures are read out of JVM artifacts until the provider is
    // klib-based, and `kotlin.RuntimeException` is a typealias for `java.lang.RuntimeException`, so
    // without the jimage the exception hierarchy has no declaration to resolve to. Nothing about
    // the EMITTED program changes — it still links only against the runtime — but a cross-check
    // whose native half cannot name what its JVM half named is a skip dressed as agreement.
    // Stdlib, the JDK jimage AND `kotlin-test`: the three the JVM lanes compile against. Each was
    // missing here at some point and each cost the same way — a program that cannot NAME what it
    // uses is reported as unresolvable, which reads as the frontend's problem rather than as a
    // gap in what this helper was given.
    let jars = std::iter::once(jar)
        .chain(krusty::toolchain::kotlin_test_jar())
        .chain(libraries.iter().cloned())
        .collect::<Vec<_>>();
    let classpath = super::cached_classpath(&jars, Some(&jdk_modules()));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let inputs: Vec<_> = sources
        .iter()
        .map(|(stem, src)| SourceInput::kotlin(src).with_file_stem(stem))
        .collect();
    let stems: Vec<String> = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect();
    let mut features = krusty::features::LangFeatures::new();
    for (_, src) in sources {
        features.apply_source_directives(src);
    }
    let platform = if plugins {
        super::with_native_plugins(platform)
    } else {
        platform.into()
    };
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, diags,
    );
    Some((analysis, stems))
}

/// Compile `sources` with the native code generator and link them into an executable image, or say
/// why there is none.
pub fn native_image(
    sources: &[(&str, &str)],
    target: krusty::native::NativeTarget,
) -> Result<Vec<u8>, NativeBox> {
    use krusty::diag::DiagSink;
    use krusty::native::{CraneliftBackend, Entry};

    let mut diags = DiagSink::new();
    let Some((analysis, stems)) = native_analysis(sources, &mut diags) else {
        return Err(NativeBox::Unavailable);
    };
    if let Some(refusal) = diags.diags.first() {
        return Err(NativeBox::Declined(format!(
            "the frontend refused it: {}",
            refusal.msg
        )));
    }
    // A conformance case answers through `box`; a program without one starts at Kotlin's `main`,
    // which is how a test pins what the backend does with that entry.
    let entry = if sources.iter().any(|(_, src)| src.contains("fun box(")) {
        Entry::Box
    } else {
        Entry::Main
    };
    let backend = CraneliftBackend::new(target).with_entry(entry).verified();
    let module_name = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    let artifacts =
        krusty::compiler::emit_analyzed(analysis, &stems, &backend, module_name, &mut diags);
    if let Some(decline) = diags
        .diags
        .iter()
        .find(|diagnostic| diagnostic.msg.starts_with(NATIVE_DECLINE))
    {
        return Err(NativeBox::Declined(decline.msg.clone()));
    }
    if let Some(refusal) = diags.diags.first() {
        return Err(NativeBox::Declined(refusal.msg.clone()));
    }
    if artifacts.is_empty() {
        return Err(NativeBox::Declined(
            "the backend emitted nothing".to_string(),
        ));
    }
    let objects: Vec<Vec<u8>> = artifacts
        .into_iter()
        .filter(|(name, _)| name.ends_with(".o"))
        .map(|(_, object)| object)
        .collect();
    let object_refs: Vec<&[u8]> = objects.iter().map(Vec::as_slice).collect();
    let image = match krusty::native::link_program(&object_refs, target) {
        Ok(image) => image,
        Err(error) => return Err(NativeBox::Failed(format!("link: {error}"))),
    };
    Ok(image)
}

/// Run a linked image with `arguments` and `input` on standard input, and an empty environment.
pub fn run_native_image(
    image: &[u8],
    stem: &str,
    arguments: &[&std::ffi::OsStr],
    input: &[u8],
) -> Result<std::process::Output, NativeBox> {
    use std::io::Write as _;

    let directory = std::env::temp_dir().join(format!(
        "krusty-native-e2e-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    if std::fs::create_dir_all(&directory).is_err() {
        return Err(NativeBox::Unavailable);
    }
    let executable = directory.join(stem);
    if let Err(error) = std::fs::write(&executable, image) {
        return Err(NativeBox::Failed(format!(
            "writing the executable: {error}"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
    }
    let child = super::spawn_freshly_written(
        std::process::Command::new(&executable)
            .args(arguments)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    );
    let output = child.and_then(|mut child| {
        // Written whole and closed before waiting: the inputs are small, and closing is what lets
        // the program see the end of its input.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let written = stdin.write_all(input);
        drop(stdin);
        // A program that stops reading early closes the pipe; that is its business, not a failure.
        if let Err(error) = written {
            if error.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(error);
            }
        }
        child.wait_with_output()
    });
    let _ = std::fs::remove_file(&executable);
    let _ = std::fs::remove_dir(&directory);
    output.map_err(|error| NativeBox::Failed(format!("running it: {error}")))
}

/// `sources` is `(file stem, text)` in the order the module is compiled. One of the files holds
/// `box` or `main`; the rest are the declarations it calls.
fn native_sources_outcome(
    sources: &[(&str, &str)],
    target: krusty::native::NativeTarget,
) -> NativeBox {
    let image = match native_image(sources, target) {
        Ok(image) => image,
        Err(outcome) => return outcome,
    };
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("program");
    let output = match run_native_image(&image, stem, &[], b"") {
        Ok(output) => output,
        Err(outcome) => return outcome,
    };
    if !output.status.success() {
        return NativeBox::Exited {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
    }
    // The entry prints what `box()` returned after a frame marker, and nothing after it: the
    // answer is every byte past the one marker. What comes before is the
    // program's own output — `when (b) { true -> println("t") … }` prints `t` and then answers
    // `OK` — which the JVM path never sees, because there the answer is a return value rather
    // than a stream. Reading the last LINE instead would take `"FAIL\nOK"` for `OK`.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    match stdout.split_once(krusty::native::BOX_RESULT_FRAME) {
        Some((_, answer)) if answer.contains(krusty::native::BOX_RESULT_FRAME) => {
            NativeBox::Failed("the program printed more than one box result frame".to_string())
        }
        Some((_, answer)) => NativeBox::Answered(answer.to_owned()),
        None => NativeBox::Failed(format!(
            "the program ended without framing an answer; stdout {stdout:?}"
        )),
    }
}
