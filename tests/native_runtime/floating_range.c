/* `Double` and `Float` ranges: membership and emptiness by IEEE comparison, so a NaN bound makes a
   range empty and contains nothing; equality of two empty ranges whatever their bounds, and of
   bounds by `==`, so `0.0..1.0` equals `-0.0..1.0` while their hashes differ; a `Float` range
   never equal to a `Double` one; `hashCode` as `31 * start.hashCode() + end.hashCode()` at the
   bound's own width, `-1` when empty; and `toString` rendering each bound as its type does.

   The driver prints each answer, a range and a bound rendered by the runtime, and the harness
   compares the lines with what `floating_range.kt` answers under the reference kotlinc. */
#include "transcript.h"

static void say_contains(KRef range, kt_double value) {
    say(" ");
    say_bool(kt_floating_range_contains(range, value));
}

static void say_equals(KRef a, KRef b) {
    say(" ");
    say_bool(kt_equals(a, b));
}

static void say_hash_code(KRef range) {
    say(" ");
    say_long(kt_hash_code(range));
}

static void say_is_empty(KRef range) {
    say(" ");
    say_bool(kt_floating_range_is_empty(range));
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    const kt_double nan = __builtin_nan("");
    const kt_double infinity = __builtin_inf();

    KRef d = kt_double_range(1.0, 2.0);
    CHECK(type_of(d) == &kt_type_double_range, "a Double range's type\n");
    say_value(d);
    say_is_empty(d);
    say_contains(d, 1.0);
    say_contains(d, 2.0);
    say_contains(d, 1.5);
    say_contains(d, 0.5);
    say_contains(d, nan);
    say_hash_code(d);
    say(" ");
    say_value(kt_box_double(kt_floating_range_start(d)));
    say(" ");
    say_value(kt_box_double(kt_floating_range_end(d)));
    say("\n");

    KRef e = kt_double_range(2.0, 1.0);
    KRef n = kt_double_range(nan, 1.0);
    KRef nn = kt_double_range(nan, nan);
    say_value(e);
    say_is_empty(e);
    say_hash_code(e);
    say(" ");
    say_value(n);
    say_is_empty(n);
    say_contains(n, 1.0);
    say(" ");
    say_value(nn);
    say_equals(nn, nn);
    say_hash_code(nn);
    say_equals(e, n);
    say("\n");

    KRef zero = kt_double_range(0.0, 1.0);
    KRef negative_zero = kt_double_range(-0.0, 1.0);
    say_bool(kt_equals(d, kt_double_range(1.0, 2.0)));
    say_equals(zero, negative_zero);
    say_hash_code(zero);
    say_hash_code(negative_zero);
    say_equals(d, kt_double_range(1.0, 3.0));
    say("\n");

    KRef f = kt_float_range(1.5f, 2.5f);
    CHECK(type_of(f) == &kt_type_float_range, "a Float range's type\n");
    say_value(f);
    say_is_empty(f);
    say_contains(f, (kt_double)2.0f);
    say_contains(f, (kt_double)2.6f);
    say_hash_code(f);
    say_equals(f, kt_double_range(1.5, 2.5));
    say_equals(f, kt_float_range(1.5f, 2.5f));
    say("\n");

    KRef g = kt_float_range(0.1f, 0.2f);
    say_value(g);
    say_hash_code(g);
    say_equals(kt_float_range(-0.0f, 1.0f), kt_float_range(0.0f, 1.0f));
    say_is_empty(kt_double_range(1e10, 1e-5));
    say(" ");
    say_value(kt_double_range(1e-5, 1e10));
    say("\n");

    KRef everything = kt_double_range(-infinity, infinity);
    say_value(everything);
    say_contains(everything, 1.7976931348623157e308);
    say("\n");

    CHECK(kt_pending_exception() == NULL, "a floating-point range raised\n");
    kt_sys_write(1, "OK\n", 3);
}
