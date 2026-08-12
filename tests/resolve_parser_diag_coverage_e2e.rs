//! Additional resolver / parser ERROR-path coverage.
//!
//! Companion to `front_end_errors_e2e.rs`, `resolver_errors_coverage_e2e.rs`,
//! `parser_errors_coverage_e2e.rs` and `front_end_errors_more_e2e.rs`. Each test feeds an
//! INVALID Kotlin snippet through `lex → parse → collect → check` (via
//! `common::front_end_diagnostics`) and asserts the front end produced at least one diagnostic —
//! i.e. it rejected the snippet. These target `diags.error(...)` branches and error-recovery paths
//! in `src/resolve.rs` and `src/parser.rs` that the *valid-only* box corpus (and the existing
//! error-coverage suites) never reach: unsupported-feature rejections, duplicate/conflicting
//! declarations, bad modifiers, bad generics, malformed mid-declaration/mid-expression syntax, etc.
//!
//! Cases are DISTINCT from the sibling suites. Snippets that need library symbols get the
//! stdlib + JDK classpath and skip cleanly (a non-empty sentinel) when the toolchain is absent;
//! pure parse snippets use no classpath and never skip.

use super::common;

/// Run the front end with stdlib + JDK on the classpath. If the toolchain is unavailable, return a
/// sentinel so the "non-empty" assertions still hold (the test effectively skips).
fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
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

// ===========================================================================
// Modifier / declaration-consistency errors (resolve.rs)
// ===========================================================================

#[test]
fn reified_without_inline() {
    // `reified` is only legal on a type parameter of an `inline` function.
    let d = diags("fun <reified T> id(x: T): T = x\nfun box(): Int = 0");
    assert_rejected(&d, "reified type parameter on a non-inline function");
}

#[test]
fn abstract_member_with_body() {
    // An abstract member cannot carry a body.
    let d = diags("abstract class A { abstract fun f(): Int { return 1 } }\nfun box(): Int = 0");
    assert_rejected(&d, "abstract member with a body");
}

#[test]
fn abstract_and_final_member() {
    let d = diags("abstract class A { final abstract fun f(): Int }\nfun box(): Int = 0");
    assert_rejected(&d, "member both abstract and final");
}

#[test]
fn data_class_param_not_val_or_var() {
    // A data-class primary-constructor parameter must be `val`/`var`.
    let d = diags("data class D(x: Int)\nfun box(): Int = 0");
    assert_rejected(&d, "data class parameter without val/var");
}

#[test]
fn multiple_vararg_parameters() {
    let d = diags("fun f(vararg a: Int, vararg b: Int): Int = 0\nfun box(): Int = 0");
    assert_rejected(&d, "multiple vararg parameters");
}

// ===========================================================================
// Duplicate / conflicting declarations (resolve.rs)
// ===========================================================================

#[test]
fn duplicate_constructor_parameter() {
    let d = diags("class C(a: Int, a: Int)\nfun box(): Int = 0");
    assert_rejected(&d, "constructor parameter declared twice");
}

#[test]
fn duplicate_enum_entry() {
    let d = diags("enum class E { A, A }\nfun box(): Int = 0");
    assert_rejected(&d, "enum entry declared twice");
}

#[test]
fn duplicate_type_parameter() {
    let d = diags("fun <T, T> f(x: T): Int = 0\nfun box(): Int = 0");
    assert_rejected(&d, "type parameter declared twice");
}

#[test]
fn conflicting_extension_functions() {
    let d = diags("fun Int.f(): Int = 1\nfun Int.f(): Int = 2\nfun box(): Int = 0");
    assert_rejected(&d, "conflicting extension functions");
}

// ===========================================================================
// Unsupported-feature rejections (resolve.rs "krusty: ...")
// ===========================================================================

#[test]
fn increment_on_string_variable() {
    // `++`/`--` is only supported on a numeric variable.
    let d = diags("fun box(): Int { var s = \"x\"; s++; return 0 }");
    assert_rejected(&d, "increment on a String variable");
}

