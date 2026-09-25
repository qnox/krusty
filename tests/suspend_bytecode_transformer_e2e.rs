//! Top-level suspend functions whose state machine kotlinc's coroutine transformer builds from the
//! finished method (`docs/JVM_INLINE_BEFORE_CPS.md` §1a, step 2).
//!
//! The emitter marks each suspension point as kotlinc's codegen does and the transformer rewrites
//! the method when its class is written. The continuation class it describes (spill fields, then
//! `result` and `label`, and the `@DebugMetadata` arrays) is byte-identical to kotlinc's; the
//! function's own method is not yet, because kotlinc's optimizer does not run after the transform.

use super::common;

const WORK: &str = "suspend fun leaf() {}\n\
suspend fun value(): Int = 1\n\
suspend fun work(count: Int, ref: String): Int {\n\
    leaf()\n\
    val v = value()\n\
    return v + count + ref.length\n\
}\n";

#[test]
fn a_continuation_spilling_an_int_and_a_reference_matches_kotlinc() {
    let Some(result) =
        common::byte_diff_against_kotlinc("TransformerWork", WORK, "TransformerWorkKt$work$1")
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.expect("the continuation class is byte-identical to kotlinc's");
}

#[test]
fn a_unit_function_continuation_matches_kotlinc() {
    // The last suspension's next line is the closing `}`, where kotlinc maps the implicit return.
    let src = "suspend fun leaf() {}\n\
suspend fun work(count: Int, ref: String) {\n\
    leaf()\n\
    leaf()\n\
}\n";
    let Some(result) =
        common::byte_diff_against_kotlinc("TransformerUnit", src, "TransformerUnitKt$work$1")
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
