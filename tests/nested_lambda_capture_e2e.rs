//! A NESTED lambda capturing a variable from 2+ levels out (`host { inner { x = outer } }`) lowered to
//! `lower None` (skipped): `lower_lambda_sam`'s capture detection stopped at a nested lambda, so the
//! outer closure never captured the transitively-used variable. Now a CLOSURE lambda captures through
//! nested lambdas — while an INLINE-spliced lambda keeps shallow captures (it accesses the variable
//! directly). Round-tripped on a real JVM.

use super::common;

#[test]
fn nested_closure_capture_runs() {
    // `host`/`inner` are NON-inline (real closures); the inner lambda captures `outer` two levels out.
    const SRC: &str = "fun host(b: () -> Unit) { b() }\n\
        fun inner(f: () -> Unit) { f() }\n\
        fun box(): String {\n\
        \x20 var x = \"\"\n\
        \x20 val outer = \"OK\"\n\
        \x20 host { inner { x = outer } }\n\
        \x20 return x\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "N");
}

/// An INNER lambda's implicit `it` SHADOWS the enclosing lambda's: `outer.forEach { inner.forEach
/// { it } }` types the inner `it` as the inner element. Both lambdas get their parameter from the
/// expected type (`forEach`'s `Function1`), so the untyped-`it` probe guard (which now treats a
/// body mentioning an already-bound `it` as a CAPTURE, fixing `s?.let { sink.emit { "$it" } }`)
/// must not leak into the typed path and demote the inner lambda to a `Function0` capture.
#[test]
fn nested_lambda_implicit_it_shadows_outer() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val outer = listOf(\"a\")\n\
        \x20 val inner = listOf(1, 2)\n\
        \x20 var s = \"\"\n\
        \x20 outer.forEach { inner.forEach { s += it.toString() } }\n\
        \x20 return if (s == \"12\") \"OK\" else \"fail: $s\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "N");
}

/// A local named `it` follows the same lexical rule as an enclosing lambda parameter. The untyped
/// lambda captures it and is therefore a `Function0`; treating the body token as a fresh implicit
/// parameter would make the zero-argument invocation inapplicable.
#[test]
fn untyped_lambda_captures_local_named_it() {
    const SRC: &str = "fun box(): String {\n\
        \x20 val it = \"OK\"\n\
        \x20 val read = { it }\n\
        \x20 return read()\n\
        }\n";
    common::expect_box_ok_with_stdlib(SRC, "N");
}
