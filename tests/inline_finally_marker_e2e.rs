//! An `inline` function brackets each copy of a `finally` with `InlineMarker.finallyStart` and
//! `finallyEnd`. The argument counts the `finally` bodies open at that copy. A function that is
//! not `inline` emits the same copies without the calls.

use super::common;

static FIXTURE_COMPILE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    // Every assertion below compiles the same intentionally broad fixture. The coverage lane runs
    // four e2e tests concurrently; compiling this try/finally-heavy source four times at once made
    // all copies bail while the exact GHA-built compiler accepts it in isolation. Keep the semantic
    // assertions distinct, but give this one fixture a single compilation slot.
    let _fixture = FIXTURE_COMPILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const SRC: &str = "\
fun side() {}\n\
inline fun runFinally(body: () -> Int): Int {\n\
    try {\n\
        return body()\n\
    } finally {\n\
        side()\n\
    }\n\
}\n\
fun plain(body: () -> Int): Int {\n\
    try {\n\
        return body()\n\
    } finally {\n\
        side()\n\
    }\n\
}\n\
inline fun nested(body: () -> Int): Int {\n\
    try {\n\
        return body()\n\
    } finally {\n\
        try {\n\
            side()\n\
        } finally {\n\
            side()\n\
        }\n\
    }\n\
}\n\
inline fun returnFinally(body: () -> Int): Int {\n\
    try {\n\
        return body()\n\
    } finally {\n\
        return 7\n\
    }\n\
}\n\
inline fun throwFinally(body: () -> Int): Int {\n\
    try {\n\
        return body()\n\
    } finally {\n\
        throw IllegalStateException()\n\
    }\n\
}\n\
inline fun continueFinally(body: () -> Int): Int {\n\
    while (true) {\n\
        try {\n\
            return body()\n\
        } finally {\n\
            continue\n\
        }\n\
    }\n\
}\n\
fun useFinally(): Int = runFinally { 1 }\n\
";

#[test]
fn an_inline_finally_is_bracketed_at_depth_one() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int runFinally(",
    );
}

#[test]
fn a_non_inline_finally_has_no_marker() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int plain(",
    );
}

#[test]
fn a_finally_inside_a_finally_uses_the_next_depth() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int nested(",
    );
}

#[test]
fn a_returning_finally_has_no_unreachable_end_marker() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int returnFinally(",
    );
}

#[test]
fn a_throwing_finally_has_no_unreachable_end_marker() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int throwFinally(",
    );
}

#[test]
fn a_loop_transfer_from_finally_has_no_unreachable_end_marker() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int continueFinally(",
    );
}

#[test]
fn a_copied_finally_consumes_its_declaration_markers() {
    expect_method_matches(
        "InlineFinally",
        SRC,
        "InlineFinallyKt",
        "public static final int useFinally(",
    );
}
