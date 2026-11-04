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
//! A copy is regenerated only from the method's `$$forInline` copy, which kotlinc and krusty both
//! write for a method that reads a `crossinline` capture.

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

const COPIED_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

inline fun <T> source(crossinline produce: suspend () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = produce()
    }

inline fun <T> plain(crossinline produce: () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = produce()
    }

inline fun <T> second(crossinline produce: suspend () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T {
            produce()
            return produce()
        }
    }

fun <T> eager(produce: suspend () -> T): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = produce()
    }
"#;

/// krusty writes kotlinc's `$$forInline` copy of an object's suspend method that reads a
/// `crossinline` capture, suspend or not: the method's marked body, public like the method, opened
/// by constructing its continuation between `InlineMarker.mark(4)` and `mark(5)`. That
/// continuation is public, part of the public ABI, and written even when the method's own body is
/// all tail calls. A method of an object outside an inline function has no copy.
#[test]
fn a_method_reading_a_crossinline_capture_writes_kotlincs_for_inline_copy() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", COPIED_LIB)]);
    let inventory = [
        "lib/LibKt",
        "lib/LibKt$eager$1",
        "lib/LibKt$plain$1",
        "lib/LibKt$plain$1$next$1",
        "lib/LibKt$second$1",
        "lib/LibKt$second$1$next$1",
        "lib/LibKt$source$1",
        "lib/LibKt$source$1$next$1",
        "lib/Source",
    ];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    let code = classes.code_differences();
    assert!(code.is_empty(), "{}", code.join("\n\n"));
    for continuation in [
        "lib/LibKt$plain$1$next$1",
        "lib/LibKt$second$1$next$1",
        "lib/LibKt$source$1$next$1",
    ] {
        let (reference, krusty) = classes.class_listing(continuation);
        assert_eq!(krusty, reference, "{continuation}");
        assert!(
            classes.reference[continuation] == classes.krusty[continuation],
            "{continuation}: the listings agree, the bytes do not"
        );
    }
}

const COPIED_RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

class Later : Source<Int> {
    override suspend fun next(): Int = suspendCoroutine { pending = it }
}

fun plainPlusOne(n: Int) = plain { n + 1 }

fun secondPlusOne(s: Source<Int>) = second { s.next() + 1 }

fun box(): String {
    var result: Any? = null
    suspend { plainPlusOne(41).next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != 42) return "FAIL plain: $result"
    result = null
    suspend { secondPlusOne(Later()).next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    pending!!.resume(1)
    if (result != null) return "FAIL completed before the second resume: $result"
    pending!!.resume(41)
    if (result != 42) return "FAIL second: $result"
    return "OK"
}
"#;

/// A caller inlines the copies of a library krusty compiled, and the regenerated objects suspend
/// and resume through their continuations.
#[test]
fn a_krusty_compiled_for_inline_copy_is_inlined_and_resumes() {
    let output = common::expect_box_run_against("copied_for_inline", COPIED_LIB, COPIED_RUN_MAIN)
        .expect("the library is compiled");
    assert_eq!(output, "OK");
}

const MOVED_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

inline fun <T> deferred(crossinline produce: suspend () -> T): () -> Source<T> = {
    val source: Source<T> = object : Source<T> {
        override suspend fun next(): T = produce()
    }
    source
}

inline fun <T> nested(crossinline produce: suspend () -> T): Source<Source<T>> =
    object : Source<Source<T>> {
        override suspend fun next(): Source<T> =
            object : Source<T> {
                override suspend fun next(): T = produce()
            }
    }
"#;

/// The control for the copy's eligibility: the object's constructor operand for the `crossinline`
/// capture is not the parameter itself but a lambda's capture of it, or the enclosing object's
/// field. The capture still holds the `crossinline` parameter, so krusty writes kotlinc's copy.
/// The lambda returns the object through a typed local: a lambda returning the object directly
/// takes the object's type for `invoke`, a separate divergence.
#[test]
fn a_crossinline_capture_moved_through_a_lambda_or_an_object_keeps_the_for_inline_copy() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", MOVED_LIB)]);
    let inventory = [
        "lib/LibKt",
        "lib/LibKt$deferred$1",
        "lib/LibKt$deferred$1$source$1",
        "lib/LibKt$deferred$1$source$1$next$1",
        "lib/LibKt$nested$1",
        "lib/LibKt$nested$1$next$1",
        "lib/LibKt$nested$1$next$2",
        "lib/LibKt$nested$1$next$2$next$1",
        "lib/Source",
    ];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    for class in [
        "lib/LibKt$deferred$1$source$1",
        "lib/LibKt$nested$1",
        "lib/LibKt$nested$1$next$2",
    ] {
        let (reference, krusty) = classes
            .method_declarations(class)
            .expect("both compilers write the class");
        assert_eq!(krusty, reference, "{class}");
        let copies = reference
            .iter()
            .filter(|declaration| declaration.contains(" next$$forInline("))
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 1, "{class}: kotlinc writes one copy");
        let (reference, krusty) = classes.method_listing(class, copies[0]);
        assert_eq!(krusty, reference, "{class}");
    }
    let code = classes.code_differences();
    assert!(code.is_empty(), "{}", code.join("\n\n"));
}

