/* `value in progression` over a `ULongRange` whose walk crosses 2^63. Membership reduced the value
   and `first` modulo the step with SIGNED arithmetic, where a `ULong` above the signed maximum
   reads as a negative `Long` and lands on the wrong residue: `9223372036854775809uL in
   (0uL..ULong.MAX_VALUE step 3)` answered false, though the walk visits it. The signed and narrower
   progressions around it are checked too, since they share the one function. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL("range_contains_unsigned: " literal "\n");                                 \
        }                                                                                          \
    } while (0)

#define ULONG_MAX_BITS ((kt_long)UINT64_MAX)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    /* 2^63 + 1 is 0 modulo 3, so the walk 0, 3, 6, … reaches it; 2^63 is 2 modulo 3 and it does
       not. */
    KRef ascending = kt_range_step(kt_ulong_range(0, ULONG_MAX_BITS), 3);
    CHECK(kt_range_contains(ascending, (kt_long)0x8000000000000001u),
          "2^63 + 1 in (0uL..ULong.MAX_VALUE step 3) answered false");
    CHECK(!kt_range_contains(ascending, (kt_long)0x8000000000000000u),
          "2^63 in (0uL..ULong.MAX_VALUE step 3) answered true");
    CHECK(kt_range_contains(ascending, ULONG_MAX_BITS),
          "ULong.MAX_VALUE in (0uL..ULong.MAX_VALUE step 3) answered false");

    /* Descending from ULong.MAX_VALUE, which is 0 modulo 3: the walk comes down to 0 through 3,
       so a value below 2^63 on the step is a member and one off it is not. */
    KRef descending = kt_range_step(kt_ulong_range_down_to(ULONG_MAX_BITS, 0), 3);
    CHECK(kt_range_contains(descending, 3),
          "3uL in (ULong.MAX_VALUE downTo 0uL step 3) answered false");
    CHECK(!kt_range_contains(descending, 4),
          "4uL in (ULong.MAX_VALUE downTo 0uL step 3) answered true");
    CHECK(!kt_range_contains(descending, ULONG_MAX_BITS - 1),
          "ULong.MAX_VALUE - 1uL in (ULong.MAX_VALUE downTo 0uL step 3) answered true");

    /* A `UInt` walk: bounds and value arrive zero-extended, above the signed `Int` maximum. */
    KRef uints = kt_range_step(kt_uint_range(1, (kt_int)4000000000u), 7);
    CHECK(kt_range_contains(uints, 2999999997u), "2999999997u in (1u..4000000000u step 7)");
    CHECK(!kt_range_contains(uints, 3000000000u), "3000000000u in (1u..4000000000u step 7)");
    CHECK(kt_range_contains(kt_uint_range(1, (kt_int)4000000000u), 3000000000u),
          "3000000000u in 1u..4000000000u");

    /* The signed progressions keep their answers. */
    KRef evens = kt_range_step(kt_int_range_down_to(10, 1), 2);
    CHECK(!kt_range_contains(evens, 5), "5 in (10 downTo 1 step 2) answered true");
    CHECK(kt_range_contains(evens, 4), "4 in (10 downTo 1 step 2) answered false");
    KRef sevens = kt_range_step(kt_int_range(-10, 10), 7);
    CHECK(kt_range_contains(sevens, -3), "-3 in (-10..10 step 7) answered false");
    CHECK(!kt_range_contains(sevens, 10), "10 in (-10..10 step 7) answered true");
    KRef longs = kt_range_step(kt_long_range(INT64_MIN, INT64_MAX), 3);
    CHECK(kt_range_contains(longs, INT64_MIN + 3), "Long.MIN_VALUE + 3 in the full Long walk");
    CHECK(!kt_range_contains(longs, 0), "0 in (Long.MIN_VALUE..Long.MAX_VALUE step 3)");

    kt_sys_write(1, "OK\n", 3);
}
