/* `substring` with bounds outside the text throws, which a program may catch. A negative start used
   to reach the unit walk, which read it as an index between the halves of a surrogate pair and
   ABORTED the process; and a raise was followed by the slicing it should have prevented, which
   built a string of negative length.

   The driver prints what each slice answered or threw, and the harness compares the lines with
   what `string_substring_bounds.kt` answers under the reference kotlinc. Bounds outside a `String`
   throw Kotlin/Native's type, `ArrayIndexOutOfBoundsException`, with the JVM's message
   (`begin -1, end 2, length 3`); the type is a declared divergence from the JVM's
   `StringIndexOutOfBoundsException`. */
#include "transcript.h"

static void show(const char *label, KRef slice) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(": ");
    if (thrown != NULL) {
        say_thrown(thrown);
    } else {
        say("answered \"");
        say_text(slice);
        say("\"");
    }
    say("\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef text = kt_string_utf8("abc", 3);
    show("substring(-1, 2)", kt_string_substring(text, -1, 2));
    show("substring(-1)", kt_string_substring_from(text, -1));
    show("substring(2, 1)", kt_string_substring(text, 2, 1));
    show("substring(2, 10)", kt_string_substring(text, 2, 10));
    show("substring(4)", kt_string_substring_from(text, 4));
    show("substring(1, 3)", kt_string_substring(text, 1, 3));
    show("substring(3)", kt_string_substring_from(text, 3));
    kt_sys_write(1, "OK\n", 3);
}
