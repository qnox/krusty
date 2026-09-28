/* `step` on a value that is not one of the runtime's ranges or progressions ends the program with
   a message saying so, rather than indexing the progression table with the kind of no range. */
#include "standins.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    (void)kt_range_step(kt_string_utf8("text", 4), 2);
    kt_sys_write(1, "OK\n", 3);
}
