//! A suspend lambda passed to a classpath inline function's `crossinline` parameter, which the
//! function's anonymous object invokes from a suspend method, is inlined into the object kotlinc
//! regenerates at the call. The copy's method is the method's `$$forInline` body with the lambda
//! in place of its `invoke`, run through the coroutine transformer: a copy whose suspensions are
//! all tail calls needs no continuation, and any other regenerates the original's continuation as
//! the copy's next object, with the machine's spill fields and `@DebugMetadata`.
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
