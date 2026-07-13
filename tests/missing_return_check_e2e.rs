//! Missing-return check: a block-body function with a non-`Unit` return type must return a value on
//! every path (kotlinc rejects `fun f(): Int { }`). Covers the checker's `body_terminates` analysis.
//!
//! The REJECT cases assert a diagnostic; the ACCEPT cases assert NO diagnostic — the latter guard
//! against the analysis regressing into false positives on valid terminating shapes (exhaustive
//! `when`, `while (true)`, `Nothing`-returning calls, `try`/`finally`, `throw`).

use super::common;

fn diags(src: &str) -> Vec<String> {
    let Some(stdlib) = common::stdlib_jar() else {
        return vec!["<skip: no stdlib>".into()];
    };
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

fn assert_missing_return(src: &str) {
    let d = diags(src);
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter().any(|m| m.contains("'return' expression required")),
        "expected a missing-return diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        !d.iter().any(|m| m.contains("'return' expression required")),
        "unexpected missing-return diagnostic on valid code: {d:?}\nsrc: {src}"
    );
}

// ---- must be REJECTED ------------------------------------------------------

#[test]
fn empty_block_body() {
    assert_missing_return("fun f(): Int { }");
}

#[test]
fn body_with_no_return() {
    assert_missing_return("fun g() {}\nfun f(): Int { g() }");
}

#[test]
fn bare_trailing_value_not_returned() {
    // A block body does NOT return its trailing expression (unlike an expression body).
    assert_missing_return("fun f(): Int { 5 }");
}

#[test]
fn if_without_else_return() {
    assert_missing_return("fun f(c: Boolean): Int { if (c) return 1 }");
}

#[test]
fn if_else_one_branch_falls_through() {
    assert_missing_return("fun f(c: Boolean): Int { if (c) return 1 else { val x = 2 } }");
}

#[test]
fn when_arm_falls_through() {
    assert_missing_return(
        "fun f(x: Int): Int { when (x) { 1 -> return 1; 2 -> { } else -> return 3 } }",
    );
}

#[test]
fn nullable_return_still_required() {
    assert_missing_return("fun f(): String? { }");
}

// ---- must be ACCEPTED (guard against false positives) ----------------------

#[test]
fn plain_return() {
    assert_accepts("fun f(): Int { return 1 }");
}

#[test]
fn if_else_both_return() {
    assert_accepts("fun f(x: Int): Int { if (x > 0) return 1 else return 2 }");
}

#[test]
fn when_with_else_all_return() {
    assert_accepts("fun f(x: Int): Int { when (x) { 1 -> return 1; else -> return 2 } }");
}

#[test]
fn exhaustive_enum_when_without_else() {
    assert_accepts(
        "enum class E { A, B }\nfun f(x: E): Int { when (x) { E.A -> return 1; E.B -> return 2 } }",
    );
}

#[test]
fn trailing_throw() {
    assert_accepts("fun f(): Int { throw RuntimeException() }");
}

#[test]
fn infinite_while_true() {
    assert_accepts("fun f(): Int { while (true) { } }");
}

#[test]
fn nothing_returning_call() {
    assert_accepts("fun f(): Int { error(\"x\") }");
}

#[test]
fn try_finally_returns() {
    assert_accepts("fun f(): Int { try { return 1 } finally { println() } }");
}

#[test]
fn try_catch_both_return() {
    assert_accepts("fun f(): Int { try { return 1 } catch (e: Exception) { return 2 } }");
}

#[test]
fn unit_body_needs_no_return() {
    assert_accepts("fun f(): Unit { }");
    assert_accepts("fun f() { }");
}

#[test]
fn nothing_return_type() {
    assert_accepts("fun f(): Nothing { throw RuntimeException() }");
}

#[test]
fn early_return_in_loop_then_return() {
    assert_accepts("fun f(xs: List<Int>): Int { for (x in xs) { if (x > 0) return x }; return 0 }");
}

#[test]
fn nothing_returning_user_function_tail_call() {
    assert_accepts("fun fail(): Nothing = throw RuntimeException()\nfun f(): Int { fail() }");
    assert_accepts("fun fail(): Nothing = throw RuntimeException()\nfun f(x: Int): Int { if (x > 0) return 1 else fail() }");
}

#[test]
fn infinite_do_while() {
    assert_accepts("fun f(): Int { do { println(\"x\") } while (true) }");
}
