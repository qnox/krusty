//! A suspend call inside a lambda inlined into an interface member with a body, or into an
//! overridable class member, suspends the member's own machine. kotlinc moves such a member's body
//! to the static `$suspendImpl` its continuation re-enters, so the member owns its machine like a
//! static function does. The inline function is this repository's own, compiled by the reference
//! compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
import kotlin.coroutines.*

var pending: Continuation<String>? = null

suspend fun pause(value: String): String = suspendCoroutine { pending = it; it.resume(value) }

interface Greeting {
    suspend fun greet(): String = runIt { pause("O") } + runIt { pause("K") }
}

open class Base {
    open suspend fun work(): String = runIt { pause("O") } + "K"
}

class Impl : Greeting

fun box(): String {
    var result = "FAIL"
    suspend { Impl().greet() + Base().work() }
        .startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    return if (result == "OKOK") "OK" else "FAIL $result"
}
"#;

#[test]
fn an_overridable_member_suspends_inside_an_inlined_lambda() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
