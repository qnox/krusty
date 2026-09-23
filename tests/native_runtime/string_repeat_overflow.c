/* `s.repeat(n)` whose text would not fit in memory the runtime can provide ends the process as an
   exhausted heap does. The length used to be multiplied out in signed 32 bits: ten bytes three
   hundred million times wrapped negative and reported a "negative array size" nobody asked for,
   and other counts wrapped to a buffer smaller than the copies written into it. The test requires
   the runtime's abort; reaching the end of this driver fails it. */
#include "standins.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef repeated = kt_string_repeat(kt_string_utf8("abcdefghij", 10), 300000000);
    (void)repeated;
    static const char said[] = "kt_string_repeat returned\n";
    kt_sys_write(1, said, sizeof(said) - 1);
}
