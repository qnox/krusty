//! kotlin.Result: construct via the inline `Result.success` and read via the inline extension
//! `getOrThrow()`. Both are `inline` (private in bytecode), so kotlinc inlines them; krusty must
//! resolve them via @Metadata (which marks them public inline) and splice their classpath bodies.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

// Full kotlin.Result support end-to-end: construction via the inline `Result.success` (a Companion
// instance inline-splice) and read via the inline extension `getOrThrow` (a value-class extension
// resolved through @Metadata + spliced), with `Result` erased to `Object`. Round-trips under
// `-Xverify:all`.
#[test]
fn result_success_get_or_throw() {
    const SRC: &str = "fun box(): String {\n\
    val r = Result.success(42)\n\
    return if (r.getOrThrow() == 42) \"OK\" else \"fail: \" + r.getOrThrow()\n\
}\n";
    let out = run(SRC).expect("Result.success + getOrThrow should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn result_success_get_or_null() {
    const SRC: &str = "fun box(): String {\n\
    val r = Result.success(42)\n\
    return if (r.getOrNull() == 42) \"OK\" else \"FAIL\"\n\
}\n";
    let out = run(SRC).expect("Result.success + getOrNull should compile + run");
    assert_eq!(out, "OK");
}
