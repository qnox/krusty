/* A spread copy whose source is not an array of the destination's kind ends the program, rather
   than reading a length and elements out of fields the object does not have. A spread Kotlin
   compiles always hands the vararg's own array type, so this is a fault in whatever emitted the
   call, never a state a program can reach. */
#include "driver_support.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef destination = kt_array_new(&kt_type_int_array, 4);
    (void)kt_array_copy_into(destination, 0, kt_string_utf8("text", 4));
    kt_sys_write(1, "OK\n", 3);
}
