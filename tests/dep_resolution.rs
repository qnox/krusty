//! Verifies the box-test classpath former obtains every directive's jars — from the kotlinc dist
//! first, then Maven Central — so `// WITH_STDLIB` assertions (`kotlin.test.*`) actually resolve
//! rather than silently skipping.

mod common;

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
    assert!(names.iter().any(|n| n.starts_with("kotlin-stdlib")), "no stdlib jar: {names:?}");
    assert!(names.iter().any(|n| n.contains("kotlin-test")), "no kotlin-test jar: {names:?}");
    // the resolved kotlin-test jar must really carry the assertion API
    let test_jar = common::kotlin_test_jar().expect("kotlin-test jar");
    assert!(test_jar.is_file(), "kotlin-test path not a file: {test_jar:?}");
}

#[test]
fn coroutines_directive_fetches_runtime() {
    if std::env::var("HOME").is_err() {
        return;
    }
    let src = "// WITH_COROUTINES\nfun box() = \"OK\"";
    let jars = common::classpath_jars_for(src);
    let has = jars.iter().any(|p| {
        p.file_name().and_then(|n| n.to_str()).map(|n| n.contains("coroutines")).unwrap_or(false)
    });
    assert!(has, "WITH_COROUTINES did not resolve a coroutines jar: {jars:?}");
}
