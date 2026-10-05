//! A suspend call returning a value class, inside a lambda inlined into a suspend function, suspends
//! that function's own machine. The callee returns the value class's carrier where it does not
//! suspend, and its continuation completes with the box: the resumed path unboxes it, as kotlinc
//! does, so both paths continue with the carrier. The function's own continuation boxes the carrier
//! its function returns again before completing with it. A nullable value class lets `null` through
//! both conversions. The inline function is this repository's own, compiled by the reference
//! compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
import kotlin.coroutines.*

@JvmInline value class Id(val value: String)

var pending: Continuation<Id>? = null
var pendingNullable: Continuation<Id?>? = null

suspend fun pause(): Id = suspendCoroutine { pending = it }
suspend fun pauseNullable(): Id? = suspendCoroutine { pendingNullable = it }

class Source {
    suspend fun <T> identity(value: T): T = value
    suspend fun get(): Id { runIt { return identity(pause()) } }
    suspend fun getNullable(): Id? { runIt { return identity(pauseNullable()) } }
}

fun box(): String {
    var observed = "FAIL"
    suspend { Source().get().value + (Source().getNullable()?.value ?: "null") }
        .startCoroutine(Continuation(EmptyCoroutineContext) { observed = it.getOrThrow() })
    if (observed != "FAIL") return "completed before resume: $observed"
    pending!!.resume(Id("O"))
    pendingNullable!!.resume(null)
    return if (observed == "Onull") "OK" else "FAIL $observed"
}
"#;

#[test]
fn a_resumed_value_class_is_unboxed_inside_an_inlined_lambda() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
