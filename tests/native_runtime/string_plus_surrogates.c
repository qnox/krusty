/* A character above U+FFFF built from its two `Char`s is the same text as the literal that spells
   it: `"" + '\uD83D' + '\uDE00' == "😀"`. A lone surrogate is stored as the three bytes its code
   unit encodes to — UTF-8 has no form for half a character — and concatenation used to leave two
   such halves side by side, six bytes that no literal holds and that are not UTF-8 on output. */
#include "standins.h"

#define GRINNING "\xF0\x9F\x98\x80" /* U+1F600, which UTF-16 writes as D83D DE00 */
#define HIGH "\xED\xA0\xBD"          /* the lone high surrogate D83D */
#define LOW "\xED\xB8\x80"           /* the lone low surrogate DE00 */

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef empty = kt_string_utf8("", 0);

    /* One `Char` at a time, as `for (c in s) sb += c` and `"" + a + b` build it. */
    KRef joined = kt_string_plus(kt_string_plus(empty, kt_box_char(0xD83D)), kt_box_char(0xDE00));
    DRIVER_CHECK(driver_text_is(joined, GRINNING, 4), "high + low is not the character they spell");

    /* With text on either side, and with the halves arriving as strings. */
    KRef framed = kt_string_plus(kt_string_plus(kt_string_utf8("a", 1), kt_box_char(0xD83D)),
                                 kt_string_utf8(LOW "b", 4));
    DRIVER_CHECK(driver_text_is(framed, "a" GRINNING "b", 6), "a + high + (low + b)");
    KRef halves = kt_string_plus(kt_string_utf8("x" HIGH, 4), kt_string_utf8(LOW, 3));
    DRIVER_CHECK(driver_text_is(halves, "x" GRINNING, 5), "(x + high) + low as strings");

    /* Halves that do not form a pair stay as they are: a low one first, a high one followed by
       something other than a low one, and two high ones. */
    KRef backwards = kt_string_plus(kt_box_char(0xDE00), kt_box_char(0xD83D));
    DRIVER_CHECK(driver_text_is(backwards, LOW HIGH, 6), "low + high changed");
    KRef unpaired = kt_string_plus(kt_string_utf8(HIGH, 3), kt_string_utf8("\xEE\x80\x80", 3));
    DRIVER_CHECK(driver_text_is(unpaired, HIGH "\xEE\x80\x80", 6), "high + U+E000 changed");
    KRef twice = kt_string_plus(kt_string_utf8(HIGH, 3), kt_string_utf8(HIGH, 3));
    DRIVER_CHECK(driver_text_is(twice, HIGH HIGH, 6), "high + high changed");

    /* A BMP character on its own still renders as itself. */
    DRIVER_CHECK(driver_text_is(kt_string_plus(empty, kt_box_char(0x20AC)), "\xE2\x82\xAC", 3),
                 "a BMP character");

    kt_sys_write(1, "OK\n", 3);
}
