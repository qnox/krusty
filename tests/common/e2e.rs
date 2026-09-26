//! Helpers used only by the product e2e binary.
//!
//! The conformance binary shares the lower-level harness in `common`, but must not compile these
//! unrelated resolver/reference-toolchain helpers and then suppress their dead-code warnings.

use std::path::PathBuf;
use std::process::Command;

use super::common_core as common;

/// A positive front-end coverage test upgraded to true e2e: the source must be checker-clean, the
/// backend must emit it (a lowering/emit bail is a failure, not a skip), and when it declares
/// `fun box()`, running it must return "OK". This belongs in the e2e-only helper module so the
/// conformance target does not compile an unused helper or require a dead-code suppression.
pub fn expect_true_e2e(tag: &str, src: &str, extra_cp: &[PathBuf]) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut cp = extra_cp.to_vec();
    cp.push(stdlib);
    let diagnostics = common::front_end_diagnostics(src, &cp, Some(jdk.as_path()));
    assert!(
        diagnostics.is_empty(),
        "{tag}: expected a checker-clean source, got: {diagnostics:?}"
    );
    let Some(classes) = common::compile_in_process(src, "Main", &cp, Some(jdk.as_path())) else {
        panic!("{tag}: the front end accepted the source but the backend bailed on emitting it");
    };
    if let Some(box_class) = common::find_box_class(&classes) {
        let out = common::run_box(&classes, &box_class, &cp)
            .unwrap_or_else(|| panic!("{tag}: emitted classes but the box() run failed to start"));
        assert!(
            !out.trim().starts_with("ERROR:"),
            "{tag}: box() threw: {out}"
        );
        // Only fixtures written for the convention are held to it; some upgraded checker tests
        // intentionally return a domain value such as `RED`.
        if src.contains("\"OK\"") {
            assert_eq!(out.trim(), "OK", "{tag}: box() returned {out:?}");
        }
    }
}

/// Assert that both compilers REJECT the fixture with the identical diagnostic set: same count, and
/// the same file, line, column, message and order.
///
/// A nonzero-exit assertion is not enough on its own — it passes when krusty rejects the fixture for
/// an unrelated reason, which is exactly how a "both compilers agree" claim goes stale.
pub fn expect_identical_rejection(result: &CompilerDiagnosticResult, tag: &str) {
    let stdout_errors = compiler_errors(&result.krusty_stdout);
    let krusty = compiler_errors(&result.krusty_stderr);
    let reference = compiler_errors(&result.reference_stderr);
    assert_eq!(
        result.reference_code, 1,
        "{tag}: kotlinc exited {} rather than rejecting the fixture: {}",
        result.reference_code, result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 1,
        "{tag}: krusty exited {} rather than rejecting the fixture: {}{}",
        result.krusty_code, result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(
        stdout_errors,
        [],
        "{tag}: krusty emitted errors on stdout instead of stderr"
    );
    assert!(
        !reference.is_empty(),
        "{tag}: kotlinc rejected without a parseable diagnostic: {}",
        result.reference_stderr
    );
    assert_eq!(
        krusty, reference,
        "{tag}: diagnostics differ.\nkrusty:    {krusty:#?}\nkotlinc:   {reference:#?}"
    );
}

pub struct CompilerDiagnosticResult {
    pub krusty_code: i32,
    pub krusty_stdout: String,
    pub krusty_stderr: String,
    pub reference_code: i32,
    pub reference_stderr: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CompilerError {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Extract every rendered compiler error in emission order. The scratch-directory prefixes differ
/// between invocations, so the stable source filename is the location boundary compared by tests.
pub fn compiler_errors(output: &str) -> Vec<CompilerError> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once("error:")?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?.trim();
            Some(CompilerError {
                file: std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string(),
                line,
                column,
                message: message.trim().to_string(),
            })
        })
        .collect()
}

fn write_fixture_sources(work: &std::path::Path, sources: &[(&str, &str)]) -> Vec<PathBuf> {
    sources
        .iter()
        .map(|(name, source)| {
            let path = work.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture source directory");
            }
            std::fs::write(&path, source).expect("write compiler fixture");
            path
        })
        .collect()
}

