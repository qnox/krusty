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

/// The flow fixture's file annotation and imports. A program that adds an import puts it after
/// these and before [`flows_declarations`].
macro_rules! flows_header {
    () => {
        r#"
@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
import kotlinx.coroutines.flow.*
"#
    };
}

/// The flow fixture's declarations: a literal and a capturing `flatMapLatest` lambda.
macro_rules! flows_declarations {
    () => {
        r#"
fun lookup(ids: Set<String>): Flow<List<String>> = flowOf(ids.sorted())

fun byLiteral(f: Flow<Set<String>>): Flow<List<String>> = f.flatMapLatest { lookup(it) }

fun capturing(f: Flow<Set<String>>, prefix: String): Flow<String> =
    f.flatMapLatest { ids -> flowOf(prefix + ids.size) }
"#
    };
}

const FLOWS: &str = concat!(flows_header!(), flows_declarations!());

/// [`FLOWS`] with `runBlocking` imported beside its own imports and a `box()` that collects both
/// flows.
const FLOWS_PROGRAM: &str = concat!(
    flows_header!(),
    "import kotlinx.coroutines.runBlocking\n",
    flows_declarations!(),
    r#"
fun box(): String = runBlocking {
    val ids = flowOf(setOf("b", "a"))
    val literal = byLiteral(ids).toList()
    if (literal != listOf(listOf("a", "b"))) return@runBlocking "FAIL literal: $literal"
    val captured = capturing(flowOf(setOf("x")), "p").toList()
    if (captured != listOf("p1")) return@runBlocking "FAIL capturing: $captured"
    "OK"
}
"#
);

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
    let jdk = common::jdk_modules();
    assert_eq!(
        common::kotlinc_box_result_with_classpath(FLOWS_PROGRAM, &[common::coroutines_jar()]),
        "OK",
        "kotlinc's MainKt"
    );
    let output = common::compile_and_run_box(
        FLOWS_PROGRAM,
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

/// krusty writes kotlinc's `invokeSuspend$$forInline` beside the state machine of a suspend lambda
/// it compiles that reads a `crossinline` capture (kotlinc's `isSuspendCapturingCrossinline`): the
/// lambda class is kotlinc's, byte for byte, the copy included.
#[test]
fn a_suspend_lambda_reading_a_crossinline_capture_writes_kotlincs_for_inline_copy() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", LIB)]);
    let inventory = ["lib/LibKt", "lib/LibKt$mapped$1"];
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), inventory);
    assert_eq!(classes.krusty.keys().collect::<Vec<_>>(), inventory);
    let lambda = "lib/LibKt$mapped$1";
    let (reference, krusty) = classes
        .method_declarations(lambda)
        .expect("both compilers write the lambda");
    assert_eq!(krusty, reference);
    let copies = reference
        .iter()
        .filter(|declaration| declaration.contains(" invokeSuspend$$forInline("))
        .count();
    assert_eq!(copies, 1, "kotlinc writes one copy");
    let (reference, krusty) = classes.class_listing(lambda);
    assert_eq!(krusty, reference);
    assert!(
        classes.reference[lambda] == classes.krusty[lambda],
        "the listings agree, the bytes do not"
    );
}

/// A caller regenerates the lambda of a library krusty compiled from its `invokeSuspend$$forInline`,
/// and the copy suspends and resumes through its own machine.
#[test]
fn a_krusty_compiled_suspend_lambda_copy_is_regenerated_and_resumes() {
    let output = common::expect_box_run_against("suspend_lambda_for_inline", LIB, RUN_MAIN)
        .expect("the library is compiled");
    assert_eq!(output, "OK");
}
