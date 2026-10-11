//! Every suspend lambda compiles to a `SuspendLambda` class whose `invokeSuspend` is kotlinc's
//! state machine: one with no suspension point in it, one nested in another suspend lambda, one
//! whose value an earlier pass rebuilt (leaving its first node behind), and one that reads its
//! own `coroutineContext`.

use super::common;

const BUILDER: &str = "import kotlin.coroutines.*\n\
object Done : Continuation<Unit> {\n\
    override val context: CoroutineContext = EmptyCoroutineContext\n\
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Done)\n\
}\n";

fn expect_invoke_suspend_matches(name: &str, src: &str, class: &str) {
    match common::class_bytes_diff_against_kotlinc(
        name,
        &[],
        src,
        class,
        "public final java.lang.Object invokeSuspend(",
    ) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

fn no_suspension_point() -> String {
    format!(
        "{BUILDER}var res = \"\"\n\
fun box(): String {{\n\
    builder {{\n\
        res = \"OK\"\n\
    }}\n\
    return res\n\
}}\n"
    )
}

fn nested() -> String {
    format!(
        "{BUILDER}suspend fun call(c: suspend Long.() -> String): String {{\n\
    return 1000L.c()\n\
}}\n\
fun box(): String {{\n\
    var res = \"\"\n\
    builder {{\n\
        res = call {{ ->\n\
            \"OK$this\"\n\
        }}\n\
    }}\n\
    if (res != \"OK1000\") return res\n\
    return \"OK\"\n\
}}\n"
    )
}

#[test]
fn a_suspend_lambda_with_no_suspension_point_matches_kotlinc() {
    expect_invoke_suspend_matches("NoPoint", &no_suspension_point(), "NoPointKt$box$1");
}

#[test]
fn a_suspend_lambda_with_no_suspension_point_runs() {
    common::expect_box_ok_with_stdlib(&no_suspension_point(), "NoPointRun");
}

#[test]
fn a_suspend_lambda_nested_in_another_matches_kotlinc() {
    expect_invoke_suspend_matches("Nested", &nested(), "NestedKt$box$1$1");
}

#[test]
fn a_suspend_lambda_nested_in_another_runs() {
    common::expect_box_ok_with_stdlib(&nested(), "NestedRun");
}

// The lambda is its own continuation, so `coroutineContext` reads its own context.
const CONTEXT: &str = "import kotlin.coroutines.*\n\
fun builder(c: suspend () -> String) {}\n\
suspend fun pause() {}\n\
fun use() {\n\
    builder { coroutineContext.toString() }\n\
    builder { pause(); coroutineContext.toString() }\n\
}\n";

#[test]
fn a_suspend_lambda_reading_its_context_matches_kotlinc() {
    expect_invoke_suspend_matches("LambdaContext", CONTEXT, "LambdaContextKt$use$1");
}

#[test]
fn a_suspending_lambda_reading_its_context_matches_kotlinc() {
    expect_invoke_suspend_matches("LambdaContext", CONTEXT, "LambdaContextKt$use$2");
}

// A `Unit` receiver is passed as the `Unit` object, never as a `void` parameter.
const UNIT_RECEIVER: &str = "import kotlin.coroutines.*\n\
fun builder(block: suspend Unit.() -> Unit) {}\n\
var y = \"\"\n\
fun use() {\n\
    builder { y = \"OK\" }\n\
}\n";

#[test]
fn a_suspend_lambda_with_a_unit_receiver_matches_kotlinc() {
    match common::class_bytes_diff_against_kotlinc(
        "UnitReceiver",
        &[],
        UNIT_RECEIVER,
        "UnitReceiverKt$use$1",
        "public final java.lang.Object invoke(kotlin.Unit,",
    ) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

// A lambda too wide for a numbered function type is no class of its own, but it is named where
// it nests, beside its enclosing lambda's class, and never takes a sibling lambda's name.
const WIDE: &str = "import kotlin.coroutines.*\n\
object Done : Continuation<Unit> {\n\
    override val context: CoroutineContext = EmptyCoroutineContext\n\
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Done)\n\
}\n\
suspend fun wide(c: suspend (Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int, Int,\n\
    Int, Int, Int, Int, Int, Int, Int, Int, String) -> String): String =\n\
    c(1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, \"O\")\n\
suspend fun narrow(c: suspend (String) -> String): String = c(\"K\")\n\
fun box(): String {\n\
    var res = \"\"\n\
    builder {\n\
        res += wide { _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, _, s -> s }\n\
    }\n\
    builder {\n\
        res += narrow { s -> s }\n\
    }\n\
    return if (res == \"OK\") \"OK\" else res\n\
}\n";

#[test]
fn a_wide_lambda_and_its_siblings_get_kotlincs_class_names() {
    let dir = common::scratch_dir().expect("scratch directory");
    let source = dir.join("Wide.kt");
    std::fs::write(&source, WIDE).unwrap();
    let args = [
        "-d".to_string(),
        dir.join("ref").to_string_lossy().into_owned(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted_by_kotlinc = |class: &str| dir.join("ref").join(format!("{class}.class")).exists();
    let classes = common::compile_in_process_metadata_cp(WIDE, "Wide", &[common::stdlib_jar()])
        .expect("krusty compiles");
    // The first `builder` lambda still nests a lifted function, so it is not yet a class of its
    // own; the wide lambda inside it and the second lambda with its nested one are.
    for class in ["WideKt$box$1$1", "WideKt$box$2", "WideKt$box$2$1"] {
        assert!(emitted_by_kotlinc(class), "kotlinc emits {class}");
        assert_eq!(
            classes.iter().filter(|(name, _)| name == class).count(),
            1,
            "krusty emits {class} once"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wide_lambda_beside_a_suspend_lambda_class_runs() {
    common::expect_box_ok_with_stdlib(WIDE, "WideRun");
}
