/* `Double` and `Float` ranges: membership and emptiness by IEEE comparison, so a NaN bound makes a
   range empty and contains nothing; equality of two empty ranges whatever their bounds, and of
   bounds by `==`, so `0.0..1.0` equals `-0.0..1.0` while their hashes differ; a `Float` range
   never equal to a `Double` one; `hashCode` as `31 * start.hashCode() + end.hashCode()` at the
   bound's own width, `-1` when empty; and `toString` rendering each bound as its type does.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           val d = 1.0..2.0
           println("$d ${d.isEmpty()} ${1.0 in d} ${2.0 in d} ${1.5 in d} ${0.5 in d} ${Double.NaN in d} ${d.hashCode()} ${d.start} ${d.endInclusive}")
           val e = 2.0..1.0
           val n = Double.NaN..1.0
           val nn = Double.NaN..Double.NaN
           println("$e ${e.isEmpty()} ${e.hashCode()} $n ${n.isEmpty()} ${1.0 in n} $nn ${nn == nn} ${nn.hashCode()} ${e == n}")
           println("${d == (1.0..2.0)} ${(0.0..1.0) == (-0.0..1.0)} ${(0.0..1.0).hashCode()} ${(-0.0..1.0).hashCode()} ${d == (1.0..3.0)}")
           val f = 1.5f..2.5f
           println("$f ${f.isEmpty()} ${2.0f in f} ${2.6f in f} ${f.hashCode()} ${(f as Any) == (1.5..2.5)} ${f == (1.5f..2.5f)}")
           val g = 0.1f..0.2f
           println("$g ${g.hashCode()} ${(-0.0f..1.0f) == (0.0f..1.0f)} ${(1e10..1e-5).isEmpty()} ${(1e-5..1e10)}")
           println("${(Double.NEGATIVE_INFINITY..Double.POSITIVE_INFINITY)} ${Double.MAX_VALUE in (Double.NEGATIVE_INFINITY..Double.POSITIVE_INFINITY)}")
       }

   which prints
   `1.0..2.0 false true true true false false -32505856 1.0 2.0`,
   `2.0..1.0 true -1 NaN..1.0 true false NaN..NaN true -1 true`,
   `true true 1072693248 -1074790400 false`,
   `1.5..2.5 false true false -127926272 false true`,
   `0.1..0.2 -1172727392 true true 1.0E-5..1.0E10` and `-Infinity..Infinity true`. */
#include "later_tiers.h"

#define RENDERS(value, literal) text_is(kt_to_string(value), literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();
    const kt_double nan = __builtin_nan("");
    const kt_double infinity = __builtin_inf();

    KRef d = kt_double_range(1.0, 2.0);
    CHECK(type_of(d) == &kt_type_double_range, "a Double range's type\n");
    CHECK(RENDERS(d, "1.0..2.0") && !kt_floating_range_is_empty(d), "1.0..2.0\n");
    CHECK(kt_floating_range_contains(d, 1.0) && kt_floating_range_contains(d, 2.0) &&
              kt_floating_range_contains(d, 1.5) && !kt_floating_range_contains(d, 0.5) &&
              !kt_floating_range_contains(d, nan),
          "membership in 1.0..2.0\n");
    CHECK(kt_hash_code(d) == -32505856, "(1.0..2.0).hashCode()\n");
    CHECK(kt_floating_range_start(d) == 1.0 && kt_floating_range_end(d) == 2.0, "start, end\n");

    KRef e = kt_double_range(2.0, 1.0);
    KRef n = kt_double_range(nan, 1.0);
    KRef nn = kt_double_range(nan, nan);
    CHECK(RENDERS(e, "2.0..1.0") && kt_floating_range_is_empty(e) && kt_hash_code(e) == -1,
          "2.0..1.0\n");
    CHECK(RENDERS(n, "NaN..1.0") && kt_floating_range_is_empty(n) &&
              !kt_floating_range_contains(n, 1.0),
          "NaN..1.0\n");
    CHECK(RENDERS(nn, "NaN..NaN") && kt_equals(nn, nn) && kt_hash_code(nn) == -1, "NaN..NaN\n");
    CHECK(kt_equals(e, n), "two empty ranges are equal\n");

    CHECK(kt_equals(d, kt_double_range(1.0, 2.0)), "1.0..2.0 == 1.0..2.0\n");
    KRef zero = kt_double_range(0.0, 1.0);
    KRef negative_zero = kt_double_range(-0.0, 1.0);
    CHECK(kt_equals(zero, negative_zero), "0.0..1.0 == -0.0..1.0\n");
    CHECK(kt_hash_code(zero) == 1072693248 && kt_hash_code(negative_zero) == -1074790400,
          "the hashes of 0.0..1.0 and -0.0..1.0\n");
    CHECK(!kt_equals(d, kt_double_range(1.0, 3.0)), "1.0..2.0 == 1.0..3.0\n");

    KRef f = kt_float_range(1.5f, 2.5f);
    CHECK(type_of(f) == &kt_type_float_range, "a Float range's type\n");
    CHECK(RENDERS(f, "1.5..2.5") && !kt_floating_range_is_empty(f), "1.5f..2.5f\n");
    CHECK(kt_floating_range_contains(f, (kt_double)2.0f) &&
              !kt_floating_range_contains(f, (kt_double)2.6f),
          "membership in 1.5f..2.5f\n");
    CHECK(kt_hash_code(f) == -127926272, "(1.5f..2.5f).hashCode()\n");
    CHECK(!kt_equals(f, kt_double_range(1.5, 2.5)), "a Float range equals a Double one\n");
    CHECK(kt_equals(f, kt_float_range(1.5f, 2.5f)), "1.5f..2.5f == 1.5f..2.5f\n");

    KRef g = kt_float_range(0.1f, 0.2f);
    CHECK(RENDERS(g, "0.1..0.2") && kt_hash_code(g) == -1172727392, "0.1f..0.2f\n");
    CHECK(kt_equals(kt_float_range(-0.0f, 1.0f), kt_float_range(0.0f, 1.0f)),
          "-0.0f..1.0f == 0.0f..1.0f\n");
    CHECK(kt_floating_range_is_empty(kt_double_range(1e10, 1e-5)), "1e10..1e-5 is not empty\n");
    CHECK(RENDERS(kt_double_range(1e-5, 1e10), "1.0E-5..1.0E10"), "1e-5..1e10\n");
    KRef everything = kt_double_range(-infinity, infinity);
    CHECK(RENDERS(everything, "-Infinity..Infinity") &&
              kt_floating_range_contains(everything, 1.7976931348623157e308),
          "-Infinity..Infinity\n");

    CHECK(kt_pending_exception() == NULL, "a floating-point range raised\n");
    kt_sys_write(1, "OK\n", 3);
}