fn kotlinc_paths_result(
    sources: &[PathBuf],
    output: &std::path::Path,
    extra_args: &[String],
) -> (i32, String) {
    let mut args = sources
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    args.extend(["-d".to_string(), output.to_string_lossy().into_owned()]);
    args.extend_from_slice(extra_args);
    common::kotlinc_compile(&args).expect("reference compiler unavailable")
}

/// Compile named sources with both compiler CLIs and retain their diagnostic streams.
pub fn compiler_diagnostics(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
) -> CompilerDiagnosticResult {
    compiler_diagnostics_with_reference_args(sources, classpath, &[])
}

/// Compile named sources with both compiler CLIs, forwarding explicit language arguments only to
/// kotlinc. Krusty reads the equivalent `// LANGUAGE:` directives from each source fixture.
pub fn compiler_diagnostics_with_reference_args(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
    reference_extra_args: &[String],
) -> CompilerDiagnosticResult {
    compiler_diagnostics_with_args(sources, classpath, &[], reference_extra_args)
}

/// Compile named sources with both compiler CLIs, handing both the same `shared_args` — switches a
/// build passes every compiler alike, such as `-Xplugin`.
pub fn compiler_diagnostics_with_shared_args(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
    shared_args: &[String],
) -> CompilerDiagnosticResult {
    compiler_diagnostics_with_args(sources, classpath, shared_args, shared_args)
}

