//! Verifies the box-test classpath former obtains every directive's jars — from the kotlinc dist
//! first, then Maven Central — so `// WITH_STDLIB` assertions (`kotlin.test.*`) actually resolve
//! rather than silently skipping.

use super::common;

#[test]
fn with_stdlib_resolves_kotlin_test() {
    if common::kotlinc_lib_dir().is_none() && std::env::var("HOME").is_err() {
        eprintln!("skip: no dist and no HOME");
        return;
    }
    let src = "// WITH_STDLIB\nimport kotlin.test.assertEquals\nfun box(): String { assertEquals(1, 1); return \"OK\" }";
    let jars = common::classpath_jars_for(src);
    let names: Vec<String> = jars
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
        .collect();
    assert!(
        names.iter().any(|n| n.starts_with("kotlin-stdlib")),
        "no stdlib jar: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains("kotlin-test")),
        "no kotlin-test jar: {names:?}"
    );
    // the resolved kotlin-test jar must really carry the assertion API
    let test_jar = common::kotlin_test_jar().expect("kotlin-test jar");
    assert!(
        test_jar.is_file(),
        "kotlin-test path not a file: {test_jar:?}"
    );
}

#[test]
fn atomicfu_directive_selects_the_runtime_and_the_compiler_plugin() {
    if std::env::var("HOME").is_err() {
        return;
    }
    let src = include_str!("box_regressions/atomicfu/fieldUpdaters.kt");
    let jars = common::classpath_jars_for(src);
    assert!(
        jars.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("atomicfu"))
        }),
        "WITH_ATOMICFU did not resolve the JVM runtime: {jars:?}"
    );
    let configuration = krusty::conformance::platform_compiler_configuration(
        src,
        krusty::conformance::TestTarget::Native,
    )
    .expect("the atomicfu plugin is a case compiler argument");
    assert!(
        configuration
            .arguments
            .iter()
            .any(|argument| { argument.starts_with("-Xplugin=") && argument.contains("atomicfu") }),
        "the reference arguments do not load the atomicfu plugin: {:?}",
        configuration.arguments
    );
    for target in [
        krusty::compilation_target::CompilationTarget::Native,
        krusty::compilation_target::CompilationTarget::WasmJs,
        krusty::compilation_target::CompilationTarget::WasmWasi,
    ] {
        let libraries = krusty::toolchain::klib_libraries_for(src, target)
            .unwrap_or_else(|error| panic!("{target:?}: {error}"));
        assert!(
            libraries.iter().any(|path| path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("atomicfu"))),
            "{target:?} did not resolve an atomicfu KLIB: {libraries:?}"
        );
    }
    assert!(krusty::toolchain::klib_libraries_for(
        "fun box() = \"OK\"\n",
        krusty::compilation_target::CompilationTarget::Native,
    )
    .expect("a case without extra libraries has an empty KLIB classpath")
    .is_empty());
}

#[test]
fn coroutines_directive_fetches_runtime() {
    if std::env::var("HOME").is_err() {
        return;
    }
    let src = "// WITH_COROUTINES\nfun box() = \"OK\"";
    let jars = common::classpath_jars_for(src);
    let has = jars.iter().any(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("coroutines"))
            .unwrap_or(false)
    });
    assert!(
        has,
        "WITH_COROUTINES did not resolve a coroutines jar: {jars:?}"
    );
}
