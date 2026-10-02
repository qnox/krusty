/* `s[index]` with a NEGATIVE index throws, as it does past the end. The walk that finds a unit asks
   whether `index` falls before the end of the current character, which every negative index does
   on the first one, so `"abc"[-1]` used to answer `'a'` with nothing thrown.

   The driver prints what each index answered or threw, and the harness compares the lines with
   what `string_get_negative_index.kt` answers under the reference kotlinc. An index outside a
   `String` throws Kotlin/Native's type, `ArrayIndexOutOfBoundsException`, with the JVM's message;
   the type is a declared divergence from the JVM's `StringIndexOutOfBoundsException`. */
#include "transcript.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef text = kt_string_utf8("abc", 3);
    const kt_int indices[] = {-1, -100, (kt_int)0x80000000u, 0, 2, 3};
    for (unsigned at = 0; at < sizeof(indices) / sizeof(indices[0]); at++) {
        kt_char unit = kt_string_get(text, indices[at]);
        KRef thrown = kt_pending_exception();
        kt_clear_pending();
        say("\"abc\"[");
        say_long(indices[at]);
        say("]: ");
        if (thrown != NULL) {
            say_thrown(thrown);
        } else {
            char one[2] = {(char)unit, 0};
            say("answered ");
            say(one);
        }
        say("\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
