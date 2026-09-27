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

   Every expected answer is Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM (each line printing `true`, `false` or `threw ArithmeticException: Index
   overflow has happened.`, recorded beside its case below):

       fun t(label: String, f: () -> Boolean) = println("$label " + try { f().toString() }
           catch (e: Throwable) { "threw ${e::class.simpleName}: ${e.message}" })
       fun main() {
           val first = 9223372036854775806uL
           val p = first..(first + 12uL) step 3
           val q = (first + 12uL) downTo first step 3
           t("") { 9223372036854775809uL in p }
           ...   // one `t` per case below, the expression its comment or call spells
       }

   and, for the walks of exactly 2^31 and 2^31 + 1 elements,

       t("fits -1") { -1L in (0L..2147483647L step 1) }          // false
       t("fits last") { 2147483647L in (0L..2147483647L step 1) } // true
       t("over last-1") { 2147483647L in (0L..2147483648L step 1) } // true
       t("over last") { 2147483648L in (0L..2147483648L step 1) }  // threw
       t("over -5") { -5L in (0L..2147483648L step 1) }            // threw */
#include "later_tiers.h"

#define ULONG_MAX_BITS ((kt_long)UINT64_MAX)
/* 2^63 + offset, as the bits of a `ULong`. */
#define TWO_63(offset) ((kt_long)(0x8000000000000000u + (uint64_t)(offset)))

enum { FALSE, TRUE, THROWS };

/* `value in range` answers `expected`: false, true, or Kotlin's index-overflow exception. */
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
    expect(p, TWO_63(1), TRUE);  /* 9223372036854775809uL in p */
    expect(p, TWO_63(0), FALSE); /* 9223372036854775808uL in p */
    expect(p, TWO_63(5), FALSE); /* 9223372036854775813uL in p */
    expect(p, TWO_63(6), FALSE); /* 9223372036854775814uL in p */
    KRef q = kt_range_step(kt_ulong_range_down_to(TWO_63(10), TWO_63(-2)), 3);
    expect(q, TWO_63(4), TRUE);  /* 9223372036854775812uL in q */
    expect(q, TWO_63(3), FALSE); /* 9223372036854775811uL in q */

    /* 0uL..ULong.MAX_VALUE step 3 and ULong.MAX_VALUE downTo 0uL step 3: far more than 2^31
       elements. */
    KRef ascending = kt_range_step(kt_ulong_range(0, ULONG_MAX_BITS), 3);
    expect(ascending, TWO_63(1), THROWS);
    expect(ascending, 3, TRUE);
    expect(ascending, ULONG_MAX_BITS, THROWS);
    KRef descending = kt_range_step(kt_ulong_range_down_to(ULONG_MAX_BITS, 0), 3);
    expect(descending, ULONG_MAX_BITS - 3, TRUE);
    expect(descending, 4, THROWS);

    /* A `UInt` walk: bounds and value arrive zero-extended, above the signed `Int` maximum. */
    KRef uints = kt_range_step(kt_uint_range(1, (kt_int)4000000000u), 7);
    expect(uints, 2999999997u, TRUE);
    expect(uints, 3000000000u, FALSE);
    expect(kt_uint_range(1, (kt_int)4000000000u), 3000000000u, TRUE);
    expect(kt_ulong_range(0, ULONG_MAX_BITS), ULONG_MAX_BITS, TRUE);

    /* The signed kinds. */
    KRef longs = kt_range_step(kt_long_range(INT64_MIN, INT64_MAX), 3);
    expect(longs, INT64_MIN + 3, TRUE);
    expect(longs, 0, THROWS);
    expect(kt_long_range(INT64_MIN, INT64_MAX), 0, TRUE);
    KRef evens = kt_range_step(kt_int_range_down_to(10, 1), 2);
    expect(evens, 5, FALSE);
    expect(evens, 4, TRUE);
    KRef sevens = kt_range_step(kt_int_range(-10, 10), 7);
    expect(sevens, -3, TRUE);
    expect(sevens, 10, FALSE);
    expect(kt_int_range_down_to(10, 1), 5, TRUE);
    expect(kt_range_step(kt_long_range(10, 0), 1), 5, FALSE);

    /* Walks of exactly 2^31 and 2^31 + 1 elements. */
    KRef fits = kt_range_step(kt_long_range(0, 2147483647), 1);
    expect(fits, -1, FALSE);
    expect(fits, 2147483647, TRUE);
    KRef over = kt_range_step(kt_long_range(0, 2147483648), 1);
    expect(over, 2147483647, TRUE);
    expect(over, 2147483648, THROWS);
    expect(over, -5, THROWS);

    kt_sys_write(1, "OK\n", 3);
}
