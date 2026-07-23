//! Front-end diagnostic (ERROR-path) coverage.
//!
//! Each test feeds an INVALID Kotlin snippet through `lex → parse → collect → check`
//! (via `common::front_end_diagnostics`) and asserts the compiler produced at least one
//! diagnostic — i.e. it rejected the snippet. These exercise the many error branches in
//! `src/parser.rs`, `src/resolve.rs`, and the checker that the box corpus (only *valid*
//! programs) never reaches.
//!
//! Snippets that need library symbols to resolve get the stdlib + JDK classpath; when the
//! toolchain isn't provisioned those tests skip cleanly (a non-empty sentinel is returned so
//! the non-empty assertion still holds). Parse-level snippets need no classpath and never skip.

use super::common;

/// Run the front end with stdlib + JDK on the classpath. If the toolchain is unavailable, return a
/// sentinel so the "non-empty" assertions still hold (the test effectively skips).
fn diags(src: &str) -> Vec<String> {
    let Some(stdlib) = common::stdlib_jar() else {
        return vec!["<skip: no stdlib>".into()];
    };
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

/// Run the front end with NO classpath — for parse-level errors that need no library symbols.
fn parse_diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(d: &[String], what: &str) {
    assert!(
        d.iter().any(|m| !m.is_empty()),
        "expected a diagnostic for {what}, got none: {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Unresolved references
// ---------------------------------------------------------------------------

#[test]
fn unresolved_variable() {
    let d = diags("fun box(): Int { return undefinedName }");
    assert_rejected(&d, "unresolved variable");
}

#[test]
fn unresolved_function_call() {
    let d = diags("fun box(): Int { return noSuchFunction(1, 2) }");
    assert_rejected(&d, "unresolved function call");
}

#[test]
fn unresolved_type_annotation() {
    let d = diags("fun box(): NoSuchType { TODO() }");
    assert_rejected(&d, "unresolved type annotation");
}

#[test]
fn unresolved_member_access() {
    let d = diags("fun box(): Int { val s = \"hi\"; return s.noSuchMember }");
    assert_rejected(&d, "unresolved member access");
}

#[test]
fn unresolved_method_call_on_receiver() {
    let d = diags("fun box(): Int { val s = \"hi\"; return s.noSuchMethod() }");
    assert_rejected(&d, "unresolved method call on receiver");
}

#[test]
fn unresolved_constructor_type() {
    let d = diags("fun box(): Int { val x = NoSuchClass(); return 0 }");
    assert_rejected(&d, "unresolved constructor type");
}

#[test]
fn unresolved_supertype() {
    let d = diags("class C : NoSuchBase()\nfun box(): Int = 0");
    assert_rejected(&d, "unresolved supertype");
    assert!(
        d.iter().any(|m| m.contains("NoSuchBase")),
        "expected the supertype name in the message: {d:?}"
    );
}

#[test]
fn unresolved_label_reference() {
    // A `break@label`/`continue@label` naming a label with no enclosing labeled loop is rejected, as
    // kotlinc does. (A label *definition* on an expression — `foo@ 1` — is valid Kotlin and accepted;
    // only a dangling label *reference* is an error.)
    let d = diags("fun box() { break@foo }");
    assert_rejected(&d, "unresolved label reference");
}

// ---------------------------------------------------------------------------
// Type errors
// ---------------------------------------------------------------------------

#[test]
fn return_type_mismatch() {
    let d = diags("fun box(): Int { return \"not an int\" }");
    assert_rejected(&d, "return type mismatch");
}

#[test]
fn assignment_type_mismatch() {
    let d = diags("fun box(): Int { val x: Int = \"hello\"; return x }");
    assert_rejected(&d, "assignment type mismatch");
}

#[test]
fn argument_type_mismatch() {
    let d = diags("fun takesInt(n: Int): Int = n\nfun box(): Int { return takesInt(\"nope\") }");
    assert_rejected(&d, "argument type mismatch");
}

#[test]
fn wrong_arity_too_few() {
    let d = diags("fun two(a: Int, b: Int): Int = a + b\nfun box(): Int { return two(1) }");
    assert_rejected(&d, "wrong arity (too few args)");
}

#[test]
fn wrong_arity_too_many() {
    let d = diags("fun one(a: Int): Int = a\nfun box(): Int { return one(1, 2, 3) }");
    assert_rejected(&d, "wrong arity (too many args)");
}

#[test]
fn calling_a_non_function() {
    let d = diags("fun box(): Int { val x: Int = 5; return x(10) }");
    assert_rejected(&d, "invoking a non-function value");
}

#[test]
fn binary_op_type_mismatch() {
    let d = diags("fun box(): Int { return \"str\" - 3 }");
    assert_rejected(&d, "binary op on incompatible operands");
}

#[test]
fn binary_operand_type_mismatch_in_init() {
    let d = diags("fun box(): Int { val x: Int = 1 + \"s\"; return x }");
    assert_rejected(&d, "binary operand type mismatch in initializer");
}

#[test]
fn condition_not_boolean() {
    let d = diags("fun box(): Int { if (42) { return 1 }; return 0 }");
    assert_rejected(&d, "non-boolean condition");
}

// ---------------------------------------------------------------------------
// Argument-binding (named / required parameters)
// ---------------------------------------------------------------------------

#[test]
fn named_arg_no_such_parameter() {
    let d = diags("fun f(a: Int): Int = a\nfun box(): Int = f(z = 1)");
    assert_rejected(&d, "named argument with no such parameter");
    assert!(
        d.iter().any(|m| m.contains("no parameter named")),
        "expected 'no parameter named': {d:?}"
    );
}

#[test]
fn named_arg_missing_required_parameter() {
    let d = diags("fun f(a: Int, b: Int): Int = a\nfun box(): Int = f(a = 1)");
    assert_rejected(&d, "missing required parameter");
    assert!(
        d.iter().any(|m| m.contains("required parameter")),
        "expected 'required parameter': {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Exceptions / try-catch
// ---------------------------------------------------------------------------

#[test]
fn catch_unknown_exception_type() {
    let d = diags("fun box(): Int { try { return 1 } catch (e: NoSuchException) { return 2 } }");
    assert_rejected(&d, "catch clause naming an unknown exception type");
}

// ---------------------------------------------------------------------------
// `this` / `super` outside a class member
// ---------------------------------------------------------------------------

#[test]
fn this_outside_class_member() {
    let d = diags("fun box(): Int { return this.hashCode() }");
    assert_rejected(&d, "'this' used outside a class member");
    assert!(
        d.iter().any(|m| m.contains("this")),
        "expected 'this' in the message: {d:?}"
    );
}

#[test]
fn super_call_outside_class() {
    let d = diags("fun box(): Int { return super.hashCode() }");
    assert_rejected(&d, "'super' used outside a class");
}

// ---------------------------------------------------------------------------
// Control-flow
// ---------------------------------------------------------------------------

#[test]
fn break_with_unresolved_label() {
    let d = diags("fun box(): Int { for (i in 0..3) { break@nope }; return 0 }");
    assert_rejected(&d, "break to an unresolved loop label");
    assert!(
        d.iter().any(|m| m.contains("label")),
        "expected 'label' in the message: {d:?}"
    );
}

#[test]
fn non_exhaustive_when_as_expression() {
    let d =
        diags("fun box(): Int { val x: Int = 3; val y = when (x) { 1 -> 10; 2 -> 20 }; return y }");
    assert_rejected(&d, "non-exhaustive when used as expression");
}

#[test]
fn when_guard_form_used_as_expression_is_unit() {
    let d = diags("fun box(): Int { val x = 3; return when { x > 0 -> 1 } }");
    assert_rejected(&d, "guard-form when without else used as an Int expression");
}

#[test]
fn when_condition_not_comparable_to_subject() {
    let d = diags("fun box(): Int { val x: Int = 1; return when (x) { \"s\" -> 1; else -> 0 } }");
    assert_rejected(&d, "when condition not comparable to subject");
    assert!(
        d.iter().any(|m| m.contains("comparable")),
        "expected 'comparable' in the message: {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Mutability / assignment targets
// ---------------------------------------------------------------------------

#[test]
fn val_reassignment() {
    let d = diags("fun box(): Int { val x = 1; x = 2; return x }");
    assert_rejected(&d, "reassigning a val");
}

#[test]
fn reassign_val_member() {
    let d = diags("class C { val x: Int = 1 }\nfun box(): Int { C().x = 5; return 0 }");
    assert_rejected(&d, "reassigning a val member");
    assert!(
        d.iter().any(|m| m.contains("reassigned")),
        "expected 'reassigned' in the message: {d:?}"
    );
}

#[test]
fn assign_to_function_name() {
    let d = diags("fun f(): Int = 1\nfun box(): Int { f = 2; return 0 }");
    assert_rejected(&d, "assigning to a function name");
}

// ---------------------------------------------------------------------------
// Parse-level errors (no classpath needed)
// ---------------------------------------------------------------------------

#[test]
fn unmatched_brace() {
    let d = parse_diags("fun box(): Int { return 0 ");
    assert_rejected(&d, "unmatched brace");
}

#[test]
fn unmatched_paren() {
    let d = parse_diags("fun box(): Int { return (1 + 2 }");
    assert_rejected(&d, "unmatched paren");
}

#[test]
fn missing_type_after_colon() {
    let d = parse_diags("fun box(): Int { val x: = 5; return x }");
    assert_rejected(&d, "missing type after colon");
}

#[test]
fn incomplete_expression() {
    let d = parse_diags("fun box(): Int { return 1 + }");
    assert_rejected(&d, "incomplete expression");
}

#[test]
fn invalid_token_sequence() {
    let d = parse_diags("fun box(): Int { val = = = }");
    assert_rejected(&d, "invalid token sequence");
}

#[test]
fn missing_function_name() {
    let d = parse_diags("fun (): Int { return 0 }");
    assert_rejected(&d, "missing function name");
}

#[test]
fn stray_closing_brace() {
    let d = parse_diags("fun box(): Int { return 0 } }");
    assert_rejected(&d, "stray closing brace");
}

#[test]
fn val_without_initializer_or_type() {
    let d = parse_diags("fun box(): Int { val x; return 0 }");
    assert_rejected(&d, "val with neither type nor initializer");
}

// ---------------------------------------------------------------------------
// Smart-cast / when-subject
// ---------------------------------------------------------------------------

#[test]
fn member_absent_without_smart_cast() {
    // No `is` narrowing has happened, so `length` is not a member of the declared type `Any`.
    let d = diags("fun box(): Int { val a: Any = \"hi\"; return a.length }");
    assert_rejected(&d, "member absent on declared type (no smart cast)");
}

#[test]
fn missing_when_subject_branch_type() {
    // `when` used as an expression with branches producing incompatible-with-target results.
    let d = diags("fun box(): String { val x = 1; return when (x) { 1 -> 100; else -> 200 } }");
    assert_rejected(&d, "when-expression branch type mismatch");
}

#[test]
fn unresolved_local_type_annotation_is_rejected() {
    // A local whose ANNOTATION doesn't resolve must fail the file — silently binding `Error` lets
    // the local take its initializer's shape while every use site's checks are Error-suppressed
    // (a lambda then SAM-converts by its own arity → IncompatibleClassChangeError at runtime).
    let d = parse_diags(
        "fun f(): String {\n    val b: Nonexist<String> = { \"OK\" }\n    return b(\"x\")\n}\n",
    );
    assert_rejected(&d, "an unresolved local type annotation");
}
