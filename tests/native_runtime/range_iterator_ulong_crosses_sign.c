/* Walking a `ULongRange` across 2^63. The iterator stepped with `kt_long` arithmetic, where the
   step from `Long.MAX_VALUE` to the next `ULong` is a SIGNED overflow — undefined behaviour, which
   an optimizing build is entitled to turn into anything and a trapping sanitizer build reports.
   The answers checked here are what Kotlin's `ULongProgressionIterator` gives. */
#include "krusty_rt.h"
#include "krusty_sys.h"

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

/* Name the walk that went wrong on stderr; the caller's `KT_SYS_FAIL` then says how. Counted by
   hand, since a freestanding driver has no `strlen` to link against. */
static void name_walk(const char *what) {
    size_t length = 0;
    while (what[length] != 0) {
        length++;
    }
    kt_sys_write(2, what, length);
}

/* Walk `range` and require exactly the `count` values at `expected`. */
static void expect_walk(KRef range, const uint64_t *expected, int count, const char *what) {
    KRef iterator = kt_range_iterator(range);
    for (int index = 0; index < count; index++) {
        if (!kt_range_iterator_has_next(iterator)
            || (uint64_t)kt_range_iterator_next(iterator) != expected[index]) {
            name_walk(what);
            KT_SYS_FAIL(": a wrong element\n");
        }
    }
    if (kt_range_iterator_has_next(iterator)) {
        name_walk(what);
        KT_SYS_FAIL(": an element past the last\n");
    }
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    const uint64_t crossing[] = {0x7FFFFFFFFFFFFFFFu, 0x8000000000000000u};
    expect_walk(kt_ulong_range((kt_long)crossing[0], (kt_long)crossing[1]), crossing, 2,
                "Long.MAX_VALUE.toULong()..(Long.MAX_VALUE.toULong() + 1uL)");

    const uint64_t descending[] = {0x8000000000000000u, 0x7FFFFFFFFFFFFFFFu};
    expect_walk(kt_ulong_range_down_to((kt_long)descending[0], (kt_long)descending[1]), descending,
                2, "(Long.MAX_VALUE.toULong() + 1uL) downTo Long.MAX_VALUE.toULong()");

    /* Two steps of `Long.MAX_VALUE` from zero: the second lands at 2^64 - 2, which as a `Long`
       sum is the overflow itself. */
    const uint64_t wide[] = {0, 0x7FFFFFFFFFFFFFFFu, 0xFFFFFFFFFFFFFFFEu};
    expect_walk(kt_range_step(kt_ulong_range(0, (kt_long)UINT64_MAX), INT64_MAX), wide, 3,
                "0uL..ULong.MAX_VALUE step Long.MAX_VALUE");

    kt_sys_write(1, "OK\n", 3);
}
