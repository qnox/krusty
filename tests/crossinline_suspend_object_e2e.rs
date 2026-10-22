//! A suspend lambda passed to a classpath inline function's `crossinline` parameter, which the
//! function's anonymous object invokes from a suspend method, is inlined into the object kotlinc
//! regenerates at the call. The copy's method is the method's `$$forInline` body with the lambda
//! in place of its `invoke`, run through the coroutine transformer: a copy whose suspensions are
//! all tail calls needs no continuation, and any other regenerates the original's continuation as
//! the copy's next object, with the machine's spill fields and `@DebugMetadata`.
//!
//! A method that constructs another object reading the lambda regenerates that object too, numbered
//! under the copy (kotlinx `Flow.map`'s collector).
//!
//! The library is compiled by the reference compiler: krusty does not write the `$$forInline`
//! copy of an object's suspend method yet, and a copy is regenerated only from one.

use super::common;

const LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

inline fun <T> source(crossinline produce: suspend () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = produce()
    }
"#;

const MAIN: &str = r#"
import lib.*

fun answer() = source { 42 }

fun plusOne(s: Source<Int>) = source { s.next() + 1 }

fun greeting(name: String) = source { "hi " + name }

suspend fun deferred(s: Source<Int>) = source { s.next() + 1 }

fun ticks(s: Source<Unit>) = source {
    s.next()
    s.next()
}
"#;

/// The copies, the regenerated continuation, and the calling facade are kotlinc's, byte for byte.
#[test]
fn crossinline_suspend_lambdas_regenerate_the_object_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib("Suspending", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "SuspendingKt",
            "SuspendingKt$answer$$inlined$source$1",
            "SuspendingKt$deferred$$inlined$source$1",
            "SuspendingKt$deferred$$inlined$source$1$1",
            "SuspendingKt$greeting$$inlined$source$1",
            "SuspendingKt$plusOne$$inlined$source$1",
            "SuspendingKt$plusOne$$inlined$source$1$1",
            "SuspendingKt$ticks$$inlined$source$1",
            "SuspendingKt$ticks$$inlined$source$1$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

class Later : Source<Int> {
    override suspend fun next(): Int = suspendCoroutine { pending = it }
}

fun plusOne(s: Source<Int>) = source { s.next() + 1 }

suspend fun deferred(s: Source<Int>) = source { s.next() + 1 }

fun box(): String {
    var result: Any? = null
    suspend { plusOne(Later()).next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(41)
    if (result != 42) return "FAIL resumed: $result"
    result = null
    suspend { deferred(Later()).next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    pending!!.resume(1)
    if (result != 2) return "FAIL deferred: $result"
    return "OK"
}
"#;

/// The copy suspends and resumes through its regenerated continuation.
#[test]
fn a_regenerated_suspend_method_resumes_through_its_continuation() {
    let output = common::expect_box_run_against_ref("crossinline_suspend_object", LIB, RUN_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

const STREAM_LIB: &str = r#"
package lib

interface Collector<in T> {
    suspend fun emit(value: T)
}

interface Stream<out T> {
    suspend fun collect(collector: Collector<T>)
}

inline fun <T, R> Stream<T>.mapped(crossinline transform: suspend (T) -> R): Stream<R> =
    object : Stream<R> {
        override suspend fun collect(collector: Collector<R>) {
            this@mapped.collect(object : Collector<T> {
                override suspend fun emit(value: T) {
                    collector.emit(transform(value))
                }
            })
        }
    }
"#;

/// An object the copied method constructs, which reads the lambda through its captured field, is
/// regenerated as the copy's nested object with the lambda inlined into it, as kotlinc does.
#[test]
fn an_object_nested_in_a_regenerated_method_is_regenerated_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib(
        "Nested",
        &[("Lib.kt", STREAM_LIB)],
        "import lib.*\n\nfun <T> Stream<T>.wrap() = mapped { listOf(it) }\n",
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "NestedKt",
            "NestedKt$wrap$$inlined$mapped$1",
            "NestedKt$wrap$$inlined$mapped$1$2",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const FLOWS: &str = r#"
import kotlinx.coroutines.flow.*

fun <T> Flow<T>.wrap() = map { listOf(it) }

suspend fun delayOne(): Int = 1

fun Flow<Int>.bump(extra: Int) = map { it + delayOne() + extra }

fun Flow<String>.keep(prefix: String) = filter { it.startsWith(prefix) }
"#;

/// kotlinx `Flow.map` and `Flow.filter`: the operator's object, its nested collector, and both
/// regenerated continuations are kotlinc's, byte for byte. The facade forwards to its multifile
/// part, so the bodies are the part's.
#[test]
fn kotlinx_flow_operators_regenerate_their_objects_like_kotlinc() {
    let mut classes = vec!["FlowsKt".to_string()];
    for (function, operator) in [("wrap", "map"), ("bump", "map"), ("keep", "filter")] {
        for suffix in ["", "$1", "$2", "$2$1"] {
            classes.push(format!("FlowsKt${function}$$inlined${operator}$1{suffix}"));
        }
    }
    let classes = classes.iter().map(String::as_str).collect::<Vec<_>>();
    common::assert_classes_identical_to_kotlinc_against(
        "Flows",
        FLOWS,
        &classes,
        &[common::coroutines_jar()],
    );
}

/// The regenerated operators run: each element flows through the copied collector, including one
/// that suspends in the lambda.
#[test]
fn kotlinx_flow_operators_run_through_the_regenerated_objects() {
    let main = format!(
        "{FLOWS}\nimport kotlinx.coroutines.runBlocking\n\n\
fun box(): String = runBlocking {{\n\
    val bumped = flowOf(1, 2).bump(3).toList()\n\
    if (bumped != listOf(5, 6)) return@runBlocking \"FAIL bump: $bumped\"\n\
    val kept = flowOf(\"ab\", \"cd\", \"ax\").keep(\"a\").wrap().toList()\n\
    if (kept != listOf(listOf(\"ab\"), listOf(\"ax\"))) return@runBlocking \"FAIL keep: $kept\"\n\
    \"OK\"\n\
}}\n"
    );
    let jdk = common::jdk_modules();
    let output = common::compile_and_run_box(
        &main,
        "Main",
        &[common::stdlib_jar(), common::coroutines_jar(), jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("the operators compile and run");
    assert_eq!(output, "OK");
}
