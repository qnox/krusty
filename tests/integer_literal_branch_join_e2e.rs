//! An integer-constant branch joins a sibling primitive even when the conditional has no expected
//! type. `if (flag) current() - start else 0` is `Long`, not `Any`.
//!
//! Each form is its own test so one protocol cannot hide behind an earlier `return` label.

use super::common;

fn both_compilers_box(source: &str, stem: &str) {
    common::assert_accepted_like_kotlinc(source);
    let reference = common::kotlinc_box_result(source);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_with_stdlib(source, stem);
}

fn current() -> &'static str {
    "fun current(): Long = 0\n"
}

#[test]
fn if_expression_adapts_an_integer_constant_to_long() {
    both_compilers_box(
        &format!(
            "{}\
             fun localDelta(flag: Boolean, start: Long): Long {{\n\
             val delta = if (flag) current() - start else 0\n\
             return delta\n\
             }}\n\
             fun exprDelta(flag: Boolean, start: Long) = if (flag) current() - start else 0\n\
             fun box(): String {{\n\
             if (localDelta(true, 5L) != -5L) return \"local\"\n\
             if (localDelta(false, 5L) != 0L) return \"local0\"\n\
             if (exprDelta(true, 5L) != -5L) return \"expr\"\n\
             if (exprDelta(false, 5L) != 0L) return \"expr0\"\n\
             return \"OK\"\n\
             }}\n",
            current()
        ),
        "if_expression_adapts_an_integer_constant_to_long",
    );
}

#[test]
fn when_adapts_an_integer_constant_including_leading_literals() {
    both_compilers_box(
        &format!(
            "{}\
             fun whenDelta(flag: Boolean, start: Long) = when {{\n\
             flag -> current() - start\n\
             else -> 0\n\
             }}\n\
             fun literalsFirst(flag: Boolean, other: Boolean, start: Long) = when {{\n\
             flag -> 0\n\
             other -> 1\n\
             else -> current() - start\n\
             }}\n\
             fun box(): String {{\n\
             if (whenDelta(false, 5L) != 0L) return \"when\"\n\
             if (literalsFirst(true, false, 5L) != 0L) return \"order\"\n\
             if (literalsFirst(false, false, 5L) != -5L) return \"order2\"\n\
             return \"OK\"\n\
             }}\n",
            current()
        ),
        "when_adapts_an_integer_constant_including_leading_literals",
    );
}

#[test]
fn nested_if_adapts_integer_constants() {
    both_compilers_box(
        &format!(
            "{}\
             fun nested(flag: Boolean, other: Boolean, start: Long) =\n\
             if (flag) current() - start else if (other) 1 else 2\n\
             fun box(): String {{\n\
             if (nested(false, true, 5L) != 1L) return \"nested\"\n\
             if (nested(false, false, 5L) != 2L) return \"nested0\"\n\
             return \"OK\"\n\
             }}\n",
            current()
        ),
        "nested_if_adapts_integer_constants",
    );
}

#[test]
fn block_branch_adapts_an_integer_constant() {
    both_compilers_box(
        &format!(
            "{}\
             fun blocked(flag: Boolean, start: Long) =\n\
             if (flag) current() - start else {{ val ignored = 1; 0 }}\n\
             fun box(): String {{\n\
             if (blocked(false, 5L) != 0L) return \"block\"\n\
             return \"OK\"\n\
             }}\n",
            current()
        ),
        "block_branch_adapts_an_integer_constant",
    );
}

#[test]
fn narrow_sibling_primitives_adapt_an_integer_constant() {
    both_compilers_box(
        "fun byteZero(flag: Boolean, value: Byte) = if (flag) value else 0\n\
         fun shortZero(flag: Boolean, value: Short) = if (flag) value else 0\n\
         fun box(): String {\n\
         if (byteZero(false, 7) != 0.toByte()) return \"byte\"\n\
         if (byteZero(true, 7) != 7.toByte()) return \"byte7\"\n\
         if (shortZero(false, 7) != 0.toShort()) return \"short\"\n\
         return \"OK\"\n\
         }\n",
        "narrow_sibling_primitives_adapt_an_integer_constant",
    );
}

#[test]
fn nullable_long_branch_adapts_an_integer_constant() {
    both_compilers_box(
        "fun nullable(flag: Boolean, value: Long?) = if (flag) value else 0\n\
         fun box(): String {\n\
         if (nullable(false, 5L) != 0L) return \"nullbr\"\n\
         if (nullable(true, null) != null) return \"nullbr2\"\n\
         return \"OK\"\n\
         }\n",
        "nullable_long_branch_adapts_an_integer_constant",
    );
}

#[test]
fn elvis_adapts_an_integer_constant() {
    both_compilers_box(
        "fun elvis(value: Long?) = value ?: 0\n\
         fun box(): String {\n\
         if (elvis(null) != 0L) return \"elvis\"\n\
         if (elvis(4L) != 4L) return \"elvis4\"\n\
         return \"OK\"\n\
         }\n",
        "elvis_adapts_an_integer_constant",
    );
}

