/* `kt_string_range_equals` is Kotlin/Native's `Kotlin_String_unsafeRangeEquals`: whether `length`
   UTF-16 units of one string from an offset equal another's from its offset, once `regionMatches`
   has checked both ranges. The walk counts units, not bytes, so an offset or a length may stop
   between the halves of a surrogate pair, and a lone half compares with the half of a whole pair.

   The driver prints each answer, and the harness compares the lines with what
   `string_range_equals.kt` answers under the reference kotlinc. */
#include "transcript.h"

#define GRINNING "\xF0\x9F\x98\x80"       /* U+1F600, which UTF-16 writes as D83D DE00 */
#define GRINNING_SMILE "\xF0\x9F\x98\x81" /* U+1F601, D83D DE01 */
#define HIGH "\xED\xA0\xBD"                 /* the lone high surrogate D83D */
#define LOW "\xED\xB8\x80"                  /* the lone low surrogate DE00 */
#define E_ACUTE "\xC3\xA9"                 /* U+00E9, one unit in two bytes */

typedef struct RangeCase {
    const char *self;
    kt_int self_length;
    kt_int self_offset;
    const char *other;
    kt_int other_length;
    kt_int other_offset;
    kt_int length;
} RangeCase;

void kt_program_entry(void) {
    DRIVER_BEGIN();
    static const RangeCase cases[] = {
        {"kotlin native", 13, 7, "a native b", 10, 2, 6},
        {"kotlin native", 13, 0, "kotlix", 6, 0, 6},
        {"kotlin native", 13, 3, "lin", 3, 0, 0},
        {"kotlin native", 13, 10, "tive", 4, 0, 3},
        {"a" GRINNING "b", 6, 2, LOW "b", 4, 0, 2},
        {"a" GRINNING "b", 6, 1, GRINNING, 4, 0, 2},
        {"a" GRINNING "b", 6, 1, GRINNING_SMILE, 4, 0, 2},
        {"a" GRINNING "b", 6, 1, "x" HIGH, 4, 1, 1},
        {E_ACUTE "t" E_ACUTE, 5, 1, "t" E_ACUTE, 3, 0, 2},
    };
    for (unsigned at = 0; at < sizeof(cases) / sizeof(cases[0]); at++) {
        const RangeCase *range = &cases[at];
        KRef self = kt_string_utf8(range->self, range->self_length);
        KRef other = kt_string_utf8(range->other, range->other_length);
        kt_boolean equal = kt_string_range_equals(self, range->self_offset, other,
                                                  range->other_offset, range->length);
        say_long(at);
        say(": ");
        say_bool(equal);
        say("\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