fn compiler_diagnostics_with_args(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
    krusty_extra_args: &[String],
    reference_extra_args: &[String],
) -> CompilerDiagnosticResult {
    let work = common::scratch_dir().expect("cannot allocate compiler-diagnostic fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let joined_classpath = (!classpath.is_empty())
        .then(|| std::env::join_paths(classpath).expect("build compiler-diagnostic classpath"));

    let mut krusty = Command::new(common::krusty_binary());
    krusty.args([
        "-d",
        work.join("krusty-out").to_str().expect("UTF-8 output path"),
    ]);
    if let Some(classpath) = &joined_classpath {
        krusty.arg("-cp").arg(classpath);
    }
    krusty.args(krusty_extra_args);
    krusty.args(&source_paths);
    let krusty = krusty.output().expect("run krusty diagnostic fixture");

    let mut reference_args = joined_classpath
        .map(|classpath| vec!["-cp".to_string(), classpath.to_string_lossy().into_owned()])
        .unwrap_or_default();
    reference_args.extend_from_slice(reference_extra_args);
    let (reference_code, reference_stderr) =
        kotlinc_paths_result(&source_paths, &work.join("reference-out"), &reference_args);
    let result = CompilerDiagnosticResult {
        krusty_code: krusty.status.code().unwrap_or_else(|| {
            panic!(
                "krusty diagnostic fixture terminated by signal: status={:?}, sources={source_paths:?}",
                krusty.status
            )
        }),
        krusty_stdout: String::from_utf8_lossy(&krusty.stdout).into_owned(),
        krusty_stderr: String::from_utf8_lossy(&krusty.stderr).into_owned(),
        reference_code,
        reference_stderr,
    };
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Every error kotlinc reports for named sources, as `file:line:column: message` in emission
/// order: the shape recorded per Kotlin version by [`common::recorded`].
pub fn reference_error_ledger(sources: &[(&str, &str)], extra_args: &[String]) -> Vec<String> {
    let work = common::scratch_dir().expect("cannot allocate reference-compiler fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let (_, stderr) = kotlinc_paths_result(&source_paths, &work.join("out"), extra_args);
    let _ = std::fs::remove_dir_all(work);
    render_errors(&stderr)
}

/// [`reference_error_ledger`] for krusty's CLI.
pub fn krusty_error_ledger(sources: &[(&str, &str)]) -> Vec<String> {
    let work = common::scratch_dir().expect("cannot allocate compiler-diagnostic fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let output = Command::new(common::krusty_binary())
        .arg("-d")
        .arg(work.join("krusty-out"))
        .arg("-no-reflect")
        .args(&source_paths)
        .output()
        .expect("run krusty diagnostic fixture");
    let _ = std::fs::remove_dir_all(work);
    render_errors(&String::from_utf8_lossy(&output.stderr))
}

/// Assert krusty's CLI reports exactly kotlinc's error ledger for `sources`, as recorded for the
/// running test under the reference version. `reference_args` go to kotlinc only (krusty reads the
/// equivalent `// LANGUAGE:` directives from the sources).
pub fn assert_errors_match_kotlinc(sources: &[(&str, &str)], reference_args: &[String]) {
    let expected =
        super::recorded_support::recorded(|| reference_error_ledger(sources, reference_args));
    assert!(!expected.is_empty(), "kotlinc reported no error");
    assert_eq!(
        krusty_error_ledger(sources),
        expected,
        "krusty's ledger against kotlinc {}",
        krusty::kotlin_version::target()
    );
}

/// Assert the frontend reports exactly the messages kotlinc reports for `source` compiled against
/// the stdlib, as recorded for the running test under the reference version.
pub fn assert_messages_match_kotlinc(source: &str) {
    let expected = super::recorded_support::recorded(|| reference_error_messages("Main", source));
    assert!(!expected.is_empty(), "kotlinc reported no error");
    assert_eq!(
        front_end_diagnostics_with_stdlib(source),
        expected,
        "krusty's messages against kotlinc {}",
        krusty::kotlin_version::target()
    );
}

/// The messages alone of every error kotlinc reports for one source compiled against the stdlib:
/// the shape [`front_end_diagnostics_with_stdlib`] returns for krusty.
pub fn reference_error_messages(tag: &str, source: &str) -> Vec<String> {
    reference_error_messages_files(&[(&format!("{tag}.kt"), source)])
}

/// [`reference_error_messages`] for several named sources compiled together.
pub fn reference_error_messages_files(sources: &[(&str, &str)]) -> Vec<String> {
    let work = common::scratch_dir().expect("cannot allocate reference-compiler fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let (_, stderr) = kotlinc_paths_result(&source_paths, &work.join("out"), &[]);
    let _ = std::fs::remove_dir_all(work);
    compiler_errors(&stderr)
        .into_iter()
        .map(|error| error.message)
        .collect()
}

fn render_errors(stderr: &str) -> Vec<String> {
    compiler_errors(stderr)
        .into_iter()
        .map(|error| {
            format!(
                "{}:{}:{}: {}",
                error.file, error.line, error.column, error.message
            )
        })
        .collect()
}

/// Run the shared frontend against the provisioned Kotlin stdlib and JDK.
pub fn front_end_diagnostics_with_stdlib(source: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(jdk.as_path()))
}

/// Run source-kind-aware inputs through the production frontend. Scripts are intentionally not sent
/// to the batch backend, but their parser/checker diagnostics still need the same semantic platform.
pub fn front_end_diagnostics_inputs(
    inputs: &[krusty::frontend::SourceInput<'_>],
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Vec<String> {
    let cp = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let mut diagnostics = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        inputs,
        common::with_native_plugins(platform),
        &krusty::features::LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diagnostics);
    diagnostics
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect()
}

/// Assert one Kotlin language feature's gate against the reference compiler.
pub fn assert_language_feature_gate(source: &str, feature: &str) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();

    for (enabled, expectation) in [(false, "reject"), (true, "accept")] {
        let sign = if enabled { '+' } else { '-' };
        let (reference_code, reference_stderr) = kotlinc_source_result_with_args(
            if enabled {
                "FeatureEnabled"
            } else {
                "FeatureDisabled"
            },
            source,
            &[format!("-XXLanguage:{sign}{feature}")],
        );
        let source = format!("// LANGUAGE: {sign}{feature}\n{source}");
        let diagnostics = common::front_end_diagnostics(
            &source,
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        let reference_accepted = reference_code == 0;
        let krusty_accepted = diagnostics.is_empty();
        assert_eq!(
            reference_accepted, enabled,
            "kotlinc should {expectation} {sign}{feature}: {reference_stderr}"
        );
        assert_eq!(
            krusty_accepted, reference_accepted,
            "krusty differs for {sign}{feature}: {diagnostics:?}"
        );
    }
}

/// Compile and invoke one synchronously completing suspend function through the JVM continuation ABI.
pub fn expect_suspend_result(tag: &str, main: &str, call: &str, expected: &str) {
    expect_suspend_result_with_classpath(tag, main, call, expected, Vec::new());
}

/// The dependency variant of [`expect_suspend_result`].
pub fn expect_suspend_result_against_ref(
    tag: &str,
    lib_src: &str,
    main: &str,
    call: &str,
    expected: &str,
) {
    let library = common::compile_lib_ref(tag, lib_src).expect("reference compiler unavailable");
    expect_suspend_result_with_classpath(tag, main, call, expected, vec![library]);
}

fn expect_suspend_result_with_classpath(
    tag: &str,
    main: &str,
    call: &str,
    expected: &str,
    mut classpath: Vec<PathBuf>,
) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let dir = std::env::temp_dir().join(format!("krusty_suspend_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create suspend test directory");
    classpath.push(stdlib);
    common::compile_to_dir(main, "Main", &classpath, Some(jdk.as_path()), &dir).unwrap_or_else(
        || {
            let diagnostics =
                common::front_end_diagnostics(main, &classpath, Some(jdk.as_path()));
            let backend =
                common::backend_outcome_in_process(main, "Main", &classpath, Some(jdk.as_path()));
            panic!(
                "{tag}: failed to compile suspend caller; diagnostics: {diagnostics:?}; backend: {backend:?}"
            )
        },
    );
    let driver = format!(
        "import kotlin.coroutines.*;\n\
         public class M {{\n\
           public static void main(String[] args) {{\n\
             Continuation<Object> continuation = new Continuation<Object>() {{\n\
               public CoroutineContext getContext() {{ return EmptyCoroutineContext.INSTANCE; }}\n\
               public void resumeWith(Object result) {{ }}\n\
             }};\n\
             Object result = MainKt.{call};\n\
             System.out.println(String.valueOf(result));\n\
           }}\n\
         }}\n"
    );
    let driver_path = dir.join("M.java");
    std::fs::write(&driver_path, driver).expect("write suspend test driver");
    let runtime_classpath = std::env::join_paths(
        std::iter::once(dir.as_path()).chain(classpath.iter().map(PathBuf::as_path)),
    )
    .expect("build suspend classpath");
    let output = common::javac_run(
        driver_path.to_str().expect("UTF-8 driver path"),
        runtime_classpath.to_str().expect("UTF-8 classpath"),
        dir.to_str().expect("UTF-8 output path"),
        "M",
    )
    .unwrap_or_else(|| panic!("{tag}: failed to run suspend caller"));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output.trim(), expected, "{tag}");
}

/// Check against the Kotlin stdlib and inspect the checker handoff while its storage is alive.
pub fn inspect_checker_with_stdlib<T>(
    main: &str,
    inspect: impl FnOnce(
        &krusty::ast::File,
        &krusty::frontend::FrontendTypeInfo,
        &krusty::frontend::FrontendSymbols,
    ) -> T,
) -> (Vec<String>, T) {
    let stdlib = common::stdlib_jar();
    let mut classpath = vec![stdlib];
    classpath.push(common::jdk_modules());
    common::inspect_checker_with_classpath(main, classpath, inspect)
}

/// Compile one in-memory fixture with the persistent reference compiler harness.
pub fn kotlinc_source_result(tag: &str, source: &str) -> (i32, String) {
    kotlinc_source_result_with_args(tag, source, &[])
}

/// Compile one `Main.kt` fixture with kotlinc and run its `box()` result on the shared JVM.
pub fn kotlinc_box_result(source: &str) -> String {
    kotlinc_box_result_with_classpath(source, &[])
}

/// Run the same `box()` fixture with kotlinc and krusty and require the exact same successful
/// result. The explicit `OK` assertion prevents an identically wrong failure string from making a
/// differential test pass.
pub fn expect_box_same_as_kotlinc(source: &str, stem: &str) {
    let reference = kotlinc_box_result(source);
    assert_eq!(reference, "OK", "{stem}: kotlinc fixture must succeed");
    assert_eq!(
        common::expect_box_run_with_stdlib(source, stem),
        reference,
        "{stem}: krusty and kotlinc box results differ",
    );
}

/// Compile one `Main.kt` fixture with kotlinc against caller-supplied dependencies and run its
/// `box()` result on the shared JVM.
pub fn kotlinc_box_result_with_classpath(source: &str, classpath: &[PathBuf]) -> String {
    let work = common::scratch_dir().expect("cannot allocate reference-runtime fixture");
    let source_paths = write_fixture_sources(&work, &[("Main.kt", source)]);
    let output = work.join("out");
    let extra_args = if classpath.is_empty() {
        Vec::new()
    } else {
        vec![
            "-cp".to_string(),
            std::env::join_paths(classpath)
                .expect("build reference-runtime classpath")
                .to_string_lossy()
                .into_owned(),
        ]
    };
    let (code, diagnostics) = kotlinc_paths_result(&source_paths, &output, &extra_args);
    assert_eq!(code, 0, "kotlinc rejected runtime fixture: {diagnostics}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut runtime_classpath = vec![output];
    runtime_classpath.extend_from_slice(classpath);
    runtime_classpath.extend([stdlib, jdk]);
    let result =
        common::run_box(&[], "MainKt", &runtime_classpath).expect("run kotlinc-built box fixture");
    let _ = std::fs::remove_dir_all(work);
    result
}

/// One class compiled from the same multi-file module by both compilers.
pub struct ModuleClassPair {
    pub kotlinc: Vec<u8>,
    pub krusty: Vec<u8>,
}

impl ModuleClassPair {
    /// Compile `sources` as ONE module with kotlinc and with krusty, and take `class` from each.
    /// Either compiler rejecting the fixture, or not emitting the class, fails the test.
    pub fn compile(sources: &[(&str, &str)], class: &str) -> Self {
        Self::compile_with_classpath(sources, &[], class)
    }

    /// [`Self::compile`] with both compilers reading the same extra `classpath` entries (such as a
    /// javac-compiled fixture library).
    pub fn compile_with_classpath(
        sources: &[(&str, &str)],
        classpath: &[PathBuf],
        class: &str,
    ) -> Self {
        let work = common::scratch_dir().expect("cannot allocate module comparison fixture");
        let source_paths = write_fixture_sources(&work, sources);
        let output = work.join("out");
        let reference_args = if classpath.is_empty() {
            Vec::new()
        } else {
            vec![
                "-cp".to_string(),
                std::env::join_paths(classpath)
                    .expect("classpath entries join")
                    .to_string_lossy()
                    .into_owned(),
            ]
        };
        let (code, diagnostics) = kotlinc_paths_result(&source_paths, &output, &reference_args);
        assert_eq!(code, 0, "kotlinc rejected module fixture: {diagnostics}");
        let kotlinc = std::fs::read(output.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("kotlinc did not emit {class}"));
        let _ = std::fs::remove_dir_all(work);
        let mut krusty_classpath = vec![common::stdlib_jar()];
        krusty_classpath.extend_from_slice(classpath);
        let jdk = common::jdk_modules();
        let classes =
            common::compile_in_process_files(sources, &krusty_classpath, Some(jdk.as_path()))
                .expect("krusty rejected module fixture");
        let krusty = classes
            .into_iter()
            .find_map(|(name, bytes)| (name == class).then_some(bytes))
            .unwrap_or_else(|| panic!("krusty did not emit {class}"));
        ModuleClassPair { kotlinc, krusty }
    }

    /// `method`'s disassembled instructions from each side (`kotlinc`, `krusty`), with constant-pool
    /// indices masked but the symbolic owner/name/descriptor of every referenced member kept: two
    /// pools interned in different orders describe the same code, while a different call target is
    /// a different program.
    pub fn method_code(&self, class: &str, method: &str) -> (String, String) {
        let disassemble = |bytes: &[u8]| {
            let work = common::scratch_dir().expect("cannot allocate disassembly fixture");
            let path = work.join(format!("{}.class", class.replace('/', "_")));
            std::fs::write(&path, bytes).expect("write class for disassembly");
            let text =
                common::javap(&["-c", "-p", &path.to_string_lossy()]).expect("javap unavailable");
            let _ = std::fs::remove_dir_all(work);
            method_instructions(&text, method)
                .unwrap_or_else(|| panic!("{class} has no method {method}"))
        };
        (disassemble(&self.kotlinc), disassemble(&self.krusty))
    }
}

/// The instruction lines of the first method whose declaration names `method`, with `#N` pool
/// references masked. javap spells the static initializer `<clinit>` as `static {};`.
fn method_instructions(disassembly: &str, method: &str) -> Option<String> {
    let mut lines = disassembly.lines().map(str::trim);
    lines.find(|line| {
        if method == "<clinit>" {
            return *line == "static {};";
        }
        line.split('(')
            .next()
            .and_then(|head| head.split_whitespace().last())
            == Some(method)
    })?;
    let mut body = String::new();
    for line in lines.skip_while(|line| *line != "Code:").skip(1) {
        if line.is_empty() || line == "}" {
            break;
        }
        let masked = line
            .split_whitespace()
            .map(|token| match token.strip_prefix('#') {
                Some(rest) if rest.trim_end_matches(',').parse::<u32>().is_ok() => "#N",
                _ => token,
            })
            .collect::<Vec<_>>()
            .join(" ");
        body.push_str(&masked);
        body.push('\n');
    }
    (!body.is_empty()).then_some(body)
}

/// Compile named in-memory fixtures with kotlinc and run `box()` from `main_class` on the shared JVM.
pub fn kotlinc_box_files_result(sources: &[(&str, &str)], main_class: &str) -> String {
    let work = common::scratch_dir().expect("cannot allocate reference-runtime fixture");
    let source_paths = write_fixture_sources(&work, sources);
    let output = work.join("out");
    let (code, diagnostics) = kotlinc_paths_result(&source_paths, &output, &[]);
    assert_eq!(code, 0, "kotlinc rejected runtime fixture: {diagnostics}");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let result = common::run_box(&[], main_class, &[output, stdlib, jdk])
        .expect("run kotlinc-built multi-file box fixture");
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Compile one named source with kotlinc. Unlike [`kotlinc_source_result`], this preserves a caller
/// supplied extension so frontend-only Kotlin script diagnostics can be compared directly.
pub fn kotlinc_named_source_result(filename: &str, source: &str) -> (i32, String) {
    let work = common::scratch_dir().expect("cannot allocate reference-compiler fixture");
    let source_paths = write_fixture_sources(&work, &[(filename, source)]);
    let result = kotlinc_paths_result(&source_paths, &work.join("out"), &[]);
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Compile one in-memory fixture with extra reference-compiler arguments.
pub fn kotlinc_source_result_with_args(
    tag: &str,
    source: &str,
    extra_args: &[String],
) -> (i32, String) {
    let work = common::scratch_dir().expect("cannot allocate reference-compiler fixture");
    let source_name = format!("{tag}.kt");
    let source_paths = write_fixture_sources(&work, &[(source_name.as_str(), source)]);
    let output = work.join("out");
    let result = kotlinc_paths_result(&source_paths, &output, extra_args);
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Front-end diagnostics rendered as `line:col: severity: message`, in emission order.
///
/// A diagnostic's position is part of its contract, so a test that pins the message alone cannot
/// tell a correctly anchored diagnostic from one reported against the wrong declaration.
pub fn front_end_diagnostics_located(
    src: &str,
    cp_jars: &[PathBuf],
    jdk_modules: Option<&std::path::Path>,
) -> Vec<String> {
    let cp = common::cached_classpath(cp_jars, jdk_modules);
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
    );
    let inputs = [krusty::source::SourceInput::kotlin(src)];
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        common::with_native_plugins(platform),
        &krusty::features::LangFeatures::new(),
        |_, _| {},
        &mut diags,
    );
    let _ = krusty::compiler::check_frontend_only(analysis, &mut diags);
    diags
        .diags
        .iter()
        .map(|diagnostic| {
            let (line, column) = krusty::diag::line_col(src, diagnostic.span.lo);
            let severity = match diagnostic.severity {
                krusty::diag::Severity::Error => "error",
                _ => "warning",
            };
            format!("{line}:{column}: {severity}: {}", diagnostic.msg)
        })
        .collect()
}
