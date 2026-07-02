//! End-to-end "box" coverage for common Kotlin language features: scope functions, inline
//! functions, varargs (incl. spread), default/named arguments, lambdas with receiver, local
//! functions, and destructuring in a for-loop over a map. Each test compiles Kotlin in-process
//! and round-trips `box()` on the persistent JVM under `-Xverify:all`.

mod common;

use std::path::PathBuf;

/// Compile `src` (which must define `fun box(): String`) and assert it returns "OK".
/// Skips silently (returns) when the JVM/stdlib toolchain isn't provisioned.
fn run_ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping feature_coverage_e_e2e::{stem}: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_coverage_e_e2e::{stem}: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn scope_let() {
    let src = "fun box(): String {\n\
        val n: Int? = 5\n\
        val r = n?.let { it * 2 } ?: -1\n\
        if (r != 10) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ScopeLet");
}

#[test]
fn scope_run() {
    let src = "fun box(): String {\n\
        val r = run { val a = 3; val b = 4; a + b }\n\
        if (r != 7) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ScopeRun");
}

#[test]
fn scope_apply() {
    let src = "fun box(): String {\n\
        val sb = StringBuilder().apply { append(\"a\"); append(\"b\") }\n\
        if (sb.toString() != \"ab\") return \"f:$sb\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ScopeApply");
}

#[test]
fn scope_also() {
    let src = "fun box(): String {\n\
        var seen = \"\"\n\
        val x = 42.also { seen = it.toString() }\n\
        if (x != 42) return \"fx:$x\"\n\
        if (seen != \"42\") return \"fs:$seen\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ScopeAlso");
}

#[test]
fn scope_with() {
    let src = "fun box(): String {\n\
        val sb = StringBuilder()\n\
        val len = with(sb) { append(\"hello\"); length }\n\
        if (len != 5) return \"f:$len\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ScopeWith");
}

#[test]
fn inline_with_lambda() {
    let src = "inline fun twice(block: () -> Int): Int = block() + block()\n\
        fun box(): String {\n\
        val r = twice { 5 }\n\
        if (r != 10) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "InlineLambda");
}

#[test]
fn inline_noninline_body() {
    let src = "inline fun compute(x: Int): Int {\n\
        var acc = 0\n\
        for (i in 1..x) acc += i\n\
        return acc\n\
        }\n\
        fun box(): String {\n\
        val r = compute(4)\n\
        if (r != 10) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "InlineBody");
}

#[test]
fn varargs_plain() {
    let src = "fun sum(vararg xs: Int): Int { var s = 0; for (x in xs) s += x; return s }\n\
        fun box(): String {\n\
        val r = sum(1, 2, 3, 4)\n\
        if (r != 10) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "VarargsPlain");
}

#[test]
fn varargs_spread() {
    let src = "fun sum(vararg xs: Int): Int { var s = 0; for (x in xs) s += x; return s }\n\
        fun box(): String {\n\
        val arr = intArrayOf(2, 3, 5)\n\
        val r = sum(*arr)\n\
        if (r != 10) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "VarargsSpread");
}

#[test]
fn default_and_named_args() {
    let src =
        "fun greet(name: String, greeting: String = \"Hi\", punct: String = \"!\"): String =\n\
        \"$greeting, $name$punct\"\n\
        fun box(): String {\n\
        if (greet(\"A\") != \"Hi, A!\") return \"f1\"\n\
        if (greet(\"B\", punct = \".\") != \"Hi, B.\") return \"f2\"\n\
        if (greet(name = \"C\", greeting = \"Yo\") != \"Yo, C!\") return \"f3\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "DefaultNamedArgs");
}

#[test]
fn lambda_with_receiver() {
    let src = "fun build(block: StringBuilder.() -> Unit): String {\n\
        val sb = StringBuilder()\n\
        sb.block()\n\
        return sb.toString()\n\
        }\n\
        fun box(): String {\n\
        val s = build { append(\"x\"); append(\"y\"); append(length) }\n\
        if (s != \"xy2\") return \"f:$s\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "LambdaReceiver");
}

#[test]
fn local_function_capture() {
    let src = "fun box(): String {\n\
        val base = 100\n\
        fun addBase(x: Int): Int = x + base\n\
        val r = addBase(5) + addBase(15)\n\
        if (r != 220) return \"f:$r\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "LocalFunCapture");
}

#[test]
fn for_over_map_destructure() {
    let src = "fun box(): String {\n\
        val m = linkedMapOf(\"a\" to 1, \"b\" to 2, \"c\" to 3)\n\
        var keys = \"\"\n\
        var total = 0\n\
        for ((k, v) in m) { keys += k; total += v }\n\
        if (keys != \"abc\") return \"fk:$keys\"\n\
        if (total != 6) return \"ft:$total\"\n\
        return \"OK\"\n\
        }\n";
    run_ok(src, "ForMapDestructure");
}
