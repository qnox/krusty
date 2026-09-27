/* A `ReadWriteProperty` read whose receiver is neither of the two delegates the runtime builds
   is a loud failure naming the receiver's descriptor. It used to take every object that was not an
   observable for a `Delegates.notNull()` and read its first field as the value. */
#include "standins.h"

/* A delegate of the program's own: one reference field where a `NotNullVar` keeps its value. */
typedef struct Custom {
    KObjectHeader header;
    KRef held;
} Custom;

static const KType custom_type = {.name = "pkg.CustomDelegate",
                                  .name_length = sizeof("pkg.CustomDelegate") - 1,
                                  .instance_size = sizeof(Custom),
                                  .super = &kt_type_any};

void kt_program_entry(void) {
    DRIVER_BEGIN();
    Custom *custom = (Custom *)kt_gc_allocate(&custom_type, sizeof(Custom));
    custom->held = kt_string_utf8("held", 4);
    (void)kt_rw_property_get((KRef)custom, kt_string_utf8("x", 1));
    kt_sys_write(1, "OK\n", 3);
}
