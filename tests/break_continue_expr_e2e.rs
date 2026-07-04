//! `break` / `continue` used in EXPRESSION position inside a loop — an elvis RHS (`val v = x ?: continue`),
//! a `when` arm (`when { x < 0 -> break; else -> x }`), with and without a loop label
//! (`continue@outer` / `break@outer`). Before, the parser only accepted `break`/`continue` as STATEMENTS,
//! so in expression position they were parsed as bare identifiers → "unresolved reference 'continue'".
//! They are now `Expr::Break`/`Expr::Continue` (bottom type `Nothing`, like `return`/`throw`), lowered to
//! the same `IrExpr::Break`/`Continue` loop jump as the statement form. Same-file, runnable.
mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn continue_as_elvis_rhs() {
    // `x ?: continue` skips a null element; the accumulated sum omits the nulls.
    const SRC: &str = "fun sum(xs: List<Int?>): Int {\n\
        \x20 var s = 0\n\
        \x20 for (x in xs) { val v = x ?: continue; s += v }\n\
        \x20 return s\n\
        }\n\
        fun box(): String = if (sum(listOf(1, null, 3, null, 5)) == 9) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("continue as elvis rhs"), "OK");
}

#[test]
fn break_as_elvis_rhs() {
    // `x ?: break` stops the loop at the first null; the sum is of the leading non-nulls.
    const SRC: &str = "fun leading(xs: List<Int?>): Int {\n\
        \x20 var s = 0\n\
        \x20 for (x in xs) { val v = x ?: break; s += v }\n\
        \x20 return s\n\
        }\n\
        fun box(): String = if (leading(listOf(1, 2, null, 9)) == 3) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("break as elvis rhs"), "OK");
}

#[test]
fn break_continue_as_when_arm() {
    // `break`/`continue` as a `when` branch value.
    const SRC: &str = "fun f(xs: List<Int>): Int {\n\
        \x20 var s = 0\n\
        \x20 for (x in xs) {\n\
        \x20   val v = when { x < 0 -> continue; x > 100 -> break; else -> x }\n\
        \x20   s += v\n\
        \x20 }\n\
        \x20 return s\n\
        }\n\
        fun box(): String = if (f(listOf(1, -5, 3, 200, 9)) == 4) \"OK\" else \"fail: ${f(listOf(1, -5, 3, 200, 9))}\"\n";
    assert_eq!(run(SRC).expect("break/continue as when arm"), "OK");
}

#[test]
fn labeled_break_continue_as_elvis_rhs() {
    // `continue@outer` / `break@outer` from a nested loop, in expression position.
    const CONT: &str = "fun f(rows: List<List<Int?>>): Int {\n\
        \x20 var s = 0\n\
        \x20 outer@ for (row in rows) { for (x in row) { val v = x ?: continue@outer; s += v } }\n\
        \x20 return s\n\
        }\n\
        fun box(): String = if (f(listOf(listOf(1, 2), listOf<Int?>(3, null, 9), listOf(4))) == 10) \"OK\" else \"fail\"\n";
    assert_eq!(run(CONT).expect("labeled continue"), "OK");

    const BRK: &str = "fun f(rows: List<List<Int?>>): Int {\n\
        \x20 var s = 0\n\
        \x20 outer@ for (row in rows) { for (x in row) { val v = x ?: break@outer; s += v } }\n\
        \x20 return s\n\
        }\n\
        fun box(): String = if (f(listOf(listOf(1, 2), listOf<Int?>(3, null, 9), listOf(4))) == 6) \"OK\" else \"fail\"\n";
    assert_eq!(run(BRK).expect("labeled break"), "OK");
}
