/* `StringBuilder(capacity)` with a NEGATIVE capacity throws what Kotlin/Native throws there:
   `IllegalArgumentException`, with no message, and makes no builder. The runtime used to read
   every negative capacity as zero and hand back an empty builder, so the exception a program
   catches there never came.

   Kotlin's common `StringBuilder(capacity: Int)` documents no exception, and the platforms differ:
   Kotlin/JVM throws Java's `NegativeArraySizeException` with the capacity as its message, and
   Kotlin/JS ignores the capacity altogether. Kotlin/Native 2.4.10's stdlib (the linux_x64 static
   cache of the distribution) compiles the constructor to `AllocArrayInstance(CharArray, capacity)`
   with no check of its own, and `AllocArrayInstance` calls `ThrowIllegalArgumentException` for a
   negative size, which throws `IllegalArgumentException()`. */
#include "later_tiers.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    static const kt_int capacities[] = {-1, -42, (kt_int)0x80000000u};
    for (unsigned at = 0; at < sizeof(capacities) / sizeof(capacities[0]); at++) {
        (void)kt_string_builder_with_capacity(capacities[at]);
        KRef thrown = kt_pending_exception();
        CHECK(thrown != NULL, "a negative capacity threw nothing\n");
        CHECK(type_of(thrown) == &kt_type_illegal_argument_exception,
              "a negative capacity threw something other than IllegalArgumentException\n");
        CHECK(kt_throwable_message(thrown) == NULL,
              "a negative capacity's exception has a message\n");
        kt_clear_pending();
    }

    /* Zero and a positive capacity still make an empty builder, which grows past what it was
       given. */
    KRef empty = kt_string_builder_with_capacity(0);
    CHECK(kt_pending_exception() == NULL, "a zero capacity threw\n");
    CHECK(text_is(empty, "", 0), "a zero-capacity builder is not empty\n");
    KRef builder = kt_string_builder_with_capacity(4);
    CHECK(kt_pending_exception() == NULL, "a positive capacity threw\n");
    (void)kt_string_builder_append(builder, kt_string_utf8("past the capacity", 17));
    CHECK(text_is(builder, "past the capacity", 17), "a builder did not grow past its capacity\n");
    kt_sys_write(1, "OK\n", 3);
}
