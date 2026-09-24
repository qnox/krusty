//! kotlinc's final `DeadCodeEliminationMethodTransformer`, which always runs after its bytecode
//! passes.
//!
//! When the redundant-`goto` pass threads a jump through a `goto` — `if (…) return 1` as a loop
//! body's last statement jumps to a `goto` back to the loop's head — that `goto` is left after the
//! `return`, reached by nothing. kotlinc removes it; krusty kept it, and the class differed in the
//! loop's code, its frames and every offset after it. The same transformer closes the gaps in the
//! local slots: a `for` loop's iterable is stored to a temporary the temporaries pass folds away,
//! and kotlinc then numbers the iterator and the element from the freed slot. Both are measured
//! byte-for-byte against kotlinc 2.4.10.

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
fn a_for_loop_numbers_its_locals_from_the_folded_temporarys_slot() {
    assert_byte_identical(
        "finalDceFor",
        "fun contains(xs: Iterable<String>, s: String): Int {\n\
         \x20   for (e in xs) {\n\
         \x20       if (e == s) return 1\n\
         \x20   }\n\
         \x20   return 0\n\
         }\n\
         \n\
         fun firstLong(xs: List<String>, min: Int): String? {\n\
         \x20   for (e in xs) {\n\
         \x20       if (e.length > min) return e\n\
         \x20   }\n\
         \x20   return null\n\
         }\n\
         \n\
         fun total(xs: Iterable<String>): Int {\n\
         \x20   var n = 0\n\
         \x20   for (e in xs) {\n\
         \x20       n = n + e.length\n\
         \x20   }\n\
         \x20   return n\n\
         }\n",
        "FinalDceForKt",
    );
}
