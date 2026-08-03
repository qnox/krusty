//! An empty loop body written as a bare `;` (`while (c);`, `for (…);`). The `;` is an explicit empty
//! body — kotlinc runs the loop for its side effects. Previously the `;` (lexed like a newline) was
//! skipped and the FOLLOWING statement was mistaken for the body. Same-file, runnable.
//!
//! Also the neighbouring case of a body that is never ENTERED: a loop or `when` branch whose condition
//! folds to a constant. The emitter must emit no code on the path the folded jump makes unreachable,
//! and must still frame the merge the live paths converge on (see docs/SPEC.md).
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn empty_while_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var x = 0\n\
        \x20 while (x++ < 5);\n\
        \x20 return if (x == 6) \"OK\" else \"no:\" + x\n\
        }\n";
    assert_eq!(run(SRC).expect("empty while"), "OK");
}

#[test]
fn empty_for_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var s = 0\n\
        \x20 for (i in 1..4) s += i\n\
        \x20 var t = 0\n\
        \x20 for (i in 1..4);\n\
        \x20 return if (s == 10 && t == 0) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("empty for"), "OK");
}

/// A loop whose condition is the literal `false` is never entered, so its body is unreachable.
///
/// The emitter folds the always-false pre-test into an unconditional jump PAST the loop, but used to
/// lay the body down after it anyway — dead code with nothing branching to it, hence no stack-map
/// frame, hence `VerifyError: Expecting a stack map frame` at the body's first instruction. kotlinc
/// emits no body for a never-entered loop. (`do … while (false)` is unaffected: a post-test body always
/// runs once and the folded back-edge simply isn't emitted.)
#[test]
fn never_entered_while_emits_no_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var s = \"z\"\n\
        \x20 while (false) { s += \"never\" }\n\
        \x20 do { s += \"once\" } while (false)\n\
        \x20 return s\n\
        }\n";
    assert_eq!(run(SRC).expect("never-entered while"), "zonce");
}

/// A `when`/`if` branch whose condition folds to a constant `false` is never selected, so it emits no
/// body — the `emit_when` half of the same rule as [`never_entered_while_emits_no_body`].
///
/// A `const val` is what reaches the emitter as `IrExpr::Const(IrConst::Boolean(false))`; a literal
/// `if (false)` is folded away earlier, in `ir_lower`. Both results are kotlinc's.
#[test]
fn never_selected_when_branch_emits_no_body() {
    const SRC: &str = "const val FLAG = false\n\
        fun box(): String {\n\
        \x20 val a = if (FLAG) \"x\" else \"b\"\n\
        \x20 val n = 1 + (if (FLAG) 2 else 3)\n\
        \x20 val v: Long = if (FLAG) 1L else 2L\n\
        \x20 return \"$a$n$v\"\n\
        }\n";
    assert_eq!(run(SRC).expect("never-selected when branch"), "b42");
}

/// The never-selected branch is skipped for its CODE, not for its merge-point accounting.
///
/// `diverges` does not fold constant conditions, so this `when` still reports as falling through and
/// the caller keeps emitting at the merge. Skipping the dead branch without marking the merge reachable
/// left it unframed and merely moved `VerifyError: Expecting a stack map frame` from the dead body to
/// the merge. Needs the dead branch to be the ONLY non-diverging one — hence the `return`/`throw` else.
#[test]
fn never_selected_branch_still_frames_the_merge() {
    const RET: &str = "const val FLAG = false\n\
        fun box(): String {\n\
        \x20 val x = if (FLAG) \"a\" else return \"b\"\n\
        \x20 return x\n\
        }\n";
    assert_eq!(run(RET).expect("dead branch, diverging else"), "b");
}
