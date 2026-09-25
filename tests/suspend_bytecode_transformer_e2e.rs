//! Top-level and final member suspend functions whose state machine kotlinc's coroutine
//! transformer builds from the finished method (`docs/JVM_INLINE_BEFORE_CPS.md` §1a, steps 2
//! and 3).
//!
//! The emitter marks each suspension point as kotlinc's codegen does and the transformer rewrites
//! the method when its class is written. The continuation class it describes (spill fields, then
//! `result` and `label`, and the `@DebugMetadata` arrays) is byte-identical to kotlinc's, and so
//! are the function's own instructions once kotlinc's optimizer has run over the transformed body.

use super::common;

const WORK: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
suspend fun work(count: Int, ref: String): Int {\n\
    leaf()\n\
    val v = value()\n\
    return v + count + ref.length\n\
}\n";

/// `class` compiled by both compilers, against the standard library. Without it krusty's runtime
/// capabilities do not include the spill clean-up the transformer needs, and the IR machine builds
/// the function instead.
fn transformed_class_diff(name: &str, src: &str, class: &str) -> Option<Result<(), String>> {
    common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()])
}

#[test]
fn a_continuation_spilling_an_int_and_a_reference_matches_kotlinc() {
    let Some(result) = transformed_class_diff("TransformerWork", WORK, "TransformerWorkKt$work$1")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn the_transformed_function_s_instructions_match_kotlinc() {
    // The optimizer runs after the transform, as kotlinc chains `OptimizationMethodVisitor` after
    // `CoroutineTransformerMethodVisitor`: it drops the transformer's leftover temporaries and
    // compacts the spill slots.
    match common::method_code_diff_against_kotlinc(
        "TransformerWorkCode",
        &[],
        WORK,
        "TransformerWorkCodeKt",
        "public static final java.lang.Object work(",
    ) {
        None => eprintln!("skipping: reference kotlinc unavailable"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

#[test]
fn a_unit_function_continuation_matches_kotlinc() {
    // The last suspension's next line is the closing `}`, where kotlinc maps the implicit return.
    let src = "suspend fun leaf() {}\n\
suspend fun work(count: Int, ref: String) {\n\
    leaf()\n\
    leaf()\n\
}\n";
    let Some(result) = transformed_class_diff("TransformerUnit", src, "TransformerUnitKt$work$1")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn a_backticked_hyphen_stays_in_the_continuation_source_name() {
    let src = "suspend fun leaf() {}\n\
suspend fun `work-item`(): Int {\n\
    leaf()\n\
    return 1\n\
}\n";
    let Some(result) = common::byte_diff_against_kotlinc(
        "TransformerBacktick",
        src,
        "TransformerBacktickKt$work-item$1",
    ) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("the continuation keeps the exact source identifier like kotlinc's");
}

#[test]
fn spilled_locals_survive_real_suspensions() {
    // `pause` really suspends; `work` resumes twice from a continuation parked outside it, so its
    // spilled `count` and `ref` are restored from the continuation's fields.
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Unit>? = null\n\
suspend fun pause() = suspendCoroutine<Unit> { parked = it }\n\
suspend fun work(count: Int, ref: String): String {\n\
    pause()\n\
    val first = ref + count\n\
    pause()\n\
    return first + ref.length\n\
}\n\
fun box(): String {\n\
    var r = \"none\"\n\
    suspend { r = work(3, \"ab\") }.startCoroutine(Done())\n\
    parked!!.resume(Unit)\n\
    parked!!.resume(Unit)\n\
    return if (r == \"ab32\") \"OK\" else \"F:$r\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerResume");
}

const MEMBERS: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
class Box(val base: Int) {\n\
    suspend fun work(count: Int, ref: String): Int {\n\
        leaf()\n\
        val v = value()\n\
        return v + count + ref.length + base\n\
    }\n\
}\n\
object Single {\n\
    suspend fun work(count: Int): Int {\n\
        leaf()\n\
        return count + value()\n\
    }\n\
}\n";

#[test]
fn a_final_member_s_continuation_captures_its_receiver_like_kotlinc() {
    // The continuation nests under the class and takes the receiver as `this$0`, which
    // `invokeSuspend` re-enters the member through.
    for class in ["Box$work$1", "Single$work$1"] {
        let Some(result) = transformed_class_diff("TransformerMembers", MEMBERS, class) else {
            eprintln!("skipping: reference kotlinc unavailable");
            return;
        };
        result.expect("the continuation class is byte-identical to kotlinc's");
    }
}

#[test]
fn a_final_member_s_instructions_match_kotlinc() {
    for (class, method) in [
        ("Box", "public final java.lang.Object work("),
        ("Single", "public final java.lang.Object work("),
    ] {
        match common::method_code_diff_against_kotlinc(
            "TransformerMemberCode",
            &[],
            MEMBERS,
            class,
            method,
        ) {
            None => eprintln!("skipping: reference kotlinc unavailable"),
            Some(Ok(())) => {}
            Some(Err(difference)) => panic!("{difference}"),
        }
    }
}

#[test]
fn a_suspend_lambda_keeps_its_own_continuation_beside_the_enclosing_function_s_classes() {
    // The lambda records `box` as its enclosing source name. Its continuation keeps the name it
    // was lifted under, so it does not take the `box$N` of a callable reference's class.
    let src = "import kotlin.coroutines.*\n\
suspend fun value(): String = \"O\"\n\
suspend fun k(): String = \"K\"\n\
fun run(c: suspend () -> String): String {\n\
    var r = \"none\"\n\
    c.startCoroutine(object : Continuation<String> {\n\
        override val context: CoroutineContext = EmptyCoroutineContext\n\
        override fun resumeWith(result: Result<String>) { r = result.getOrThrow() }\n\
    })\n\
    return r\n\
}\n\
fun box(): String {\n\
    val first = run {\n\
        value()\n\
        value()\n\
    }\n\
    return first + run(::k) + run(::k).drop(1)\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "TransformerLambdaNames");
}
