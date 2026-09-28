//! The generic `Signature` of a suspend function's continuation. kotlinc writes the continuation's
//! `in` projection as `? super R` except over `Any`, where it is redundant: a suspend function
//! returning `Any` or `Any?`, and a suspend function type with that result, sign
//! `Continuation<Object>`.

use super::common;

#[test]
fn a_continuation_over_any_has_no_wildcard() {
    const SRC: &str = "class Token\n\
\n\
suspend fun anything(): Any? = null\n\
\n\
suspend fun something(): Any = Token()\n\
\n\
suspend fun token(): Token = Token()\n\
\n\
fun takes(f: suspend (Token) -> Any) {}\n\
\n\
fun passes(f: suspend () -> Any?): suspend () -> Any? = f\n";
    common::byte_diff_against_kotlinc("ContinuationOverAny", SRC, "ContinuationOverAnyKt")
        .expect("reference kotlinc is provisioned")
        .expect("ContinuationOverAnyKt byte-identical to kotlinc");
}
