/* `StringBuilder(text)` where `text` is a `CharSequence` the PROGRAM implements: the builder
   starts out holding that text, read through the sequence's own `length` and `get`. The builder
   used to read the object's fields as if it were a `kotlin.String`, which it is not, and copied
   whatever bytes that named.

   The text holds a supplementary character, so its two units must also come out joined. */
#include "later_tiers.h"

static const kt_char units[] = {'h', 0xD83D, 0xDE00, '!'};

static kt_int sequence_length(KRef self) {
    (void)self;
    return (kt_int)(sizeof(units) / sizeof(units[0]));
}

static kt_char sequence_char_at(KRef self, kt_int index) {
    (void)self;
    return units[index];
}

static const KType sequence_type = {
    .name = "Sequence",
    .name_length = sizeof("Sequence") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .walk_length = sequence_length,
    .walk_char_at = sequence_char_at,
};

void kt_program_entry(void) {
    KRef sequence = (KRef)kt_gc_allocate(&sequence_type, sizeof(KObjectHeader));
    KRef builder = kt_string_builder_with_text(sequence);
    CHECK(text_is(builder, "h\xF0\x9F\x98\x80!", 6),
          "a builder made from a program's CharSequence does not hold its text\n");

    kt_string_builder_append(builder, kt_string_utf8("?", 1));
    CHECK(text_is(builder, "h\xF0\x9F\x98\x80!?", 7),
          "the builder does not take appends after it\n");
    kt_sys_write(1, "OK\n", 3);
}
