//! A suspend function with no suspension point still goes through kotlinc's coroutine
//! transformer, which returns early but first boxes primitives through
//! `kotlin.coroutines.jvm.internal.Boxing` instead of the wrappers' `valueOf`.
//!
//! `coroutineContext` reads the context of the continuation the transformer puts in place of the
//! fake one, and ends like the inlined getter it is.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    expect_method_matches_with_lib(name, &[], src, class, method);
}

fn expect_method_matches_with_lib(
    name: &str,
    lib: &[(&str, &str)],
    src: &str,
    class: &str,
    method: &str,
) {
    match common::method_code_diff_against_kotlinc(name, lib, src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const PRIMITIVES: &str = "suspend fun count(): Int = 1\n\
suspend fun wide(n: Long): Long = n * 3L\n\
suspend fun flag(n: Int): Boolean = n > 2\n\
suspend fun letter(): Char = 'k'\n\
suspend fun ratio(n: Int): Double = n / 2.0\n\
suspend fun nothingToBox(n: Int) {\n\
    if (n > 0) return\n\
}\n";

#[test]
fn a_suspend_function_without_a_suspension_point_boxes_through_the_coroutine_helpers() {
    for method in [
        "public static final java.lang.Object count(",
        "public static final java.lang.Object wide(",
        "public static final java.lang.Object flag(",
        "public static final java.lang.Object letter(",
        "public static final java.lang.Object ratio(",
        "public static final java.lang.Object nothingToBox(",
    ] {
        expect_method_matches("SuspendBoxing", PRIMITIVES, "SuspendBoxingKt", method);
    }
}

const HIDDEN: &str = "private suspend fun hidden(n: Int): Int = n + 1\n\
suspend fun callsHidden(): Int = hidden(1)\n";

/// A private function has no state machine to build either, so it boxes through the coroutine
/// helpers too.
#[test]
fn a_private_function_without_a_suspension_point_boxes_through_the_coroutine_helpers() {
    expect_method_matches(
        "SuspendHiddenBoxing",
        HIDDEN,
        "SuspendHiddenBoxingKt",
        "private static final java.lang.Object hidden(",
    );
}

const NESTED: &str = "interface Api {\n\
    suspend fun count(): Int = 3\n\
}\n\
class Impl : Api\n\
suspend fun outer(): Int {\n\
    suspend fun local(n: Int): Int = n + 1\n\
    return local(2)\n\
}\n";

/// An interface's default body and a local function are neither top-level nor class members, but
/// with no suspension point there is no continuation class to place, so they box through the
/// coroutine helpers too.
#[test]
fn an_interface_default_or_local_function_without_a_suspension_point_boxes_through_the_helpers() {
    for (class, method) in [
        ("Api", "public static java.lang.Object count$suspendImpl("),
        (
            "SuspendNestedBoxingKt",
            "private static final java.lang.Object outer$local(",
        ),
    ] {
        expect_method_matches("SuspendNestedBoxing", NESTED, class, method);
    }
}

const INLINE_LIB: &str = "package lib\ninline fun twice(n: Int): Int = n * 2\n";

const INLINE_RESULT: &str = "import lib.twice\n\
suspend fun doubled(n: Int): Int = twice(n)\n\
suspend fun doubledFlag(n: Int): Boolean = twice(n) > 3\n";

/// A result an inline call produces is spliced into the method before the transformer boxes it,
/// with no state machine to build.
#[test]
fn an_inline_result_without_a_suspension_point_boxes_through_the_coroutine_helpers() {
    for method in [
        "public static final java.lang.Object doubled(",
        "public static final java.lang.Object doubledFlag(",
    ] {
        expect_method_matches_with_lib(
            "SuspendInlineBoxing",
            &[("Lib.kt", INLINE_LIB)],
            INLINE_RESULT,
            "SuspendInlineBoxingKt",
            method,
        );
    }
}

const CONTEXT: &str = "import kotlin.coroutines.*\n\
suspend fun context(): CoroutineContext = coroutineContext\n\
suspend fun step(): Int = 1\n\
suspend fun around(): Int {\n\
    val before = coroutineContext\n\
    val v = step()\n\
    return if (before == coroutineContext) v else 0\n\
}\n";

#[test]
fn a_context_read_without_a_suspension_point_matches_kotlinc() {
    expect_method_matches(
        "SuspendContext",
        CONTEXT,
        "SuspendContextKt",
        "public static final java.lang.Object context(",
    );
}

#[test]
fn a_context_read_around_a_suspension_point_matches_kotlinc() {
    expect_method_matches(
        "SuspendContext",
        CONTEXT,
        "SuspendContextKt",
        "public static final java.lang.Object around(",
    );
}

#[test]
fn a_context_read_around_a_suspension_point_runs() {
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Int>? = null\n\
suspend fun step(): Int = suspendCoroutine { parked = it }\n\
suspend fun around(): Int {\n\
    val before = coroutineContext\n\
    val v = step()\n\
    return if (before == coroutineContext) v else 0\n\
}\n\
suspend fun count(): Int = 4\n\
fun box(): String {\n\
    var a = 0\n\
    var b = 0\n\
    suspend { a = around() }.startCoroutine(Done())\n\
    parked!!.resume(7)\n\
    suspend { b = count() }.startCoroutine(Done())\n\
    return if (a == 7 && b == 4) \"OK\" else \"F:$a:$b\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SuspendContextRuns");
}

/// A member with no suspension point that builds a suspend lambda: the lambda is realized as its
/// own class before the member's body is reshaped for the transformer, so krusty writes kotlinc's
/// classes, and the member and the lambda class declare kotlinc's methods.
#[test]
fn a_member_without_a_suspension_point_builds_its_suspend_lambda_class() {
    let src = "fun builder(c: suspend () -> Unit) {}\n\
class A {\n\
    suspend fun step(): String = \"s\"\n\
    suspend fun member(): String {\n\
        builder { step() }\n\
        return \"m\"\n\
    }\n\
}\n";
    let classes = common::classes_against_kotlinc_lib(
        "SuspendLambdaHost",
        &[("Lib.kt", "package lib\n")],
        src,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for class in classes.reference.keys() {
        let (reference, krusty) = classes
            .method_declarations(class)
            .expect("both compilers write the class");
        assert_eq!(krusty, reference, "{class}");
    }
}

/// A suspend lambda with no suspension point that reads `coroutineContext` reads the context of its
/// own class, which is its continuation.
#[test]
fn a_suspend_lambda_without_a_suspension_point_reads_its_context() {
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
fun box(): String {\n\
    var seen: CoroutineContext? = null\n\
    suspend { seen = coroutineContext }.startCoroutine(Done())\n\
    return if (seen == EmptyCoroutineContext) \"OK\" else \"F:$seen\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SuspendLambdaContext");
}
