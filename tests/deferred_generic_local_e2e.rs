//! An uninitialized local of a type parameter keeps that parameter's erased JVM slot
//! after the inline call specializes the parameter to a primitive.
//!
//! `var result: R` is lowered as a store of `R`'s zero. Unbounded `R` erases to `Object`,
//! so the zero is `null` and a later `Int` is boxed into the slot; the caller's `Int`
//! return unboxes it. A primitive bound (`R : Int`) is already an `int`, and its zero is
//! `0`. Specializing the unbounded local itself to `int` unboxes the `null`.

use super::common;

const UNBOUNDED: &str = "\
inline fun <R> f(size: Int, block: () -> R): R {\n\
    var result: R\n\
    while (true) {\n\
        result = block()\n\
        if (size == 0) break\n\
    }\n\
    return result\n\
}\n\
fun computeResult(size: Int) = f(size) { 42 }\n\
fun box() = if (computeResult(0) == 42) \"OK\" else \"FAIL\"\n\
";

const PRIMITIVE_BOUND: &str = "\
inline fun <R : Int> f(block: () -> R): R {\n\
    var result: R\n\
    result = block()\n\
    return result\n\
}\n\
fun computeResult() = f { 42 }\n\
fun box() = if (computeResult() == 42) \"OK\" else \"FAIL\"\n\
";

fn assert_same_instructions(stem: &str, src: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        stem,
        src,
        &format!("{stem}Kt"),
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{member}"
    );
}

#[test]
fn an_uninitialized_unbounded_local_stays_boxed_like_kotlinc() {
    assert_same_instructions("DeferredGenericLocal", UNBOUNDED, "int computeResult(int);");
    assert_same_instructions(
        "DeferredGenericLocal",
        UNBOUNDED,
        "f(int, kotlin.jvm.functions.Function0",
    );
}

#[test]
fn an_uninitialized_primitive_bound_local_stays_unboxed_like_kotlinc() {
    assert_same_instructions(
        "DeferredPrimitiveBoundLocal",
        PRIMITIVE_BOUND,
        "int computeResult();",
    );
}

#[test]
fn an_uninitialized_generic_local_runs() {
    common::expect_box_ok_with_stdlib(UNBOUNDED, "DeferredGenericLocal");
    common::expect_box_ok_with_stdlib(PRIMITIVE_BOUND, "DeferredPrimitiveBoundLocal");
}
