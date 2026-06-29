//! Safe calls `?.`: `recv?.member` / `recv?.method(args)` evaluate to `null` when the receiver is
//! null, else the member/call result — composing with the Elvis operator `?:`.

mod common;

#[test]
fn safe_calls_run() {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping safe_call_e2e: set JAVA_HOME");
        return;
    };
    // Reference `==`/`!=` compiles to `kotlin/jvm/internal/Intrinsics.areEqual` — needs kotlin-stdlib
    // on the runtime classpath, as any real Kotlin program does.
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping safe_call_e2e: no kotlin-stdlib jar found");
        return;
    };
    let src = "class Box(val label: String) { fun shout(): String = label }\nfun pick(b: Boolean): Box? = if (b) Box(\"hi\") else null\nfun safeLabel(b: Boolean): String = pick(b)?.shout() ?: \"none\"\nfun safeProp(b: Boolean): String = pick(b)?.label ?: \"none\"\nfun box(): String {\n  if (safeLabel(true) != \"hi\") return \"f1\"\n  if (safeLabel(false) != \"none\") return \"f2\"\n  if (safeProp(true) != \"hi\") return \"f3\"\n  if (safeProp(false) != \"none\") return \"f4\"\n  if (pick(true)?.shout() != \"hi\") return \"f5\"\n  if (pick(false)?.shout() != null) return \"f6\"\n  return \"OK\"\n}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "S", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

/// A safe call on a statically-`null` receiver (`null?.member()`): the receiver is always null, so the
/// member is never invoked and the call yields `null` — the whole expression folds to `null`.
/// Round-tripped on the JVM.
#[test]
fn safe_call_on_null_literal_yields_null() {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let src = "fun box(): String {\n\
    val r: String? = null?.toString()\n\
    if (r != null) return \"f1\"\n\
    try { return \"OK\" } finally { null?.toString() }\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "S", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

/// A safe call to a SAME-MODULE extension function (`recv?.ext()`): the checker resolves the module
/// extension on the non-null receiver and the lowerer emits the static extension call guarded by the
/// null check. Member/classpath lookups don't see module extensions, so this is its own path.
#[test]
fn safe_call_to_module_extension() {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let src = "fun String.shout(): String = this + \"!\"\n\
fun maybe(s: String?): String = s?.shout() ?: \"none\"\n\
fun box(): String {\n\
    if (maybe(\"hi\") != \"hi!\") return \"f1\"\n\
    if (maybe(null) != \"none\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, "S", &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}
