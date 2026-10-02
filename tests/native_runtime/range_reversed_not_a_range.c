/* `reversed` on a value that is not one of the runtime's ranges or progressions ends the program
   with the same refusal as `step`, before reading that value through the range layout. */
#include "driver_support.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    (void)kt_range_reversed(kt_string_utf8("text", 4));
    kt_sys_write(1, "OK\n", 3);
}
