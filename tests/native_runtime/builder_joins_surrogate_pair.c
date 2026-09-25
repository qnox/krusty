/* A supplementary character appended to a builder one `Char` at a time -- `for (c in s)
   sb.append(c)` over any text holding an emoji -- is stored as the character's own four-byte
   UTF-8, so the builder's text equals the literal. Each half used to be rendered alone and copied
   as-is, six bytes of CESU-8 that no literal equals and no terminal prints.

   The rule is a join at the tail: a low surrogate arriving right after a stored high one becomes
   one character with it. Anything else stays as it came. */
#include "later_tiers.h"

#define HIGH 0xD83D /* the leading half of U+1F600 */
#define LOW 0xDE00  /* its trailing half */

void kt_program_entry(void) {
    /* The pair, one unit per append. */
    KRef builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_box_char(HIGH));
    kt_string_builder_append(builder, kt_box_char(LOW));
    CHECK(text_is(builder, "\xF0\x9F\x98\x80", 4),
          "a pair appended unit by unit is not the character's UTF-8\n");
    CHECK(kt_string_length(builder) == 2, "the joined pair is not two UTF-16 units long\n");

    /* A low half at the START of appended text joins as well: `"" + '\uDE00'` is such a text. */
    builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_string_utf8("a", 1));
    kt_string_builder_append(builder, kt_box_char(HIGH));
    kt_string_builder_append(builder, kt_string_utf8("\xED\xB8\x80!", 4));
    CHECK(text_is(builder, "a\xF0\x9F\x98\x80!", 6),
          "a pair split across a char and a string is not joined\n");

    /* Only the LAST high half is the one a low half completes. */
    builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_box_char(HIGH));
    kt_string_builder_append(builder, kt_box_char(HIGH));
    kt_string_builder_append(builder, kt_box_char(LOW));
    CHECK(text_is(builder, "\xED\xA0\xBD\xF0\x9F\x98\x80", 7),
          "a low half joined with something other than the high half before it\n");

    /* Nothing else joins: a high half followed by an ordinary character, and a low half with no
       high half before it, stay the lone units they are. */
    builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_box_char(HIGH));
    kt_string_builder_append(builder, kt_box_char('x'));
    kt_string_builder_append(builder, kt_box_char(LOW));
    CHECK(text_is(builder, "\xED\xA0\xBDx\xED\xB8\x80", 7),
          "units that are not a pair were rewritten\n");
    kt_sys_write(1, "OK\n", 3);
}
