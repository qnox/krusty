//! A DEFAULTED parameter of a `@JvmInline value class` with a NULLABLE underlying (`VC(val s:
//! String?)`) stays BOXED in a `$default` synthetic (kotlinc: its omitted placeholder is a null box,
//! which the carrier could not tell from a supplied null). `emit_default_stub` takes the value class,
//! `box-impl`s any default-filled field value, and `unbox-impl`s before delegating; the CALL site boxes
//! a provided value-class arg to match. A parameter without a default keeps its carrier in the stub.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn copy_default_boxes_nullable_underlying_value_class() {
    const SRC: &str = "@JvmInline value class VC(val s: String?)\n\
        data class D(val vc: VC, val n: Int)\n\
        fun box(): String {\n\
        \x20 val d = D(VC(\"a\"), 1)\n\
        \x20 val e = d.copy(n = 2)\n\
        \x20 val f = d.copy(vc = VC(\"b\"))\n\
        \x20 return if (e.vc.s == \"a\" && e.n == 2 && f.vc.s == \"b\" && f.n == 1) \"OK\"\n\
        \x20   else \"FAIL:${e.vc.s}|${e.n}|${f.vc.s}|${f.n}\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("nullable-underlying value-class copy$default"),
        "OK"
    );
}

const FUNCTION_SRC: &str = "@JvmInline value class N(val s: String?)\n\
    fun undefaulted(n: N, k: Int = 1): Int = if (n.s == null) k else k + 10\n\
    fun defaulted(k: Int = 1, n: N = N(null)): Int = if (n.s == null) k else k + 20\n\
    fun box(): String {\n\
    \x20   if (undefaulted(N(\"a\")) != 11) return \"fail: undefaulted\"\n\
    \x20   if (defaulted(2, N(\"a\")) != 22) return \"fail: supplied\"\n\
    \x20   if (defaulted() != 1) return \"fail: omitted\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn assert_same_method(method: &str) {
    match common::class_bytes_diff_against_kotlinc("Main", &[], FUNCTION_SRC, "MainKt", method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("MainKt.{method}: reference toolchain unavailable"),
    }
}

#[test]
fn an_undefaulted_nullable_underlying_parameter_takes_its_carrier_in_the_stub() {
    assert_same_method(
        "public static int undefaulted-4Qvz1jU$default(java.lang.String, int, int, java.lang.Object)",
    );
}

#[test]
fn a_defaulted_nullable_underlying_parameter_stays_boxed_in_the_stub() {
    assert_same_method(
        "public static int defaulted-if15GJk$default(int, N, int, java.lang.Object)",
    );
}

#[test]
fn nullable_underlying_function_stubs_run() {
    assert_eq!(
        run(FUNCTION_SRC).expect("nullable-underlying function stubs"),
        "OK"
    );
}
