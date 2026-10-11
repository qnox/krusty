//! A suspension nested inside the value `try` that is the whole body of a lambda spliced into a
//! CLASSPATH inline function whose parameter result is wider than the body:
//! `items.mapNotNull { item -> try { Out(source.fetch(item)) } catch (e: Exception) { Out(0) } }`.
//! `mapNotNull` takes `(T) -> R?`, so lowering wraps the lambda's WHOLE body block in the
//! `Out -> Out?` result coercion, `coerce(Block { try { … } })`. The value-`try` desugar for a
//! spliced body recognized the coercion only directly around the `try`, so this `try` stayed a value
//! and the emit-time coroutine machine declined the function. It was then emitted with no
//! continuation to pass to `fetch`: "JVM backend inline error: call arity mismatch".
//!
//! Binding the call first (`val v = source.fetch(item); Out(v)`) or a lambda result the body already
//! matches (`map`) never had the wrapper. Each callee parks its continuation and is resumed from
//! `box`, so the suspension really suspends; one item resumes with an exception, which the `catch`
//! arm has to receive after the resume.
use super::common;

const DRIVER: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

class Parked(val continuation: Continuation<Int>, val item: String)

var parked: Parked? = null

suspend fun park(tag: String): Int =
    suspendCoroutineUninterceptedOrReturn { c -> parked = Parked(c, tag); COROUTINE_SUSPENDED }

class Source {
    suspend fun fetch(item: String): Int = park(item)
}

class Flow(val value: Int) {
    suspend fun collect(f: (Int) -> Int): Out = Out(f(park("collect")))
}

class Flows(private val source: Source) {
    suspend fun fetch(item: String): Flow = Flow(source.fetch(item))
}

class Out(val n: Int) {
    override fun toString(): String = "Out($n)"
}

fun <T> drive(block: suspend () -> T): String {
    var outcome: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { outcome = it })
    while (true) {
        val next = parked ?: break
        parked = null
        when (next.item) {
            "bad" -> next.continuation.resumeWithException(IllegalStateException("bad"))
            "collect" -> next.continuation.resume(5)
            else -> next.continuation.resume(next.item.length * 10)
        }
    }
    return outcome!!.getOrThrow().toString()
}
"#;

#[test]
fn suspension_inside_a_coerced_spliced_value_try_matches_kotlinc() {
    let main = format!(
        "{DRIVER}{}",
        r#"
suspend fun constructed(source: Source, items: List<String>): List<Out> =
    items.mapNotNull { item -> try { Out(source.fetch(item)) } catch (e: Exception) { Out(0) } }

suspend fun received(flows: Flows, items: List<String>): List<Out> =
    items.mapNotNull { item -> try { flows.fetch(item).collect { it + 1 } } catch (e: Exception) { Out(-1) } }

suspend fun boxed(source: Source, items: List<String>): List<Int> =
    items.mapNotNull { item -> try { source.fetch(item) + 1 } catch (e: Exception) { 0 } }

suspend fun first(source: Source, items: List<String>): Out? =
    items.firstNotNullOfOrNull { item -> try { Out(source.fetch(item)) } catch (e: Exception) { null } }

fun box(): String {
    val items = listOf("a", "bad", "ccc")
    val a = drive { constructed(Source(), items) }
    if (a != "[Out(10), Out(0), Out(30)]") return "constructed $a"
    val b = drive { received(Flows(Source()), items) }
    if (b != "[Out(6), Out(-1), Out(6)]") return "received $b"
    val c = drive { boxed(Source(), items) }
    if (c != "[11, 0, 31]") return "boxed $c"
    val d = drive { first(Source(), listOf("bad", "ab")) }
    if (d != "Out(20)") return "first $d"
    return "OK"
}
"#
    );
    common::expect_box_same_as_kotlinc(&main, "Main");
}

/// The same shape through a repository-owned inline function compiled into a separate library, so
/// the lambda is spliced into classpath bytecode exactly as it is into `mapNotNull`.
#[test]
fn suspension_inside_a_coerced_value_try_spliced_into_a_library_inline_matches_kotlinc() {
    const LIB: &str = r#"package lib

inline fun <T, R : Any> List<T>.keepNotNull(f: (T) -> R?): List<R> {
    val out = ArrayList<R>()
    for (x in this) f(x)?.let { out.add(it) }
    return out
}
"#;
    let main = format!(
        "import lib.*\n{DRIVER}{}",
        r#"
suspend fun constructed(source: Source, items: List<String>): List<Out> =
    items.keepNotNull { item -> try { Out(source.fetch(item)) } catch (e: Exception) { Out(0) } }

fun box(): String {
    val a = drive { constructed(Source(), listOf("a", "bad", "ccc")) }
    return if (a == "[Out(10), Out(0), Out(30)]") "OK" else "constructed $a"
}
"#
    );
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let lib = common::compile_lib("spliced_coerced_value_try", LIB)
        .expect("compile the repository-owned inline library");
    let reference =
        common::kotlinc_box_result_with_classpath(&main, &[lib.clone(), stdlib.clone()]);
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    let output = common::compile_and_run_box(
        &main,
        "Main",
        &[lib, stdlib, jdk.clone()],
        Some(jdk.as_path()),
    )
    .expect("krusty compiles and runs the library-inline fixture");
    assert_eq!(output, reference);
}