const MOVED_RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

fun deferredPlusOne(n: Int) = deferred { n + 1 }

fun nestedPlusOne(n: Int) = nested { n + 1 }

fun box(): String {
    var result: Any? = null
    suspend { deferredPlusOne(41)().next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != 42) return "FAIL deferred: $result"
    result = null
    suspend { nestedPlusOne(41).next().next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != 42) return "FAIL nested: $result"
    return "OK"
}
"#;

/// A caller inlines the copies krusty wrote for the moved captures, and the regenerated objects
/// run. The lambda's `invoke` only passes the suspend lambda on to the object it constructs, so it
/// needs no `$$forInline` copy of its own: the object it regenerates takes the lambda.
#[test]
fn a_krusty_compiled_copy_of_a_moved_crossinline_capture_runs() {
    let output = common::expect_box_run_against("moved_for_inline", MOVED_LIB, MOVED_RUN_MAIN)
        .expect("the library is compiled");
    assert_eq!(output, "OK");
}

const MOVED_CALLER: &str = r#"
import lib.*

fun deferredPlusOne(n: Int) = deferred { n + 1 }

fun nestedPlusOne(n: Int) = nested { n + 1 }
"#;

/// The caller of the moved captures, against kotlinc's library: krusty regenerates the lambda and
/// the object it constructs, with the suspend lambda inlined into the object's copy, as kotlinc
/// does.
#[test]
fn a_caller_regenerates_a_lambda_that_passes_a_suspend_lambda_on_like_kotlinc() {
    let Some(classes) =
        common::classes_against_kotlinc_lib("moved_caller", &[("Lib.kt", MOVED_LIB)], MOVED_CALLER)
    else {
        return;
    };
    let inventory = [
        "Moved_callerKt",
        "Moved_callerKt$deferredPlusOne$$inlined$deferred$1",
        "Moved_callerKt$deferredPlusOne$$inlined$deferred$1$1",
        "Moved_callerKt$nestedPlusOne$$inlined$nested$1",
        "Moved_callerKt$nestedPlusOne$$inlined$nested$1$2",
    ];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    let code = classes.code_differences();
    assert!(code.is_empty(), "{}", code.join("\n\n"));
}

const FORWARDED_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

inline fun <reified T> factory(crossinline produce: suspend () -> Any?): Source<T> =
    object : Source<T> {
        override suspend fun next(): T {
            val value = produce()
            return value as T
        }
    }

inline fun <reified T> wrapper(crossinline produce: suspend () -> Any?): Source<T> =
    factory<T>(produce)
"#;

/// A reified inline wrapper forwards its own `crossinline` parameter to a reified inline factory
/// whose object captures it. The wrapper's expansion copies the object for its reified type, and
/// the copy's capture is bound to the wrapper's `crossinline` parameter, so the copy keeps the
/// `$$forInline` method and its continuation (`{copy}$1`), as kotlinc's regenerated object does.
/// The factory's object has kotlinc's methods and code (its metadata still lists the capture as a
/// property, a known divergence) and its continuation is kotlinc's to the byte. Of the copy, the
/// methods and the `$$forInline` body are kotlinc's. kotlinc writes a regenerated copy's generic
/// signatures differently, a divergence of every reified copy, so the copy's declarations are
/// compared by descriptor.
#[test]
fn a_forwarded_crossinline_parameter_keeps_the_copys_for_inline_method() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", FORWARDED_LIB)]);
    let inventory = [
        "lib/LibKt",
        "lib/LibKt$factory$1",
        "lib/LibKt$factory$1$next$1",
        "lib/LibKt$wrapper$$inlined$factory$1",
        "lib/LibKt$wrapper$$inlined$factory$1$1",
        "lib/Source",
    ];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    let declaration = "lib/LibKt$factory$1";
    let (reference, krusty) = classes
        .method_declarations(declaration)
        .expect("both compilers write the class");
    assert_eq!(krusty, reference, "{declaration}");
    let code = classes
        .code_differences()
        .into_iter()
        .filter(|difference| difference.starts_with("lib/LibKt$factory$1 "))
        .collect::<Vec<_>>();
    assert!(code.is_empty(), "{}", code.join("\n\n"));
    let continuation = "lib/LibKt$factory$1$next$1";
    let (reference, krusty) = classes.class_listing(continuation);
    assert_eq!(krusty, reference, "{continuation}");
    assert!(
        classes.reference[continuation] == classes.krusty[continuation],
        "{continuation}: the listings agree, the bytes do not"
    );
    let copy = "lib/LibKt$wrapper$$inlined$factory$1";
    let (reference, krusty) = classes.class_listing(copy);
    assert_eq!(
        method_descriptors(&krusty),
        method_descriptors(&reference),
        "{copy}"
    );
    let body = |listing: &str| {
        let lines = listing.lines().collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|line| line.contains(" next$$forInline("))
            .expect("the copy declares `next$$forInline`");
        lines[start + 1..]
            .iter()
            .take_while(|line| !line.is_empty() && **line != "}")
            .filter(|line| !line.starts_with("Signature:"))
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(body(&krusty), body(&reference), "{copy}: next$$forInline");
}

