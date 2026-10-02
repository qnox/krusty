/* Two strings that each fit can be too long together. `kt_string_plus` summed their lengths in
   signed 32 bits, so the sum could wrap to a small or negative length, and the copies that followed
   wrote past the array allocated for it. Text longer than any array can hold is memory the runtime
   cannot provide, so the runtime ends the program as it does for an exhausted heap.

   The two operands are stand-in string headers declaring lengths no heap here could hold, with no
   text behind them: the guard has to come before either payload is read, or the driver faults on
   the NULL bytes instead of ending with the runtime's message. The test requires the runtime's
   abort; reaching the end of this driver fails it. */
#include "driver_support.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    static DriverValue left = {
        .header = {.type = &kt_type_string},
        .as.string = {.storage = NULL, .bytes = NULL, .byte_length = 0x7FFFFFF0},
    };
    static DriverValue right = {
        .header = {.type = &kt_type_string},
        .as.string = {.storage = NULL, .bytes = NULL, .byte_length = 0x7FFFFFF0},
    };
    KRef joined = kt_string_plus((KRef)&left, (KRef)&right);
    (void)joined;
    static const char said[] = "kt_string_plus returned\n";
    kt_sys_write(1, said, sizeof(said) - 1);
}
