/* Walking a `ULongRange` across 2^63. The iterator stepped with `kt_long` arithmetic, where the
   step from `Long.MAX_VALUE` to the next `ULong` is a SIGNED overflow — undefined behaviour, which
   an optimizing build is entitled to turn into anything and a trapping sanitizer build reports.
   The answers checked here are what Kotlin's `ULongProgressionIterator` gives.

   The driver prints each walk as a list, and the harness compares the lines with what
   `range_iterator_ulong_crosses_sign.kt` answers under the reference kotlinc. */
#include "transcript.h"

/* The range iterator first asks whether it was handed one of the array and string walks, which a
   later tier defines. Until that tier lands the calls resolve here, and answer the way the real
   ones do for an iterator that is not a walk. They are WEAK, and the runtime defines the real ones
   with internal linkage, so from that tier on its calls never reach these. */
__attribute__((weak)) kt_boolean kt_walk_is(KRef iterator) {
    (void)iterator;
    return false;
}

__attribute__((weak)) kt_boolean kt_walk_has_next(KRef iterator) {
    (void)iterator;
    KT_SYS_FAIL("range_iterator_ulong_crosses_sign: not a walk\n");
    return false;
}

__attribute__((weak)) kt_long kt_walk_next_long(KRef iterator) {
    (void)iterator;
    KT_SYS_FAIL("range_iterator_ulong_crosses_sign: not a walk\n");
    return 0;
}

/* Walk `range` and print its elements as `[a, b, ...]`. Every walk here has at most three, so a
   fourth is a walk that went past its end, and printing on would not end. */
static void say_walk(KRef range) {
    KRef iterator = kt_range_iterator(range);
    say("[");
    for (kt_int count = 0; kt_range_iterator_has_next(iterator); count++) {
        CHECK(count < 3, "a walk went past its last element\n");
        say(count == 0 ? "" : ", ");
        say_ulong((uint64_t)kt_range_iterator_next(iterator));
    }
    say("]\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    say_walk(kt_ulong_range(INT64_MAX, (kt_long)0x8000000000000000u));
    say_walk(kt_ulong_range_down_to((kt_long)0x8000000000000000u, INT64_MAX));
    /* Two steps of `Long.MAX_VALUE` from zero: the second lands at 2^64 - 2, which as a `Long`
       sum is the overflow itself. */
    say_walk(kt_range_step(kt_ulong_range(0, (kt_long)UINT64_MAX), INT64_MAX));

    CHECK(kt_pending_exception() == NULL, "a walk raised\n");
    kt_sys_write(1, "OK\n", 3);
}
