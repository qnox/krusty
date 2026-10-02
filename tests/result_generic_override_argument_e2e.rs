//! `kotlin.Result` is the one value class whose parameter does not mangle its function's name, so
//! an override of a generic `foo(x: T)` with `T = Result<…>` keeps the erased `foo(Object)` and
//! receives the box there. A caller passes that box: kotlinc boxes the carrier with `box-impl`.
//! krusty boxed it and then unboxed it again for the `Object` slot it shares with the carrier, and
//! the override's cast of the carrier to `Result` failed. The override also checks no null: the
//! box's underlying accepts null, as for any `Result` parameter.
//!
//! `Result.success` is the stdlib call here because `Result`'s constructor is internal to the
//! stdlib; no repo-owned declaration can stand for the one exempt class.

use super::common;

const SRC: &str = "interface Sink<T> {\n\
    \x20   fun take(x: T): Any?\n\
    }\n\
    fun reboxed(r: Result<Any?>): Any? = r\n\
    class Keep : Sink<Result<Any?>> {\n\
    \x20   override fun take(x: Result<Any?>): Any? = reboxed(x)\n\
    }\n\
    fun pass(): Any? = Keep().take(Result.success(\"OK\"))\n\
    fun box(): String = if (pass() is Result<*>) \"OK\" else \"fail\"\n";

fn compare(class: &str, method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ResultGenericOverride",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_caller_passes_the_box_to_an_override_of_a_generic_parameter() {
    compare(
        "ResultGenericOverrideKt",
        "public static final java.lang.Object pass()",
    );
}

#[test]
fn the_override_does_not_null_check_its_boxed_result() {
    compare("Keep", "public java.lang.Object take(java.lang.Object)");
}

#[test]
fn a_result_passed_to_a_generic_override_runs() {
    common::expect_box_same_as_kotlinc(SRC, "ResultGenericOverride");
}

/// The override shares its name with an overload declared before it. Only the override the bridge
/// delegates to receives the box; the overload keeps its own `int` parameter.
const OVERLOADED: &str = "interface Sink<T> {\n\
    \x20   fun take(x: T): Any?\n\
    }\n\
    fun reboxed(r: Result<Any?>): Any? = r\n\
    class Keep : Sink<Result<Any?>> {\n\
    \x20   fun take(x: Int): Any? = x\n\
    \x20   override fun take(x: Result<Any?>): Any? = reboxed(x)\n\
    }\n\
    fun pass(): Any? = Keep().take(Result.success(\"OK\"))\n\
    fun count(): Any? = Keep().take(7)\n\
    fun box(): String = if (pass() is Result<*> && count() == 7) \"OK\" else \"fail\"\n";

#[test]
fn only_the_bridged_overload_receives_the_box() {
    let built = common::compare_with_kotlinc_plugin(
        "ResultGenericOverrideOverload",
        OVERLOADED,
        "Keep",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for method in [
        "public final java.lang.Object take(int)",
        "public java.lang.Object take(java.lang.Object)",
    ] {
        let reference = common::method_instructions(&built.reference, method);
        assert!(!reference.is_empty(), "kotlinc writes {method}");
        assert_eq!(
            common::method_instructions(&built.krusty, method),
            reference,
            "{method}"
        );
    }
}

#[test]
fn an_overloaded_result_override_runs() {
    common::expect_box_same_as_kotlinc(OVERLOADED, "ResultGenericOverrideOverload");
}
