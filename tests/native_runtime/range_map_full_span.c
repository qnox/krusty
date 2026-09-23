/* `(Long.MIN_VALUE..Long.MAX_VALUE).map { it }` has 2^64 elements, which no list can hold: Kotlin
   runs out of memory, and this runtime says the range is too long to collect. The count used to be
   formed as `span / step + 1` in 64 bits, which wraps to ZERO for the full span with a step of one,
   so the cap never fired and the map answered an empty list without a word.

   The test requires the runtime's abort; reaching the end of this driver fails it. */
#include "collections_later_tiers.h"

static KRef identity_invoke(KRef self, KRef element) {
    (void)self;
    return element;
}

FUNCTION_VALUE(identity, identity_invoke)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    KRef mapped = kt_iterable_map(kt_long_range(INT64_MIN, INT64_MAX), identity);
    CHECK(kt_list_size(mapped) != 0, "the full Long range mapped to an empty list\n");
    KT_SYS_FAIL("the full Long range mapped to a list at all\n");
}
