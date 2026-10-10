//! `for ((i, v) in X.withIndex())` in the shape of kotlinc's `ForLoopsLowering` (`WithIndexHandler`
//! and `WithIndexLoopHeader`).
//!
//! A loop that destructures a `withIndex()` call in its header iterates `X` directly and counts the
//! index itself, so no `IndexingIterable` or `IndexedValue` is allocated:
//!
//!   * the loop over `X` is the one a plain `for (v in X)` would get, except that an `Iterable` or a
//!     `Sequence` is iterated through its own `iterator()` here;
//!   * when that loop counts an `Int` from constant `0` by `1` (an array, a `CharSequence`, `0 until
//!     n`), its counter is the index;
//!   * otherwise `var index = 0` follows the nested loop's variables, and each iteration reads
//!     `val i = index; index = index + 1` before the element;
//!   * a `CharSequence` other than a `String` reads its `length` before every iteration;
//!   * the first component (or `.index`) is the index and the second (or `.value`) the element.
//!
//! A loop variable that is not destructured in the header keeps the `IndexedValue` iteration.
use super::common;

fn assert_identical(name: &str, src: &str) {
    assert_identical_cp(name, src, &[common::stdlib_jar()]);
}

fn assert_identical_cp(name: &str, src: &str, classpath: &[std::path::PathBuf]) {
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, classpath)
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn iterables_iterate_their_own_iterator_and_count_an_index() {
    assert_identical(
        "WithIndexIterables",
        "fun list(xs: List<String>): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i + v.length; return s }\n\
         fun <T> iterable(xs: Iterable<T>): Int { var s = 0; for ((i, v) in xs.withIndex()) if (v != null) s += i; return s }\n\
         fun <I : Iterable<String>> bounded(xs: I): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i + v.length; return s }\n",
    );
}

#[test]
fn arrays_use_their_counter_as_the_index() {
    assert_identical(
        "WithIndexArrays",
        "fun ints(xs: IntArray): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i * v; return s }\n\
         fun strings(xs: Array<String>): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i + v.length; return s }\n\
         fun nested(xs: Array<Array<String>>): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i + v.size; return s }\n\
         fun size(xs: Array<Array<String>>): Int = xs.size\n",
    );
}

#[test]
fn char_sequences_use_their_counter_as_the_index() {
    // `StringBuilder` is the JDK's class.
    assert_identical_cp(
        "WithIndexText",
        "fun string(text: String): Int { var s = 0; for ((i, c) in text.withIndex()) if (c == 'a') s += i; return s }\n\
         fun constant(): Int { var s = 0; for ((i, c) in \"abca\".withIndex()) if (c == 'a') s += i; return s }\n\
         fun chars(text: CharSequence): Int { var s = 0; for ((i, c) in text.withIndex()) if (c == 'a') s += i; return s }\n\
         fun builder(text: StringBuilder): Int { var s = 0; for ((i, c) in text.withIndex()) if (c == 'a') s += i; return s }\n\
         fun <C : CharSequence> bounded(text: C): Int { var s = 0; for ((i, _) in text.withIndex()) s += i; return s }\n\
         fun computed(text: () -> CharSequence): Int { var s = 0; for ((_, c) in text().withIndex()) if (c == 'a') s += 1; return s }\n",
        &[common::stdlib_jar(), common::jdk_modules()],
    );
}

#[test]
fn sequences_iterate_their_own_iterator_and_count_an_index() {
    assert_identical(
        "WithIndexSequences",
        "fun sequence(xs: Sequence<Int>): Int { var s = 0; for ((i, v) in xs.withIndex()) s += i * v; return s }\n",
    );
}

#[test]
fn progressions_from_zero_by_one_use_their_counter_as_the_index() {
    assert_identical(
        "WithIndexFromZero",
        "fun until(n: Int): Int { var s = 0; for ((i, v) in (0 until n).withIndex()) s += i * v; return s }\n",
    );
}

#[test]
fn other_progressions_count_an_index() {
    assert_identical(
        "WithIndexProgressions",
        "fun rangeTo(n: Int): Int { var s = 0; for ((i, v) in (1..n).withIndex()) s += i * v; return s }\n\
         fun downTo(n: Int): Int { var s = 0; for ((i, v) in (n downTo 0).withIndex()) s += i * v; return s }\n\
         fun step(n: Int): Int { var s = 0; for ((i, v) in (1..n step 2).withIndex()) s += i * v; return s }\n\
         fun reversed(n: Int): Int { var s = 0; for ((i, v) in (0 until n).reversed().withIndex()) s += i * v; return s }\n",
    );
}

