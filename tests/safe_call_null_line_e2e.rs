//! A safe call over an inline function re-marks its line where the null path begins, as kotlinc
//! does.
//!
//! The inlined selector moves the current line into the inline function, so the null path marks
//! the call's line again. kotlinc normalizes that line's label into the null target's own and puts
//! the `pop` dropping the checked value right after it, so the line describes the `pop`. A
//! discarded call's null result leaves a `nop` behind that line.

use super::common;

const SOURCE: &str = r#"inline fun <T, R> T.pass(block: (T) -> R): R = block(this)

fun discarded(a: String?, f: (String) -> Unit) {
    a?.pass { f(it) }
}

fun used(a: String?, f: (String) -> String): String? {
    return a?.pass { f(it) }
}
"#;

#[test]
fn a_safe_call_over_an_inline_function_marks_its_null_path_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SafeCallNullLine",
        SOURCE,
        &["SafeCallNullLineKt"],
    );
}

/// A library `@InlineOnly` selector (`Continuation.resume`) moves the line into its body just as a
/// same-file inline function does, so the null path marks the call's line again. A discarded safe
/// call, before another statement or at the end of a `Unit` body, keeps the null result's `nop`
/// behind that line; a returned one has `aconst_null` there instead.
const INLINE_ONLY_SOURCE: &str = r#"import kotlin.coroutines.*

var c: Continuation<Any>? = null
var s: String? = null

fun beforeStatement(): String {
    c?.resume("OK")
    s?.length
    return "OK"
}

fun beforeReturn(): String {
    c?.resume("OK")
    return "OK"
}

fun atEnd() {
    c?.resumeWithException(IllegalStateException())
}

fun returned(): Any? {
    return c?.resume("OK")
}

fun notInlined(): String {
    s?.hashCode()
    return "OK"
}
"#;

#[test]
fn a_safe_call_over_an_inline_only_library_function_marks_its_null_path_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SafeCallInlineOnlyNullLine",
        INLINE_ONLY_SOURCE,
        &["SafeCallInlineOnlyNullLineKt"],
    );
}
