//! LABELLED trailing lambdas (`run outer@{ … }`) and the LOCAL returns they name.
//!
//! Two independent gaps met here. The parser did not attach a `{ … }` preceded by a `label@` to the
//! call, so the callee stayed a bare name ("unresolved reference 'run'"). And the lowerer treated a
//! `return@x` that matched no active splice frame as a return from the ENCLOSING FUNCTION — so a
//! `return@run`/`return@forEach` inside a spliced stdlib lambda emitted a bare `return` in a
//! value-returning method, which the JVM verifier rejects at class load ("Bad return type"). Both the
//! implicit (callee-name) and the explicit label forms are covered; each box runs on a real JVM.
use super::common;

#[test]
fn a_labelled_trailing_lambda_returns_locally_from_run() {
    let src = "fun box(): String {\n\
        \x20 val r = run rr@{\n\
        \x20   for (i in 0 until 10) { if (i == 3) return@rr i * 10 }\n\
        \x20   -1\n\
        \x20 }\n\
        \x20 return if (r == 30) \"OK\" else \"r=$r\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "LabelledRunReturn");
}

/// The IMPLICIT label (the callee's own name) on the receiver-less `run { … }` splice.
#[test]
fn an_implicit_label_returns_locally_from_run() {
    let src = "fun box(): String {\n\
        \x20 val r = run {\n\
        \x20   for (i in 0 until 10) { if (i == 3) return@run i * 10 }\n\
        \x20   -1\n\
        \x20 }\n\
        \x20 return if (r == 30) \"OK\" else \"r=$r\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "ImplicitRunReturn");
}

/// `forEach` splices to a for-each LOOP, so a local return from its lambda is that loop's `continue`.
#[test]
fn an_implicit_label_continues_the_foreach_splice() {
    let src = "fun box(): String {\n\
        \x20 var n = 0\n\
        \x20 listOf(1, 2, 3).forEach {\n\
        \x20   if (it == 2) return@forEach\n\
        \x20   n += it\n\
        \x20 }\n\
        \x20 return if (n == 4) \"OK\" else \"n=$n\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "ForEachLocalReturn");
}

/// An explicit label on the `forEach` lambda REPLACES the implicit `forEach` one.
#[test]
fn an_explicit_label_continues_the_foreach_splice() {
    let src = "fun box(): String {\n\
        \x20 var n = 0\n\
        \x20 listOf(1, 2, 3).forEach each@{\n\
        \x20   if (it == 2) return@each\n\
        \x20   n += it\n\
        \x20 }\n\
        \x20 return if (n == 4) \"OK\" else \"n=$n\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "ForEachLabelledReturn");
}

/// A `return@outer` written INSIDE a nested spliced lambda still targets the outer labelled one — the
/// nested `forEach` becomes a loop, so it is not its own return scope.
#[test]
fn a_labelled_return_crosses_a_nested_foreach_splice() {
    let src = "fun box(): String {\n\
        \x20 val r = run label@{\n\
        \x20   listOf(1, 2, 3).forEach inner@{\n\
        \x20     if (it == 2) return@inner\n\
        \x20     if (it == 3) return@label \"three\"\n\
        \x20   }\n\
        \x20   \"end\"\n\
        \x20 }\n\
        \x20 return if (r == \"three\") \"OK\" else \"r=$r\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "NestedLabelledReturn");
}
