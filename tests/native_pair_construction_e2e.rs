//! `Pair(a, b)` — the two-field carrier written as a constructor.
//!
//! The runtime owns `Pair`: no file declares it, and one is already built for every `a to b` and
//! for each step of a `withIndex` walk. The written constructor reaches that same object rather
//! than a second shape of it, which is what lets a pair built one way be read, destructured and
//! compared the same as a pair built the other.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// The written constructor, read back through `first` and `second`.
#[test]
fn a_written_pair_carries_both_operands() {
    let source = "fun box(): String {\n\
         \x20   val p = Pair(\"O\", \"K\")\n\
         \x20   return p.first + p.second\n\
         }\n";
    every_backend_agrees_with_kotlinc("WrittenPair", source);
}

/// It is the SAME object `a to b` builds, so the two are equal and read alike.
#[test]
fn a_written_pair_is_the_pair_the_infix_form_builds() {
    let source = "fun box(): String {\n\
         \x20   val written = Pair(1, \"x\")\n\
         \x20   val infix = 1 to \"x\"\n\
         \x20   if (written != infix) return \"fail equal\"\n\
         \x20   if (written.first != infix.first) return \"fail first\"\n\
         \x20   if (written.second != infix.second) return \"fail second\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WrittenPairEqualsInfix", source);
}

/// DESTRUCTURING reads the same two fields, through `component1`/`component2`.
#[test]
fn a_written_pair_destructures() {
    let source = "fun box(): String {\n\
         \x20   val (first, second) = Pair(\"O\", \"K\")\n\
         \x20   return first + second\n\
         }\n";
    every_backend_agrees_with_kotlinc("WrittenPairDestructures", source);
}

/// A pair of PRIMITIVES: both operands cross as references, which is what its fields hold, and
/// each reads back as the value it was given.
#[test]
fn a_written_pair_of_primitives_boxes_both_operands() {
    let source = "fun box(): String {\n\
         \x20   val p = Pair(40, 2)\n\
         \x20   if (p.first != 40) return \"fail first\"\n\
         \x20   if (p.second != 2) return \"fail second\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WrittenPairOfPrimitives", source);
}

/// Nested: a pair's field may hold another pair, which reads back through a binding of its own.
///
/// Through a `val` rather than as `p.second.first`, because reading a member of a pair that is
/// itself a pair's FIELD is a separate gap in this generator — the receiver's type does not reach
/// the pair reader — and it has nothing to do with how the pair was constructed.
#[test]
fn a_written_pair_may_hold_another() {
    let source = "fun box(): String {\n\
         \x20   val p = Pair(\"O\", Pair(\"K\", \"!\"))\n\
         \x20   val inner: Pair<String, String> = p.second\n\
         \x20   return p.first + inner.first\n\
         }\n";
    every_backend_agrees_with_kotlinc("NestedWrittenPair", source);
}

/// `toString` is the pair's own rendering, which is how a failure would show a wrong shape.
#[test]
fn a_written_pair_renders_as_kotlin_renders_one() {
    let source = "fun box(): String {\n\
         \x20   val text = Pair(1, 2).toString()\n\
         \x20   return if (text == \"(1, 2)\") \"OK\" else \"fail \" + text\n\
         }\n";
    every_backend_agrees_with_kotlinc("WrittenPairRenders", source);
}
