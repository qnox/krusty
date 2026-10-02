/* The companion object of every built-in type Kotlin gives one, `UByte`, `UShort`, `UInt` and
   `ULong` among them: one object each, the same one every time, distinct from every other, whose
   class is named `kotlin.<Type>.Companion` with the simple name `Companion`. The unsigned four
   used to have none.

   The driver prints each companion's class names, how many of the thirteen each one is, and whether
   the unsigned types' are their signed types', and the harness compares the lines with what
   `companion_objects.kt` answers under the reference kotlinc. */
#include "collections_later_tiers.h"
#include "transcript.h"

#define COMPANIONS 13

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef (*const makers[COMPANIONS])(void) = {
        kt_byte_companion,  kt_short_companion,  kt_int_companion,    kt_long_companion,
        kt_char_companion,  kt_boolean_companion, kt_float_companion, kt_double_companion,
        kt_string_companion, kt_ubyte_companion, kt_ushort_companion, kt_uint_companion,
        kt_ulong_companion};

    KRef all[COMPANIONS];
    for (int at = 0; at < COMPANIONS; at++) {
        all[at] = makers[at]();
        KRef literal = kt_class_of(all[at]);
        say_value(kt_class_qualified_name(literal));
        say(" ");
        say_value(kt_class_simple_name(literal));
        say("\n");
    }
    say("[");
    for (int at = 0; at < COMPANIONS; at++) {
        int count = 0;
        for (int other = 0; other < COMPANIONS; other++) {
            count += all[other] == all[at];
        }
        say(at == 0 ? "" : ", ");
        say_long(count);
        CHECK(makers[at]() == all[at], "a companion is not the same object every time\n");
    }
    say("]\n");
    say_bool(kt_uint_companion() == kt_uint_companion());
    say(" ");
    say_bool(kt_uint_companion() == kt_int_companion());
    say(" ");
    say_bool(kt_ulong_companion() == kt_long_companion());
    say("\n");

    kt_sys_write(1, "OK\n", 3);
}
