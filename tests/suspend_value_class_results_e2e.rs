//! How a value-class result crosses a suspension, compared against kotlinc.
//!
//! A suspend function whose declared result is a value class returns its carrier where it does not
//! suspend, and its continuation hands the value to the completion as the box. A caller that resumes
//! with the box unboxes it, so it cannot forward its own continuation to such a call. A function that
//! overrides a declaration returning a type parameter returns the box instead, and a call to a callee
//! returning a type parameter receives the box on either path, so such a call stays a tail call.

use super::common;

fn expect_method_matches(src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(
        "SuspendValueClassResults",
        &[],
        src,
        class,
        method,
    ) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const RESULTS: &str = "@JvmInline value class Name(val s: String)\n\
interface Gate { suspend fun open(): Int }\n\
suspend fun get(g: Gate): Name { g.open(); return Name(\"x\") }\n\
suspend fun maybe(g: Gate, n: Int): Name? { g.open(); return if (n > 0) Name(\"y\") else null }\n\
suspend fun forward(g: Gate): String = get(g).s\n";

/// The continuation boxes the carrier after the `COROUTINE_SUSPENDED` check, and keeps a `null`
/// of a nullable carrier as it is.
#[test]
fn a_continuation_hands_a_value_class_result_to_its_completion_boxed() {
    for class in [
        "SuspendValueClassResultsKt$get$1",
        "SuspendValueClassResultsKt$maybe$1",
    ] {
        expect_method_matches(
            RESULTS,
            class,
            "public final java.lang.Object invokeSuspend(java.lang.Object);",
        );
    }
}

/// `forward` unboxes the box it resumes with, so it keeps a continuation of its own instead of
/// handing `get` the one it was given.
#[test]
fn a_call_resumed_with_a_boxed_value_class_is_not_a_tail_call() {
    expect_method_matches(
        RESULTS,
        "SuspendValueClassResultsKt$forward$1",
        "public final java.lang.Object invokeSuspend(java.lang.Object);",
    );
}

const OVERRIDES: &str = "@JvmInline value class Name(val s: String)\n\
interface Gate { suspend fun open(): Int }\n\
suspend fun <T> pick(g: Gate, v: T): T { g.open(); return v }\n\
interface Base<T : Name?> { suspend fun generic(): T }\n\
class Derived(val g: Gate) : Base<Name> { override suspend fun generic(): Name = pick(g, Name(\"z\")) }\n\
suspend fun viaBase(b: Base<*>): String = b.generic()!!.s\n";

/// `Derived.generic` overrides a declaration returning a type parameter, so it returns the box that
/// the generic `pick` completes with and forwards its continuation to it.
#[test]
fn an_override_of_a_type_parameter_result_returns_the_box_of_a_generic_call() {
    expect_method_matches(
        OVERRIDES,
        "Derived",
        "public java.lang.Object generic-t1DQ2nc(kotlin.coroutines.Continuation<? super Name>);",
    );
}

/// A call through the type parameter's declaration unboxes the box it receives on either path.
#[test]
fn a_call_to_a_type_parameter_result_unboxes_it() {
    expect_method_matches(
        OVERRIDES,
        "SuspendValueClassResultsKt$viaBase$1",
        "public final java.lang.Object invokeSuspend(java.lang.Object);",
    );
}

const PRIVATE_MEMBER: &str = "class Holder {\n\
    private suspend fun h(x: Int): String = \"OK\"\n\
    fun k(): suspend () -> String = { h(1) }\n\
}\n\
fun box(): String { Holder().k(); return \"OK\" }\n";

/// A private suspend member called from a suspend lambda class gets one `access$` bridge, which
/// also serves as its continuation's re-entry. A second one would stop `Holder` from loading.
#[test]
fn a_private_suspend_member_called_from_a_lambda_class_has_one_access_bridge() {
    expect_method_matches(
        PRIVATE_MEMBER,
        "Holder",
        "public static final java.lang.Object access$h(Holder, int, kotlin.coroutines.Continuation);",
    );
    let jdk = common::jdk_modules();
    let out = common::compile_and_run_box(
        PRIVATE_MEMBER,
        "Main",
        &[common::stdlib_jar(), jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}
