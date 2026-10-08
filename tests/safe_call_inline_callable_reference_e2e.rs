//! `String.trim` inlined under a duplicated safe-call receiver.
//!
//! The receiver is already on the stack, and `trim` contains a loop, so the call used to leave
//! the method inliner. The spliced `Char::isWhitespace` parameter then occupied its own slot and
//! the source map stretched across `String.trim`'s closing line. kotlinc stores that receiver and
//! inlines the callee, reusing the predicate's slot with the following local.

use super::common;

#[test]
fn trim_of_a_callable_reference_matches_kotlinc_on_a_safe_call() {
    let src = "\
fun direct(value: String) = value.trim(Char::isWhitespace)\n\
fun safe(value: String?) = value?.trim(Char::isWhitespace)\n\
fun present(value: String?) = (value ?: \"\").trim(Char::isWhitespace)\n\
fun optional(value: String?) = (value ?: \"\")?.trim(Char::isWhitespace)\n\
";
    match common::byte_diff_against_kotlinc_cp(
        "SafeCallInlineTrim",
        src,
        "SafeCallInlineTrimKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (SafeCallInlineTrim: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("SafeCallInlineTrim: class bytes differ from kotlinc:\n{diff}"),
    }
}
