//! A suspend call returning a reference-carrier value class is cast to the carrier where its two
//! paths meet, whatever consumes it.
//!
//! kotlinc materializes such a call at the value class's JVM type, `String` for `IC`. The direct
//! path leaves the call's erased `Object` result and the resumed path the carrier `unbox-impl`
//! returned, so the `checkcast String` after the merge point is written even when the consumer
//! takes `Object`: `bar().s` returned from a suspend function, and a value-class result returned
//! from a function that suspends before it (`chained`).

use super::common;

const SOURCE: &str = "\
import kotlin.coroutines.*\n\
\n\
@JvmInline\n\
value class IC(val s: String)\n\
\n\
var c: Continuation<Any>? = null\n\
\n\
suspend fun <T> suspendMe(): T = suspendCoroutine {\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    c = it as Continuation<Any>\n\
}\n\
\n\
class Box {\n\
    suspend fun bar(): IC = IC(suspendMe<String>())\n\
\n\
    suspend fun test() = bar().s\n\
\n\
    suspend fun length(): Int = bar().s.length\n\
\n\
    suspend fun wrap(s: String): IC = IC(s)\n\
\n\
    suspend fun chained(): IC = wrap(suspendMe<String>())\n\
}\n\
\n\
fun builder(c: suspend () -> Unit) {\n\
    c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
}\n\
\n\
fun box(): String {\n\
    var result = \"FAIL\"\n\
    builder { result = Box().test() }\n\
    c?.resume(\"OK\")\n\
    var length = 0\n\
    builder { length = Box().length() }\n\
    c?.resume(\"OK\")\n\
    var chained = \"FAIL\"\n\
    builder { chained = Box().chained().s }\n\
    c?.resume(\"OK\")\n\
    return if (length == 2 && chained == \"OK\") result else \"FAIL $length $chained\"\n\
}\n";

#[test]
fn a_value_class_suspension_result_is_cast_to_its_carrier_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "SuspendValueClassCarrierCast");
    let built =
        common::classes_against_kotlinc_module(&[("SuspendValueClassCarrierCast.kt", SOURCE)]);
    let reference = built.reference.get("Box").expect("kotlinc writes Box");
    let ours = built.krusty.get("Box").expect("krusty writes Box");
    assert!(
        reference == ours,
        "{}",
        common::exact_class_difference("Box", reference, ours)
    );
}
