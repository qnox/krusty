//! Helpers used only by the product e2e binary.
//!
//! The conformance binary shares the lower-level harness in `common`, but must not compile these
//! unrelated resolver/reference-toolchain helpers and then suppress their dead-code warnings.

use std::path::PathBuf;

use super::common_core as common;

/// Assert one Kotlin language feature's gate against the reference compiler.
pub fn assert_language_feature_gate(source: &str, feature: &str) {
    let Some(work) = common::scratch_dir() else {
        panic!("cannot allocate language-feature fixture");
    };
    let source_path = work.join("Feature.kt");
    std::fs::write(&source_path, source).expect("write language-feature fixture");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();

    for (enabled, expectation) in [(false, "reject"), (true, "accept")] {
        let sign = if enabled { '+' } else { '-' };
        let args = vec![
            source_path.to_string_lossy().into_owned(),
            "-d".to_string(),
            work.join(if enabled { "enabled" } else { "disabled" })
                .to_string_lossy()
                .into_owned(),
            format!("-XXLanguage:{sign}{feature}"),
        ];
        let Some((reference_code, reference_stderr)) = common::kotlinc_compile(&args) else {
            eprintln!("skip: kotlinc unavailable");
            return;
        };
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
pub fn expect_suspend_result_against(
    tag: &str,
    lib_src: &str,
    main: &str,
    call: &str,
    expected: &str,
) {
    let Some(library) = common::compile_lib(tag, lib_src) else {
        return;
    };
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
) -> Option<(Vec<String>, T)> {
    let stdlib = common::stdlib_jar();
    let mut classpath = vec![stdlib];
    classpath.push(common::jdk_modules());
    Some(common::inspect_checker_with_classpath(
        main, classpath, inspect,
    ))
}

/// Compile one in-memory fixture with the persistent reference compiler harness.
pub fn kotlinc_source_result(tag: &str, source: &str) -> Option<(i32, String)> {
    let work = common::scratch_dir()?;
    let source_path = work.join(format!("{tag}.kt"));
    let output = work.join("out");
    std::fs::write(&source_path, source).ok()?;
    let result = common::kotlinc_compile(&[
        source_path.to_string_lossy().into_owned(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ]);
    let _ = std::fs::remove_dir_all(work);
    result
}
