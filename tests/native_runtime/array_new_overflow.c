/* An array whose size in bytes does not fit the allocator's 32-bit request is memory the runtime
   cannot provide, and the runtime says so the way it says so for any exhausted heap. The size used
   to be multiplied out in 32 bits, so `LongArray(600_000_000)` — 4.8e9 bytes — wrapped to about
   half a gigabyte, the allocation succeeded, and the array claimed six hundred million elements
   over it. The test requires the runtime's abort; reaching the end of this driver fails it. */
#include "standins.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef array = kt_array_new(&kt_type_long_array, 600000000);
    (void)array;
    static const char said[] = "kt_array_new returned an array too small for its length\n";
    kt_sys_write(1, said, sizeof(said) - 1);
}
