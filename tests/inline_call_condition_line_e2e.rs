//! kotlinc's `markLineNumberAfterInlineIfNeeded`: after an inline call that is a `when` branch
//! condition, the caller's line is written again for the jump that consumes the result. A bare loop
//! condition is not such a condition, so after an inline call there the line is forgotten and the
//! next instruction's line is written where it begins. An `&&`/`||` operand marks its own line again
//! before its jump. A same-module inline call stores its operands with no line, then marks its own
//! line and a `nop` where the callee's frame opens.

use crate::common;

const SOURCE: &str = r#"package lines

inline fun lt(a: Int, b: Int): Boolean = a < b
inline fun ne(a: Any, b: Any): Boolean = a != b
inline fun observed(value: Int): Int = value

fun orElse(p: Int, q: Int) = lt(p, q) || ne("a", "b")
fun andAlso(p: Int, q: Int) = lt(p, q) && ne("a", "b")
fun evaluatedArguments(p: Int, q: Int) =
    lt(
        observed(p),
        observed(q)
    ) && p >= 0
fun stored(p: Int, q: Int): Boolean {
    val r = lt(p, q) || p > 0
    return r
}
fun rightOperand(p: Int, q: Int) = p > 0 || lt(p, q)
fun split(p: Int, q: Int) =
    lt(p, q) ||
        ne("a", "b")
fun branch(p: Int, q: Int) = if (lt(p, q)) 1 else 2
fun loop(p: Int): Int {
    var i = p
    while (lt(i, 3)) i++
    return i
}
fun loopOperands(p: Int): Int {
    var i = p
    while (lt(i, 3) && lt(0, 1)) i++
    do {
        i--
    } while (lt(0, i) || lt(i, -5))
    return i
}
fun plain(p: Int, q: Int): Boolean {
    val r = lt(p, q)
    return r
}
fun library(s: StringBuilder): Int {
    while (s.isNotEmpty()) s.setLength(s.length - 1)
    do {
        s.append('x')
    } while (lt(s.length, 2))
    return s.length
}

fun lineArguments(p: Int, q: Int) =
    lt(
        p,
        q
    ) && p >= 0
fun lastEvaluated(p: Int, q: Int) =
    lt(
        p,
        observed(q)
    ) && p >= 0
fun boundEvaluated(p: Int, q: Int): Boolean {
    val r = lt(
        observed(p),
        q + 1
    )
    return r
}
fun ifCondition(p: Int, q: Int) =
    if (lt(
        observed(p),
        observed(q)
    )) 1 else 2
fun oneLine(p: Int, q: Int) = lt(observed(p), observed(q)) && p >= 0

fun box(): String =
    if (orElse(1, 2) && !andAlso(2, 1) && evaluatedArguments(1, 2) && stored(0, 1) &&
        rightOperand(0, 1) && split(2, 1) && branch(1, 2) == 1 && loop(0) == 3 &&
        loopOperands(0) == 0 && plain(1, 2) && library(StringBuilder("ab")) == 2 &&
        lineArguments(1, 2) && lastEvaluated(1, 2) && boundEvaluated(1, 1) &&
        ifCondition(1, 2) == 1 && oneLine(1, 2)
    ) "OK" else "fail"
"#;

#[test]
fn an_inline_call_in_a_condition_restores_the_caller_line() {
    common::byte_diff_against_kotlinc_cp(
        "InlineCallConditionLine",
        SOURCE,
        "lines/InlineCallConditionLineKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("lines/InlineCallConditionLineKt differs from kotlinc: {diff}"));
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("InlineCallConditionLine.kt", SOURCE)])
            .as_deref(),
        Some("OK")
    );
}
