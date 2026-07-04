//! Resolver / type-checker ERROR-path coverage (companion to `front_end_errors_e2e.rs`).
//!
//! Each test feeds a semantically-INVALID Kotlin snippet through `lex → parse → collect → check`
//! (via `common::front_end_diagnostics`) and asserts the front end produced at least one
//! diagnostic — i.e. it rejected the snippet. These exercise `diags.error(...)` branches in
//! `src/resolve.rs` (unresolved members, arity, incompatible branches, conflicting declarations,
//! bad assignment targets, unsupported casts/ranges, destructuring, etc.) that the *valid-only*
//! box corpus never reaches. Cases here are DISTINCT from those in `front_end_errors_e2e.rs`.
//!
//! Snippets get the stdlib + JDK classpath so library symbols resolve; when the toolchain isn't
//! provisioned those tests skip cleanly (a non-empty sentinel keeps the non-empty assertion true).

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

fn assert_rejected(d: &[String], what: &str) {
    assert!(
        d.iter().any(|m| !m.is_empty()),
        "expected a diagnostic for {what}, got none: {d:?}"
    );
}

// ---------------------------------------------------------------------------
// Duplicate / conflicting declarations
// ---------------------------------------------------------------------------

#[test]
fn conflicting_top_level_functions_same_signature() {
    // Two top-level functions with an identical erased signature — not an overload.
    let d = diags("fun f(): Int = 1\nfun f(): Int = 2\nfun box(): Int = 0");
    assert_rejected(&d, "conflicting top-level function declarations");
}

#[test]
fn conflicting_top_level_classes() {
    let d = diags("class C\nclass C\nfun box(): Int = 0");
    assert_rejected(&d, "conflicting class declarations");
}

#[test]
fn conflicting_local_val() {
    let d = diags("fun box(): Int { val x = 1; val x = 2; return x }");
    assert_rejected(&d, "conflicting local declaration");
}

// ---------------------------------------------------------------------------
// Arity on constructors / local functions
// ---------------------------------------------------------------------------

#[test]
fn constructor_too_many_args() {
    let d = diags("class C(a: Int)\nfun box(): Int { C(1, 2); return 0 }");
    assert_rejected(&d, "constructor with too many args");
}

#[test]
fn constructor_too_few_args() {
    let d = diags("class C(a: Int, b: Int)\nfun box(): Int { C(1); return 0 }");
    assert_rejected(&d, "constructor with too few args");
}

#[test]
fn local_function_wrong_arity() {
    let d = diags("fun box(): Int { fun g(a: Int): Int = a; return g(1, 2) }");
    assert_rejected(&d, "local function wrong arity");
}

// ---------------------------------------------------------------------------
// Unresolved members / methods on distinct receiver types
// ---------------------------------------------------------------------------

#[test]
fn unresolved_member_on_int() {
    let d = diags("fun box(): Int { val n = 5; return n.bogusProp }");
    assert_rejected(&d, "unresolved member on Int");
}

#[test]
fn unresolved_method_on_user_class() {
    let d = diags("class C\nfun box(): Int { C().doesNotExist(); return 0 }");
    assert_rejected(&d, "unresolved method on user class");
}

#[test]
fn unresolved_member_on_boolean() {
    let d = diags("fun box(): Int { val b = true; return b.width }");
    assert_rejected(&d, "unresolved member on Boolean");
}

// ---------------------------------------------------------------------------
// Indexing / index-assign on non-arrays
// ---------------------------------------------------------------------------

#[test]
fn index_read_on_non_array() {
    let d = diags("fun box(): Int { val n = 5; return n[0] }");
    assert_rejected(&d, "indexing a non-array");
}

#[test]
fn index_assign_on_non_array() {
    let d = diags("fun box(): Int { val n = 5; n[0] = 1; return 0 }");
    assert_rejected(&d, "index-assign on a non-array");
}

// ---------------------------------------------------------------------------
// Unary / operator misuse
// ---------------------------------------------------------------------------

#[test]
fn unary_minus_on_string() {
    let d = diags("fun box(): Int { val s = \"x\"; val y = -s; return 0 }");
    assert_rejected(&d, "unary minus on a String");
}

#[test]
fn unary_not_on_int() {
    let d = diags("fun box(): Int { val n = 5; val b = !n; return 0 }");
    assert_rejected(&d, "logical-not on an Int");
}

// ---------------------------------------------------------------------------
// if-expression branch incompatibility
// ---------------------------------------------------------------------------

