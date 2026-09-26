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
//!     the direction taken from the step's sign unless the value is a `*Range`;
//!   * `step` checks its argument, follows the nested progression's direction, and moves `last` to
//!     the last element it reaches; `reversed` swaps first and last and negates the step;
//!   * an unsigned progression compares through `uintCompare`/`ulongCompare`, is never made
//!     exclusive, and copies the induction variable into its loop variable.
use super::common;

fn assert_identical(name: &str, src: &str) {
    let class = format!("{name}Kt");
    common::byte_diff_against_kotlinc_cp(name, src, &class, &[common::stdlib_jar()])
        .expect("the reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc:\n{diff}"));
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
    let actual =
        common::compile_and_run_box(src, "counted_loop_edges", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
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
    let actual =
        common::compile_and_run_box(src, "progression_values", &[common::stdlib_jar()], None)
            .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "22 15 0 2 3 ace");
}

#[test]
fn stepped_progressions_check_and_follow_their_step() {
    assert_identical(
        "SteppedProgressions",
        "fun byArgument(n: Int, s: Int): Int { var t = 0; for (i in 1..n step s) t += i; return t }\n\
         fun downBy(n: Int): Int { var t = 0; for (i in n downTo 1 step 2) t += i; return t }\n\
         fun unknownDirection(p: IntProgression, s: Int): Int { var t = 0; for (i in p step s) t += i; return t }\n\
         fun constant(): Int { var t = 0; for (i in 1..10 step 3) t += i; return t }\n\
         fun constantDown(): Int { var t = 0; for (i in 10 downTo 1 step 3) t += i; return t }\n\
         fun longs(n: Long): Long { var t = 0L; for (i in 0L..n step 2L) t += i; return t }\n\
         fun chars(c: Char): Int { var t = 0; for (x in 'a'..c step 2) t += x - 'a'; return t }\n\
         fun unit(n: Int): Int { var t = 0; for (i in 1..n step 1) t += i; return t }\n\
         fun until(n: Int): Int { var t = 0; for (i in 0 until n step 3) t += i; return t }\n\
         fun range(r: IntRange): Int { var t = 0; for (i in r step 2) t += i; return t }\n",
    );
}

#[test]
fn reversed_progressions_swap_their_bounds() {
    assert_identical(
        "ReversedProgressions",
        "fun constant(): Int { var t = 0; for (i in (1..10).reversed()) t += i; return t }\n\
         fun up(n: Int): Int { var t = 0; for (i in (1..n).reversed()) t += i; return t }\n\
         fun until(a: Int, b: Int): Int { var t = 0; for (i in (a until b).reversed()) t += i; return t }\n\
         fun down(a: Int, b: Int): Int { var t = 0; for (i in (a downTo b).reversed()) t += i; return t }\n\
         fun steppedFirst(n: Int): Int { var t = 0; for (i in (1..n step 2).reversed()) t += i; return t }\n\
         fun steppedAfter(n: Int, s: Int): Int { var t = 0; for (i in (1..n).reversed() step s) t += i; return t }\n\
         fun longs(n: Long): Long { var t = 0L; for (i in (0L..n).reversed()) t += i; return t }\n\
         fun chars(c: Char): Int { var t = 0; for (x in (c downTo 'a').reversed()) t += x - 'a'; return t }\n\
         fun value(p: IntProgression): Int { var t = 0; for (i in p.reversed()) t += i; return t }\n",
    );
}

/// A non-positive step throws before the loop runs, and the bounds are still evaluated in source
/// order once each.
#[test]
fn stepped_and_reversed_progressions_still_iterate_correctly() {
    let src = "fun steps(n: Int, s: Int): String { var t = \"\"; for (i in 1..n step s) t += i; return t }\n\
               fun reversedSteps(n: Int, s: Int): String { var t = \"\"; for (i in (1..n).reversed() step s) t += i; return t }\n\
               var log = \"\"\n\
               fun logged(v: Int): Int { log += v; return v }\n\
               fun box(): String {\n\
               \x20   val failure = try { steps(3, 0) } catch (e: IllegalArgumentException) { e.message }\n\
               \x20   var order = 0\n\
               \x20   for (i in (logged(1)..logged(4)).reversed() step logged(2)) order += i\n\
               \x20   var top = 0\n\
               \x20   for (i in Int.MAX_VALUE - 4..Int.MAX_VALUE step 2) top += 1\n\
               \x20   var chars = \"\"\n\
               \x20   for (c in ('a'..'g' step 3).reversed()) chars += c\n\
               \x20   return \"${steps(9, 4)} ${reversedSteps(9, 4)} $failure $order $log $top $chars\"\n\
               }\n";
    // `IllegalArgumentException` is a JDK class, so this compile needs the JDK's modules.
    let actual = common::compile_and_run_with_stdlib(src, "stepped_progressions")
        .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "159 951 Step must be positive, was: 0. 6 142 3 gda");
}