#[test]
fn try_adapts_an_integer_constant() {
    both_compilers_box(
        "fun current(): Long = 0\n\
         fun tried() = try { current() } catch (e: Exception) { 0 }\n\
         fun triedLocal(): Long {\n\
         val value = try { current() } catch (e: Exception) { 0 }\n\
         return value\n\
         }\n\
         fun box(): String {\n\
         if (tried() != 0L) return \"try\"\n\
         if (triedLocal() != 0L) return \"trylocal\"\n\
         return \"OK\"\n\
         }\n",
        "try_adapts_an_integer_constant",
    );
}

#[test]
fn unsigned_constant_adapts_to_ulong() {
    both_compilers_box(
        "fun ulongZero(flag: Boolean, value: ULong) = if (flag) value else 0u\n\
         fun box(): String {\n\
         if (ulongZero(false, 7uL) != 0uL) return \"ulong\"\n\
         return \"OK\"\n\
         }\n",
        "unsigned_constant_adapts_to_ulong",
    );
}

/// A folded unsigned sum is not a `ULong` result. The reference compiler rejects an explicit
/// return; inferring one lowers the sum as `Long` and throws.
#[test]
fn folded_unsigned_sum_is_not_a_ulong_result() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "fun folded(flag: Boolean, value: ULong): ULong =\n\
                 if (flag) value else 65535u + 1u\n",
        )],
        &[],
    );
}

#[test]
fn folded_unsigned_constant_outside_ushort_is_not_adapted() {
    both_compilers_box(
        "fun narrowFolded(flag: Boolean, value: UShort) =\n\
             if (flag) value else 65535u + 1u\n\
         fun classify(value: UShort) = \"ushort\"\n\
         fun classify(value: UInt) = \"uint\"\n\
         fun classify(value: Any) = \"any\"\n\
         fun box(): String {\n\
         if (classify(narrowFolded(false, 1u)) != \"any\") return \"narrow-folded\"\n\
         return \"OK\"\n\
         }\n",
        "folded_unsigned_constant_outside_ushort_is_not_adapted",
    );
}

#[test]
fn unsigned_constant_above_i32_max_adapts_to_ulong() {
    both_compilers_box(
        "fun above(flag: Boolean, value: ULong) = if (flag) value else 2147483648u\n\
         fun box(): String {\n\
         if (above(false, 7uL) != 2147483648uL) return \"above\"\n\
         if (above(true, 9uL) != 9uL) return \"above9\"\n\
         return \"OK\"\n\
         }\n",
        "unsigned_constant_above_i32_max_adapts_to_ulong",
    );
}

#[test]
fn uint_max_constant_adapts_to_ulong() {
    both_compilers_box(
        "fun atMax(flag: Boolean, value: ULong) = if (flag) value else 4294967295u\n\
         fun box(): String {\n\
         if (atMax(false, 7uL) != 4294967295uL) return \"max\"\n\
         return \"OK\"\n\
         }\n",
        "uint_max_constant_adapts_to_ulong",
    );
}

#[test]
fn unsigned_constant_above_i32_max_does_not_adapt_to_ushort() {
    both_compilers_box(
        "fun narrow(flag: Boolean, value: UShort) = if (flag) value else 2147483648u\n\
         fun box(): String {\n\
         if (narrow(false, 1u).toString() != \"2147483648\") return \"narrow\"\n\
         return \"OK\"\n\
         }\n",
        "unsigned_constant_above_i32_max_does_not_adapt_to_ushort",
    );
}

#[test]
fn long_literal_beside_an_int_constant_stays_long() {
    both_compilers_box(
        "fun longLiteral(flag: Boolean) = if (flag) 1L else 0\n\
         fun box(): String {\n\
         if (longLiteral(false) != 0L) return \"lit\"\n\
         return \"OK\"\n\
         }\n",
        "long_literal_beside_an_int_constant_stays_long",
    );
}

#[test]
fn out_of_range_constant_is_not_adapted() {
    both_compilers_box(
        "fun doesNotFit(flag: Boolean, value: Byte) = if (flag) value else 200\n\
         fun box(): String {\n\
         if (doesNotFit(false, 1).toString() != \"200\") return \"fit\"\n\
         return \"OK\"\n\
         }\n",
        "out_of_range_constant_is_not_adapted",
    );
}

#[test]
fn unadapted_integer_constants_prefer_the_int_overload() {
    both_compilers_box(
        "fun preferInt(x: Int) = \"int\"\n\
         fun preferInt(x: Long) = \"long\"\n\
         fun box(): String {\n\
         if (preferInt(if (true) 1 else 2) != \"int\") return \"prefer\"\n\
         return \"OK\"\n\
         }\n",
        "unadapted_integer_constants_prefer_the_int_overload",
    );
}

#[test]
fn same_spelled_user_unary_member_is_not_an_integer_constant() {
    common::assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            "class Counter {\n\
                 fun unaryMinus(): Int = 1\n\
             }\n\
             fun narrowed(counter: Counter): Byte = counter.unaryMinus()\n",
        )],
        &[],
    );
}