#[test]
fn incompatible_if_branches() {
    let d = diags("fun box(): Int { val y = if (true) 1 else \"s\"; return 0 }");
    assert_rejected(&d, "incompatible if branches");
}

// ---------------------------------------------------------------------------
// Destructuring on a non-destructurable type
// ---------------------------------------------------------------------------

#[test]
fn destructure_non_destructurable() {
    let d = diags("fun box(): Int { val (a, b) = 5; return a }");
    assert_rejected(&d, "destructuring a type with no componentN");
}

// ---------------------------------------------------------------------------
// Constructor argument type mismatch
// ---------------------------------------------------------------------------

#[test]
fn constructor_arg_type_mismatch() {
    let d = diags("class C(a: Int)\nfun box(): Int { C(\"s\"); return 0 }");
    assert_rejected(&d, "constructor arg type mismatch");
}

// ---------------------------------------------------------------------------
// Augmented assignment to a val
// ---------------------------------------------------------------------------

#[test]
fn augmented_assign_val() {
    let d = diags("fun box(): Int { val x = 1; x += 1; return x }");
    assert_rejected(&d, "augmented-assign to a val");
}

#[test]
fn increment_val_member() {
    let d = diags("class C { val x: Int = 1 }\nfun box(): Int { C().x += 1; return 0 }");
    assert_rejected(&d, "augmented-assign to a val member");
}

// ---------------------------------------------------------------------------
// Unresolved loop label on continue
// ---------------------------------------------------------------------------

#[test]
fn continue_unresolved_label() {
    let d = diags("fun box(): Int { for (i in 0..3) { continue@nope }; return 0 }");
    assert_rejected(&d, "continue to an unresolved label");
}

// ---------------------------------------------------------------------------
// for over a non-iterable
// ---------------------------------------------------------------------------

#[test]
fn for_over_non_iterable() {
    let d = diags("fun box(): Int { for (x in 5) { }; return 0 }");
    assert_rejected(&d, "for over a non-iterable");
}

#[test]
fn for_over_boolean() {
    let d = diags("fun box(): Int { for (x in true) { }; return 0 }");
    assert_rejected(&d, "for over a Boolean");
}

// ---------------------------------------------------------------------------
// Generic local function (unsupported)
// ---------------------------------------------------------------------------

#[test]
fn generic_local_function() {
    let d = diags("fun box(): Int { fun <T> id(x: T): T = x; return id(1) }");
    assert_rejected(&d, "generic local function");
}

// ---------------------------------------------------------------------------
// Non-exhaustive `when` over an enum / sealed subject used as an expression
// ---------------------------------------------------------------------------

#[test]
fn non_exhaustive_when_over_enum() {
    let d =
        diags("enum class E { A, B }\nfun box(): Int { val e = E.A; val y = when (e) { E.A -> 1 }; return y }");
    assert_rejected(&d, "non-exhaustive when over an enum used as an expression");
}

#[test]
fn non_exhaustive_when_over_sealed() {
    let d = diags(
        "sealed class S\nclass A : S()\nclass B : S()\nfun box(): Int { val s: S = A(); val y = when (s) { is A -> 1 }; return y }",
    );
    assert_rejected(
        &d,
        "non-exhaustive when over a sealed hierarchy used as an expression",
    );
}

// ---------------------------------------------------------------------------
// Assign wrong type through a member
// ---------------------------------------------------------------------------

#[test]
fn assign_wrong_type_to_var_member() {
    let d = diags("class C { var x: Int = 0 }\nfun box(): Int { C().x = \"s\"; return 0 }");
    assert_rejected(&d, "assigning a String to an Int member");
}

#[test]
fn var_reassign_wrong_type() {
    let d = diags("fun box(): Int { var x = 1; x = \"s\"; return x }");
    assert_rejected(&d, "reassigning a var with the wrong type");
}

// ---------------------------------------------------------------------------
// Assigning a Boolean to a declared Int
// ---------------------------------------------------------------------------

#[test]
fn assign_boolean_to_int() {
    let d = diags("fun box(): Int { val n: Int = true; return n }");
    assert_rejected(&d, "assigning a Boolean to an Int");
}

// ---------------------------------------------------------------------------
// Calling a stdlib function with the wrong argument type
// ---------------------------------------------------------------------------

#[test]
fn stdlib_arg_type_mismatch() {
    // `Char.digitToInt(radix)` needs an Int radix, not a String.
    let d = diags("fun box(): Int { return '7'.digitToInt(\"ten\") }");
    assert_rejected(&d, "stdlib arg type mismatch");
}

