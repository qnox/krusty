/* `<string.h>` through the POSIX layer: `memmove` between two separate objects in either order,
   and within one object overlapping forwards and backwards. */
#include "driver_posix.h"

static char first[16];
static char second[16];

static void check_memmove_between_objects(void) {
    memcpy(first, "abcdefghijklmno", 16);
    memcpy(second, "ABCDEFGHIJKLMNO", 16);
    DRIVER_CHECK(memmove(second, first, 16) == second && memcmp(second, "abcdefghijklmno", 16) == 0,
                 "memmove into the second object");
    memcpy(second, "ABCDEFGHIJKLMNO", 16);
    DRIVER_CHECK(memmove(first, second, 16) == first && memcmp(first, "ABCDEFGHIJKLMNO", 16) == 0,
                 "memmove into the first object");
}

static void check_memmove_within_one_object(void) {
    char text[16];
    memcpy(text, "0123456789abcde", 16);
    /* The destination after the source: copied from the end, or the source is overwritten. */
    DRIVER_CHECK(memmove(text + 3, text, 8) == text + 3 &&
                     memcmp(text, "01201234567bcde", 16) == 0,
                 "memmove to a later overlapping destination");
    memcpy(text, "0123456789abcde", 16);
    /* The destination before the source: copied from the start. */
    DRIVER_CHECK(memmove(text, text + 3, 8) == text && memcmp(text, "3456789a89abcde", 16) == 0,
                 "memmove to an earlier overlapping destination");
    memcpy(text, "0123456789abcde", 16);
    DRIVER_CHECK(memmove(text + 2, text + 2, 5) == text + 2 &&
                     memcmp(text, "0123456789abcde", 16) == 0,
                 "memmove onto itself");
    DRIVER_CHECK(memmove(text + 1, text, 0) == text + 1 && memcmp(text, "0123456789abcde", 16) == 0,
                 "memmove of nothing");
}

void kt_program_entry(void) {
    check_memmove_between_objects();
    check_memmove_within_one_object();
    driver_say("OK\n");
}
