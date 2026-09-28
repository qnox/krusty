//! A suspend function returning a value class as its reference carrier returns the carrier when it
//! completes without suspending, but its continuation's `invokeSuspend` boxes the carrier, so the
//! caller it resumes reads the box and unboxes it, as kotlinc does. krusty's caller read the
//! resumed value as the carrier, so a kotlinc-compiled callee's box reached it unconverted.

use super::common;

const LIB: &str = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Any?>? = null\n\
@Suppress(\"UNCHECKED_CAST\")\n\
suspend fun <T> park(): T = suspendCoroutine { parked = it as Continuation<Any?> }\n\
@JvmInline value class Tag(val s: String)\n\
suspend fun tag(): Tag = Tag(park<String>())\n\
suspend fun maybe(): Tag? = park<Tag?>()\n";

/// kotlinc's continuation of `tag` resumes krusty's caller with the box.
#[test]
fn a_caller_unboxes_the_box_a_reference_carrier_resumes_with() {
    let main = "import kotlin.coroutines.*\n\
suspend fun text(): String { val t = tag(); return t.s + t.s }\n\
fun box(): String {\n\
    var seen = \"\"\n\
    suspend { seen = text() }.startCoroutine(Done())\n\
    parked!!.resume(\"K\")\n\
    return if (seen == \"KK\") \"OK\" else \"F:$seen\"\n\
}\n";
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, main).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_nullable_reference_carrier_resumes_with_null_or_its_box() {
    let main = "import kotlin.coroutines.*\n\
suspend fun text(): String { val t = maybe(); return if (t == null) \"null\" else t.s }\n\
fun box(): String {\n\
    var first = \"\"\n\
    var second = \"\"\n\
    suspend { first = text() }.startCoroutine(Done())\n\
    parked!!.resume(null)\n\
    suspend { second = text() }.startCoroutine(Done())\n\
    parked!!.resume(Tag(\"K\"))\n\
    return if (first == \"null\" && second == \"K\") \"OK\" else \"F:$first:$second\"\n\
}\n";
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, main).as_deref(),
        Some("OK")
    );
}

/// The continuation boxes a nullable carrier null-safely, as kotlinc's `invokeSuspend` does.
#[test]
fn a_nullable_carrier_continuation_boxes_like_kotlinc() {
    let src = "suspend fun <T> pass(t: T): T = t\n\
@JvmInline value class Tag(val s: String)\n\
suspend fun maybe(t: Tag?): Tag? = pass(t)\n";
    let built = common::compare_with_kotlinc_plugin(
        "NullableCarrierReentry",
        src,
        "NullableCarrierReentryKt$maybe$1",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public final java.lang.Object invokeSuspend(java.lang.Object)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}
