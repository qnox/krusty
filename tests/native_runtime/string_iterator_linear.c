/* Walking a string is linear in its length, and yields its UTF-16 units: a character above U+FFFF
   is its two surrogates, high first. The string iterator used to find each unit by rescanning the
   UTF-8 text from its first byte, and to recount the length on every `hasNext`, so this walk --
   two hundred thousand units -- took tens of billions of steps; it now keeps its place in the text.

   Both protocols are walked: the general one, which answers boxed `Char`s, and the narrow one a
   `CharIterator` is used through. A builder is still walked by index, because a builder can be
   written to while it is walked and a place in its bytes would then point into the wrong text. */
#include "collections_later_tiers.h"

/* One of each width: 'a', U+00E9, U+20AC and U+1F600, which is five UTF-16 units in ten bytes. */
static const char pattern[] = "a\xC3\xA9\xE2\x82\xAC\xF0\x9F\x98\x80";
static const kt_char units[] = {'a', 0x00E9, 0x20AC, 0xD83D, 0xDE00};
#define PATTERN_BYTES 10
#define REPEATS 40000

static char text[PATTERN_BYTES * REPEATS];

static void walk_general(KRef over, int repeats) {
    KRef walk = kt_iterable_iterator(over);
    for (int unit = 0; unit < 5 * repeats; unit++) {
        CHECK(kt_iterator_has_next(walk), "the general walk ended early\n");
        CHECK(kt_unbox_char(kt_iterator_next(walk)) == units[unit % 5],
              "the general walk yielded the wrong unit\n");
    }
    CHECK(!kt_iterator_has_next(walk), "the general walk went past the end\n");
    CHECK(kt_pending_exception() == NULL, "the general walk raised\n");
}

static void walk_narrow(KRef over, int repeats) {
    KRef walk = kt_iterable_iterator(over);
    for (int unit = 0; unit < 5 * repeats; unit++) {
        CHECK(kt_range_iterator_has_next(walk), "the narrow walk ended early\n");
        CHECK(kt_range_iterator_next(walk) == units[unit % 5],
              "the narrow walk yielded the wrong unit\n");
    }
    CHECK(!kt_range_iterator_has_next(walk), "the narrow walk went past the end\n");
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    for (int repeat = 0; repeat < REPEATS; repeat++) {
        for (int at = 0; at < PATTERN_BYTES; at++) {
            text[repeat * PATTERN_BYTES + at] = pattern[at];
        }
    }
    KRef string = kt_string_utf8(text, PATTERN_BYTES * REPEATS);
    walk_general(string, REPEATS);
    walk_narrow(string, REPEATS);

    KRef builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_string_utf8(pattern, PATTERN_BYTES));
    kt_string_builder_append(builder, kt_string_utf8(pattern, PATTERN_BYTES));
    walk_general(builder, 2);
    walk_narrow(builder, 2);
    kt_sys_write(1, "OK\n", 3);
}