#[test]
fn companion_property_custom_accessor() {
    // A FIELD-LESS custom-accessor companion property is supported (it lowers to `getX`/`setX` on
    // `C$Companion`, kotlinc's own layout), so this must type-check clean — it guards that feature
    // against regressing back to a rejection.
    let d = diags("class C { companion object { val x: Int get() = 5 } }\nfun box(): Int = 0");
    let real: Vec<&String> = d.iter().filter(|m| !m.is_empty()).collect();
    assert!(
        real.is_empty() || d.iter().any(|m| m.contains("<skip")),
        "expected a computed companion property to type-check clean, got: {real:?}"
    );
    const SOURCE: &str = "class C { companion object { val x: Int = 1\n    get() = field + 1 } }\n\
         fun box(): String = if (C.x == 2) \"OK\" else \"FAIL\"";
    common::expect_box_ok_with_stdlib(SOURCE, "CompanionBackingAccessor");
}

#[test]
fn referential_equality_on_strings() {
    let d = diags("fun box(): Int { val a = \"x\"; val b = \"y\"; val c = a === b; return 0 }");
    assert_rejected(&d, "referential equality on Strings");
}

#[test]
fn referential_equality_on_a_value_class_operand() {
    // kotlinc: "identity equality for arguments of types 'Any' and 'UInt' is prohibited." An inline /
    // `@JvmInline value` class has no stable boxed identity, so `===` on one is an ERROR there — not the
    // "unstable because of implicit boxing" warning a plain primitive operand gets. krusty must reject
    // it too: the backend would otherwise box it through `box-impl` and happily emit an `if_acmp*` for a
    // program kotlinc refuses to compile.
    let d = diags("fun box(): Int { val a: Any = 1; val b: UInt = 1u; val c = a === b; return 0 }");
    assert_rejected(&d, "referential equality on an unsigned operand");
    let d = diags(
        "@JvmInline value class VC(val x: Int)\n\
         fun box(): Int { val a: Any = 1; val b = VC(1); val c = a === b; return 0 }",
    );
    assert_rejected(&d, "referential equality on a value-class operand");
    // Both operands the same value class is prohibited as well.
    let d = diags(
        "@JvmInline value class VC(val x: Int)\n\
         fun box(): Int { val a = VC(1); val b = VC(1); val c = a === b; return 0 }",
    );
    assert_rejected(&d, "referential equality on two value-class operands");
    // A dependency inline class (`kotlin.time.Duration`) must be caught by the SAME semantic query.
    // Such a type reaches the backend as its unboxed carrier (`Duration` → `Ty::Long`), so an
    // unrejected `===` silently compares carriers or boxes one as an unrelated JVM wrapper. Keeping
    // source value metadata in the provider-neutral classifier shape makes this regression exercise
    // the federated resolver instead of an identity-specific source/classpath branch.
    let d = diags(
        "import kotlin.time.Duration\n\
         fun box(a: Duration, b: Duration): Int { val c = a === b; return 0 }",
    );
    assert_rejected(
        &d,
        "referential equality on a classpath value-class operand",
    );
    // Pin the message, not merely "some diagnostic": `assert_rejected` accepts any non-empty string,
    // so without this an unrelated resolution failure would keep the test green.
    if common::stdlib_toolchain_ready() {
        assert!(
            d.iter().any(|m| m.contains("is prohibited")),
            "expected kotlinc's \"is prohibited\" wording, got: {d:?}"
        );
    }
}