// ---------------------------------------------------------------------------
// Boolean/comparison misuse
// ---------------------------------------------------------------------------

#[test]
fn and_operands_not_boolean() {
    let d = diags("fun box(): Int { val b = 1 && 2; return 0 }");
    assert_rejected(&d, "&& on non-Boolean operands");
}

#[test]
fn while_condition_not_boolean() {
    let d = diags("fun box(): Int { while (5) { }; return 0 }");
    assert_rejected(&d, "non-Boolean while condition");
}

// ---------------------------------------------------------------------------
// Return outside the target type / Unit function returning a value
// ---------------------------------------------------------------------------

#[test]
fn return_value_from_unit_function() {
    let d = diags("fun u(): Unit { return 5 }\nfun box(): Int = 0");
    assert_rejected(&d, "returning a value from a Unit function");
}

// ---------------------------------------------------------------------------
// Elvis / null-safety
// ---------------------------------------------------------------------------

#[test]
fn elvis_on_non_null_operand() {
    // The left operand is a non-null Int; `?:` and its Char rhs is a type soup the checker rejects.
    let d = diags("fun box(): Int { val n = 5; val r = n ?: \"x\"; return 0 }");
    assert_rejected(&d, "elvis with a non-null left operand mixing types");
}

// ---------------------------------------------------------------------------
// Override with an incompatible signature
// ---------------------------------------------------------------------------

#[test]
fn override_with_wrong_signature() {
    let d = diags(
        "open class A { open fun f(): Int = 1 }\nclass B : A() { override fun f(x: Int): Int = x }\nfun box(): Int = 0",
    );
    assert_rejected(&d, "override with an incompatible signature");
}

// ---------------------------------------------------------------------------
// Assignment to a temporary / non-lvalue
// ---------------------------------------------------------------------------

#[test]
fn assign_to_literal() {
    let d = diags("fun box(): Int { 1 = 2; return 0 }");
    assert_rejected(&d, "assigning to a literal");
}

// ---------------------------------------------------------------------------
// String range / non-numeric range
// ---------------------------------------------------------------------------

#[test]
fn range_over_strings() {
    let d = diags("fun box(): Int { for (x in \"a\"..\"z\") { }; return 0 }");
    assert_rejected(&d, "range over Strings");
}

// ---------------------------------------------------------------------------
// Enum: unresolved entry
// ---------------------------------------------------------------------------

#[test]
fn unresolved_enum_entry() {
    let d = diags("enum class E { A, B }\nfun box(): Int { val e = E.Z; return 0 }");
    assert_rejected(&d, "unresolved enum entry");
}

// ---------------------------------------------------------------------------
// Wrong receiver: extension declared on Int called on String
// ---------------------------------------------------------------------------

#[test]
fn extension_on_wrong_receiver() {
    let d = diags(
        "fun Int.twice(): Int = this * 2\nfun box(): Int { val s = \"x\"; return s.twice() }",
    );
    assert_rejected(&d, "calling an Int extension on a String");
}

// ---------------------------------------------------------------------------
// Calling a property as a function
// ---------------------------------------------------------------------------

#[test]
fn call_non_callable_property() {
    let d = diags("class C { val x: Int = 1 }\nfun box(): Int { return C().x(0) }");
    assert_rejected(&d, "calling a non-callable property");
}

// ---------------------------------------------------------------------------
// Smart-cast on a `var` does not narrow (member absent after `is`)
// ---------------------------------------------------------------------------

#[test]
fn no_smart_cast_on_var() {
    let d = diags(
        "fun box(): Int { var a: Any = \"hi\"; if (a is String) { return a.length }; return 0 }",
    );
    assert_rejected(&d, "no smart-cast on a var receiver");
}

// ---------------------------------------------------------------------------
// Binary operator on incompatible operand types
// ---------------------------------------------------------------------------

#[test]
fn multiply_two_strings() {
    let d = diags("fun box(): Int { val s = \"a\" * \"b\"; return 0 }");
    assert_rejected(&d, "multiplying two Strings");
}

#[test]
fn compare_string_with_int() {
    let d = diags("fun box(): Int { val b = \"a\" < 1; return 0 }");
    assert_rejected(&d, "comparing a String with an Int");
}

// ---------------------------------------------------------------------------
// `this@Label` with an unresolved outer label
// ---------------------------------------------------------------------------

#[test]
fn this_with_unresolved_outer_label() {
    let d = diags("fun box(): Int { return this@Nope.hashCode() }");
    assert_rejected(&d, "this@ with an unresolved outer label");
}
