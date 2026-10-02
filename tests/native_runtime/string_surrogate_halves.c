/* Kotlin indexes text by UTF-16 unit, so a bound may fall between the two halves of a character
   above U+FFFF, and a search may look for one half on its own. The JVM answers both: `"😀"` cut at
   unit 1 is its lone high surrogate and its lone low one, and `"😀"` contains, starts with and ends
   with the matching half. A substring bound there used to end the program, and the searches
   compared bytes, which answer false: the character's four bytes share none of a lone half's three.
   A lone half is stored the way a surrogate `Char` renders — the three bytes its code unit encodes
   to — which is the form every answer here takes. */
#include "standins.h"

#define GRINNING "\xF0\x9F\x98\x80" /* U+1F600, which UTF-16 writes as D83D DE00 */
#define HIGH "\xED\xA0\xBD"          /* the lone high surrogate D83D */
#define LOW "\xED\xB8\x80"           /* the lone low surrogate DE00 */
#define OTHER_HIGH "\xED\xA0\xBE"    /* D83E, the high half of a different character */

static KRef text(const char *bytes, kt_int length) { return kt_string_utf8(bytes, length); }

#define TEXT(literal) text(literal, (kt_int)sizeof(literal) - 1)

/* Whether `result` is a string holding exactly `literal`, with no exception raised on the way. */
#define IS(result, literal)                                                                        \
    (driver_text_is((result), literal, (kt_int)sizeof(literal) - 1)                                \
     && driver_take_pending() == NULL)

