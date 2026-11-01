//! A suspend lambda an inline function's body creates around one of its `crossinline` lambdas
//! (kotlinx `flatMapLatest`'s `transformLatest { emitAll(transform(it)) }`) is regenerated at the
//! call with the lambda inlined, as kotlinc's `CoroutineTransformer.newStateMachineForLambda` does:
//! the copy's `invokeSuspend` is the original's `invokeSuspend$$forInline` with the lambda in place
//! of its `invoke`, its state machine rebuilt with the lambda class as its own continuation. The
//! copy's `invoke` and `create`, which construct the object anew, construct the copy and pass it
//! the lambda's captured values in place of the lambda.
//!
//! A literal lambda takes this route; a suspend callable reference is not an inline lambda shape
//! yet (`IrFile::callable_reference_inline_template` declines it), and its copy is refused.

use super::common;

const FLOWS: &str = r#"
@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
import kotlinx.coroutines.flow.*

fun lookup(ids: Set<String>): Flow<List<String>> = flowOf(ids.sorted())

fun byLiteral(f: Flow<Set<String>>): Flow<List<String>> = f.flatMapLatest { lookup(it) }

fun capturing(f: Flow<Set<String>>, prefix: String): Flow<String> =
    f.flatMapLatest { ids -> flowOf(prefix + ids.size) }
"#;

/// The regenerated suspend lambdas and the calling facade are kotlinc's, byte for byte.
#[test]
fn flat_map_latest_regenerates_its_suspend_lambda_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against(
        "Flows",
        FLOWS,
        &[
            "FlowsKt",
            "FlowsKt$byLiteral$$inlined$flatMapLatest$1",
            "FlowsKt$capturing$$inlined$flatMapLatest$1",
        ],
        &[common::coroutines_jar()],
    );
}

/// The regenerated lambdas run, each element mapped through the inlined lambda.
#[test]
fn flat_map_latest_runs_through_the_regenerated_suspend_lambda() {
    let main = format!(
        "{FLOWS}\nimport kotlinx.coroutines.runBlocking\n\n\
fun box(): String = runBlocking {{\n\
    val ids = flowOf(setOf(\"b\", \"a\"))\n\
    val literal = byLiteral(ids).toList()\n\
    if (literal != listOf(listOf(\"a\", \"b\"))) return@runBlocking \"FAIL literal: $literal\"\n\
    val captured = capturing(flowOf(setOf(\"x\")), \"p\").toList()\n\
    if (captured != listOf(\"p1\")) return@runBlocking \"FAIL capturing: $captured\"\n\
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
    .expect("the calls compile and run");
    assert_eq!(output, "OK");
}

const LIB: &str = r#"
package lib

fun <T, R> deferred(block: suspend (T) -> R): suspend (T) -> R = block

inline fun <T, R> mapped(crossinline transform: suspend (T) -> R): suspend (T) -> R =
    deferred { value -> transform(value) }
"#;

const MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

suspend fun later(): Int = suspendCoroutine { pending = it }

fun plus(extra: Int): suspend (Int) -> Int = mapped { it + later() + extra }
"#;

const RUN_MAIN: &str = r#"
import lib.*
import kotlin.coroutines.*

var pending: Continuation<Int>? = null

suspend fun later(): Int = suspendCoroutine { pending = it }

fun plus(extra: Int): suspend (Int) -> Int = mapped { it + later() + extra }

fun box(): String {
    var result: Any? = null
    val block = plus(10)
    suspend { block(1) }.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    if (result != null) return "FAIL completed before resuming: $result"
    pending!!.resume(100)
    if (result != 111) return "FAIL resumed: $result"
    return "OK"
}
"#;

/// A suspend lambda of one parameter has a `create` as well as an `invoke`, both constructing the
/// copy; a lambda that suspends spills into the copy's own machine.
#[test]
fn a_suspending_lambda_regenerates_the_suspend_lambda_like_kotlinc() {
    let classes = common::classes_against_kotlinc_lib("Mapped", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["MappedKt", "MappedKt$plus$$inlined$mapped$1"]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// The copy suspends inside the inlined lambda and resumes through its own machine.
#[test]
fn a_regenerated_suspend_lambda_resumes_through_its_machine() {
    let output = common::expect_box_run_against_ref("suspend_lambda_regeneration", LIB, RUN_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
