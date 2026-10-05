//! `suspendCoroutine` is inlined from the stdlib like any other `@InlineOnly` suspend function, as
//! kotlinc does: its body's suspension markers and `SafeContinuation` protocol land in the caller's
//! state machine, the inlined body reads the caller's continuation local, and the locals live
//! across the inline call are spilled around its suspension. The transformed method's code and
//! debug lines match kotlinc's, and the coroutine suspends and resumes the same way.

use super::common;

const SOURCE: &str = r#"import kotlin.coroutines.*

var pending: Any? = null
var trace = ""

fun join(head: Any?, value: Any?, tail: Any?): String = "$head$value$tail"

suspend fun head(): String = suspendCoroutine { continuation ->
    trace += "head;"
    continuation.resume("O")
}

suspend fun stored(tail: String): String {
    val first = head()
    val second = suspendCoroutine<Any?> { continuation ->
        trace += "suspended;"
        pending = continuation
    }
    return join(first, second, tail)
}

class Completion : Continuation<String> {
    override val context: CoroutineContext
        get() = EmptyCoroutineContext

    override fun resumeWith(result: Result<String>) {
        trace += result.getOrThrow()
    }
}

fun box(): String {
    val body: suspend () -> String = { stored("") }
    body.startCoroutine(Completion())
    if (trace != "head;suspended;") return trace
    @Suppress("UNCHECKED_CAST")
    (pending as Continuation<Any?>).resume("K")
    return if (trace == "head;suspended;OK") "OK" else trace
}
"#;

#[test]
fn suspend_coroutine_is_inlined_into_the_callers_state_machine_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendCoroutineInline",
        SOURCE,
        &[
            "SuspendCoroutineInlineKt",
            "SuspendCoroutineInlineKt$stored$1",
        ],
    );
}

#[test]
fn suspend_coroutine_suspends_and_resumes_like_kotlinc() {
    common::expect_box_same_as_kotlinc(SOURCE, "SuspendCoroutineInline");
}

/// The resumed result narrowed to the local's type. kotlinc's `visitVariable` marks the
/// initializer's line before it materializes the inline call's erased result, and the inlined body
/// left the caller's line forgotten, so the `checkcast` sits on the call's line (behind the `nop`
/// the resumption keeps for the transformer's own entry on that line).
const TYPED_SOURCE: &str = r#"import kotlin.coroutines.*

var pending: Any? = null

fun take(value: Any?): String = "K"

suspend fun typed(tail: String): String {
    val second = suspendCoroutine<String> { continuation ->
        pending = continuation
    }
    return take(second) + tail
}
"#;

#[test]
fn a_resumed_inline_result_is_narrowed_on_the_calls_line_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendCoroutineTyped",
        TYPED_SOURCE,
        &["SuspendCoroutineTypedKt", "SuspendCoroutineTypedKt$typed$1"],
    );
}

/// A repository-owned inline suspend function on the classpath, whose erased `T` result the caller
/// narrows after resuming. The inlined body leaves the caller's line forgotten whatever the callee,
/// so the cast is on the call's line here too.
const PARKING: &str = r#"package parking

import kotlin.coroutines.*

var parked: Any? = null

suspend inline fun <T> park(): T = suspendCoroutine { continuation -> parked = continuation }
"#;

const PARKED_SOURCE: &str = r#"import parking.*

fun take(value: Any?): String = "K"

suspend fun parkedResult(tail: String): String {
    val second = park<String>()
    return take(second) + tail
}
"#;

#[test]
fn a_classpath_inline_suspend_result_is_narrowed_on_the_calls_line_like_kotlinc() {
    let library =
        common::kotlinc_library(PARKING).expect("reference compiler builds the dependency");
    common::assert_classes_identical_to_kotlinc_against(
        "SuspendInlineParked",
        PARKED_SOURCE,
        &[
            "SuspendInlineParkedKt",
            "SuspendInlineParkedKt$parkedResult$1",
        ],
        &[library],
    );
}

/// The same callee's result read where a call consumes it rather than by a declaration: kotlinc's
/// `visitVariable` does not run, so the cast takes no line of its own and the line after the
/// inlined body is the consuming call's.
const ARGUMENT_SOURCE: &str = r#"import parking.*

