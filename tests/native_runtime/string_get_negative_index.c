/* `s[index]` with a NEGATIVE index is Kotlin's `IndexOutOfBoundsException`. The walk that finds a
   unit asks whether `index` falls before the end of the current character, which every negative
   index does on the first one, so `"abc"[-1]` used to answer `'a'` with nothing thrown. */
#include "standins.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef text = kt_string_utf8("abc", 3);

    const kt_int negatives[] = {-1, -100, (kt_int)0x80000000u};
    for (unsigned at = 0; at < sizeof(negatives) / sizeof(negatives[0]); at++) {
        (void)kt_string_get(text, negatives[at]);
        KRef thrown = driver_take_pending();
        DRIVER_CHECK(thrown != NULL, "a negative index threw nothing");
        DRIVER_CHECK(driver_type_of(thrown) == &kt_type_index_out_of_bounds_exception,
                     "a negative index threw something other than IndexOutOfBoundsException");
    }

    /* The indices inside the text still answer, and the one past its end still throws. */
    DRIVER_CHECK(kt_string_get(text, 0) == 'a' && kt_string_get(text, 2) == 'c',
                 "an index inside the text answered the wrong unit");
    DRIVER_CHECK(driver_take_pending() == NULL, "an index inside the text threw");
    (void)kt_string_get(text, 3);
    DRIVER_CHECK(driver_take_pending() != NULL, "the index past the end threw nothing");

    kt_sys_write(1, "OK\n", 3);
}
