/* `value in progression` and `value in range` at the extremes: unsigned bounds across 2^63, and
   walks longer than an `Int` can index.

   A progression has no `contains` of its own, so Kotlin's `in` on one is `Iterable.contains`: it
   walks with an `Int` index and throws `ArithmeticException("Index overflow has happened.")` on
   reaching index 2^31. A member at a smaller index is found, a walk of at most 2^31 elements ends
   without one, and any other question throws. The runtime answered every membership in constant
   time instead, so `0L in (Long.MIN_VALUE..Long.MAX_VALUE step 3)` said false where Kotlin throws.
   A range's own `contains` compares its bounds and never throws.

   Membership also reduced the value and `first` modulo the step with SIGNED arithmetic, where a
   `ULong` above 2^63 reads as a negative `Long` and lands on the wrong residue, so a short `ULong`
   walk across 2^63 misreported its members.

   The driver prints each answer, and the harness compares the lines with what
   `range_contains_unsigned.kt` answers under the reference kotlinc. The questions whose walk
   reaches index 2^31 cost the JVM 5 to 13 seconds each, past the harness's limit on one run, so
   the driver pins those answers, which the program lists beside the ones it runs. */
#include "transcript.h"

#define ULONG_MAX_BITS ((kt_long)UINT64_MAX)
/* 2^63 + offset, as the bits of a `ULong`. */
#define TWO_63(offset) ((kt_long)(0x8000000000000000u + (uint64_t)(offset)))

enum { FALSE, TRUE, THROWS };

/* `label` and the answer to `value in range`: `true`, `false`, or the exception it threw. */
static void ask(const char *label, KRef range, kt_long value) {
    kt_boolean answer = kt_range_contains(range, value);
    KRef thrown = kt_pending_exception();
    say(label);
    say(" ");
    if (thrown != NULL) {
        say_thrown(thrown);
        kt_clear_pending();
    } else {
        say_bool(answer);
    }
    say("\n");
}

/* `value in range` answers `expected`: false, true, or Kotlin's index-overflow exception. Only the
   questions too costly for the JVM to answer each run are pinned this way. */
static void expect(KRef range, kt_long value, int expected) {
    kt_boolean answer = kt_range_contains(range, value);
    KRef thrown = kt_pending_exception();
    if (expected == THROWS) {
        static const char message[] = "Index overflow has happened.";
        CHECK(thrown != NULL && type_of(thrown) == &kt_type_arithmetic_exception &&
                  text_is(kt_throwable_message(thrown), message, sizeof(message) - 1),
              "a walk past index 2^31 did not throw Kotlin's index overflow\n");
        kt_clear_pending();
        return;
    }
    CHECK(thrown == NULL, "a membership test threw\n");
    CHECK(answer == (expected == TRUE), "a membership test answered wrong\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    /* p = first..(first + 12uL) step 3 with first = 2^63 - 2, whose walk is 2^63-2, 2^63+1,
       2^63+4, 2^63+7, 2^63+10; q is the same walk descending. */
    KRef p = kt_range_step(kt_ulong_range(TWO_63(-2), TWO_63(10)), 3);
    ask("9223372036854775809uL in p", p, TWO_63(1));
    ask("9223372036854775808uL in p", p, TWO_63(0));
    ask("9223372036854775813uL in p", p, TWO_63(5));
    ask("9223372036854775814uL in p", p, TWO_63(6));
    KRef q = kt_range_step(kt_ulong_range_down_to(TWO_63(10), TWO_63(-2)), 3);
    ask("9223372036854775812uL in q", q, TWO_63(4));
    ask("9223372036854775811uL in q", q, TWO_63(3));

    /* 0uL..ULong.MAX_VALUE step 3 and ULong.MAX_VALUE downTo 0uL step 3: far more than 2^31
       elements. */
    KRef ascending = kt_range_step(kt_ulong_range(0, ULONG_MAX_BITS), 3);
    ask("3uL in (0uL..ULong.MAX_VALUE step 3)", ascending, 3);
    KRef descending = kt_range_step(kt_ulong_range_down_to(ULONG_MAX_BITS, 0), 3);
    ask("ULong.MAX_VALUE - 3uL in (ULong.MAX_VALUE downTo 0uL step 3)", descending,
        ULONG_MAX_BITS - 3);

    /* A `UInt` walk: bounds and value arrive zero-extended, above the signed `Int` maximum. */
    KRef uints = kt_range_step(kt_uint_range(1, (kt_int)4000000000u), 7);
    ask("2999999997u in (1u..4000000000u step 7)", uints, 2999999997u);
    ask("3000000000u in (1u..4000000000u step 7)", uints, 3000000000u);
    ask("3000000000u in 1u..4000000000u", kt_uint_range(1, (kt_int)4000000000u), 3000000000u);
    ask("ULong.MAX_VALUE in 0uL..ULong.MAX_VALUE", kt_ulong_range(0, ULONG_MAX_BITS),
        ULONG_MAX_BITS);

    /* The signed kinds. */
    KRef longs = kt_range_step(kt_long_range(INT64_MIN, INT64_MAX), 3);
    ask("Long.MIN_VALUE + 3 in (Long.MIN_VALUE..Long.MAX_VALUE step 3)", longs, INT64_MIN + 3);
    ask("0L in Long.MIN_VALUE..Long.MAX_VALUE", kt_long_range(INT64_MIN, INT64_MAX), 0);
    KRef evens = kt_range_step(kt_int_range_down_to(10, 1), 2);
    ask("5 in (10 downTo 1 step 2)", evens, 5);
    ask("4 in (10 downTo 1 step 2)", evens, 4);
    KRef sevens = kt_range_step(kt_int_range(-10, 10), 7);
    ask("-3 in (-10..10 step 7)", sevens, -3);
    ask("10 in (-10..10 step 7)", sevens, 10);
    ask("5 in 10 downTo 1", kt_int_range_down_to(10, 1), 5);
    ask("5L in (10L..0L step 1)", kt_range_step(kt_long_range(10, 0), 1), 5);

    /* The walks that reach index 2^31, pinned (see `range_contains_unsigned.kt`): the three
       progressions above, and walks of exactly 2^31 and 2^31 + 1 elements. */
    expect(ascending, TWO_63(1), THROWS);
    expect(ascending, ULONG_MAX_BITS, THROWS);
    expect(descending, 4, THROWS);
    expect(longs, 0, THROWS);
    KRef fits = kt_range_step(kt_long_range(0, 2147483647), 1);
    expect(fits, -1, FALSE);
    expect(fits, 2147483647, TRUE);
    KRef over = kt_range_step(kt_long_range(0, 2147483648), 1);
    expect(over, 2147483647, TRUE);
    expect(over, 2147483648, THROWS);
    expect(over, -5, THROWS);

    kt_sys_write(1, "OK\n", 3);
}
