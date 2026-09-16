//! What a `Double` and a `Float` LOOK like.
//!
//! Kotlin's `toString` for a floating-point value is the SHORTEST decimal that reads back as
//! exactly that value, in a shape with two branches: a plain decimal while the magnitude is in
//! `[10^-3, 10^7)`, and `d.dddEn` outside it. Neither half is a formatting preference — a program
//! prints these, and two compilers that agree on everything else must agree on them.
//!
//! The cases below are the ones where a wrong algorithm shows: a value with no exact binary form
//! (`0.1`), one that needs all seventeen digits, the two ends of each format branch, the subnormal
//! extremes (where the shortest decimal is NOT the answer — see `krusty_fp.c`), and the
//! non-finite values. Run on both backends, so the answer is one answer.

use super::common;

fn run_ok(stem: &str, body: &str) {
    common::expect_box_same_as_kotlinc(body, stem);
}

#[test]
fn doubles_render_as_the_shortest_decimal_that_reads_back() {
    run_ok(
        "DoubleText",
        "fun box(): String {\n\
         \x20   if (0.0.toString() != \"0.0\") return \"0.0: \" + 0.0.toString()\n\
         \x20   if ((-0.0).toString() != \"-0.0\") return \"-0.0: \" + (-0.0).toString()\n\
         \x20   if (1.0.toString() != \"1.0\") return \"1.0: \" + 1.0.toString()\n\
         \x20   if ((-1.5).toString() != \"-1.5\") return \"-1.5: \" + (-1.5).toString()\n\
         \x20   if (0.1.toString() != \"0.1\") return \"0.1: \" + 0.1.toString()\n\
         \x20   if ((0.1 + 0.2).toString() != \"0.30000000000000004\") return \"0.30000000000000004: \" + (0.1 + 0.2).toString()\n\
         \x20   if ((1.0 / 3.0).toString() != \"0.3333333333333333\") return \"0.3333333333333333: \" + (1.0 / 3.0).toString()\n\
         \x20   if (3.141592653589793.toString() != \"3.141592653589793\") return \"3.141592653589793: \" + 3.141592653589793.toString()\n\
         \x20   if (100.0.toString() != \"100.0\") return \"100.0: \" + 100.0.toString()\n\
         \x20   if (9999999.0.toString() != \"9999999.0\") return \"9999999.0: \" + 9999999.0.toString()\n\
         \x20   if (1.0E7.toString() != \"1.0E7\") return \"1.0E7: \" + 1.0E7.toString()\n\
         \x20   if (0.001.toString() != \"0.001\") return \"0.001: \" + 0.001.toString()\n\
         \x20   if (1.0E-4.toString() != \"1.0E-4\") return \"1.0E-4: \" + 1.0E-4.toString()\n\
         \x20   if (1.0E20.toString() != \"1.0E20\") return \"1.0E20: \" + 1.0E20.toString()\n\
         \x20   if (Double.MAX_VALUE.toString() != \"1.7976931348623157E308\") return \"1.7976931348623157E308: \" + Double.MAX_VALUE.toString()\n\
         \x20   if (Double.MIN_VALUE.toString() != \"4.9E-324\") return \"4.9E-324: \" + Double.MIN_VALUE.toString()\n\
         \x20   if ((1.0 / 0.0).toString() != \"Infinity\") return \"Infinity: \" + (1.0 / 0.0).toString()\n\
         \x20   if ((-1.0 / 0.0).toString() != \"-Infinity\") return \"-Infinity: \" + (-1.0 / 0.0).toString()\n\
         \x20   if ((0.0 / 0.0).toString() != \"NaN\") return \"NaN: \" + (0.0 / 0.0).toString()\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn floats_render_as_the_shortest_decimal_that_reads_back() {
    run_ok(
        "FloatText",
        "fun box(): String {\n\
         \x20   if (0.0f.toString() != \"0.0\") return \"0.0: \" + 0.0f.toString()\n\
         \x20   if (1.0f.toString() != \"1.0\") return \"1.0: \" + 1.0f.toString()\n\
         \x20   if (0.1f.toString() != \"0.1\") return \"0.1: \" + 0.1f.toString()\n\
         \x20   if ((1.0f / 3.0f).toString() != \"0.33333334\") return \"0.33333334: \" + (1.0f / 3.0f).toString()\n\
         \x20   if (1.0E7f.toString() != \"1.0E7\") return \"1.0E7: \" + 1.0E7f.toString()\n\
         \x20   if (0.001f.toString() != \"0.001\") return \"0.001: \" + 0.001f.toString()\n\
         \x20   if (Float.MAX_VALUE.toString() != \"3.4028235E38\") return \"3.4028235E38: \" + Float.MAX_VALUE.toString()\n\
         \x20   if (Float.MIN_VALUE.toString() != \"1.4E-45\") return \"1.4E-45: \" + Float.MIN_VALUE.toString()\n\
         \x20   if ((1.0f / 0.0f).toString() != \"Infinity\") return \"Infinity: \" + (1.0f / 0.0f).toString()\n\
         \x20   if ((0.0f / 0.0f).toString() != \"NaN\") return \"NaN: \" + (0.0f / 0.0f).toString()\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}

#[test]
fn a_boxed_floating_point_value_keeps_kotlins_equality_and_hash() {
    // `equals` on a boxed `Double` compares BITS and `==` on two `Double`s compares values, which
    // is the one place the two rules visibly disagree: `NaN` equals itself as a box and does not
    // as a number, and `0.0` and `-0.0` the other way round.
    run_ok(
        "DoubleBox",
        "fun box(): String {\n\
         \x20   val nan: Any = 0.0 / 0.0\n\
         \x20   val zero: Any = 0.0\n\
         \x20   val minusZero: Any = -0.0\n\
         \x20   if (nan != nan) return \"fail: NaN box\"\n\
         \x20   if (zero == minusZero) return \"fail: signed zero box\"\n\
         \x20   if (0.0 / 0.0 == 0.0 / 0.0) return \"fail: NaN value\"\n\
         \x20   if (0.0 != -0.0) return \"fail: signed zero value\"\n\
         \x20   if (zero.hashCode() != 0.0.hashCode()) return \"fail: hash\"\n\
         \x20   if (\"$nan\" != \"NaN\") return \"fail: template $nan\"\n\
         \x20   return \"OK\"\n\
         }\n",
    );
}
