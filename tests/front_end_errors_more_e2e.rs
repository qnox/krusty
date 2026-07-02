//! More front-end diagnostic (ERROR-path) coverage — companion to `front_end_errors_e2e.rs`,
//! `parser_errors_coverage_e2e.rs`, and `resolver_errors_coverage_e2e.rs`.
//!
//! Each test feeds a DISTINCT invalid Kotlin snippet through `lex → parse → collect → check` (via
//! `common::front_end_diagnostics`) and asserts the compiler produced at least one diagnostic — i.e.
//! it rejected the snippet. These reach parser/checker error branches not touched by the sibling
//! files: missing type positions, dangling `is`/`as` operands, malformed member access, spread
//! misuse, malformed object/companion/interface headers, malformed `catch`, bad accessors, empty /
//! malformed `when`, duplicate named arguments, `return`/`it` placement, and literal-range errors.
//! Cases here are DISTINCT from those in the three sibling files.
//!
//! Parse-level snippets need no classpath and never skip. Snippets that need library symbols get the
//! stdlib + JDK classpath; when the toolchain isn't provisioned those tests skip cleanly (a non-empty
//! sentinel keeps the non-empty assertion true).

mod common;

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
// Missing type positions
// ---------------------------------------------------------------------------

#[test]
fn parameter_without_a_type() {
    // A value parameter must carry a type — `x` alone has none.
    let d = parse_diags("fun f(x): Int = 0\nfun box(): Int = 0");
    assert_rejected(&d, "parameter with no type");
}

#[test]
fn top_level_val_without_type_or_initializer() {
    // A top-level `val` with neither a type nor an initializer.
    let d = parse_diags("val x\nfun box(): Int = 0");
    assert_rejected(&d, "top-level val with neither type nor initializer");
}

// ---------------------------------------------------------------------------
// Dangling operands: is / as / range / member access
// ---------------------------------------------------------------------------

#[test]
fn is_without_a_type_operand() {
    let d = parse_diags("fun box(): Int { val x: Any = 1; if (x is) { return 1 }; return 0 }");
    assert_rejected(&d, "`is` with no type operand");
}

#[test]
fn as_without_a_type_operand() {
    let d = parse_diags("fun box(): Int { val x: Any = 1; val y = x as; return 0 }");
    assert_rejected(&d, "`as` with no type operand");
}

#[test]
fn range_with_missing_upper_bound() {
    let d = parse_diags("fun box(): Int { for (i in 1..) {}; return 0 }");
    assert_rejected(&d, "range with a missing upper bound");
}

#[test]
fn dot_without_a_member_name() {
    let d = parse_diags("fun box(): Int { val a = 1; a.; return 0 }");
    assert_rejected(&d, "member access with no member name");
}

#[test]
fn safe_call_without_a_member_name() {
    let d = parse_diags("fun box(): Int { val a: String? = null; a?.; return 0 }");
    assert_rejected(&d, "safe-call with no member name");
}

// ---------------------------------------------------------------------------
// Expression / assignment malformations
// ---------------------------------------------------------------------------

#[test]
fn spread_outside_a_call_argument() {
    let d = parse_diags("fun box(): Int { val a = intArrayOf(1); val x = *a; return 0 }");
    assert_rejected(&d, "spread `*` outside a call argument");
}

#[test]
fn chained_double_initializer() {
    // `val x: Int = 1 = 2` — an initializer that is itself an assignment chain.
    let d = parse_diags("fun box(): Int { val x: Int = 1 = 2; return 0 }");
    assert_rejected(&d, "chained assignment in an initializer");
}

#[test]
fn lambda_with_two_arrows() {
    let d = parse_diags("fun box(): Int { val f = { x: Int -> -> x }; return 0 }");
    assert_rejected(&d, "lambda with two parameter arrows");
}

// ---------------------------------------------------------------------------
// Declaration headers: enum / object / companion / interface
// ---------------------------------------------------------------------------

#[test]
fn enum_class_without_a_name() {
    let d = parse_diags("enum class { A }\nfun box(): Int = 0");
    assert_rejected(&d, "enum class with no name");
}

#[test]
fn object_with_a_primary_constructor() {
    let d = parse_diags("object O(val x: Int)\nfun box(): Int = 0");
    assert_rejected(&d, "object declaration with a primary constructor");
}

#[test]
fn companion_object_at_top_level() {
    let d = parse_diags("companion object Foo\nfun box(): Int = 0");
    assert_rejected(&d, "companion object at the top level");
}

#[test]
fn interface_with_a_primary_constructor() {
    let d = parse_diags("interface I(x: Int)\nfun box(): Int = 0");
    assert_rejected(&d, "interface with a primary constructor");
}

// ---------------------------------------------------------------------------
// catch clauses
// ---------------------------------------------------------------------------

#[test]
fn catch_binding_without_a_type() {
    let d = parse_diags("fun box(): Int { try { return 1 } catch (e) { return 2 } }");
    assert_rejected(&d, "catch binding without a type");
}

#[test]
fn multi_catch_union_type() {
    let d = parse_diags(
        "fun box(): Int { try { return 1 } catch (e: RuntimeException | Error) { return 2 } }",
    );
    assert_rejected(&d, "union type in a catch clause");
}

// ---------------------------------------------------------------------------
// Property accessors
// ---------------------------------------------------------------------------

#[test]
fn getter_declared_with_a_parameter() {
    let d = parse_diags("class C { val x: Int get(q) = q }\nfun box(): Int = 0");
    assert_rejected(&d, "a getter declared with a parameter");
}

// ---------------------------------------------------------------------------
// when
// ---------------------------------------------------------------------------

#[test]
fn when_with_no_branches() {
    let d = parse_diags("fun box(): Int { val x = 1; return when (x) { } }");
    assert_rejected(&d, "when with no branches");
}

// ---------------------------------------------------------------------------
// String interpolation with a malformed embedded expression
// ---------------------------------------------------------------------------

#[test]
fn interpolation_with_a_dangling_operator() {
    let d = parse_diags("fun box(): String { val n = 1; return \"v=${n +}\" }");
    assert_rejected(&d, "dangling operator inside a template interpolation");
}

// ---------------------------------------------------------------------------
// Illegal statement placement
// ---------------------------------------------------------------------------

#[test]
fn return_at_top_level() {
    let d = parse_diags("return 5\nfun box(): Int = 0");
    assert_rejected(&d, "return outside a function");
}

// ---------------------------------------------------------------------------
// Resolver / checker-level errors
// ---------------------------------------------------------------------------

#[test]
fn duplicate_named_argument() {
    let d = diags("fun f(a: Int): Int = a\nfun box(): Int = f(a = 1, a = 2)");
    assert_rejected(&d, "the same named argument supplied twice");
}

#[test]
fn return_without_a_value_from_non_unit() {
    let d = diags("fun f(): Int { return }\nfun box(): Int = 0");
    assert_rejected(&d, "value-less return from a non-Unit function");
}

#[test]
fn integer_literal_out_of_int_range() {
    let d = diags("fun box(): Int { val x: Int = 99999999999; return 0 }");
    assert_rejected(&d, "integer literal too large for Int");
}

#[test]
fn equality_between_incompatible_types() {
    let d = diags("fun box(): Int { val b = 1 == \"s\"; return 0 }");
    assert_rejected(&d, "== between an Int and a String");
}

#[test]
fn implicit_it_outside_a_lambda() {
    let d = diags("fun box(): Int { val x = it; return 0 }");
    assert_rejected(&d, "`it` used outside a lambda");
}
