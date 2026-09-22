//! Members of a primitive, asked of a value that arrived as an OBJECT.
//!
//! Two questions with different answers about who knows what is in the box. `Number.toInt()` is
//! asked of a value the site could type only as a `Number`, so the runtime reads the descriptor;
//! `Int?.inc()` names the primitive in the member it selected, so the generator takes it at that
//! type and never consults the box at all.

use super::common::{expect_box_run_with_stdlib, expect_native_box};

#[test]
fn a_number_answers_its_conversions_from_the_descriptor() {
    expect_native_box(
        "fun test(n: Number) = n.toInt().toLong() + n.toLong()\n\
         fun box(): String {\n\
         \x20   val n: Number = 10\n\
         \x20   return if (test(n) == 20L) \"OK\" else \"fail: ${test(n)}\"\n\
         }\n",
        "NumberConversions",
        "OK",
    );
}

#[test]
fn a_number_holding_a_double_truncates_toward_zero() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val n: Number = 3.9\n\
         \x20   if (n.toInt() != 3) return \"fail int: ${n.toInt()}\"\n\
         \x20   if (n.toLong() != 3L) return \"fail long: ${n.toLong()}\"\n\
         \x20   if (n.toDouble() != 3.9) return \"fail double: ${n.toDouble()}\"\n\
         \x20   val negative: Number = -3.9\n\
         \x20   return if (negative.toInt() == -3) \"OK\" else \"fail negative: ${negative.toInt()}\"\n\
         }\n",
        "NumberFromADouble",
        "OK",
    );
}

#[test]
fn a_number_holding_a_double_saturates_and_answers_zero_for_nan() {
    // Kotlin's own rule, which is not C's cast: out of range clamps to the nearest end and `NaN`
    // is zero, where C leaves both undefined.
    expect_native_box(
        "fun box(): String {\n\
         \x20   val huge: Number = 1.0e30\n\
         \x20   if (huge.toInt() != Int.MAX_VALUE) return \"fail high: ${huge.toInt()}\"\n\
         \x20   val tiny: Number = -1.0e30\n\
         \x20   if (tiny.toInt() != Int.MIN_VALUE) return \"fail low: ${tiny.toInt()}\"\n\
         \x20   if (huge.toLong() != Long.MAX_VALUE) return \"fail high long: ${huge.toLong()}\"\n\
         \x20   val nan: Number = Double.NaN\n\
         \x20   if (nan.toInt() != 0) return \"fail nan: ${nan.toInt()}\"\n\
         \x20   return if (nan.toLong() == 0L) \"OK\" else \"fail nan long: ${nan.toLong()}\"\n\
         }\n",
        "NumberSaturates",
        "OK",
    );
}

#[test]
fn a_number_narrows_through_int_the_way_kotlin_defines_it() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val wide: Number = 258L\n\
         \x20   if (wide.toByte() != 2.toByte()) return \"fail byte: ${wide.toByte()}\"\n\
         \x20   if (wide.toShort() != 258.toShort()) return \"fail short: ${wide.toShort()}\"\n\
         \x20   val real: Number = 258.9\n\
         \x20   return if (real.toByte() == 2.toByte()) \"OK\" else \"fail real: ${real.toByte()}\"\n\
         }\n",
        "NumberNarrows",
        "OK",
    );
}

#[test]
fn a_boxed_int_steps_through_the_member_it_selected() {
    expect_native_box(
        "operator fun Int?.inc() = this!!.inc()\n\
         fun box(): String {\n\
         \x20   var i: Int? = 10\n\
         \x20   val j = i++\n\
         \x20   return if (j == 10 && 11 == i) \"OK\" else \"fail: $j $i\"\n\
         }\n",
        "BoxedIntSteps",
        "OK",
    );
}

#[test]
fn a_step_wraps_in_the_width_of_the_type_it_steps() {
    // `Byte.inc()` answers a `Byte`, so 127 steps to -128 rather than to 128.
    expect_native_box(
        "fun box(): String {\n\
         \x20   if (Byte.MAX_VALUE.inc() != Byte.MIN_VALUE) return \"fail byte: ${Byte.MAX_VALUE.inc()}\"\n\
         \x20   if (Short.MIN_VALUE.dec() != Short.MAX_VALUE) return \"fail short: ${Short.MIN_VALUE.dec()}\"\n\
         \x20   if (Int.MAX_VALUE.inc() != Int.MIN_VALUE) return \"fail int: ${Int.MAX_VALUE.inc()}\"\n\
         \x20   if (Long.MAX_VALUE.inc() != Long.MIN_VALUE) return \"fail long: ${Long.MAX_VALUE.inc()}\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "StepsWrap",
        "OK",
    );
}

#[test]
fn the_conversion_table_agrees_with_the_jvm_backend() {
    // The rules above are Kotlin's, not this generator's, so the other backend is the oracle:
    // `expect_box_run_with_stdlib` runs the program on the JVM and requires the native answer to
    // match. Every interesting case in one string, so a disagreement names which one it was.
    assert_eq!(
        expect_box_run_with_stdlib(
            "fun box(): String {\n\
             \x20   val huge: Number = 1.0e30\n\
             \x20   val tiny: Number = -1.0e30\n\
             \x20   val nan: Number = Double.NaN\n\
             \x20   val wide: Number = 258L\n\
             \x20   val real: Number = 258.9\n\
             \x20   return \"${huge.toInt()}/${tiny.toInt()}/${huge.toLong()}/${nan.toInt()}/\
             ${nan.toLong()}/${wide.toByte()}/${wide.toShort()}/${real.toByte()}\"\n\
             }\n",
            "NumberConversionTable",
        ),
        "2147483647/-2147483648/9223372036854775807/0/0/2/258/2"
    );
}
