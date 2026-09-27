/* `a until 0u` is empty, and the empty range Kotlin answers is its declared `EMPTY`: for
   `UIntRange` that is `UInt.MAX_VALUE..UInt.MIN_VALUE`, for `ULongRange`
   `ULong.MAX_VALUE..ULong.MIN_VALUE`. The runtime answered `1..0`, the SIGNED types' empty
   range, so `(5u until 0u).first` was 1 where Kotlin says 4294967295.

   The driver prints each answer, an unsigned bound read as the unsigned number it is, and the
   harness compares the lines with what `range_unsigned_until_empty.kt` answers under the reference
   kotlinc. */
#include "transcript.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef uints = kt_uint_range_until(5, 0);
    KRef ulongs = kt_ulong_range_until(5, 0);
    say_bool(kt_range_is_empty(uints));
    say(" ");
    say_ulong((uint64_t)kt_range_first(uints));
    say(" ");
    say_ulong((uint64_t)kt_range_last(uints));
    say(" ");
    say_bool(kt_range_is_empty(ulongs));
    say(" ");
    say_ulong((uint64_t)kt_range_first(ulongs));
    say(" ");
    say_ulong((uint64_t)kt_range_last(ulongs));
    say("\n");

    /* A last bound above zero still ends one short of it; the signed types' empty range is `1..0`,
       and stays so. */
    say_ulong((uint64_t)kt_range_last(kt_uint_range_until(1, (kt_int)4000000000u)));
    say(" ");
    say_ulong((uint64_t)kt_range_first(kt_ulong_range_until(7, 8)));
    say(" ");
    say_long(kt_range_first(kt_int_range_until(5, INT32_MIN)));
    say(" ");
    say_long(kt_range_first(kt_long_range_until(5, INT64_MIN)));
    say("\n");

    CHECK(kt_pending_exception() == NULL, "an unsigned until raised\n");
    kt_sys_write(1, "OK\n", 3);
}
