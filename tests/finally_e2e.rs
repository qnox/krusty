//! `try { … } [catch …] finally { … }` — the `finally` runs on the normal path, after a caught
//! exception, and (via a catch-all that re-throws) on an uncaught one. Round-tripped under
//! `-Xverify:all`; the run order is asserted via a log string.

mod common;

#[test]
fn finally_runs() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping finally_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping finally_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "var log = \"\"\n\
fun mark(s: String): Int { log = log + s; return 1 }\n\
fun box(): String {\n\
try { mark(\"a\") } finally { mark(\"b\") }\n\
try { throw RuntimeException(\"x\") } catch (e: RuntimeException) { mark(\"c\") } finally { mark(\"d\") }\n\
val r = try { mark(\"e\") } finally { mark(\"f\") }\n\
if (r != 1) return \"fr\"\n\
return if (log == \"abcdef\") \"OK\" else log\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "F", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

/// A `return` inside both the `try` body and the `finally`: the finally's `return` overrides the
/// try's, and the finally still runs. Regression: inlining the finally at the try's `return` used
/// to re-inline the same finally at the finally's own `return`, recursing until the stack overflowed.
#[test]
fn finally_return_overrides_try_return() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping finally_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping finally_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "var log = \"\"\n\
fun foo(): Int {\n\
try { log = log + \"Done\"; return 0 } finally { log = log + \"Finally\"; return 1 }\n\
}\n\
fun box(): String {\n\
val r = foo()\n\
return if (r == 1 && log == \"DoneFinally\") \"OK\" else \"r=\" + r + \" log=\" + log\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "F", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
