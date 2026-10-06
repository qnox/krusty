//! An `inline` function brackets each copy of a `finally` with `InlineMarker.finallyStart` and
//! `finallyEnd`. The argument counts the `finally` bodies open at that copy. A function that is
//! not `inline` emits the same copies without the calls.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
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
