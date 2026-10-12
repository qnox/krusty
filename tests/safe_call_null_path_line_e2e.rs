//! A safe call's null path is a source position of its selector's line. fir2ir gives the `null`
//! branch the selector's offsets, so kotlinc marks it like any constant: after an inlined selector,
//! whose body forgot the caller's line, the null path writes that line again. The entry lands on the
//! `pop` the null-check fold puts at the target, and a discarded safe call keeps the `nop` its
//! dropped `null` push leaves as the only instruction of that line (`pop; nop` in every coroutine
//! box test's `handleExceptionContinuation`). A selector that is not inlined leaves the line in
//! effect, so its null path writes no entry and no `nop`, and so does a selector that diverges:
//! kotlinc marks the null path's line where the selector's code ends, which is dead after it.
use super::common;

const SAFE_CALLS: &str = "package store\n\
                          \n\
                          import kotlin.coroutines.*\n\
                          \n\
                          fun handleExceptionContinuation(x: (Throwable) -> Unit): Continuation<Any?> = object : Continuation<Any?> {\n\
                          \x20   override val context = EmptyCoroutineContext\n\
                          \x20   override fun resumeWith(result: Result<Any?>) {\n\
                          \x20       result.exceptionOrNull()?.let(x)\n\
                          \x20   }\n\
                          }\n\
                          \n\
                          fun discarded(r: Throwable?, x: (Throwable) -> Unit) {\n\
                          \x20   r?.let(x)\n\
                          }\n\
                          \n\
                          fun returned(r: Throwable?, x: (Throwable) -> String): String? {\n\
                          \x20   return r?.let(x)\n\
                          }\n\
                          \n\
                          fun stored(r: Throwable?, x: (Throwable) -> String): Int {\n\
                          \x20   val s = r?.let(x)\n\
                          \x20   return 1\n\
                          }\n\
                          \n\
                          fun notInline(r: Throwable?) {\n\
                          \x20   r?.toString()\n\
                          }\n\
                          \n\
                          fun failure(a: Long): String? = if (a > 0) null else \"fail\"\n\
                          \n\
                          fun firstFailure(xs: List<Long>): String {\n\
                          \x20   for (a in xs) {\n\
                          \x20       failure(a)?.let { return it }\n\
                          \x20   }\n\
                          \x20   return \"OK\"\n\
                          }\n";

#[test]
fn safe_call_null_path_marks_the_selector_line_like_kotlinc() {
    for class in [
        "store/SafeCallNullPathKt",
        "store/SafeCallNullPathKt$handleExceptionContinuation$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "SafeCallNullPath",
            SAFE_CALLS,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
