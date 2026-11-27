//! Running the suite's `box()` programs on the native backend.
//!
//! The reference answer comes from the JVM path in `super`; everything here is about getting the
//! same program through krusty's own code generator, its linker and the prebuilt runtime, and
//! saying precisely what came back. The four outcomes are the whole vocabulary: an ANSWER to
//! compare, a DECLINE the generator made, an abnormal END, and a build that cannot reach the
//! target at all. Which of those a given test treats as a failure is the test's business, which is
//! why each helper below is a different answer to that one question.

use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use krusty_native::ImportLibrary;

/// The prefix every native decline carries, so a decline can be told from a wrong answer.
const NATIVE_DECLINE: &str = "krusty: the native backend does not support ";

/// One cross-checked target: the native backend, when this build has a runtime for the host.
///
/// A program the generator ACCEPTS must answer what the JVM answered; anything else — a different
/// string, a crash, a failed link — fails the test it came from, which is where the shape that
/// provoked it is already written down.
pub(super) fn also_run_natively(src: &str, stem: &str, expected: &str) {
    let Some(target) = krusty_native::NativeTarget::host() else {
        return;
    };
    if !krusty_native::can_link(target) {
        return;
    }
    match native_sources_outcome(&[(stem, src)], target, Some(expected)) {
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
    let Some(target) = krusty_native::NativeTarget::host() else {
        return;
    };
    if !krusty_native::can_link(target) {
        return;
    }
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    match native_sources_outcome(sources, target, Some(expected)) {
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
    expect_native_sources_decline(&[(stem, src)], reason);
}

/// [`expect_native_decline`] for a program of several files (`(stem, text)`).
pub fn expect_native_sources_decline(sources: &[(&str, &str)], reason: &str) {
    if let Some(said) = native_decline(sources) {
        let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
        assert!(
            said.contains(reason),
            "{stem}: declined for {said:?}, which does not name {reason:?}"
        );
    }
}

/// What the native backend says declining the module `sources` (`(stem, text)`), or `None` where
/// this host cannot link. Emitting or running the program fails the test.
pub fn native_decline(sources: &[(&str, &str)]) -> Option<String> {
    let target = krusty_native::NativeTarget::host()?;
    if !krusty_native::can_link(target) {
        return None;
    }
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    match native_sources_outcome(sources, target, None) {
        NativeBox::Unavailable => None,
        NativeBox::Declined(said) => Some(said),
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
    let Some(target) = krusty_native::NativeTarget::host() else {
        return;
    };
    if !krusty_native::can_link(target) {
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

#[derive(Debug)]
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

fn native_box_outcome(src: &str, stem: &str, target: krusty_native::NativeTarget) -> NativeBox {
    native_sources_outcome(&[(stem, src)], target, None)
}

/// Native's stdlib includes its `kotlin.test` declarations and bodies. Extra dependencies are
/// selected KLIBs too; JVM jars and the JDK cannot authenticate Native declaration identities.
pub(crate) fn native_library_inputs(
    libraries: &[std::path::PathBuf],
) -> Option<Vec<KlibLibraryInput>> {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    );
    if stdlib.is_none()
        && (std::env::var_os("KRUSTY_REQUIRE_KLIB").is_some()
            || std::env::var_os("KRUSTY_REQUIRE_KOTLIN_NATIVE").is_some())
    {
        panic!("Native tests require the selected Native stdlib KLIB");
    }
    Some(
        std::iter::once(KlibLibraryInput::platform_stdlib(
            stdlib?,
            KlibStdlibSemantics::Native,
        ))
        .chain(libraries.iter().cloned().map(KlibLibraryInput::dependency))
        .collect(),
    )
}

pub(crate) fn analyze_with_native_libraries(
    sources: &[(&str, &str)],
    libraries: KlibLibraries,
    plugins: bool,
    diags: &mut krusty::diag::DiagSink,
) -> (krusty::frontend::StreamingSourceSetAnalysis, Vec<String>) {
    use krusty::source::SourceInput;

    let prepared: Vec<String> = sources
        .iter()
        .map(|(_, src)| {
            krusty::conformance::prepare_test_source(src, krusty::conformance::TestTarget::Native)
        })
        .collect();
    let inputs: Vec<_> = sources
        .iter()
        .zip(&prepared)
        .map(|((stem, _), src)| SourceInput::kotlin(src).with_file_stem(stem))
        .collect();
    let stems: Vec<String> = sources
        .iter()
        .map(|(stem, _)| (*stem).to_string())
        .collect();
    let mut features = krusty::features::LangFeatures::from_environment();
    for (_, src) in sources {
        features.apply_source_directives(src);
    }
    // Every Native test analyzes for the Native target, so Native rules are not a choice this
    // helper can get wrong.
    let target = krusty::compilation_target::CompilationTarget::Native;
    let platform = if plugins {
        super::with_native_plugins(target, Box::new(libraries))
    } else {
        krusty::frontend::PlatformProvider::new(target, Box::new(libraries))
    };
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, diags,
    );
    (analysis, stems)
}

/// Compile `sources` with the native code generator and link them into an executable image, or say
/// why there is none.
pub fn native_image(
    sources: &[(&str, &str)],
    target: krusty_native::NativeTarget,
) -> Result<Vec<u8>, NativeBox> {
    let (objects, imports) = compile_native_sources(sources, target)?.into_parts();
    let object_refs: Vec<&[u8]> = objects.iter().map(Vec::as_slice).collect();
    krusty_native::link_program_with_libraries(&object_refs, &imports, target)
        .map_err(|error| NativeBox::Failed(format!("link: {error}")))
}

pub(crate) struct NativeCompilation {
    objects: Vec<Vec<u8>>,
    imports: Vec<ImportLibrary>,
}

impl NativeCompilation {
    /// Consume the objects and imports emitted from one selected Native KLIB snapshot together.
    pub(crate) fn into_parts(self) -> (Vec<Vec<u8>>, Vec<ImportLibrary>) {
        (self.objects, self.imports)
    }
}

/// Keep link facts beside emitted objects from the same selected KLIB snapshot.
pub(crate) fn compile_native_sources(
    sources: &[(&str, &str)],
    target: krusty_native::NativeTarget,
) -> Result<NativeCompilation, NativeBox> {
    use krusty::diag::DiagSink;
    use krusty_native::{CraneliftBackend, Entry, NativeKlibInputs};

    let inputs = native_library_inputs(&[]).ok_or(NativeBox::Unavailable)?;
    let (libraries, imports) = NativeKlibInputs::open(&inputs, target)
        .map_err(|error| NativeBox::Failed(format!("Native KLIB inputs: {error}")))?
        .into_parts();
    let mut diags = DiagSink::new();
    let (analysis, stems) = analyze_with_native_libraries(sources, libraries, false, &mut diags);
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
    // A warning, such as kotlinc's on a reified intersection, refuses nothing.
    if let Some(refusal) = diags
        .diags
        .iter()
        .find(|diagnostic| diagnostic.severity == krusty::diag::Severity::Error)
    {
        return Err(NativeBox::Declined(refusal.msg.clone()));
    }
    if artifacts.is_empty() {
        return Err(NativeBox::Declined(
            "the backend emitted nothing".to_string(),
        ));
    }
    Ok(NativeCompilation {
        objects: artifacts
            .into_iter()
            .filter(|(name, _)| name.ends_with(".o"))
            .map(|(_, object)| object)
            .collect(),
        imports,
    })
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
    let failure = match &output {
        Ok(output) if !output.status.success() => Some(format!(
            "native execution: {:?}; arguments {arguments:?}; status {}; stdout {:?}; stderr {:?}",
            executable,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(error) => Some(format!(
            "native execution: {:?}; arguments {arguments:?}; could not run: {error}",
            executable
        )),
        _ => None,
    };
    if let Some(report) = failure {
        let mut files: Vec<(&str, &[u8])> = vec![("program.elf", image), ("stdin", input)];
        if let Ok(output) = &output {
            files.push(("stdout", &output.stdout));
            files.push(("stderr", &output.stderr));
        }
        match crate::failure_evidence::retain(stem, &report, &files) {
            Ok(path) => eprintln!("{report}; failure artifacts: {}", path.display()),
            Err(error) => eprintln!("{report}; could not preserve failure artifacts: {error}"),
        }
    }
    let _ = std::fs::remove_file(&executable);
    let _ = std::fs::remove_dir(&directory);
    output.map_err(|error| NativeBox::Failed(format!("running it: {error}")))
}

/// `sources` is `(file stem, text)` in the order the module is compiled. One of the files holds
/// `box` or `main`; the rest are the declarations it calls.
fn native_sources_outcome(
    sources: &[(&str, &str)],
    target: krusty_native::NativeTarget,
    expected: Option<&str>,
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
    let outcome = if !output.status.success() {
        NativeBox::Exited {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        // The answer is every byte after the sole frame, including any embedded newline.
        match stdout.split_once(krusty_native::BOX_RESULT_FRAME) {
            Some((_, answer)) if answer.contains(krusty_native::BOX_RESULT_FRAME) => {
                NativeBox::Failed("the program printed more than one box result frame".to_string())
            }
            Some((_, answer)) => NativeBox::Answered(answer.to_owned()),
            None => NativeBox::Failed(format!(
                "the program ended without framing an answer; stdout {stdout:?}"
            )),
        }
    };
    let matches_expected =
        matches!(&outcome, NativeBox::Answered(answer) if expected == Some(answer.as_str()));
    if !matches_expected {
        let source = sources
            .iter()
            .map(|(stem, text)| format!("// FILE: {stem}.kt\n{text}\n"))
            .collect::<String>();
        let report =
            format!("target: {target:?}; expected box result: {expected:?}; actual: {outcome:?}");
        let files: &[(&str, &[u8])] = &[
            ("program.elf", &image),
            ("source.kt", source.as_bytes()),
            ("stdout", &output.stdout),
            ("stderr", &output.stderr),
        ];
        match crate::failure_evidence::retain(stem, &report, files) {
            Ok(path) => eprintln!("{report}; failure artifacts: {}", path.display()),
            Err(error) => eprintln!("{report}; could not preserve failure artifacts: {error}"),
        }
    }
    outcome
}
