//! `CallableReferencesToContextual`: a reference to a declaration with context parameters selects
//! a scope value for each of them where the reference is written and binds it, so the reference's
//! own type shows only the remaining parameters. Without the feature such a reference is an error.

use crate::common;

/// With the feature on, every reference form binds its context arguments and the program has no
/// diagnostics.
#[test]
fn function_references_with_context_parameters_are_accepted_with_the_feature() {
    let source = include_str!("contextual_references/contextualFunctionReferences.kt");
    let diagnostics = common::front_end_diagnostics(
        source,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

/// Without the feature a reference to a declaration with context parameters is an error, and
/// krusty reports kotlinc's complete ledger.
#[test]
fn a_function_reference_with_context_parameters_is_rejected_without_the_feature() {
    let source = "\
context(c: String)
fun single(): String = c

context(s: String, c: Char)
fun two(x: Int): String = s + c + x

fun box(): String {
    context(\"ctx\", 'c') {
        val s: () -> String = ::single
        val t: (Int) -> String = ::two
    }
    return \"OK\"
}
";
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

/// Property references bind their context arguments the same way: top-level, member and extension
/// properties, read-only and mutable, bound and unbound, all compile without diagnostics.
#[test]
fn property_references_with_context_parameters_are_accepted_with_the_feature() {
    let source = include_str!("box_regressions/contextParameters/contextualPropertyReferences.kt");
    let diagnostics = common::front_end_diagnostics(
        source,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

/// Without the feature a reference to a property with context parameters is an error, and krusty
/// reports kotlinc's complete ledger for every property shape.
#[test]
fn a_property_reference_with_context_parameters_is_rejected_without_the_feature() {
    let source = "\
context(c: String, n: Int)
val top: String
    get() = c + n

context(c: String)
var topVar: String
    get() = c
    set(value) {}

class A(val tag: String) {
    context(c: String)
    val member: String
        get() = tag + c
}

context(c: String)
val String.ext: String
    get() = this + c

fun box(): String {
    context(\"ctx\", 1) {
        val a = ::top
        val b = ::topVar
        val c = A::member
        val d = A(\"t\")::member
        val e = String::ext
    }
    return \"OK\"
}
";
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}
