//! Same-package visibility for CLASSPATH types: Kotlin makes declarations in the file's own package
//! visible WITHOUT an import, including ones compiled separately and read from the classpath. krusty
//! resolved a constructor-call-by-name only for in-file classes and explicitly-imported/default-import
//! classpath types; a same-package sibling read from the classpath (`WorkspaceId(x)`, `Plain(n)`) was
//! "unresolved function". The file's own package is now an implicit wildcard, so the construction
//! resolves and round-trips on the JVM. The dependency is compiled by the real kotlinc, so its
//! `@Metadata`/bytecode is authoritative.

mod common;

use std::fs;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[test]
fn same_package_classpath_constructors_resolve_without_import() {
    let Some(_kotlinc) = env("KRUSTY_KOTLINC") else {
        eprintln!("skipping: set KRUSTY_KOTLINC");
        return;
    };
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let Some(stdlib_path) = common::stdlib_jar() else {
        eprintln!("skipping: no kotlin-stdlib jar");
        return;
    };
    let stdlib = stdlib_path.to_str().unwrap().to_string();
    let jdk_modules = std::path::PathBuf::from(format!("{java_home}/lib/modules"));

    let work = std::env::temp_dir().join(format!("krusty_same_pkg_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    let libout = work.join("libout");
    fs::create_dir_all(&libout).unwrap();

    // 1. A library declaring a data class and a plain class in package `app`, compiled by real kotlinc.
    let lib_kt = work.join("Lib.kt");
    fs::write(
        &lib_kt,
        "package app\ndata class WorkspaceId(val v: String)\nclass Plain(val n: Int)\n",
    )
    .unwrap();
    let kc_args = vec![
        "-d".to_string(),
        libout.to_string_lossy().into_owned(),
        "-cp".to_string(),
        stdlib.clone(),
        lib_kt.to_string_lossy().into_owned(),
    ];
    match common::kotlinc_compile(&kc_args) {
        Some((0, _)) => {}
        Some((_, e)) => panic!("kotlinc(lib): {e}"),
        None => return,
    }

    // 2. A SAME-PACKAGE caller (package `app`, NO import) constructing both classpath siblings by name.
    let main_src = "package app\n\
        fun box(): String {\n\
        \x20   val w = WorkspaceId(\"b\")\n\
        \x20   val p = Plain(3)\n\
        \x20   return if (w.v == \"b\" && p.n == 3) \"OK\" else \"fail\"\n\
        }\n";
    let cp = vec![libout.clone(), stdlib_path.clone()];
    let classes = common::compile_in_process(main_src, "Main", &cp, Some(&jdk_modules))
        .expect("krusty(main) failed to compile a same-package classpath constructor call");

    let Some(out) = common::run_box(&classes, "app.MainKt", &[libout.clone(), stdlib_path]) else {
        eprintln!("skipping: box runner unavailable");
        return;
    };
    assert_eq!(out.trim(), "OK", "box() returned {out:?}");

    let _ = fs::remove_dir_all(&work);
}