fun take(value: String): String = value

suspend fun argument(): String = take(park<String>())
"#;

#[test]
fn a_classpath_inline_suspend_argument_keeps_the_consumers_line_like_kotlinc() {
    let library =
        common::kotlinc_library(PARKING).expect("reference compiler builds the dependency");
    common::assert_classes_identical_to_kotlinc_against(
        "SuspendInlineArgument",
        ARGUMENT_SOURCE,
        &[
            "SuspendInlineArgumentKt",
            "SuspendInlineArgumentKt$argument$1",
        ],
        &[library],
    );
}

/// A library whose callable suspend inline function has lost kotlinc's `$$forInline` copy offers
/// no body to splice: its `park` is the callee's own state machine, which spliced into the caller
/// would leave it nothing to suspend at. The call stays a real one, and the coroutine still
/// suspends in the callee and resumes.
const UNCOPIED_SOURCE: &str = r#"import parking.*
import kotlin.coroutines.*

suspend fun parkedResult(): String = park<String>()

class Completion : Continuation<String> {
    override val context: CoroutineContext
        get() = EmptyCoroutineContext

    override fun resumeWith(result: Result<String>) {
        Main.result = result.getOrThrow()
    }
}

object Main {
    var result = ""
}

fun box(): String {
    val body: suspend () -> String = { parkedResult() }
    body.startCoroutine(Completion())
    @Suppress("UNCHECKED_CAST")
    (parked as Continuation<String>).resume("OK")
    return Main.result
}
"#;

#[test]
fn a_suspend_inline_function_without_its_for_inline_copy_is_called_not_spliced() {
    let library = uncopied_parking_library();
    let stdlib = common::stdlib_jar();
    let classes = common::compile_in_process_metadata_cp(
        UNCOPIED_SOURCE,
        "Main",
        &[library.clone(), stdlib.clone()],
    )
    .expect("krusty compiles the caller");
    let root = common::scratch_dir().expect("a scratch directory for the disassembly");
    let dir = root.join("uncopied");
    for (internal, bytes) in &classes {
        let path = dir.join(format!("{internal}.class"));
        std::fs::create_dir_all(path.parent().expect("a class file has a parent directory"))
            .expect("create the class output directory");
        std::fs::write(path, bytes).expect("write the emitted class");
    }
    let disassembly = common::javap(&["-p", "-c", "-cp", &dir.to_string_lossy(), "MainKt"])
        .expect("javap must be available to this regression");
    assert!(
        disassembly.contains("Method parking/LibKt.park:(Lkotlin/coroutines/Continuation;)"),
        "expected a real call to the callee's own state machine:\n{disassembly}"
    );
    let output = common::run_box(&classes, "MainKt", &[library, stdlib])
        .expect("a JVM must be available to this regression");
    assert_eq!(output, "OK");
}

/// [`PARKING`] built by kotlinc, with `park$$forInline` renamed so no copy is declared. The name
/// is a same-length constant-pool string that nothing references, so the class stays valid.
fn uncopied_parking_library() -> std::path::PathBuf {
    let built = common::kotlinc_library(PARKING).expect("reference compiler builds the dependency");
    let root = common::scratch_dir().expect("a scratch directory for the library");
    let class = root.join("uncopied-library/parking/LibKt.class");
    std::fs::create_dir_all(class.parent().expect("a class file has a parent directory"))
        .expect("create the library directory");
    let mut bytes =
        std::fs::read(built.join("parking/LibKt.class")).expect("read the library facade");
    let (copy, renamed) = (b"park$$forInline".as_slice(), b"park$$xorInline".as_slice());
    let at = bytes
        .windows(copy.len())
        .position(|window| window == copy)
        .expect("kotlinc declares the $$forInline copy");
    bytes[at..at + copy.len()].copy_from_slice(renamed);
    assert!(
        !bytes.windows(copy.len()).any(|window| window == copy),
        "one name names the copy"
    );
    std::fs::write(&class, bytes).expect("write the library facade");
    root.join("uncopied-library")
}
