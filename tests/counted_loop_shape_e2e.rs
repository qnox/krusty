//! Counted `for` loops over a range literal or a progression value, in the shape of kotlinc's
//! `ForLoopsLowering` on the JVM.
//!
//! The shape depends on the bound, not on the operator alone:
//!
//!   * an exclusive bound (`until`, `..<`), and an inclusive constant bound that can step outward
//!     (`0..10` becomes `i < 11`, `n downTo 0` becomes `-1 < i`), is a Java counter loop that tests
//!     at the top and steps at the bottom;
//!   * an inclusive bound that may be the extreme value of its type is an if-guarded loop that leaves
//!     by comparing the counter with the bound before stepping, and loops back into the body;
//!   * a decreasing comparison reads the bound first;
//!   * a bound that cannot change while the loop runs (a constant or an immutable local) is read in
//!     place, anything else is copied into a temporary first;
//!   * a progression value is read once and iterated through its `first`, `last` and `step`, with
//!     the direction taken from the step's sign unless the value is a `*Range`.
use super::common;

fn assert_identical(name: &str, src: &str) {
    let class = format!("{name}Kt");
    let Some(result) =
        common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
    else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    result.unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
}

#[test]
fn exclusive_bounds_are_java_counter_loops() {
    assert_identical(
        "ExclusiveBounds",
        "fun until(n: Int): Int { var s = 0; for (i in 0 until n) s += i; return s }\n\
         fun rangeUntil(n: Long): Long { var s = 0L; for (i in 0L..<n) s += i; return s }\n",
    );
}

#[test]
fn constant_inclusive_bounds_become_exclusive() {
    assert_identical(
        "ConstantBounds",
        "fun up(): Int { var s = 0; for (i in 0..10) s += i; return s }\n\
         fun down(n: Int): Int { var s = 0; for (i in n downTo 0) s += i; return s }\n\
         fun chars(): Int { var s = 0; for (c in 'a'..'z') s += c - 'a'; return s }\n\
         fun longs(n: Long): Long { var s = 0L; for (i in n downTo 1L) s += i; return s }\n",
    );
}

#[test]
fn bounds_that_may_overflow_guard_the_loop() {
    assert_identical(
        "OverflowingBounds",
        "fun up(n: Int): Int { var s = 0; for (i in 1..n) s += i; return s }\n\
         fun down(n: Int): Int { var s = 0; for (i in 10 downTo n) s += i; return s }\n\
         fun max(): Int { var s = 0; for (i in 2147483645..Int.MAX_VALUE) s += 1; return s }\n\
         fun longMax(): Long { var s = 0L; for (i in 5L..Long.MAX_VALUE) s += i; return s }\n\
         fun min(): Int { var s = 0; for (i in 0 downTo Int.MIN_VALUE) s += i; return s }\n\
         fun charsDown(c: Char): Int { var s = 0; for (x in 'z' downTo c) s += x - 'a'; return s }\n\
         fun chars(c: Char): Int { var s = 0; for (x in 'a'..c) s += x - 'a'; return s }\n\
         fun skip(n: Int): Int { var s = 0; for (i in 0..n) { if (i == 3) continue; s += i }; return s }\n",
    );
}

#[test]
fn only_a_changing_bound_is_copied() {
    assert_identical(
        "CachedBounds",
        "fun variable(n: Int): Int { var m = n; var s = 0; for (i in 0..m) { m = 0; s += i }; return s }\n\
         fun computed(n: Int): Int { var s = 0; for (i in 0..n + 1) s += i; return s }\n\
         fun widened(n: Int): Long { var s = 0L; for (i in 0L..n) s += i; return s }\n",
    );
}

