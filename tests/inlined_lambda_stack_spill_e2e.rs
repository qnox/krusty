//! kotlinc brackets every lambda it inlines into a classpath inline body with
//! `InlineMarker.beforeInlineCall`/`afterInlineCall`, and FixStack in the finished caller stores
//! whatever is on the operand stack under the lambda into fresh locals, reloading it under the
//! lambda's result. That is the body's own operands (stdlib `map`'s inlined `mapTo` has the
//! destination on the stack at the transform's `invoke`) and the caller's, pushed before the inline
//! call. A lambda with a `try` must be entered on that saved, empty stack: its handler starts with
//! only the exception, or the paths join with different stacks (the private corpus crashed on
//! exactly this: "an inlined body's stack was already followed: Mismatch").

use super::common;

/// A spliced `runCatching` in a lambda invoked over a non-empty stack in the inline body.
const RUN_CATCHING_LAMBDA: &str = r#"
fun parsed(values: List<String>): List<Int> =
    values.map { text -> runCatching { text.toInt() }.getOrDefault(-1) }

fun box(): String {
    val result = parsed(listOf("1", "x", "3"))
    return if (result == listOf(1, -1, 3)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_run_catching_lambda_inlined_over_a_stacked_value_runs() {
    common::expect_box_ok_with_stdlib(RUN_CATCHING_LAMBDA, "InlinedLambdaRunCatchingSpill");
}

/// A source `try`/`catch` in the lambda: the handler must catch on the throwing element only.
const TRY_CATCH_LAMBDA: &str = r#"
fun parsed(values: List<String>): List<Int> =
    values.map { text ->
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    }

fun box(): String {
    val result = parsed(listOf("7", "seven", "9"))
    return if (result == listOf(7, -1, 9)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_over_a_stacked_value_runs() {
    common::expect_box_ok_with_stdlib(TRY_CATCH_LAMBDA, "InlinedLambdaTryCatchSpill");
}

/// The inline call is a later argument, so caller operands are pushed before it.
const TRY_CATCH_LAMBDA_AFTER_ARGUMENTS: &str = r#"
fun consume(n: Int, total: Long, values: List<Int>): String = "$n$total$values"

fun parsed(values: List<String>): String =
    consume(1, 2L, values.map { text ->
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    })

fun box(): String {
    val result = parsed(listOf("7", "seven", "9"))
    return if (result == "12[7, -1, 9]") "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_after_caller_arguments_runs() {
    common::expect_box_ok_with_stdlib(
        TRY_CATCH_LAMBDA_AFTER_ARGUMENTS,
        "InlinedLambdaTryCatchAfterArguments",
    );
}

/// A kotlinc-compiled inline function whose lambda is invoked with a `StringBuilder` on the stack.
/// It is `@InlineOnly`, so it has no callable fallback: a run proves the body was inlined.
const LIB: &str = r#"
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

@kotlin.internal.InlineOnly
inline fun tagged(prefix: String, block: () -> Int): String = prefix + block()
"#;

/// The inline call is a constructor argument, so the caller's `new Holder; dup` is under the whole
/// inlined body: the bracket around the lambda saves the caller's operands and the body's.
const TRY_CATCH_LAMBDA_UNDER_A_CONSTRUCTION: &str = r#"
class Holder(val value: String)

fun parsed(text: String): Holder =
    Holder(tagged("v") {
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    })

fun box(): String {
    val result = parsed("7").value + parsed("seven").value
    return if (result == "v7v-1") "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_under_a_caller_construction_runs() {
    let library = common::kotlinc_library(LIB).expect("reference compiler unavailable");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            TRY_CATCH_LAMBDA_UNDER_A_CONSTRUCTION,
            "InlinedLambdaTryCatchUnderConstruction",
            &[library, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}

/// The same caller operands under a lambda without a `try`, saved and reloaded as kotlinc does.
const LAMBDA_UNDER_A_CONSTRUCTION: &str = r#"
class Holder(val value: String)

fun parsed(text: String): Holder = Holder(tagged("v") { text.length })
"#;

#[test]
fn a_lambda_inlined_under_a_caller_construction_spills_like_kotlinc() {
    let library = common::kotlinc_library(LIB).expect("reference compiler unavailable");
    common::assert_class_code_matches_kotlinc_jdk_cp(
        "InlinedLambdaUnderConstructionParity",
        LAMBDA_UNDER_A_CONSTRUCTION,
        "InlinedLambdaUnderConstructionParityKt",
        std::slice::from_ref(&library),
    );
}

/// The same save and reload happens around a lambda with no `try` at all: the destination goes to
/// a local above the inlined frame and comes back under the lambda's result (`aload; swap`).
const PLAIN_LAMBDA: &str = r#"
fun lengths(values: List<String>): List<Int> = values.map { it.length + 1 }

fun box(): String {
    val result = lengths(listOf("a", "bb"))
    return if (result == listOf(2, 3)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_plain_lambda_inlined_over_a_stacked_value_spills_like_kotlinc() {
    common::assert_class_code_matches_kotlinc(
        "InlinedLambdaPlainSpillParity",
        PLAIN_LAMBDA,
        "InlinedLambdaPlainSpillParityKt",
    );
}

/// A suspension in a lambda whose inline body brackets an inner lambda: the coroutine machine's
/// spill discovery types the body itself and has to follow each bracket as the frames it reads
/// were computed, saving the destination the inner `map` keeps under its lambda and putting it
/// back under the lambda's result. It used to keep that destination through the bracket, meet the
/// recorded frame inside it with a cleared stack, and find nothing under the element at the
/// closing `add`: "suspension inside a lambda that was not spliced".
const SUSPENSION_BEFORE_A_BRACKETED_LAMBDA: &str = r#"
import kotlin.coroutines.*

val pending = ArrayList<() -> Unit>()

class Source {
    suspend fun fetch(item: String): List<Int> = suspendCoroutine { continuation ->
        pending.add {
            if (item == "bad") continuation.resumeWithException(IllegalStateException(item))
            else continuation.resume(listOf(item.length, item.length * 2))
        }
    }
}

class Out(val values: List<Int>) {
    override fun toString() = "Out($values)"
}

suspend fun fetchAll(source: Source, items: List<String>): List<Out> =
    items.mapNotNull { item ->
        try {
            val values = source.fetch(item)
            Out(values.map { it + 1 })
        } catch (e: Exception) {
            Out(emptyList())
        }
    }

fun box(): String {
    var result: Any? = null
    suspend { fetchAll(Source(), listOf("a", "bad", "ccc")) }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    while (pending.isNotEmpty()) pending.removeAt(0)()
    return if (result.toString() == "[Out([2, 3]), Out([]), Out([4, 7])]") "OK" else "fail: $result"
}
"#;

#[test]
fn a_suspension_before_a_bracketed_inlined_lambda_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        SUSPENSION_BEFORE_A_BRACKETED_LAMBDA,
        "SuspensionBeforeBracketedLambda",
    );
}

/// The same shape over repository-owned inline functions compiled by kotlinc: `collect` keeps its
/// destination on the stack under the transform's `invoke`, so its lambda is bracketed, and
/// `eachNotNull` invokes a lambda that suspends before reaching it.
const COLLECTING_LIB: &str = r#"
inline fun <T, R> Iterable<T>.collect(transform: (T) -> R): List<R> {
    val destination = ArrayList<R>()
    for (element in this) destination.add(transform(element))
    return destination
}

inline fun <T, R : Any> Iterable<T>.eachNotNull(transform: (T) -> R?): List<R> {
    val destination = ArrayList<R>()
    for (element in this) {
        val mapped = transform(element)
        if (mapped != null) destination.add(mapped)
    }
    return destination
}
"#;

const SUSPENSION_BEFORE_A_BRACKETED_LIBRARY_LAMBDA: &str = r#"
import kotlin.coroutines.*

val pending = ArrayList<() -> Unit>()

suspend fun fetch(item: String): List<Int> = suspendCoroutine { continuation ->
    pending.add {
        if (item == "bad") continuation.resumeWithException(IllegalStateException(item))
        else continuation.resume(listOf(item.length, item.length * 2))
    }
}

class Out(val values: List<Int>) {
    override fun toString() = "Out($values)"
}

suspend fun fetchAll(items: List<String>): List<Out> =
    items.eachNotNull { item ->
        try {
            val values = fetch(item)
            Out(values.collect { it + 1 })
        } catch (e: Exception) {
            Out(emptyList())
        }
    }

fun box(): String {
    var result: Any? = null
    suspend { fetchAll(listOf("a", "bad", "ccc")) }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    while (pending.isNotEmpty()) pending.removeAt(0)()
    return if (result.toString() == "[Out([2, 3]), Out([]), Out([4, 7])]") "OK" else "fail: $result"
}
"#;

#[test]
fn a_suspension_before_a_bracketed_library_lambda_runs() {
    let library = common::kotlinc_library(COLLECTING_LIB).expect("reference compiler unavailable");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            SUSPENSION_BEFORE_A_BRACKETED_LIBRARY_LAMBDA,
            "SuspensionBeforeBracketedLibraryLambda",
            &[library, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}