/// Each method's name and descriptor in a `javap -v` listing, in class-file order.
fn method_descriptors(listing: &str) -> Vec<(String, String)> {
    let lines = listing.lines().collect::<Vec<_>>();
    lines
        .windows(2)
        .filter_map(|pair| {
            let descriptor = pair[1].strip_prefix("descriptor: (")?;
            let name = pair[0].split('(').next()?.rsplit(' ').next()?;
            Some((name.to_string(), format!("({descriptor}")))
        })
        .collect()
}

const FORWARDED_RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Any?>? = null

fun later(): Source<String> = wrapper { suspendCoroutine<Any?> { pending = it } }

fun box(): String {
    var result: Any? = null
    suspend { later().next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before the resume: $result"
    pending!!.resume("OK")
    return result as String
}
"#;

/// The same module inlines the wrapper into a caller: the wrapper's copy binds the factory's
/// parameter to the wrapper's own, and the caller's copy binds it to a literal lambda, which is no
/// `crossinline` parameter. The caller's object suspends and resumes.
#[test]
fn a_same_module_caller_of_a_forwarded_crossinline_copy_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", FORWARDED_LIB), ("Main.kt", FORWARDED_RUN_MAIN)],
        "forwarded_for_inline",
    );
}

const RELAYED_LIB: &str = r#"
package lib

interface Source<out T> {
    suspend fun next(): T
}

inline fun <reified T> factory(crossinline produce: suspend () -> Any?): Source<T> =
    object : Source<T> {
        override suspend fun next(): T {
            val value = produce()
            return value as T
        }
    }

inline fun <reified T> relay(crossinline produce: suspend () -> Any?): Source<T> =
    object : Source<T> {
        override suspend fun next(): T = factory<T>(produce).next()
    }
"#;

/// An object's method forwards the outer `crossinline` parameter, which the object holds in a
/// capture field, to a reified inline factory. The expansion stages the field read in a temporary,
/// and the factory's copy still binds its capture to the outer parameter, so it keeps the
/// `$$forInline` method and its continuation, as kotlinc's regenerated object does. The copy's
/// declarations are compared by descriptor for the reason given above.
#[test]
fn a_crossinline_capture_an_object_forwards_keeps_the_copys_for_inline_method() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", RELAYED_LIB)]);
    let inventory = [
        "lib/LibKt",
        "lib/LibKt$factory$1",
        "lib/LibKt$factory$1$next$1",
        "lib/LibKt$relay$1",
        "lib/LibKt$relay$1$next$$inlined$factory$1",
        "lib/LibKt$relay$1$next$$inlined$factory$1$1",
        "lib/LibKt$relay$1$next$1",
        "lib/Source",
    ];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    let copy = "lib/LibKt$relay$1$next$$inlined$factory$1";
    let (reference, krusty) = classes.class_listing(copy);
    assert_eq!(
        method_descriptors(&krusty),
        method_descriptors(&reference),
        "{copy}"
    );
}

const RELAYED_RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Any?>? = null

fun later(): Source<String> = relay { suspendCoroutine<Any?> { pending = it } }

fun box(): String {
    var result: Any? = null
    suspend { later().next() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before the resume: $result"
    pending!!.resume("OK")
    return result as String
}
"#;

/// A same-module caller inlines the relaying object's function: its copy suspends in the factory's
/// object and resumes.
#[test]
fn a_same_module_caller_of_a_relayed_crossinline_copy_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", RELAYED_LIB), ("Main.kt", RELAYED_RUN_MAIN)],
        "relayed_for_inline",
    );
}
