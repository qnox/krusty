/* `a until 0u` is empty, and the empty range Kotlin answers is its declared `EMPTY`: for
   `UIntRange` that is `UInt.MAX_VALUE..UInt.MIN_VALUE`, for `ULongRange`
   `ULong.MAX_VALUE..ULong.MIN_VALUE`. The runtime answered `1..0`, the SIGNED types' empty
   range, so `(5u until 0u).first` was 1 where Kotlin says 4294967295. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL("range_unsigned_until_empty: " literal "\n");                              \
        }                                                                                          \
    } while (0)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    KRef uints = kt_uint_range_until(5, 0);
    CHECK(kt_range_is_empty(uints), "5u until 0u is not empty");
    CHECK(kt_range_first(uints) == (kt_long)UINT32_MAX,
          "(5u until 0u).first is not UInt.MAX_VALUE");
    CHECK(kt_range_last(uints) == 0, "(5u until 0u).last is not 0u");

    KRef ulongs = kt_ulong_range_until(5, 0);
    CHECK(kt_range_is_empty(ulongs), "5uL until 0uL is not empty");
    CHECK(kt_range_first(ulongs) == (kt_long)UINT64_MAX,
          "(5uL until 0uL).first is not ULong.MAX_VALUE");
    CHECK(kt_range_last(ulongs) == 0, "(5uL until 0uL).last is not 0uL");

    /* A last bound above zero still ends one short of it. */
    KRef below = kt_uint_range_until(1, (kt_int)4000000000u);
    CHECK(kt_range_last(below) == 3999999999, "(1u until 4000000000u).last");
    CHECK(kt_range_first(kt_ulong_range_until(7, 8)) == 7, "(7uL until 8uL).first");

    /* The signed types' empty range is `1..0`, and stays so. */
    CHECK(kt_range_first(kt_int_range_until(5, INT32_MIN)) == 1, "(5 until Int.MIN_VALUE).first");
    CHECK(kt_range_first(kt_long_range_until(5, INT64_MIN)) == 1,
          "(5L until Long.MIN_VALUE).first");

    kt_sys_write(1, "OK\n", 3);
}
