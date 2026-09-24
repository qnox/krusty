//! kotlinc's final `DeadCodeEliminationMethodTransformer`, which always runs after its bytecode
//! passes.
//!
//! When the redundant-`goto` pass threads a jump through a `goto` — `if (…) return 1` as a loop
//! body's last statement jumps to a `goto` back to the loop's head — that `goto` is left after the
//! `return`, reached by nothing. kotlinc removes it; krusty kept it, and the class differed in the
//! loop's code, its frames and every offset after it. The complete rewritten classes are measured
//! byte-for-byte against kotlinc 2.4.10; slot-compaction edge cases live beside their owning module.

use super::common;

fn assert_byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

#[test]
fn a_goto_bypassed_by_threaded_jumps_is_removed() {
    assert_byte_identical(
        "finalDceWhile",
        "fun halve(n: Int, k: Int): Int {\n\
         \x20   var i = n\n\
         \x20   while (i > 0) {\n\
         \x20       i = i / 2\n\
         \x20       if (i == k) return 1\n\
         \x20   }\n\
         \x20   return 0\n\
         }\n\
         \n\
         fun scan(xs: IntArray, k: Int): Boolean {\n\
         \x20   var i = xs.size\n\
         \x20   while (i > 0) {\n\
         \x20       i = i - 2\n\
         \x20       if (xs[i] == k) return true\n\
         \x20   }\n\
         \x20   return false\n\
         }\n",
        "FinalDceWhileKt",
    );
}

#[test]
fn iterator_loops_with_early_returns_receive_final_dce() {
    assert_byte_identical(
        "finalDceIterator",
        "class Cursor(private val end: Int) {\n\
         \x20   private var current = 0\n\
         \x20   operator fun hasNext(): Boolean = current < end\n\
         \x20   operator fun next(): Int = current++\n\
         }\n\
         \n\
         class Values(private val end: Int) {\n\
         \x20   operator fun iterator(): Cursor = Cursor(end)\n\
         }\n\
         \n\
         fun contains(xs: Values, k: Int): Boolean {\n\
         \x20   for (e in xs) {\n\
         \x20       if (e == k) return true\n\
         \x20   }\n\
         \x20   return false\n\
         }\n\
         \n\
         fun firstLarge(xs: Values, min: Int): Int {\n\
         \x20   for (e in xs) {\n\
         \x20       if (e > min) return e\n\
         \x20   }\n\
         \x20   return -1\n\
         }\n\
         \n\
         fun total(xs: Values): Int {\n\
         \x20   var n = 0\n\
         \x20   for (e in xs) n += e\n\
         \x20   return n\n\
         }\n",
        "FinalDceIteratorKt",
    );
}
