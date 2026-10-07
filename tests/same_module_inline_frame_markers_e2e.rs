//! A same-module `inline` call is expanded before emission, from its checked body rather than from
//! its bytecode, and still opens the frames kotlinc's inliner opens: the expansion's
//! `$i$f$<callee>` marker once its operands are bound, and for each lambda spliced into it the
//! lambda's parameter in a local of its own and a `$i$a$-<callee>-<lambda class>` marker, named
//! after the class the naming walk gave the lambda (`$bound$r$1` for a lambda bound to `val r`,
//! `$2` in a suspend function whose continuation takes the first position). Where the lambda's
//! value is the expansion's own, kotlinc returns to the inline body's line with a `nop` the method
//! keeps. The inline functions are this file's own, so no library function's special handling is
//! involved.

use super::common;

const SOURCE: &str = r#"
inline fun twice(f: (String) -> Int): Int = f("a")

inline fun text(f: (String) -> String): String = f("a")

inline fun plusOne(f: (String) -> Int): Int = f("a") + 1

inline fun constant(f: () -> Int): Int = f()

inline fun one(a: Int): Int = a + 1

fun length(): Int = twice { x -> x.length }

fun implicit(): Int = twice { it.length }

fun suffixed(): String = text { x -> x + "b" }

fun consumed(): Int = plusOne { x -> x.length }

fun unused(): Int = constant { 5 }

fun captured(s: String): Int = twice { x -> x.length + s.length }

fun bound(): Int {
    val r = twice { x -> x.length }
    return r
}

fun plain(): Int = one(2)

fun declaring(tag: String): String = text { p ->
    val first = p + tag
    val second = first + "!"
    second + p
}

fun box(): String {
    if (length() != 1) return "FAIL length: " + length()
    if (implicit() != 1) return "FAIL implicit: " + implicit()
    if (suffixed() != "ab") return "FAIL suffixed: " + suffixed()
    if (consumed() != 2) return "FAIL consumed: " + consumed()
    if (unused() != 5) return "FAIL unused: " + unused()
    if (captured("xyz") != 4) return "FAIL captured: " + captured("xyz")
    if (bound() != 1) return "FAIL bound: " + bound()
    if (plain() != 3) return "FAIL plain: " + plain()
    if (declaring("t") != "at!a") return "FAIL declaring: " + declaring("t")
    return "OK"
}
"#;

/// The frames of an expansion whose lambda suspends: kotlinc's coroutine transformer, which builds
/// this machine, keeps the markers out of the continuation and stores them again on resumption.
const SUSPENDING: &str = r#"
import kotlin.coroutines.*

object Completion : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) = result.getOrThrow()
}

inline fun twice(f: (String) -> Int): Int = f("a")

suspend fun other(): Int = 1

suspend fun suspended(): Int = twice { x -> other() + x.length }

fun box(): String {
    var result = 0
    val block: suspend () -> Unit = { result = suspended() }
    block.startCoroutine(Completion)
    return if (result == 2) "OK" else "FAIL suspended: " + result
}
"#;

#[test]
fn same_module_inline_frames_run() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "InlineFrames"),
        "OK"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(SUSPENDING, "SuspendingFrames"),
        "OK"
    );
}

/// The complete class file, kotlinc's exactly. The method names below identify the behavior under
/// test in a failure, but the pass condition includes the constant pool, every method and attribute,
/// line and local tables, SMAP, metadata, and their serialized order.
#[test]
fn same_module_inline_frames_are_the_reference_compilers() {
    for method in [
        "public static final int length()",
        "public static final int implicit()",
        "public static final java.lang.String suffixed()",
        "public static final int consumed()",
        "public static final int unused()",
        "public static final int captured(java.lang.String)",
        "public static final int bound()",
        "public static final int plain()",
        "public static final java.lang.String declaring(java.lang.String)",
    ] {
        common::method_code_diff_against_kotlinc(
            "InlineFrames",
            &[],
            SOURCE,
            "InlineFramesKt",
            method,
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{difference}"));
    }
}

#[test]
fn a_suspending_expansions_frames_are_the_reference_compilers() {
    common::method_code_diff_against_kotlinc(
        "SuspendingFrames",
        &[],
        SUSPENDING,
        "SuspendingFramesKt",
        "public static final java.lang.Object suspended(",
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}