/// Instructions only: kotlinc inlines `UInt.compareTo` into the header, and the line entries an
/// inlined call leaves behind belong to inline-function debug info, not to the loop's shape.
#[test]
fn unsigned_progressions_compare_through_the_unsigned_comparator() {
    let src = "fun up(n: UInt) { for (i in 1u..n) use(i) }\n\
               fun stepped(n: UInt) { for (i in 1u..n step 2) use(i) }\n\
               fun value(p: UIntProgression) { for (i in p) use(i) }\n\
               fun range(r: UIntRange) { for (i in r) use(i) }\n\
               fun down(n: UInt) { for (i in n downTo 1u) use(i) }\n\
               fun reversed(n: ULong) { for (i in (0uL..n).reversed()) useLong(i) }\n\
               fun until(n: UInt) { for (i in 0u until n) use(i) }\n\
               fun constant() { for (i in 1u..10u) use(i) }\n\
               fun toZero() { for (i in 10u downTo 0u) use(i) }\n\
               fun longStep(n: ULong, s: Long) { for (i in 0uL..n step s) useLong(i) }\n\
               fun valueStep(p: UIntProgression, s: Int) { for (i in p step s) use(i) }\n\
               fun rangeUntil() { for (i in 0u..<10u) use(i) }\n\
               fun use(x: UInt) {}\n\
               fun useLong(x: ULong) {}\n";
    let comparison = common::compare_with_kotlinc_plugin(
        "UnsignedProgressions",
        src,
        "UnsignedProgressionsKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("the reference kotlinc is provisioned");
    for method in [
        " up-",
        " stepped-",
        " value(",
        " range(",
        " down-",
        " reversed-",
        " until-",
        " constant(",
        " toZero(",
        " longStep-",
        " valueStep(",
        " rangeUntil(",
    ] {
        let reference = common::method_instructions(&comparison.reference, method);
        assert!(!reference.is_empty(), "kotlinc emitted no{method}");
        assert_eq!(
            common::method_instructions(&comparison.krusty, method),
            reference,
            "{method} differs from kotlinc"
        );
    }
}

/// `uintCompare` orders values past the sign bit, and a bound at `MAX_VALUE` still ends the loop.
#[test]
fn unsigned_progressions_still_iterate_correctly() {
    let src = "fun sum(p: UIntProgression): UInt { var t = 0u; for (i in p) t = t * 10u + i; return t }\n\
               fun box(): String {\n\
               \x20   var reversed = 0u\n\
               \x20   for (i in (5u downTo 3u).reversed()) reversed = reversed * 10u + i\n\
               \x20   var stepped = 0u\n\
               \x20   for (i in (1u..9u).reversed() step 2) stepped = stepped * 10u + i\n\
               \x20   var top = 0\n\
               \x20   for (i in UInt.MAX_VALUE - 2u..UInt.MAX_VALUE) top += 1\n\
               \x20   var longTop = 0\n\
               \x20   for (i in ULong.MAX_VALUE - 4uL..ULong.MAX_VALUE step 2L) longTop += 1\n\
               \x20   var down = 0u\n\
               \x20   for (i in 2u downTo 0u) down = down * 10u + i\n\
               \x20   var until = 0u\n\
               \x20   for (i in 0u until 10u step 3) until = until * 10u + i\n\
               \x20   var bytes = 0\n\
               \x20   for (b in UByte.MAX_VALUE downTo UByte.MIN_VALUE step 2) bytes += 1\n\
               \x20   val failure = try { for (i in 1u..3u step 0) top += 1; \"no\" } catch (e: IllegalArgumentException) { e.message }\n\
               \x20   return \"$reversed $stepped $top $longTop $down $until ${sum(1u..7u step 3)} ${sum(7u downTo 1u step 3)} $bytes $failure\"\n\
               }\n";
    // `IllegalArgumentException` is a JDK class, so this compile needs the JDK's modules.
    let actual = common::compile_and_run_with_stdlib(src, "unsigned_progressions")
        .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(
        actual,
        "345 97531 3 3 210 369 147 741 128 Step must be positive, was: 0."
    );
}

/// The unsigned comparison a counted loop calls is the declaration the provider publishes in that
/// role, never whichever same-shaped declaration candidate order reaches first: a second
/// `uintCompare(Int, Int): Int` in `kotlin` on the classpath leaves the loop without a stable
/// target instead of redirecting it.
#[test]
fn a_duplicate_unsigned_compare_cannot_redirect_a_loop() {
    let duplicate = common::compile_lib(
        "duplicate_unsigned_compare",
        "package kotlin\n\
         @PublishedApi internal fun uintCompare(v1: Int, v2: Int): Int = 0\n",
    )
    .expect("the duplicate declaration compiles");
    let diagnostics = common::compile_in_process_diagnostics(
        "fun box(): String {\n    var sum = 0u\n    for (i in 1u..3u) sum += i\n    return if (sum == 6u) \"OK\" else \"fail\"\n}\n",
        "duplicate_unsigned_compare",
        &[duplicate, common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(
        diagnostics,
        [
            "internal error: checked FIR construction failed: Check(BodyCheckFailure { span: \
             Some(Span { lo: 51, hi: 53 }), kind: MissingStableCallTarget })"
        ]
    );
}