#[test]
fn an_unused_component_is_not_bound() {
    assert_identical(
        "WithIndexUnderscores",
        "fun indexOnly(xs: List<String>): Int { var s = 0; for ((i, _) in xs.withIndex()) s += i; return s }\n\
         fun valueOnly(xs: List<String>): Int { var s = 0; for ((_, v) in xs.withIndex()) s += v.length; return s }\n\
         fun neither(xs: List<String>): Int { var s = 0; for ((_, _) in xs.withIndex()) s += 1; return s }\n\
         fun arrayIndexOnly(xs: IntArray): Int { var s = 0; for ((i, _) in xs.withIndex()) s += i; return s }\n\
         fun arrayValueOnly(xs: IntArray): Int { var s = 0; for ((_, v) in xs.withIndex()) s += v; return s }\n\
         fun arrayNeither(xs: IntArray): Int { var s = 0; for ((_, _) in xs.withIndex()) s += 1; return s }\n",
    );
}

/// A name-based entry reads the `IndexedValue` property it selects, whatever it is named and in
/// whichever order the entries are written.
#[test]
fn name_based_entries_read_the_property_they_select() {
    let src = "// LANGUAGE: +NameBasedDestructuring +EnableNameBasedDestructuringShortForm\n\
               fun short(xs: Array<String>): Int { var s = 0; for ((index, value) in xs.withIndex()) s += index + value.length; return s }\n\
               fun swapped(xs: Array<String>): Int { var s = 0; for ((value, index) in xs.withIndex()) s += index + value.length; return s }\n\
               fun renamed(xs: List<String>): Int { var s = 0; for ((v = value, i = index) in xs.withIndex()) s += i + v.length; return s }\n\
               fun crossed(n: Int): Int { var s = 0; for ((index = value, value = index) in (1..n).withIndex()) s += index * value; return s }\n";
    let class = "WithIndexNamesKt";
    let comparison = common::compare_with_kotlinc_plugin(
        "WithIndexNames",
        src,
        class,
        &[common::stdlib_jar()],
        "1.8",
        &common::language_directives::kotlinc_args(src),
    )
    .expect("the reference kotlinc is provisioned");
    assert!(
        comparison.krusty_bytes == comparison.reference_bytes,
        "{class} differs from kotlinc:\nkrusty:\n{}\nkotlinc:\n{}",
        comparison.krusty,
        comparison.reference,
    );
}

#[test]
fn a_loop_variable_not_destructured_in_the_header_keeps_indexed_values() {
    assert_identical(
        "WithIndexValues",
        "fun values(xs: List<String>): Int { var s = 0; for (iv in xs.withIndex()) s += iv.index; return s }\n\
         fun later(xs: IntArray): Int { var s = 0; for (iv in xs.withIndex()) { val (i, v) = iv; s += i * v }; return s }\n",
    );
}

/// The index advances before the body runs, so `continue` cannot skip it and `break` leaves with
/// the elements seen so far.
#[test]
fn break_and_continue_keep_the_index_in_step() {
    let src = "class Numbers(val n: Int) : Iterable<Int> {\n\
               \x20   override fun iterator(): Iterator<Int> = (10..10 + n - 1).iterator()\n\
               }\n\
               fun box(): String {\n\
               \x20   var out = \"\"\n\
               \x20   val xs = IntArray(6) { it * 10 }\n\
               \x20   for ((i, v) in xs.withIndex()) { if (i == 1) continue; if (v == 40) break; out += \"$i:$v \" }\n\
               \x20   for ((i, c) in \"abcde\".withIndex()) { if (c == 'b') continue; if (i == 3) break; out += \"$i$c \" }\n\
               \x20   val text: CharSequence = \"vwxyz\"\n\
               \x20   for ((i, c) in text.withIndex()) { if (c == 'w') continue; if (i == 3) break; out += \"$i~$c \" }\n\
               \x20   for ((i, v) in Numbers(5).withIndex()) { if (i % 2 == 1) continue; if (v == 14) break; out += \"$i=$v \" }\n\
               \x20   for ((i, v) in (5 downTo 1).withIndex()) { if (v == 4) continue; if (i == 3) break; out += \"$i/$v \" }\n\
               \x20   for ((i, _) in (1..9 step 3).withIndex()) out += \"#$i\"\n\
               \x20   return out\n\
               }\n";
    let actual = common::compile_and_run_box(
        src,
        "with_index_jumps",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(
        actual,
        "0:0 2:20 3:30 0a 2c 0~v 2~x 0=10 2=12 0/5 2/3 #0#1#2"
    );
}
