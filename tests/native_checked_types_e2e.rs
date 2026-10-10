//! The Native generator reads an expression's type from the frontend, not from the node's shape.
//!
//! Each program puts a value whose CHECKED type is a concrete Kotlin type behind a node whose
//! PHYSICAL form is a reference: the result of a generic function, whose declared return is a type
//! parameter. Which member answers, what a `when` merges to and which scalar is read all follow the
//! checked type; only the carrier conversion follows the physical one.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A `when` whose arms are both erased generic results merges at its checked `Int`, so the sum
/// after it is integer arithmetic rather than a question about two references.
#[test]
fn a_when_over_generic_results_merges_at_its_checked_type() {
    let source = "fun <T> id(x: T): T = x\n\
         fun pick(flag: Boolean): Int = when (flag) {\n\
         \x20   true -> id(40)\n\
         \x20   else -> id(1)\n\
         }\n\
         fun box(): String {\n\
         \x20   val sum = pick(true) + pick(false) + 1\n\
         \x20   return if (sum == 42) \"OK\" else \"fail $sum\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("WhenOverGenericResults", source);
}

/// A range reached through a generic function is still a range: the receiver's checked type
/// selects the range's own `contains`, where the erased result alone names no range at all.
#[test]
fn a_range_behind_a_generic_result_is_asked_as_a_range() {
    let source = "fun <T> id(x: T): T = x\n\
         fun box(): String {\n\
         \x20   val range = id(1..5)\n\
         \x20   if (!(3 in id(1..5))) return \"fail in\"\n\
         \x20   if (6 in range) return \"fail out\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("RangeBehindGenericResult", source);
}

/// `!` on a `Boolean` that arrives boxed from a generic function: the checked type says it is a
/// `Boolean`, and the operand is unboxed to be negated.
#[test]
fn a_boolean_behind_a_generic_result_is_negated() {
    let source = "fun <T> id(x: T): T = x\n\
         fun box(): String {\n\
         \x20   if (!id(false)) return \"OK\"\n\
         \x20   return \"fail\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("BooleanBehindGenericResult", source);
}

/// The checked-type contract covers both erased library reads and nodes synthesized after FIR.
/// `Pair.first` physically returns a reference, while its checked scalar type selects arithmetic,
/// unsigned ordering and IEEE equality. The remaining checks exercise each producer which must
/// explicitly type its synthesized `when` or delegate-storage read.
#[test]
fn erased_scalars_and_synthesized_nodes_keep_their_checked_types() {
    let source = "data class NullableHash(val value: Int?)\n\
         class Added(private val delta: Int) {\n\
         \x20   operator fun getValue(thisRef: Int, property: Any?): Int = thisRef + delta\n\
         }\n\
         val Int.topShifted by Added(1)\n\
         class Scope {\n\
         \x20   val Int.memberShifted by Added(2)\n\
         \x20   fun read(value: Int): Int = value.memberShifted\n\
         }\n\
         fun castOrForty(value: Any): Int = (value as? Int) ?: 40\n\
         fun sameNullable(left: Int?, right: Int?): Boolean = left === right\n\
         fun steppedSum(progression: IntProgression, amount: Int): Int {\n\
         \x20   var total = 0\n\
         \x20   for (value in progression step amount) total += value\n\
         \x20   return total\n\
         }\n\
         fun box(): String {\n\
         \x20   val integer = Pair(40, \"integer\").first\n\
         \x20   if (integer + 2 != 42) return \"int\"\n\
         \x20   val unsigned = Pair(UInt.MAX_VALUE, \"unsigned\").first\n\
         \x20   if (unsigned <= 0x7fffffffu) return \"uint\"\n\
         \x20   val nan = Pair(Double.NaN, \"nan\").first\n\
         \x20   if (nan == nan) return \"nan\"\n\
         \x20   val negativeZero = Pair(-0.0, \"zero\").first\n\
         \x20   if (negativeZero != 0.0) return \"zero\"\n\
         \x20   if (!sameNullable(null, null)) return \"nullable identity\"\n\
         \x20   if (NullableHash(null).hashCode() != 0) return \"null hash\"\n\
         \x20   if (NullableHash(7).hashCode() != 7) return \"value hash\"\n\
         \x20   if (castOrForty(\"no\") + castOrForty(1) != 41) return \"safe cast\"\n\
         \x20   if (steppedSum(1..5, 2) != 9) return \"ascending step\"\n\
         \x20   if (steppedSum(5 downTo 1, 2) != 9) return \"descending step\"\n\
         \x20   if (40.topShifted != 41) return \"top delegate\"\n\
         \x20   if (Scope().read(40) != 42) return \"member delegate\"\n\
         \x20   return \"OK\"\n\
         }\n";
    every_backend_agrees_with_kotlinc("CheckedTypeProducerFamilies", source);
}
