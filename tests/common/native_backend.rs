//! Running the suite's `box()` programs on the native backend.
//!
//! The reference answer comes from the JVM path in `super`; everything here is about getting the
//! same program through krusty's own code generator, its linker and the prebuilt runtime, and
//! saying precisely what came back. The four outcomes are the whole vocabulary: an ANSWER to
//! compare, a DECLINE the generator made, an abnormal END, and a build that cannot reach the
//! target at all. Which of those a given test treats as a failure is the test's business, which is
//! why each helper below is a different answer to that one question.

use super::{jdk_modules, run_freshly_written};

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

/// Require the native backend to LOWER `src` and answer `expected`: a decline fails the test.
///
/// [`cross_check_backends`] treats a decline as a skip, which is right for the suite at large — a
/// young backend must not turn every JVM-side test into its to-do list. A test written FOR a native
/// construct wants the opposite: it exists to pin that the construct is lowered, so "not supported
/// yet" is the failure it is meant to catch. Skips only when this build cannot reach the target.
#[allow(dead_code)]
pub fn expect_native_box(src: &str, stem: &str, expected: &str) {
    let Some(target) = krusty::native::NativeTarget::host() else {
        return;
    };
    if !krusty::native::can_link(target) {
        return;
    }
    match native_box_outcome(src, stem, target) {
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

enum NativeBox {
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
    fn as_failure(&self) -> String {
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
    use krusty::diag::DiagSink;
    use krusty::native::{CraneliftBackend, Entry};
    use krusty::source::SourceInput;

    let Some(jar) = krusty::toolchain::stdlib_jar() else {
        return NativeBox::Unavailable;
    };
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
        .collect::<Vec<_>>();
    let classpath = super::cached_classpath(&jars, Some(&jdk_modules()));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath.clone())
            .expect("JVM provider initialization"),
    );
    let inputs = vec![SourceInput::kotlin(src).with_file_stem(stem)];
    let stems = vec![stem.to_string()];
    let mut features = krusty::features::LangFeatures::new();
    features.apply_source_directives(src);
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs, platform, &features, &mut diags,
    );
    if let Some(refusal) = diags.diags.first() {
        return NativeBox::Declined(format!("the frontend refused it: {}", refusal.msg));
    }
    // The backend asks the PROVIDER what it realized an identity as, not a classpath. This is a
    // second view over the very same `Rc<Classpath>` the frontend's provider wrapped: the interned
    // identity tables live in the classpath, so both views answer from one set of records.
    let provider: std::rc::Rc<dyn krusty::libraries::SemanticPlatform> = std::rc::Rc::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    // A conformance case answers through `box`; a program without one starts at Kotlin's `main`,
    // which is how a test pins what the backend does with that entry.
    let entry = if src.contains("fun box(") {
        Entry::Box
    } else {
        Entry::Main
    };
    let backend = CraneliftBackend::new(provider, target).with_entry(entry);
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, stem, &mut diags);
    if let Some(decline) = diags
        .diags
        .iter()
        .find(|diagnostic| diagnostic.msg.starts_with(NATIVE_DECLINE))
    {
        return NativeBox::Declined(decline.msg.clone());
    }
    if let Some(refusal) = diags.diags.first() {
        return NativeBox::Declined(refusal.msg.clone());
    }
    let Some((_, object)) = artifacts.into_iter().next() else {
        return NativeBox::Declined("the backend emitted nothing".to_string());
    };
    let image = match krusty::native::link_program(&[&object], target) {
        Ok(image) => image,
        Err(error) => return NativeBox::Failed(format!("link: {error}")),
    };
    let directory = std::env::temp_dir().join(format!(
        "krusty-native-e2e-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    if std::fs::create_dir_all(&directory).is_err() {
        return NativeBox::Unavailable;
    }
    let executable = directory.join(stem);
    if let Err(error) = std::fs::write(&executable, &image) {
        return NativeBox::Failed(format!("writing the executable: {error}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755));
    }
    let output = run_freshly_written(
        std::process::Command::new(&executable)
            .env_clear()
            .stdin(std::process::Stdio::null()),
    );
    let _ = std::fs::remove_file(&executable);
    let _ = std::fs::remove_dir(&directory);
    match output {
        Err(error) => NativeBox::Failed(format!("running it: {error}")),
        Ok(output) if !output.status.success() => NativeBox::Exited {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        // The entry prints what `box()` returned after a frame marker, and nothing after it: the
        // answer is every byte past the marker's LAST occurrence. What comes before is the
        // program's own output — `when (b) { true -> println("t") … }` prints `t` and then answers
        // `OK` — which the JVM path never sees, because there the answer is a return value rather
        // than a stream. Reading the last LINE instead would take `"FAIL\nOK"` for `OK`.
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            match stdout.rsplit_once(krusty::native::BOX_RESULT_FRAME) {
                Some((_, answer)) => NativeBox::Answered(answer.to_owned()),
                None => NativeBox::Failed(format!(
                    "the program ended without framing an answer; stdout {stdout:?}"
                )),
            }
        }
    }
}