/* Whether the call that answered `result` raised `IndexOutOfBoundsException`. */
/* A `String`'s bounds outside the text raise Kotlin/Native's `ArrayIndexOutOfBoundsException`. */
static kt_boolean out_of_bounds(KRef result) {
    (void)result;
    KRef thrown = driver_take_pending();
    return thrown != NULL &&
           driver_type_of(thrown) == &kt_type_array_index_out_of_bounds_exception;
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef emoji = TEXT(GRINNING);

    /* Into each half, both halves, and neither. */
    DRIVER_CHECK(IS(kt_string_substring(emoji, 0, 1), HIGH), "\"😀\".substring(0, 1)");
    DRIVER_CHECK(IS(kt_string_substring(emoji, 1, 2), LOW), "\"😀\".substring(1, 2)");
    DRIVER_CHECK(IS(kt_string_substring(emoji, 0, 2), GRINNING), "\"😀\".substring(0, 2)");
    DRIVER_CHECK(IS(kt_string_substring(emoji, 1, 1), ""), "\"😀\".substring(1, 1)");
    DRIVER_CHECK(IS(kt_string_substring_from(emoji, 1), LOW), "\"😀\".substring(1)");
    DRIVER_CHECK(IS(kt_string_substring_from(emoji, 0), GRINNING), "\"😀\".substring(0)");
    DRIVER_CHECK(IS(kt_string_substring_from(emoji, 2), ""), "\"😀\".substring(2)");

    /* With text around the pair, and across two pairs. */
    KRef framed = TEXT("a" GRINNING "b");
    DRIVER_CHECK(IS(kt_string_substring(framed, 1, 2), HIGH), "\"a😀b\".substring(1, 2)");
    DRIVER_CHECK(IS(kt_string_substring(framed, 2, 4), LOW "b"), "\"a😀b\".substring(2, 4)");
    DRIVER_CHECK(IS(kt_string_substring(framed, 0, 2), "a" HIGH), "\"a😀b\".substring(0, 2)");
    DRIVER_CHECK(IS(kt_string_substring(framed, 2, 3), LOW), "\"a😀b\".substring(2, 3)");
    DRIVER_CHECK(IS(kt_string_substring(framed, 1, 3), GRINNING), "\"a😀b\".substring(1, 3)");
    DRIVER_CHECK(IS(kt_string_substring_from(framed, 2), LOW "b"), "\"a😀b\".substring(2)");
    KRef twice = TEXT(GRINNING GRINNING);
    DRIVER_CHECK(IS(kt_string_substring(twice, 1, 3), LOW HIGH), "\"😀😀\".substring(1, 3)");
    DRIVER_CHECK(IS(kt_string_substring(twice, 1, 4), LOW GRINNING), "\"😀😀\".substring(1, 4)");

    /* The two halves concatenated back together are the character again. */
    DRIVER_CHECK(IS(kt_string_plus(kt_string_substring(emoji, 0, 1),
                                   kt_string_substring(emoji, 1, 2)),
                    GRINNING),
                 "the halves do not rejoin");

    /* A bound past the end is still out of bounds, whether or not the other one splits a pair. */
    DRIVER_CHECK(out_of_bounds(kt_string_substring(emoji, 1, 3)),
                 "\"😀\".substring(1, 3) is not out of bounds");
    DRIVER_CHECK(out_of_bounds(kt_string_substring(emoji, -1, 1)),
                 "\"😀\".substring(-1, 1) is not out of bounds");
    DRIVER_CHECK(out_of_bounds(kt_string_substring_from(emoji, 3)),
                 "\"😀\".substring(3) is not out of bounds");

    /* The searches see each half inside the whole character. */
    DRIVER_CHECK(kt_string_contains(emoji, TEXT(HIGH)), "\"😀\".contains(\"\\uD83D\")");
    DRIVER_CHECK(kt_string_contains(emoji, TEXT(LOW)), "\"😀\".contains(\"\\uDE00\")");
    DRIVER_CHECK(kt_string_starts_with(emoji, TEXT(HIGH)), "\"😀\".startsWith(\"\\uD83D\")");
    DRIVER_CHECK(kt_string_ends_with(emoji, TEXT(LOW)), "\"😀\".endsWith(\"\\uDE00\")");
    DRIVER_CHECK(kt_string_contains(framed, TEXT(LOW "b")), "\"a😀b\".contains(\"\\uDE00b\")");
    DRIVER_CHECK(kt_string_ends_with(framed, TEXT(LOW "b")), "\"a😀b\".endsWith(\"\\uDE00b\")");
    DRIVER_CHECK(kt_string_starts_with(framed, TEXT("a" HIGH)), "\"a😀b\".startsWith(\"a\\uD83D\")");
    /* A receiver holding lone halves is searched by unit too. */
    DRIVER_CHECK(kt_string_contains(TEXT(LOW HIGH), TEXT(HIGH)),
                 "\"\\uDE00\\uD83D\".contains(\"\\uD83D\")");
    DRIVER_CHECK(kt_string_starts_with(TEXT(HIGH "x"), TEXT(HIGH)),
                 "\"\\uD83Dx\".startsWith(\"\\uD83D\")");

    /* ... and not a half in the wrong place, nor another character's half. */
    DRIVER_CHECK(!kt_string_starts_with(emoji, TEXT(LOW)), "\"😀\".startsWith(\"\\uDE00\")");
    DRIVER_CHECK(!kt_string_ends_with(emoji, TEXT(HIGH)), "\"😀\".endsWith(\"\\uD83D\")");
    DRIVER_CHECK(!kt_string_contains(emoji, TEXT(OTHER_HIGH)), "\"😀\".contains(\"\\uD83E\")");
    DRIVER_CHECK(!kt_string_contains(TEXT("abc"), TEXT(HIGH)), "\"abc\".contains(\"\\uD83D\")");
    DRIVER_CHECK(!kt_string_ends_with(TEXT(LOW), TEXT(GRINNING)), "\"\\uDE00\".endsWith(\"😀\")");
    DRIVER_CHECK(!kt_string_contains(framed, TEXT(HIGH "b")), "\"a😀b\".contains(\"\\uD83Db\")");

    /* `removeSuffix` cuts the pair when the suffix is its low half, and leaves the text alone when
       the suffix is a half it does not end with. */
    DRIVER_CHECK(IS(kt_string_remove_suffix(emoji, TEXT(LOW)), HIGH),
                 "\"😀\".removeSuffix(\"\\uDE00\")");
    DRIVER_CHECK(IS(kt_string_remove_suffix(TEXT("a" GRINNING), TEXT(LOW)), "a" HIGH),
                 "\"a😀\".removeSuffix(\"\\uDE00\")");
    DRIVER_CHECK(kt_string_remove_suffix(emoji, TEXT(HIGH)) == emoji,
                 "\"😀\".removeSuffix(\"\\uD83D\") is not the receiver");
    DRIVER_CHECK(IS(kt_string_remove_suffix(framed, TEXT(LOW "b")), "a" HIGH),
                 "\"a😀b\".removeSuffix(\"\\uDE00b\")");

    /* Text with no lone half anywhere takes the answers it always did. */
    KRef hello = TEXT("h\xC3\xA9llo");
    DRIVER_CHECK(kt_string_starts_with(hello, TEXT("h\xC3\xA9")), "\"héllo\".startsWith(\"hé\")");
    DRIVER_CHECK(!kt_string_starts_with(hello, TEXT("lo")), "\"héllo\".startsWith(\"lo\")");
    DRIVER_CHECK(kt_string_ends_with(hello, TEXT("lo")), "\"héllo\".endsWith(\"lo\")");
    DRIVER_CHECK(!kt_string_ends_with(hello, TEXT("h")), "\"héllo\".endsWith(\"h\")");
    DRIVER_CHECK(kt_string_contains(hello, TEXT("\xC3\xA9l")), "\"héllo\".contains(\"él\")");
    DRIVER_CHECK(!kt_string_contains(hello, TEXT("lx")), "\"héllo\".contains(\"lx\")");
    DRIVER_CHECK(kt_string_contains(hello, TEXT("")), "\"héllo\".contains(\"\")");
    DRIVER_CHECK(kt_string_contains(framed, emoji), "\"a😀b\".contains(\"😀\")");
    DRIVER_CHECK(IS(kt_string_remove_suffix(hello, TEXT("lo")), "h\xC3\xA9l"),
                 "\"héllo\".removeSuffix(\"lo\")");
    DRIVER_CHECK(kt_string_remove_suffix(hello, TEXT("x")) == hello,
                 "\"héllo\".removeSuffix(\"x\") is not the receiver");
    DRIVER_CHECK(IS(kt_string_substring(hello, 1, 3), "\xC3\xA9l"), "\"héllo\".substring(1, 3)");
    DRIVER_CHECK(IS(kt_string_substring_from(framed, 1), GRINNING "b"), "\"a😀b\".substring(1)");

    DRIVER_CHECK(driver_take_pending() == NULL, "a search threw");
    kt_sys_write(1, "OK\n", 3);
}