/// The shapes above only move where the loop tests and steps. The values at the edges of each
/// type, empty ranges, and a bound reassigned inside the body still iterate as Kotlin requires.
#[test]
fn counted_loops_still_iterate_correctly() {
    let src = "fun box(): String {\n\
               \x20   var top = 0\n\
               \x20   for (i in Int.MAX_VALUE - 2..Int.MAX_VALUE) top += 1\n\
               \x20   var bottom = 0\n\
               \x20   for (i in Int.MIN_VALUE + 2 downTo Int.MIN_VALUE) bottom += 1\n\
               \x20   var empty = 0\n\
               \x20   for (i in 5..4) empty += 1\n\
               \x20   for (i in 4 downTo 5) empty += 1\n\
               \x20   for (i in 3 until 3) empty += 1\n\
               \x20   var m = 3\n\
               \x20   var bound = 0\n\
               \x20   for (i in 0..m) { m = 0; bound += 1 }\n\
               \x20   var chars = \"\"\n\
               \x20   for (c in 'x'..'\\uFFFF') { if (c > 'z') break; chars += c }\n\
               \x20   for (c in 'c' downTo 'a') chars += c\n\
               \x20   var longs = 0L\n\
               \x20   for (i in Long.MAX_VALUE - 1..Long.MAX_VALUE) longs += 1L\n\
               \x20   return \"$top $bottom $empty $bound $chars $longs\"\n\
               }\n";
    let Some(actual) =
        common::compile_and_run_box(src, "counted_loop_edges", &[common::stdlib_jar()], None)
    else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(actual, "3 3 0 4 xyzcba 2");
}

#[test]
fn progression_values_read_first_last_and_step() {
    assert_identical(
        "ProgressionValues",
        "fun range(r: IntRange): Int { var s = 0; for (i in r) s += i; return s }\n\
         fun ints(p: IntProgression): Int { var s = 0; for (i in p) s += i; return s }\n\
         fun longs(p: LongProgression): Long { var s = 0L; for (i in p) s += i; return s }\n\
         fun chars(p: CharRange): Int { var s = 0; for (c in p) s += c - 'a'; return s }\n\
         fun charSteps(p: CharProgression): Int { var s = 0; for (c in p) s += c - 'a'; return s }\n\
         fun make(): IntRange = 1..3\n\
         fun called(): Int { var s = 0; for (i in make()) s += i; return s }\n",
    );
}

#[test]
fn a_progression_takes_its_most_precise_type() {
    assert_identical(
        "PreciseProgression",
        "fun declared(n: Int): Int { val r: IntProgression = 1..n; var s = 0; for (i in r) s += i; return s }\n\
         fun iterable(n: Int): Int { val r: Iterable<Int> = 1..n; var s = 0; for (i in r) s += i; return s }\n\
         fun reassignable(n: Int): Int { var r: IntProgression = 1..n; var s = 0; for (i in r) s += i; return s }\n\
         fun nullable(p: CharProgression?): Int { var s = 0; if (p != null) for (c in p) s += c - 'a'; return s }\n",
    );
}

#[test]
fn progression_values_still_iterate_correctly() {
    let src = "fun sum(p: IntProgression): Int { var s = 0; for (i in p) s += i; return s }\n\
               fun count(p: LongProgression): Int { var n = 0; for (i in p) n += 1; return n }\n\
               fun box(): String {\n\
               \x20   val down = sum(10 downTo 1 step 3)\n\
               \x20   val up = sum(1..10 step 4)\n\
               \x20   val empty = sum(5..4) + sum(1 downTo 2)\n\
               \x20   val top = count(Long.MAX_VALUE - 1..Long.MAX_VALUE)\n\
               \x20   val bottom = count(Long.MIN_VALUE + 4 downTo Long.MIN_VALUE step 2)\n\
               \x20   var chars = \"\"\n\
               \x20   val letters: CharProgression = 'a'..'e' step 2\n\
               \x20   for (c in letters) chars += c\n\
               \x20   return \"$down $up $empty $top $bottom $chars\"\n\
               }\n";
    let Some(actual) =
        common::compile_and_run_box(src, "progression_values", &[common::stdlib_jar()], None)
    else {
        eprintln!("skipping: JVM runner unavailable");
        return;
    };
    assert_eq!(actual, "22 15 0 2 3 ace");
}
