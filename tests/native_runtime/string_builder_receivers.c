/* `substring` and `removeSuffix` are declared on `CharSequence`, so a `StringBuilder` reaches them
   as a receiver and, for `removeSuffix`, as the suffix. They used to read a string's own fields off
   whatever arrived, which on a builder name its storage array and its length rather than its text.
   A string cut from a builder must also be a COPY: a view into the builder's storage would change
   under the program the next time the builder does. */
#include "driver_support.h"

#define GRINNING "\xF0\x9F\x98\x80" /* U+1F600, which UTF-16 writes as D83D DE00 */
#define HIGH "\xED\xA0\xBD"          /* the lone high surrogate D83D */

/* Overwrite every byte of the builder's text, as an `append` that replaced it would. */
static void scribble(KRef builder) {
    kt_int length = ((DriverStringBuilder *)builder)->byte_length;
    char *body = driver_builder_bytes(builder);
    for (kt_int index = 0; index < length; index++) {
        body[index] = '#';
    }
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    /* `StringBuilder("hello").substring(1, 4)`, `.substring(2)` and `.removeSuffix("lo")`. */
    KRef builder = driver_builder_of("hello", 5);
    KRef middle = kt_string_substring(builder, 1, 4);
    KRef rest = kt_string_substring_from(builder, 2);
    KRef removed = kt_string_remove_suffix(builder, kt_string_utf8("lo", 2));
    KRef unchanged = kt_string_remove_suffix(builder, kt_string_utf8("x", 1));
    DRIVER_CHECK(driver_take_pending() == NULL, "a builder receiver threw");
    DRIVER_CHECK(driver_type_of(middle) == &kt_type_string, "substring(1, 4) is not a string");
    DRIVER_CHECK(driver_text_is(middle, "ell", 3), "builder.substring(1, 4)");
    DRIVER_CHECK(driver_text_is(rest, "llo", 3), "builder.substring(2)");
    DRIVER_CHECK(driver_text_is(removed, "hel", 3), "builder.removeSuffix(\"lo\")");
    /* A builder that does not end with the suffix answers a string of its own, not itself. */
    DRIVER_CHECK(unchanged != builder && driver_type_of(unchanged) == &kt_type_string,
                 "builder.removeSuffix(\"x\") handed back the builder");
    DRIVER_CHECK(driver_text_is(unchanged, "hello", 5), "builder.removeSuffix(\"x\")");

    /* Every one of them owns its text: changing the builder's bytes changes none of them. */
    scribble(builder);
    DRIVER_CHECK(driver_text_is(middle, "ell", 3), "substring(1, 4) changed with the builder");
    DRIVER_CHECK(driver_text_is(rest, "llo", 3), "substring(2) changed with the builder");
    DRIVER_CHECK(driver_text_is(removed, "hel", 3), "removeSuffix changed with the builder");
    DRIVER_CHECK(driver_text_is(unchanged, "hello", 5), "an unmatched removeSuffix changed too");

    /* A builder SUFFIX against a string receiver: `"hello".removeSuffix(StringBuilder("lo"))`, and
       one it does not end with, which answers the receiver itself. */
    KRef hello = kt_string_utf8("hello", 5);
    KRef hel = kt_string_remove_suffix(hello, driver_builder_of("lo", 2));
    DRIVER_CHECK(driver_text_is(hel, "hel", 3), "\"hello\".removeSuffix(StringBuilder(\"lo\"))");
    DRIVER_CHECK(kt_string_remove_suffix(hello, driver_builder_of("he", 2)) == hello,
                 "\"hello\".removeSuffix(StringBuilder(\"he\")) is not the receiver");

    /* A builder cut between the halves of a pair owns its text as well. */
    KRef emoji = driver_builder_of("a" GRINNING "b", 6);
    KRef high = kt_string_substring(emoji, 1, 2);
    scribble(emoji);
    DRIVER_CHECK(driver_text_is(high, HIGH, 3), "StringBuilder(\"a😀b\").substring(1, 2)");

    DRIVER_CHECK(driver_take_pending() == NULL, "a builder receiver threw");
    kt_sys_write(1, "OK\n", 3);
}
