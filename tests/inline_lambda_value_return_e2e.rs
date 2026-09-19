//! A VALUE-result labeled return from an inline scope-function lambda — `x.let { if (c) return@let a;
//! b }`. The `return@let` yields a value (not `Unit`), so it binds a result slot; the inline-lambda
//! return mechanism assigns it and the wrapper loop yields it. Both the early-return path and the
//! fall-through path must produce the right value. Needs the JVM toolchain + kotlin-stdlib + kotlinc.
use super::common;

#[test]
fn inline_lambda_value_labeled_return_runs() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    //   pick(5):  it>0 -> return@let "pos"          => "pos"
    //   pick(-1): fall through to "nonpos"          => "nonpos"
    //   grade(90): return@run early                 => "A"
    //   grade(50): fall through                      => "F"
    const MAIN: &str = "\
        fun pick(n: Int): String = n.let { if (it > 0) return@let \"pos\"; \"nonpos\" }\n\
        fun grade(n: Int): String = n.run { if (this >= 60) return@run \"A\"; \"F\" }\n\
        fun box(): String {\n\
            val ok = pick(5) == \"pos\" && pick(-1) == \"nonpos\" && grade(90) == \"A\" && grade(50) == \"F\"\n\
            return if (ok) \"OK\" else \"F ${pick(5)} ${pick(-1)} ${grade(90)} ${grade(50)}\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "value-result `return@let`/`return@run` on both the early and fall-through paths"
    );
}

#[test]
fn classpath_inline_lambda_boxes_primitive_body_for_erased_result() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    const SOURCE: &str = "fun box(): String {\n\
        val boolean: Any? = run<Any?> { true }\n\
        val integer: Any? = run<Any?> { 7 }\n\
        return if (boolean == true && integer == 7) \"OK\" else \"FAIL\"\n\
    }\n";

    assert_eq!(
        common::compile_and_run_box(
            SOURCE,
            "InlineErasedPrimitiveResult",
            &[stdlib, jdk.clone()],
            Some(jdk.as_path()),
        )
        .as_deref(),
        Some("OK")
    );
}

/// A function-typed argument FORWARDED from one inline expansion into another keeps the caller's
/// slot. An ordinary argument that is already a local read is copied into a local of the expansion,
/// so the inline parameter has its own name and lifetime; a function-typed one must not be, because
/// an inline function parameter is SPLICED at each call site rather than stored. Copying it hid the
/// lambda from the splicer, so `block { … }` materialized a `Function0` whose implementation method
/// was never emitted and the program died at its first call with `NoSuchMethodError`.
///
/// Reduced from box `labels/nestedInlineLabels.kt`, which is what caught it.
#[test]
fn a_forwarded_inline_lambda_parameter_is_still_spliced() {
    let sources: &[(&str, &str)] = &[
        (
            "lib",
            "package fwd\n\
             \n\
             var state = false\n\
             \n\
             inline fun blockImpl(p: () -> Unit) {\n\
             \x20   if (state) return\n\
             \x20   p()\n\
             }\n\
             \n\
             inline fun block(p: () -> Unit) {\n\
             \x20   if (state) return\n\
             \x20   blockImpl(p)\n\
             }\n",
        ),
        (
            "main",
            "package fwd\n\
             \n\
             fun test(x: Int): Int {\n\
             \x20   block outer@ {\n\
             \x20       block inner@ {\n\
             \x20           if (x < 10) return@inner\n\
             \x20           if (x == 10) return@outer\n\
             \x20           return x\n\
             \x20       }\n\
             \x20       if (x < 5) return@outer\n\
             \x20       return x + 10\n\
             \x20   }\n\
             \x20   return x + 100\n\
             }\n\
             \n\
             fun box(): String {\n\
             \x20   if (test(6) != 16) return \"six=\" + test(6)\n\
             \x20   if (test(4) != 104) return \"four=\" + test(4)\n\
             \x20   if (test(10) != 110) return \"ten=\" + test(10)\n\
             \x20   if (test(11) != 11) return \"eleven=\" + test(11)\n\
             \x20   return \"OK\"\n\
             }\n",
        ),
    ];
    let jdk = common::jdk_modules();
    let out =
        common::compile_and_run_box_files(sources, &[common::stdlib_jar()], Some(jdk.as_path()))
            .expect("a JVM runner is required to observe that the forwarded lambda was spliced");
    assert_eq!(out.trim(), "OK");
}