#[test]
fn referential_equality_on_mismatched_primitive_types() {
    // Plain primitives of the SAME type may use `===` (with kotlinc's warning) and lower as value
    // equality. Different primitive types are a hard error: they have neither object identity nor a
    // shared scalar comparison domain. Pin all JVM comparison families so the emitter can never be
    // asked to choose an int/long/float/double operation from only one mismatched operand.
    for (source, description) in [
        (
            "fun bad(a: Int, b: Long): Boolean = a === b",
            "referential equality between Int and Long",
        ),
        (
            "fun bad(a: Int, b: Char): Boolean = a === b",
            "referential equality between Int and Char",
        ),
        (
            "fun bad(a: Float, b: Double): Boolean = a === b",
            "referential equality between Float and Double",
        ),
    ] {
        let d = diags(source);
        assert_rejected(&d, description);
        assert!(
            d.iter().any(|m| m.contains("is prohibited")),
            "expected the identity-prohibition diagnostic for {description}, got: {d:?}"
        );
    }
}

#[test]
fn nested_try_with_finally() {
    let d = diags("fun box(): Int { try { try { return 1 } finally {} } finally {}; return 0 }");
    assert_rejected(&d, "nested try combined with finally");
}

#[test]
fn range_over_doubles() {
    // Floating-point `..` ranges are now supported (they build a `ClosedFloatingPointRange` via
    // `RangesKt.rangeTo`), so this must type-check clean — no diagnostic. Guards the range-support
    // feature against regressing back to a rejection.
    let d = diags("fun box(): Int { val r = 1.0..2.0; return 0 }");
    let real: Vec<&String> = d.iter().filter(|m| !m.is_empty()).collect();
    assert!(
        real.is_empty() || d.iter().any(|m| m.contains("<skip")),
        "expected float range to type-check clean, got: {real:?}"
    );
}

#[test]
fn empty_array_of_without_type() {
    let d = diags("fun box(): Int { val a = arrayOf(); return 0 }");
    assert_rejected(&d, "empty arrayOf without explicit type");
}

#[test]
fn non_dispatchable_operator_extension_on_nullable_primitive() {
    // Operator names whose call paths never dispatch by receiver nullability (`get`, `equals`, …)
    // stay rejected on a nullable primitive — accepting them would silently keep the builtin.
    let d = diags("operator fun Int?.get(i: Int): Int = 0\nfun box(): Int = 0");
    assert_rejected(
        &d,
        "non-dispatchable operator extension on a nullable primitive receiver",
    );
}

#[test]
fn secondary_constructor_not_delegating_to_primary() {
    let d = diags("class C(val a: Int) { constructor(b: String) { } }\nfun box(): Int = 0");
    assert_rejected(&d, "secondary constructor not delegating to primary");
}

#[test]
fn plus_on_booleans() {
    let d = diags("fun box(): Int { val x = true + false; return 0 }");
    assert_rejected(&d, "'+' applied to Booleans");
}

// ===========================================================================
// Control-flow / when (resolve.rs)
// ===========================================================================

#[test]
fn when_with_two_else_branches() {
    let d = diags("fun box(): Int { val x = 1; return when (x) { 1 -> 1; else -> 2; else -> 3 } }");
    assert_rejected(&d, "when with two else branches");
}

#[test]
fn when_guard_requires_its_language_feature() {
    let d = diags(
        "sealed interface V\nclass A(val ok: Boolean) : V\nfun f(v: V) = when (v) { is A if v.ok -> 1; else -> 0 }",
    );
    assert!(
        d.iter()
            .any(|message| message.contains("when guards are disabled")),
        "{d:?}"
    );
}

// ===========================================================================
// Parser: unsupported constructs
// ===========================================================================

