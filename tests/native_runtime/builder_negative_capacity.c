/* `StringBuilder(capacity)` with a NEGATIVE capacity throws what the JVM throws: Java's builder
   allocates its storage as `new byte[capacity]`, so the exception is that allocation's,
   `java.lang.NegativeArraySizeException`, and its message is the capacity in decimal. The runtime
   used to read every negative capacity as zero and hand back an empty builder, so the exception a
   program catches there never came. */
#include "later_tiers.h"

typedef struct Case {
    kt_int capacity;
    const char *message;
    kt_int message_length;
} Case;

void kt_program_entry(void) {
    static const Case cases[] = {
        {-1, "-1", 2},
        {-42, "-42", 3},
        {(kt_int)0x80000000u, "-2147483648", 11},
    };
    for (unsigned at = 0; at < sizeof(cases) / sizeof(cases[0]); at++) {
        (void)kt_string_builder_with_capacity(cases[at].capacity);
        KRef thrown = kt_pending_exception();
        CHECK(thrown != NULL, "a negative capacity threw nothing\n");
        CHECK(type_of(thrown) == &kt_type_negative_array_size_exception,
              "a negative capacity threw something other than NegativeArraySizeException\n");
        /* Held in a local, so the text allocated to compare it with cannot collect it. */
        KRef message = kt_throwable_message(thrown);
        CHECK(message != NULL && text_is(message, cases[at].message, cases[at].message_length),
              "a negative capacity's message is not the capacity\n");
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
