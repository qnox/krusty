//! kotlinc's `PopBackwardPropagationTransformer` and the dead-code step after it: a discarded value
//! is not pushed at all when every instruction that can push it is a load or a constant, and the
//! branches that only chose it collapse.
//!
//! krusty kept `iload; goto; iload; pop` for the result of an inline `if` used as a statement.
use super::common::{self, compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "var calls = 0\n\
    fun ext(): Int {\n\
    \x20   calls += 1\n\
    \x20   return 7\n\
    }\n\
    inline fun sel(c: Boolean, a: Int, b: Int): Int = if (c) a else b\n\
    inline fun selAny(c: Boolean, a: Int, b: Int): Any = if (c) a else b\n\
    inline fun selCall(c: Boolean, a: Int): Int = if (c) a else ext()\n\
    inline fun selLong(c: Boolean, a: Long, b: Long): Long = if (c) a else b\n\
    fun discardSel(c: Boolean, a: Int, b: Int) { sel(c, a, b) }\n\
    fun discardBoxed(c: Boolean, a: Int, b: Int) { selAny(c, a, b) }\n\
    fun discardSelCall(c: Boolean, a: Int) { selCall(c, a) }\n\
    fun discardLong(c: Boolean, a: Long, b: Long) { selLong(c, a, b) }\n";

fn rows(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|row| row.to_string()).collect()
}

#[test]
fn a_discarded_inline_result_is_not_pushed_like_kotlincs() {
    let built = compare_with_kotlinc_plugin(
        "PopBackward",
        SOURCE,
        "PopBackwardKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are required");
    // kotlinc also keeps its `$i$f$` marker local per inlined call (`iconst_0; istore`), which
    // krusty's inliner does not write yet; otherwise the listings are the same.
    for (member, reference, krusty) in [
        (
            "void discardSel(boolean, int, int)",
            &[
                "0: iload_0",
                "1: istore_3",
                "2: iload_1",
                "3: istore 4",
                "5: iload_2",
                "6: istore 5",
                "8: iconst_0",
                "9: istore 6",
                "11: iload_3",
                "12: ifeq 15",
                "15: return",
            ][..],
            &[
                "0: iload_0",
                "1: istore_3",
                "2: iload_1",
                "3: istore 4",
                "5: iload_2",
                "6: istore 5",
                "8: iload_3",
                "9: ifeq 12",
                "12: return",
            ][..],
        ),
        // The box of the discarded result goes first (RedundantBoxing), then its operands.
        (
            "void discardBoxed(boolean, int, int)",
            &[
                "0: iload_0",
                "1: istore_3",
                "2: iload_1",
                "3: istore 4",
                "5: iload_2",
                "6: istore 5",
                "8: iconst_0",
                "9: istore 6",
                "11: iload_3",
                "12: ifeq 15",
                "15: return",
            ][..],
            &[
                "0: iload_0",
                "1: istore_3",
                "2: iload_1",
                "3: istore 4",
                "5: iload_2",
                "6: istore 5",
                "8: iload_3",
                "9: ifeq 12",
                "12: return",
            ][..],
        ),
        // Of a load merged with a call, the load goes and the call pops its own result; the jump
        // around the `goto` left in front of the call is then negated.
        (
            "void discardSelCall(boolean, int)",
            &[
                "0: iload_0",
                "1: istore_2",
                "2: iload_1",
                "3: istore_3",
                "4: iconst_0",
                "5: istore 4",
                "7: iload_2",
                "8: ifne 15",
                "11: invokestatic # // Method ext:()I",
                "14: pop",
                "15: return",
            ][..],
            &[
                "0: iload_0",
                "1: istore_2",
                "2: iload_1",
                "3: istore_3",
                "4: iload_2",
                "5: ifne 12",
                "8: invokestatic # // Method ext:()I",
                "11: pop",
                "12: return",
            ][..],
        ),
        // A `pop2` is never propagated.
        (
            "void discardLong(boolean, long, long)",
            &[
                "0: iload_0",
                "1: istore 5",
                "3: lload_1",
                "4: lstore 6",
                "6: lload_3",
                "7: lstore 8",
                "9: iconst_0",
                "10: istore 10",
                "12: iload 5",
                "14: ifeq 22",
                "17: lload 6",
                "19: goto 24",
                "22: lload 8",
                "24: pop2",
                "25: return",
            ][..],
            &[
                "0: iload_0",
                "1: istore 5",
                "3: lload_1",
                "4: lstore 6",
                "6: lload_3",
                "7: lstore 8",
                "9: iload 5",
                "11: ifeq 19",
                "14: lload 6",
                "16: goto 21",
                "19: lload 8",
                "21: pop2",
                "22: return",
            ][..],
        ),
    ] {
        assert_eq!(
            method_instructions(&built.reference, member),
            rows(reference),
            "{member} (kotlinc)"
        );
        assert_eq!(
            method_instructions(&built.krusty, member),
            rows(krusty),
            "{member}"
        );
    }
}

/// Nothing that runs changes: the discarded results were never used, and the call still happens
/// on the branch that makes it.
#[test]
fn discarded_inline_results_still_run() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   discardSel(true, 1, 2)\n\
             \x20   discardBoxed(false, 1, 2)\n\
             \x20   discardLong(true, 1L, 2L)\n\
             \x20   discardSelCall(true, 1)\n\
             \x20   if (calls != 0) return \"call taken\"\n\
             \x20   discardSelCall(false, 1)\n\
             \x20   return if (calls == 1) \"OK\" else \"call dropped\"\n\
             }}\n"
        ),
        "PopBackwardBox",
    );
}
