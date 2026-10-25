//! PLATFORM-SPECIFIC: declaration-owned constructor slots select JVM boxing casts.

use super::common;

const DEPENDENT: &str = "class Outer<O : Number> { inner class Inner<T : O>(val value: T) }\n";

#[test]
fn dependent_constructor_parameters_keep_their_declared_boxing_casts() {
    let source = format!("{DEPENDENT}fun make(outer: Outer<Int>) = outer.Inner<Int>(1)\n");
    let classes = common::classes_against_kotlinc_module(&[("Use.kt", &source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn direct_reference_constructor_parameters_keep_ordinary_boxing() {
    let source = "class Outer<O : Number> { inner class Inner(val value: Number) }\nfun make(outer: Outer<Int>) = outer.Inner(1)\n";
    let classes = common::classes_against_kotlinc_module(&[("Use.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn independently_bounded_constructor_parameters_keep_their_declared_casts() {
    let source = "class Box<T : Number>(val value: T)\nfun make() = Box<Int>(1)\n";
    let classes = common::classes_against_kotlinc_module(&[("Use.kt", source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn independently_bounded_dependency_constructor_parameters_keep_their_declared_casts() {
    let classes = common::classes_against_kotlinc_lib(
        "BoundedConstructorBoxing",
        &[("Declarations.kt", "class Box<T : Number>(val value: T)\n")],
        "fun make() = Box<Int>(1)\n",
    )
    .expect("reference compiler provisions the independently bounded constructor");
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const BOUNDARY_DECLARATIONS: &str = "class Wide<T : Number>(val value: T)\nclass Unbounded<T>(val value: T)\nclass Exact<T : Int>(val value: T)\n";
const BOUNDARY_CALLS: &str = "fun wide() = Wide<Long>(2L)\nfun unbounded() = Unbounded<Int>(3)\nfun exact() = Exact<Int>(4)\n";

#[test]
fn wide_object_and_exact_wrapper_constructor_slots_match_the_reference() {
    let source = format!("{BOUNDARY_DECLARATIONS}{BOUNDARY_CALLS}");
    let classes = common::classes_against_kotlinc_module(&[("Use.kt", &source)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn wide_object_and_exact_wrapper_dependency_slots_match_the_reference() {
    let classes = common::classes_against_kotlinc_lib(
        "BoundaryConstructorBoxing",
        &[("Declarations.kt", BOUNDARY_DECLARATIONS)],
        BOUNDARY_CALLS,
    )
    .expect("reference compiler provisions the constructor boundaries");
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const RUNTIME: &str = r#"
var effects = 0
fun next(): Int { effects += 1; return effects }
fun box(): String {
    val outer = Outer<Int>()
    val inner = outer.Inner<Int>(next())
    val wide = Wide<Long>(2L)
    val unbounded = Unbounded<Int>(next())
    val exact = Exact<Int>(next())
    if (inner.value != 1 || wide.value != 2L || unbounded.value != 2 || exact.value != 3 || effects != 3) return "FAIL"
    return "OK"
}
"#;

#[test]
fn constructor_boxing_preserves_values_and_single_evaluation() {
    let source = format!("{DEPENDENT}{BOUNDARY_DECLARATIONS}{RUNTIME}");
    common::expect_box_ok_with_stdlib(&source, "ConstructorBoxingRuntime");
}

#[test]
fn dependency_constructor_boxing_preserves_values_and_single_evaluation() {
    let runtime = r#"
var effects = 0
fun next(): Int { effects += 1; return effects }
fun box(): String {
    val bounded = Wide<Int>(next())
    val wide = Wide<Long>(2L)
    val unbounded = Unbounded<Int>(next())
    val exact = Exact<Int>(next())
    if (bounded.value != 1 || wide.value != 2L || unbounded.value != 2 || exact.value != 3 || effects != 3) return "FAIL"
    return "OK"
}
"#;
    assert_eq!(
        common::expect_box_run_against("ConstructorBoxingRuntime", BOUNDARY_DECLARATIONS, runtime),
        Some("OK".into()),
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(BOUNDARY_DECLARATIONS, runtime),
        Some("OK".into()),
    );
}
