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

pub struct CompilerDiagnosticResult {
    pub krusty_code: i32,
    pub krusty_stdout: String,
    pub krusty_stderr: String,
    pub reference_code: i32,
    pub reference_stderr: String,
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
    krusty.args(&source_paths);
    let krusty = krusty.output().expect("run krusty diagnostic fixture");

    let reference_args = joined_classpath
        .map(|classpath| vec!["-cp".to_string(), classpath.to_string_lossy().into_owned()])
        .unwrap_or_default();
    let (reference_code, reference_stderr) =
        kotlinc_paths_result(&source_paths, &work.join("reference-out"), &reference_args);
    let result = CompilerDiagnosticResult {
        krusty_code: krusty
            .status
            .code()
            .expect("krusty diagnostic fixture terminated by signal"),
        krusty_stdout: String::from_utf8_lossy(&krusty.stdout).into_owned(),
        krusty_stderr: String::from_utf8_lossy(&krusty.stderr).into_owned(),
        reference_code,
        reference_stderr,
    };
    let _ = std::fs::remove_dir_all(work);
    result
}

/// Run the shared frontend against the provisioned Kotlin stdlib and JDK.
pub fn front_end_diagnostics_with_stdlib(source: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(jdk.as_path()))
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