#[test]
fn type_parameter_primitive_upper_bound() {
    // Primitive bounds are source-valid; physical specialization is not parser policy.
    let d = parse_diags("fun <T : Double> f(x: T): Int = 0\nfun box(): Int = 0");
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn secondary_constructor_in_enum() {
    let d = parse_diags("enum class E { A; constructor() { } }\nfun box(): Int = 0");
    assert_rejected(&d, "secondary constructor in an enum class");
}

#[test]
fn enum_entry_body_with_nested_class() {
    let d = parse_diags("enum class E { A { class X } }\nfun box(): Int = 0");
    assert_rejected(&d, "unsupported member in an enum entry body");
}

#[test]
fn property_without_initializer_not_lateinit() {
    // A top-level property with no initializer/getter/delegate must be `lateinit`.
    let d = parse_diags("val x: Int\nfun box(): Int = 0");
    assert_rejected(&d, "non-lateinit property without initializer");
}

// ===========================================================================
// Parser: mid-declaration / mid-expression recovery
// ===========================================================================

#[test]
fn type_parameter_bound_missing_type() {
    let d = parse_diags("fun <T : > f(): Int = 0\nfun box(): Int = 0");
    assert_rejected(&d, "type-parameter bound missing a type");
}

#[test]
fn when_subject_unclosed_paren() {
    let d = parse_diags("fun box(): Int { return when (1 { else -> 0 } }");
    assert_rejected(&d, "when subject with an unclosed paren");
}

#[test]
fn call_argument_trailing_comma_then_close() {
    // `f(1,)` malformed on a call site followed by junk.
    let d = parse_diags("fun box(): Int { return maxOf(1, ) }");
    assert_rejected(&d, "call argument missing after comma");
}

#[test]
fn double_colon_without_reference() {
    let d = parse_diags("fun box(): Int { val r = ::; return 0 }");
    assert_rejected(&d, "callable reference with no name");
}

#[test]
fn while_missing_body_at_eof() {
    let d = parse_diags("fun box(): Int { while (true)");
    assert_rejected(&d, "while with no body at EOF");
}

#[test]
fn do_while_missing_while() {
    let d = parse_diags("fun box(): Int { do { } return 0 }");
    assert_rejected(&d, "do-block without a trailing while");
}

#[test]
fn else_without_if() {
    let d = parse_diags("fun box(): Int { else { return 1 }; return 0 }");
    assert_rejected(&d, "else with no matching if");
}

#[test]
fn catch_type_missing() {
    let d = parse_diags("fun box(): Int { try { return 1 } catch (e: ) { return 2 } }");
    assert_rejected(&d, "catch binding with an empty type");
}

#[test]
fn generic_call_missing_type_argument() {
    let d = parse_diags("fun box(): Int { return listOf<>(1).size }");
    assert_rejected(&d, "generic call with an empty type-argument list");
}

#[test]
fn string_template_unclosed_brace_expr() {
    let d = parse_diags("fun box(): Int { val n = 1; val s = \"v=${n\"; return 0 }");
    assert_rejected(&d, "string template with an unclosed ${ } expression");
}

#[test]
fn assignment_missing_rhs_at_eof() {
    let d = parse_diags("fun box(): Int { var x = 1; x =");
    assert_rejected(&d, "assignment with no rhs at EOF");
}

#[test]
fn nested_generic_unclosed() {
    let d = parse_diags("fun box(): Int { val m: Map<String, List<Int = 0; return 0 }");
    assert_rejected(&d, "nested generic type never closed");
}

// ===========================================================================
// Resolve: misc reference / type errors distinct from the sibling suites
// ===========================================================================

#[test]
fn unresolved_qualified_type() {
    let d = diags("fun box(): java.util.NoSuchThing { TODO() }");
    assert_rejected(&d, "unresolved qualified type");
}

#[test]
fn assign_to_this() {
    let d = diags("class C { fun f() { this = C() } }\nfun box(): Int = 0");
    assert_rejected(&d, "assigning to this");
}

#[test]
fn return_outside_function() {
    let d = diags("val x = return 5\nfun box(): Int = 0");
    assert_rejected(&d, "return in a property initializer / top level");
}

#[test]
fn instantiate_abstract_class() {
    let d = diags("abstract class A\nfun box(): Int { val a = A(); return 0 }");
    assert_rejected(&d, "instantiating an abstract class");
}

#[test]
fn instantiate_interface() {
    let d = diags("interface I\nfun box(): Int { val i = I(); return 0 }");
    assert_rejected(&d, "instantiating an interface");
}
