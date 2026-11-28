//! A nullable `var` is flow-narrowed to its non-null type after a non-null assignment, matching
//! kotlinc's smart-cast: `var i: Int?; i = 10; i += 1` reads `i` as `Int`. Its initializer does not
//! narrow a declared type, as kotlinc rejects `var i: Int? = 10; i + 1`. The
//! narrowing is dropped at branch/loop/closure boundaries (sound), and a later nullable reassignment
//! widens it back to the declared type. Same-file, runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn assign_then_compound_plus() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var i: Int?\n\
        \x20 i = 10\n\
        \x20 i += 1\n\
        \x20 return if (i == 11) \"OK\" else \"fail: $i\"\n\
        }\n";
    assert_eq!(run(SRC).expect("assign then +="), "OK");
}

#[test]
fn nonnull_initializer_does_not_narrow_a_declared_type() {
    const SRC: &str = "fun sum(): Int {\n\
        \x20 var i: Int? = 10\n\
        \x20 return i.inc()\n\
        }\n";
    let result = common::compiler_diagnostics(
        &[("Main.kt", SRC)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "declared initializer");
}

#[test]
fn reassign_renarrows_to_new_value() {
    // A second non-null assignment re-narrows the var, so it stays usable as a non-null Int.
    const SRC: &str = "fun box(): String {\n\
        \x20 var i: Int? = null\n\
        \x20 i = 5\n\
        \x20 val a = i + 1\n\
        \x20 i = 20\n\
        \x20 val b = i + 2\n\
        \x20 return if (a == 6 && b == 22) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("reassign renarrows"), "OK");
}

#[test]
fn narrow_dropped_after_null_reassign_and_across_branch() {
    // `x ?: -1` sees the narrowed non-null value; a nullable reassignment (`x = null`) drops the
    // narrowing so a later `x ?: -5` correctly takes the fallback. Proves the narrowing is not leaked
    // unsoundly into the elvis fallback or across the reassignment.
    const SRC: &str = "fun box(): String {\n\
        \x20 var x: Int? = 10\n\
        \x20 val y = x ?: -1\n\
        \x20 x = null\n\
        \x20 val z = x ?: -5\n\
        \x20 return if (y == 10 && z == -5) \"OK\" else \"no:$y,$z\"\n\
        }\n";
    assert_eq!(run(SRC).expect("elvis + null reassign"), "OK");
}

#[test]
fn string_var_narrows_to_nonnull() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var s: String? = \"O\"\n\
        \x20 return s + \"K\"\n\
        }\n";
    assert_eq!(run(SRC).expect("string narrow"), "OK");
}

#[test]
fn proof_dies_after_a_branch_that_writes_the_var_joins() {
    // The `else` edge keeps the guard's proof, but once the edges join, the `then` edge's write
    // leaves the variable nullable again, as kotlinc reports.
    const SRC: &str = "class F(val p: F?, var t: String?)\n\
        fun g(start: F?, c: Boolean): String? {\n\
        \x20 var f = start\n\
        \x20 if (f != null) {\n\
        \x20   if (c) { f = f.p } else { f.t = \"x\" }\n\
        \x20   return f.t\n\
        \x20 }\n\
        \x20 return null\n\
        }\n";
    let result = common::compiler_diagnostics(
        &[("Main.kt", SRC)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    common::expect_identical_rejection(&result, "join after a write in one branch");
}

#[test]
fn a_plain_assignment_in_a_loop_ends_the_smart_cast_at_its_entry() {
    // Any `=` in the loop body ends the smart cast before the body runs, even one that assigns a
    // value of the narrowed type; an operator write (`i++`) alone keeps it
    // (box_regressions/loopOperatorWriteKeepsSmartCast.kt).
    const SRC: &str =
        "fun same() { var i: Any = 1; if (i is Int) { while (i != 10) { i++; i = 5 } } }\n\
        fun other() { var i: Int? = 1; if (i != null) { while (i != 10) { i.inc(); i = i } } }\n";
    let result = common::compiler_diagnostics(
        &[("Main.kt", SRC)],
        &[common::stdlib_jar(), common::jdk_modules()],
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr).len(), 2);
    common::expect_identical_rejection(&result, "plain assignment in a loop");
}
