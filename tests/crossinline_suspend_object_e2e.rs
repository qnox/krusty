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

const SAM_LIB: &str = r#"
package lib

import java.util.function.Supplier

interface Source<out T> {
    suspend fun next(): T
}

fun describe(message: Supplier<String>): String = message.get()

inline fun <T> labelled(text: String, crossinline produce: suspend () -> T): Source<String> =
    object : Source<String> {
        override suspend fun next(): String {
            val label: () -> String = { text }
            return produce().toString() + describe(label)
        }
    }
"#;

/// A regenerated suspend method that converts a function value to a Java interface constructs the
/// library's SAM wrapper. kotlinc copies the wrapper into the calling class, shared by its inline
/// calls and taking no number under the copy, while the lambda object beside it is regenerated as
/// the copy's next object; the copy's own `InnerClasses` row still names the original wrapper.
#[test]
fn a_sam_wrapper_in_a_regenerated_method_is_copied_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib(
        "Wrapped",
        &[("Lib.kt", SAM_LIB)],
        "import lib.*\n\nfun answer() = labelled(\"!\") { 42 }\n\nfun again() = labelled(\"?\") { 7 }\n",
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "WrappedKt",
            "WrappedKt$again$$inlined$labelled$1",
            "WrappedKt$again$$inlined$labelled$1$2",
            "WrappedKt$answer$$inlined$labelled$1",
            "WrappedKt$answer$$inlined$labelled$1$2",
            "WrappedKt$inlined$sam$i$java_util_function_Supplier$0",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// The copied wrapper runs: the regenerated method really suspends, resumes through its
/// continuation, and reads the label through the wrapper.
#[test]
fn a_sam_wrapper_copied_into_a_regenerated_method_runs() {
    const MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

fun answer() = labelled("!") { suspendCoroutine<Int> { pending = it } }

fun box(): String {
    var result: Any? = null
    suspend { answer().next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(42)
    return if (result == "42!") "OK" else "FAIL $result"
}
"#;
    let output = common::expect_box_run_against_ref("crossinline_suspend_sam", SAM_LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The same factory declared `suspend inline`: its `$$forInline` copy still only hands the lambda to
/// the object it constructs, so the lambda's suspensions are the object's method's, not the
/// caller's, and the call is no suspension of its own.
const SUSPEND_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

suspend inline fun <T> source(crossinline produce: suspend () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = produce()
    }
"#;

const SUSPEND_MAIN: &str = r#"
import lib.*

suspend fun answer() = source { 42 }

suspend fun plusOne(s: Source<Int>) = source { s.next() + 1 }

suspend fun ticks(s: Source<Unit>) = source {
    s.next()
    s.next()
}
"#;

/// A `suspend inline` factory's object is regenerated like a plain inline one's, byte for byte.
#[test]
fn a_suspend_inline_factory_regenerates_the_object_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib(
        "SuspendFactory",
        &[("Lib.kt", SUSPEND_LIB)],
        SUSPEND_MAIN,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "SuspendFactoryKt",
            "SuspendFactoryKt$answer$$inlined$source$1",
            "SuspendFactoryKt$plusOne$$inlined$source$1",
            "SuspendFactoryKt$plusOne$$inlined$source$1$1",
            "SuspendFactoryKt$ticks$$inlined$source$1",
            "SuspendFactoryKt$ticks$$inlined$source$1$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const SUSPEND_RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

class Later : Source<Int> {
    override suspend fun next(): Int = suspendCoroutine { pending = it }
}

suspend fun plusOne(s: Source<Int>) = source { s.next() + 1 }

fun box(): String {
    var result: Any? = null
    suspend { plusOne(Later()).next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(41)
    if (result != 42) return "FAIL resumed: $result"
    return "OK"
}
"#;

/// The object a `suspend inline` factory returns suspends after the factory has returned, and
/// resumes through its regenerated continuation.
#[test]
fn a_suspend_inline_factorys_object_resumes_through_its_continuation() {
    let output = common::expect_box_run_against_ref(
        "suspend_inline_factory_object",
        SUSPEND_LIB,
        SUSPEND_RUN_MAIN,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// A factory that both invokes its lambda and hands it to the object it returns places one body
/// in two frames. Placement has no answer for that yet, so the call is refused, naming the body.
const PRIMED_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

suspend inline fun <T> primed(crossinline produce: suspend () -> T): Source<T> {
    produce()
    return object : Source<T> {
        override suspend fun next(): T = produce()
    }
}
"#;

#[test]
fn a_lambda_both_invoked_and_retained_is_refused_with_its_placement() {
    let library =
        common::kotlinc_library(PRIMED_LIB).expect("reference compiler must build the fixture");
    let jdk = common::jdk_modules();
    let classpath = [library, common::stdlib_jar(), jdk.clone()];
    let outcome = common::backend_outcome_in_process(
        "import lib.*\n\nsuspend fun answer() = primed { 42 }\n",
        "Main",
        &classpath,
        Some(jdk.as_path()),
    )
    .expect("the call is frontend-valid");
    assert_eq!(
        outcome,
        common::BackendOutcome::Rejected(vec![
            "krusty: cannot tell where lib/LibKt.primed$$forInline\
             (Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)Ljava/lang/Object; \
             places its suspend lambdas: inline lambda 0 is both invoked and retained"
                .to_string()
        ])
    );
}

/// A `suspend inline` function whose body never uses its lambda runs the lambda nowhere: the
/// lambda's suspension is no state of the caller's machine, and the lambda is written nowhere.
const IGNORING_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

suspend inline fun <T> ignored(crossinline produce: suspend () -> T): Int = 7
"#;

const IGNORING_MAIN: &str = r#"
import lib.*

suspend fun count(s: Source<Int>) = ignored { s.next() + 1 }

suspend fun twice(s: Source<Int>): Int {
    val a = ignored { s.next() }
    s.next()
    return a
}
"#;

#[test]
fn an_unused_suspend_lambda_suspends_in_no_frame_like_kotlinc() {
    let classes =
        common::classes_against_kotlinc_lib("Ignoring", &[("Lib.kt", IGNORING_LIB)], IGNORING_MAIN)
            .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["IgnoringKt", "IgnoringKt$twice$1"]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
