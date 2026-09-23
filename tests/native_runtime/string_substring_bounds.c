/* `substring` with bounds outside the text is Kotlin's `IndexOutOfBoundsException`, which a program
   may catch. A negative start used to reach the unit walk, which read it as an index between the
   halves of a surrogate pair and ABORTED the process; and a raise was followed by the slicing it
   should have prevented, which built a string of negative length. */
#include "standins.h"

static void expect_index_out_of_bounds(KRef result) {
    KRef thrown = driver_take_pending();
    DRIVER_CHECK(thrown != NULL, "bounds outside the text threw nothing");
    DRIVER_CHECK(driver_type_of(thrown) == &kt_type_index_out_of_bounds_exception,
                 "bounds outside the text threw something other than IndexOutOfBoundsException");
    /* The call site reads the exception rather than the answer, but an answer that is a string at
       all must be one. */
    if (result != NULL && driver_type_of(result) == &kt_type_string) {
        kt_int byte_length = 0;
        (void)driver_text(result, &byte_length);
        DRIVER_CHECK(byte_length >= 0, "bounds outside the text built a string of negative length");
    }
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef text = kt_string_utf8("abc", 3);

    expect_index_out_of_bounds(kt_string_substring(text, -1, 2));
    expect_index_out_of_bounds(kt_string_substring_from(text, -1));
    expect_index_out_of_bounds(kt_string_substring(text, 2, 1));
    expect_index_out_of_bounds(kt_string_substring(text, 2, 10));
    expect_index_out_of_bounds(kt_string_substring_from(text, 4));

    /* Bounds inside the text still slice it. */
    DRIVER_CHECK(driver_text_is(kt_string_substring(text, 1, 3), "bc", 2), "substring(1, 3)");
    DRIVER_CHECK(driver_text_is(kt_string_substring_from(text, 3), "", 0), "substring(3)");
    DRIVER_CHECK(driver_take_pending() == NULL, "bounds inside the text threw");

    kt_sys_write(1, "OK\n", 3);
}
