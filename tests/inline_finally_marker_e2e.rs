//! An `inline` function brackets each copy of a `finally` with `InlineMarker.finallyStart` and
//! `finallyEnd`. The argument counts the `finally` bodies open at that copy. A function that is
//! not `inline` emits the same copies without the calls.

use super::common;

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
fn inline_finally_markers_match_kotlinc() {
    let methods = [
        // An inline finalizer is bracketed at depth one.
        "public static final int runFinally(",
        // A non-inline finalizer has no markers.
        "public static final int plain(",
        // A finalizer nested inside another finalizer uses the next depth.
        "public static final int nested(",
        // A returning finalizer has no unreachable end marker.
        "public static final int returnFinally(",
        // A throwing finalizer has no unreachable end marker.
        "public static final int throwFinally(",
        // A loop transfer from a finalizer has no unreachable end marker.
        "public static final int continueFinally(",
        // A copied finalizer consumes its declaration markers at the call site.
        "public static final int useFinally(",
    ];
    let results = common::class_bytes_diffs_against_kotlinc(
        "InlineFinally",
        &[],
        SRC,
        "InlineFinallyKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    for (method, result) in methods.into_iter().zip(results) {
        result.unwrap_or_else(|difference| panic!("{method}: {difference}"));
    }
}
